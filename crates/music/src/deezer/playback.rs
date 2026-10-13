//! Playback for Deezer: what [`crate::engine`] needs that is Deezer's own.
//!
//! One thing, really. The body arrives encrypted in 2048-byte stripes, so the stream decrypts
//! each one as it lands and what the buffer holds is always in the clear. From there rodio
//! decodes an ordinary file, and the threads, the queue, the preload and the gapless join are
//! the engine's.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use rodio::Source as _;

use crate::cache::{CachedMeta, StreamCache, Tee};
use crate::deezer::client::DeezerClient;
use crate::deezer::{Stream, Striped};
use crate::engine::{self, Fetch, Loudness};
use crate::stream::Reader;
use crate::{PlaybackConfig, PlaybackEvents, PlaybackFactory, Player};

/// A track downloading and decrypting, and how long and how loud Deezer says it is.
#[derive(Clone)]
pub struct Loaded {
    stream: Stream,
    duration: Option<Duration>,
    loudness: Option<Loudness>,
}

pub struct Factory {
    client: DeezerClient,
}

impl Factory {
    pub fn new(client: DeezerClient) -> Self {
        Self { client }
    }
}

impl PlaybackFactory for Factory {
    fn start(&self, config: PlaybackConfig) -> (Box<dyn Player>, Box<dyn PlaybackEvents>) {
        engine::start(
            Deezer {
                client: self.client.clone(),
                cache: config.stream_cache.clone(),
            },
            config,
        )
    }
}

struct Deezer {
    client: DeezerClient,
    cache: Option<StreamCache>,
}

#[async_trait]
impl Fetch for Deezer {
    type Loaded = Loaded;
    type Source = rodio::Decoder<Reader<Striped>>;

    fn name(&self) -> &'static str {
        "deezer"
    }

    /// Resolves the stream url and its key together, since one answer carries both, and starts
    /// the download decrypting as it arrives. A cached track skips all of that: its stored
    /// bytes are still the encrypted body, so they go back through the same decrypting
    /// stream and not one request is made.
    async fn load(&self, id: &str) -> Result<Loaded> {
        if let Some(hit) = self.cache.as_ref().and_then(|cache| cache.get(id)) {
            let key = self.client.stream_key(&hit.meta.track);
            let total = hit.bytes;
            let meta = hit.meta.clone();
            let stream = Stream::pulling(hit, Some(total), Striped::new(&key))
                .primed()
                .await?;
            return Ok(Loaded {
                stream,
                duration: meta.duration(),
                loudness: meta.loudness(),
            });
        }
        let opened = self.client.open_stream(id).await?;
        let total = opened.response.content_length();
        let meta = CachedMeta {
            track: opened.id.clone(),
            duration_secs: opened.duration.map(|d| d.as_secs_f64()),
            lufs: opened.loudness.map(|loudness| loudness.lufs),
            ..CachedMeta::default()
        };
        let store = self.cache.as_ref().and_then(|cache| cache.store(id, meta));
        let stream = match store {
            Some(store) => {
                Stream::pulling(
                    Tee::new(opened.response, store),
                    total,
                    Striped::new(&opened.key),
                )
                .primed()
                .await?
            }
            None => Stream::open(opened.response, Striped::new(&opened.key)).await?,
        };
        Ok(Loaded {
            stream,
            duration: opened.duration,
            loudness: opened.loudness,
        })
    }

    fn length(&self, loaded: &Loaded) -> Option<Duration> {
        loaded.duration
    }

    fn loudness(&self, loaded: &Loaded) -> Option<Loudness> {
        loaded.loudness
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
                log::warn!("playback: cannot decode the deezer track {id}: {error}");
                return None;
            }
        };
        log::debug!(
            "playback: {id} decodes at {} Hz, {} channels, {:?} long",
            decoder.sample_rate(),
            decoder.channels(),
            decoder.total_duration()
        );
        if !at.is_zero()
            && let Err(error) = decoder.try_seek(at)
        {
            log::warn!(
                "playback: cannot start the deezer track {id} at {}s: {error}",
                at.as_secs()
            );
        }
        Some(decoder)
    }
}
