//! Playback for a Subsonic server: what [`crate::engine`] needs that is Subsonic's own.
//!
//! Almost nothing, as it turns out. The server hands over an ordinary audio file, so the stream
//! takes the bytes as they come and rodio decodes them. The threads, the queue, the preload and
//! the gapless join are the engine's.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use crate::cache::{CachedMeta, StreamCache, Tee};
use crate::engine::{self, Fetch, Loudness};
use crate::stream::{Plain, Reader, Stream};
use crate::subsonic::client::{Details, SubsonicClient};
use crate::{PlaybackConfig, PlaybackEvents, PlaybackFactory, Player};

/// A track downloading, and what the server says about its length and loudness.
#[derive(Clone)]
pub struct Loaded {
    stream: Stream,
    details: Details,
}

pub struct Factory {
    client: SubsonicClient,
}

impl Factory {
    pub fn new(client: SubsonicClient) -> Self {
        Self { client }
    }
}

impl PlaybackFactory for Factory {
    fn start(&self, config: PlaybackConfig) -> (Box<dyn Player>, Box<dyn PlaybackEvents>) {
        engine::start(
            Subsonic {
                client: self.client.clone(),
                cache: config.stream_cache.clone(),
            },
            config,
        )
    }
}

struct Subsonic {
    client: SubsonicClient,
    cache: Option<StreamCache>,
}

#[async_trait]
impl Fetch for Subsonic {
    type Loaded = Loaded;
    type Source = rodio::Decoder<Reader>;

    fn name(&self) -> &'static str {
        "subsonic"
    }

    /// Opens the stream and asks for the length and loudness at the same time, so neither round
    /// trip waits on the other. The preroll usually covers the lookup entirely. A cached
    /// track skips both requests and streams from the stored file instead.
    async fn load(&self, id: &str) -> Result<Loaded> {
        if let Some(hit) = self.cache.as_ref().and_then(|cache| cache.get(id)) {
            let total = hit.bytes;
            let meta = hit.meta.clone();
            let stream = Stream::pulling(hit, Some(total), Plain).primed().await?;
            return Ok(Loaded {
                stream,
                details: Details {
                    duration: meta.duration(),
                    loudness: meta.loudness(),
                },
            });
        }
        let (response, details) =
            tokio::join!(self.client.open_stream(id), self.client.details(id),);
        let response = response?;
        let total = response.content_length();
        let meta = CachedMeta {
            track: id.to_owned(),
            duration_secs: details.duration.map(|d| d.as_secs_f64()),
            lufs: details.loudness.map(|loudness| loudness.lufs),
            ..CachedMeta::default()
        };
        let stream = match self.cache.as_ref().and_then(|cache| cache.store(id, meta)) {
            Some(store) => {
                Stream::pulling(Tee::new(response, store), total, Plain)
                    .primed()
                    .await?
            }
            None => Stream::open(response, Plain).await?,
        };
        Ok(Loaded { stream, details })
    }

    fn length(&self, loaded: &Loaded) -> Option<Duration> {
        loaded.details.duration
    }

    fn loudness(&self, loaded: &Loaded) -> Option<Loudness> {
        loaded.details.loudness
    }

    async fn downloaded(&self, loaded: &Loaded) {
        loaded.stream.finished().await;
    }

    /// Builds a decoder over a stream and places it at `at`. The bytes past the preroll are
    /// still arriving, so this only reads the header.
    fn open(&self, id: &str, loaded: &Loaded, at: Duration) -> Option<Self::Source> {
        let mut builder = rodio::Decoder::builder()
            .with_data(loaded.stream.reader())
            .with_seekable(true);
        if let Some(total) = loaded.stream.total() {
            builder = builder.with_byte_len(total);
        }
        let mut decoder = match builder.build() {
            Ok(decoder) => decoder,
            Err(error) => {
                log::warn!("playback: cannot decode the subsonic track {id}: {error}");
                return None;
            }
        };
        if !at.is_zero()
            && let Err(error) = rodio::Source::try_seek(&mut decoder, at)
        {
            log::warn!(
                "playback: cannot start the subsonic track {id} at {}s: {error}",
                at.as_secs()
            );
        }
        Some(decoder)
    }
}
