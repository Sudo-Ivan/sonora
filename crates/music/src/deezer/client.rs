//! Deezer data access over two APIs: the public `api.deezer.com` for the catalog, and the
//! web gateway `gw-light.php` (what the browser player uses) for anything scoped to the
//! account - profile, favorites, playlists - plus the `media.deezer.com/v1/get_url` call that
//! hands out the encrypted stream urls.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tokio::time::Instant;

use crate::deezer::{decrypt, wire};
use crate::engine::Loudness;
use crate::{
    Album, AlbumCatalogue, AlbumDetail, Artist, ArtistCatalogue, ArtistProfile, ArtistRef,
    Encoding, Genre, GenreDetail, GenreItem, GenreSection, HomeFeed, MediaKind, MusicApi, Offer,
    Playlist, PlaylistDetail, Quality, SUGGESTIONS, SavedArtist, Track, UserDetail, UserProfile,
    distinct_covers, escape,
};

const GATEWAY: &str = "https://www.deezer.com/ajax/gw-light.php";
const PUBLIC: &str = "https://api.deezer.com";
const MEDIA: &str = "https://media.deezer.com/v1/get_url";

/// The audio formats `get_url` knows, best first. The license decides which ones come back.
const FORMATS: [&str; 3] = ["FLAC", "MP3_320", "MP3_128"];

/// The levels Deezer serves, best first, one for each of `FORMATS`.
pub(crate) const QUALITIES: &[Offer] = &[
    Offer {
        quality: Quality::Lossless,
        encoding: Encoding::Flac {
            depth: 16,
            rate: 44_100,
        },
        up_to: false,
    },
    Offer {
        quality: Quality::High,
        encoding: Encoding::Mp3 { kbps: 320 },
        up_to: false,
    },
    Offer {
        quality: Quality::Normal,
        encoding: Encoding::Mp3 { kbps: 128 },
        up_to: false,
    },
];

/// The one format user-uploaded files are served as.
const UPLOAD_FORMAT: &str = "MP3_MISC";

/// How many favorites a library page asks for at once.
const LIBRARY_PAGE: u32 = 2000;

/// How many releases an artist page asks for. Deezer answers this many in one page, and even
const PORTRAIT_LIMIT: usize = 24;
/// How many artist portraits the client remembers at once.
const PORTRAITS: u64 = 4096;
/// How many related artists lend their albums to a thin rail, and how many albums each
/// lends.
const SIMILAR_ARTISTS: usize = 6;
const SIMILAR_RELEASES: usize = 2;

/// How many releases one `album.getDiscography` page holds, and the most pages fetched:
/// the api answers in small pages and an exhaustive discography is a few hundred albums.
const DISCOGRAPHY_PAGE: u32 = 100;
const DISCOGRAPHY_MAX: u32 = 300;

/// The id prefixes `page.get` items and curated radios get, so `playlist` can tell a real
/// playlist id from the endless mixes and the account's Flow.
const RADIO_PREFIX: &str = "radio:";
const SMART_PREFIX: &str = "smarttracklist:";
const FLOW_ID: &str = "flow";
const CHANNEL_PREFIX: &str = "channel:";

/// The smart tracklists Deezer deals for an account: the weekly discoveries, the monthly
/// top and the mixes inspired by the artists the listener keeps coming back to. They are
/// the service's own mixes, listed on home under one shelf.
const SMART_TRACKLISTS: [&str; 8] = [
    "discovery",
    "new-releases",
    "monthly-top",
    "inspired-by-1",
    "inspired-by-2",
    "inspired-by-3",
    "inspired-by-4",
    "inspired-by-5",
];

/// The public api allows fifty calls per five seconds from one address, so calls leave one
/// at a time this far apart. A call above the limit waits for its slot instead of being
/// refused.
const PACE: Duration = Duration::from_millis(110);

/// How often a refused public call is asked again, and how long the first wait is. Each
/// retry doubles the wait, so a call gives up about seven seconds after the first refusal.
const RETRIES: u32 = 3;
const BACKOFF: Duration = Duration::from_secs(1);

/// The public api's error codes for a call that goes through once asked again later.
const QUOTA_ERROR: u64 = 4;
const SERVICE_BUSY: u64 = 700;

/// A public api refusal that clears on its own: the quota is spent or the service is busy.
/// The code is Deezer's own, or the HTTP status when the refusal never reached JSON.
#[derive(Debug)]
struct Busy(u64);

impl fmt::Display for Busy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the deezer api is busy (code {})", self.0)
    }
}

impl std::error::Error for Busy {}

/// The account-scoped half of the handshake `deezer.getUserData` answers.
struct Session {
    /// The `sid` session cookie `deezer.ping` hands out; the `checkForm` token is bound to
    /// it, so every gateway call has to carry both.
    sid: String,
    profile: UserProfile,
    /// The `checkForm` token every later gateway call carries.
    api_token: String,
    license_token: String,
    /// Where in `FORMATS` the best format the plan covers sits. It stays 0, every format, when
    /// the account says nothing about its plan.
    plan: usize,
}

#[derive(Clone)]
pub struct DeezerClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    arl: String,
    session: RwLock<Session>,
    /// The master stream secret, fetched once from the web player bundle at connect time.
    secret: OnceLock<decrypt::Secret>,
    /// The earliest moment the next public call may leave. Every call moves it forward by
    /// `PACE`, and a refusal pushes it out by the backoff, so every caller waits it out.
    slot: Mutex<Instant>,
    /// Artist portraits already looked up, so a search that ranks the same artists on
    /// every keystroke does not spend the quota on them again. Bounded: the least used
    /// entries leave first, so a long session does not grow it forever.
    portraits: moka::sync::Cache<String, String>,
}

/// A track's audio as it starts to arrive, with the key that decrypts it and what the gateway
/// said about its length and loudness.
pub struct Opened {
    /// The id the stream answers for, which a region fallback may not share with the
    /// requested id.
    pub id: String,
    pub response: reqwest::Response,
    pub key: decrypt::Secret,
    /// The id the stream belongs to, which differs from the one asked for when Deezer
    /// substitutes another release of the same recording. The key is derived from it.
    pub effective_id: String,
    pub duration: Option<Duration>,
    /// The gateway's `GAIN`. Despite the name it is the track's integrated loudness rather than
    /// an adjustment, and it lands within about a decibel of the LUFS other services report for
    /// the same recording.
    pub loudness: Option<Loudness>,
}

impl DeezerClient {
    /// Signs in with an `arl` cookie: fetches the user data (which validates the cookie) and
    /// the stream secret. An invalid or expired arl answers a user id of 0.
    pub async fn connect(arl: &str) -> Result<Self> {
        let client = Self::unopened(arl)?;
        client.ping().await?;
        client.refresh().await?;
        if client.inner.session.read().await.profile.id.is_empty() {
            bail!("the arl was refused; sign in to deezer.com again");
        }
        let secret = decrypt::secret(&client.inner.http).await;
        // set once here, before the client is cloned anywhere
        client.inner.secret.set(secret).ok();
        Ok(client)
    }

    /// A client for `arl` that has not reached Deezer, holding the last known stream secret. It
    /// can derive the key of a downloaded track and do nothing else until it connects.
    pub fn offline(arl: &str) -> Result<Self> {
        let client = Self::unopened(arl)?;
        client.inner.secret.set(decrypt::FALLBACK).ok();
        Ok(client)
    }

    fn unopened(arl: &str) -> Result<Self> {
        let http = crate::tls::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:142.0) Gecko/20100101 Firefox/142.0")
            .build()
            .context("cannot build the deezer http client")?;
        let client = Self {
            inner: Arc::new(Inner {
                http,
                arl: arl.to_owned(),
                session: RwLock::new(Session {
                    sid: String::new(),
                    profile: UserProfile {
                        id: String::new(),
                        display_name: String::new(),
                        avatar: None,
                    },
                    api_token: String::new(),
                    license_token: String::new(),
                    plan: 0,
                }),
                secret: OnceLock::new(),
                slot: Mutex::new(Instant::now()),
                portraits: moka::sync::Cache::new(PORTRAITS),
            }),
        };
        Ok(client)
    }

    /// Opens the session: `deezer.ping` answers a `SESSION` id that goes out as the `sid`
    /// cookie from here on. Without it the gateway refuses authenticated calls with a CSRF
    /// error, because the `checkForm` token is bound to that session.
    async fn ping(&self) -> Result<()> {
        let url = format!("{GATEWAY}?method=deezer.ping&input=3&api_version=1.0&api_token=");
        let text = self
            .inner
            .http
            .get(url)
            .header("Cookie", format!("arl={}", self.inner.arl))
            .send()
            .await
            .context("cannot reach the deezer gateway for deezer.ping")?
            .text()
            .await
            .context("cannot read the deezer gateway answer")?;
        let answer: Value = serde_json::from_str(&text)
            .context("the deezer gateway answered deezer.ping with no json")?;
        let sid = wire::text(&answer["results"], &["SESSION"])
            .unwrap_or_default()
            .to_owned();
        self.inner.session.write().await.sid = sid;
        Ok(())
    }

    /// Pulls a fresh `checkForm` api token and license token from `deezer.getUserData`.
    async fn refresh(&self) -> Result<()> {
        let results = self.gw_raw("deezer.getUserData", "", json!({})).await?;
        let user = &results["USER"];
        let profile = wire::profile(user);
        let api_token = wire::text(&results, &["checkForm"])
            .unwrap_or_default()
            .to_owned();
        let license_token = wire::text(&user["OPTIONS"], &["license_token"])
            .unwrap_or_default()
            .to_owned();
        let plan = plan(&user["OPTIONS"]);
        let mut session = self.inner.session.write().await;
        session.profile = profile;
        session.api_token = api_token;
        session.license_token = license_token;
        session.plan = plan;
        Ok(())
    }

    /// The format a track is first offered in at `quality` on this account's plan, which is the
    /// one it streams as unless the track lacks it.
    pub async fn served(&self, quality: Quality) -> &'static str {
        let plan = self.inner.session.read().await.plan;
        let first = FORMATS.len() - formats_from(quality).len();
        FORMATS[first.max(plan).min(FORMATS.len() - 1)]
    }

    async fn user_id(&self) -> String {
        self.inner.session.read().await.profile.id.clone()
    }

    /// A gateway call with the current api token. One retry with a refreshed token when the
    /// first answer complains, which is how an expired `checkForm` shows up.
    async fn gw(&self, method: &str, body: Value) -> Result<Value> {
        let token = self.inner.session.read().await.api_token.clone();
        match self.gw_raw(method, &token, body.clone()).await {
            Ok(results) => Ok(results),
            Err(first) => {
                self.refresh().await.ok();
                let token = self.inner.session.read().await.api_token.clone();
                self.gw_raw(method, &token, body).await.map_err(|_| first)
            }
        }
    }

    /// One gateway round trip: POST JSON to `gw-light.php` with the `arl` and `sid` cookies.
    /// The gateway answers HTTP 200 to almost everything, so the `error` field is what
    /// actually decides.
    async fn gw_raw(&self, method: &str, api_token: &str, body: Value) -> Result<Value> {
        let cid = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|at| at.subsec_nanos())
            .unwrap_or(0);
        let url = format!(
            "{GATEWAY}?method={}&input=3&api_version=1.0&api_token={}&cid={cid}",
            escape::component(method),
            escape::component(api_token)
        );
        let sid = self.inner.session.read().await.sid.clone();
        let text = self
            .inner
            .http
            .post(url)
            .header("Cookie", format!("arl={}; sid={sid}", self.inner.arl))
            .header("Content-Type", "text/plain;charset=UTF-8")
            .body(body.to_string())
            .send()
            .await
            .with_context(|| format!("cannot reach the deezer gateway for {method}"))?
            .text()
            .await
            .context("cannot read the deezer gateway answer")?;
        let answer: Value = serde_json::from_str(&text)
            .with_context(|| format!("the deezer gateway answered {method} with no json"))?;
        if let Some(error) = answer.get("error") {
            let trouble = match error {
                Value::Object(map) if !map.is_empty() => Some(error.to_string()),
                Value::Array(list) if !list.is_empty() => Some(error.to_string()),
                _ => None,
            };
            if let Some(trouble) = trouble {
                bail!("deezer {method} refused the call: {trouble}");
            }
        }
        Ok(answer.get("results").cloned().unwrap_or(Value::Null))
    }

    /// A public REST call, paced to the api's quota and asked again with a growing wait
    /// when the quota is spent. Fails after `RETRIES` refusals in a row, or at once on any
    /// other error.
    async fn public(&self, path: &str) -> Result<Value> {
        let mut attempt = 0;
        loop {
            self.pace().await;
            let error = match self.public_once(path).await {
                Ok(answer) => return Ok(answer),
                Err(error) => error,
            };
            let Some(Busy(code)) = error.downcast_ref::<Busy>() else {
                return Err(error);
            };
            if attempt >= RETRIES {
                return Err(error).with_context(|| {
                    format!("deezer api {path} stayed busy through {RETRIES} retries")
                });
            }
            let wait = BACKOFF * 2u32.pow(attempt);
            log::debug!(
                "deezer: {path} refused with code {code}, asking again in {}ms",
                wait.as_millis()
            );
            self.hold(wait);
            attempt += 1;
        }
    }

    /// Takes the next slot on the public api and waits until it comes up.
    async fn pace(&self) {
        let slot = {
            let mut next = lock(&self.inner.slot);
            let slot = (*next).max(Instant::now());
            *next = slot + PACE;
            slot
        };
        tokio::time::sleep_until(slot).await;
    }

    /// Holds every public call back for `wait`, so the calls queued behind a refusal do not
    /// each run into the same spent quota.
    fn hold(&self, wait: Duration) {
        let mut next = lock(&self.inner.slot);
        *next = (*next).max(Instant::now() + wait);
    }

    /// One public round trip. The api answers HTTP 200 with an `error` object on failure,
    /// and a quota or busy code comes back as `Busy` so `public` can ask again.
    async fn public_once(&self, path: &str) -> Result<Value> {
        let url = format!("{PUBLIC}{path}");
        let response = self
            .inner
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("cannot reach {url}"))?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(Busy(response.status().as_u16().into()).into());
        }
        let text = response
            .text()
            .await
            .context("cannot read the deezer api answer")?;
        let answer: Value = serde_json::from_str(&text)
            .with_context(|| format!("the deezer api answered {path} with no json"))?;
        if let Some(error) = answer.get("error")
            && error.is_object()
        {
            if let Some(code @ (QUOTA_ERROR | SERVICE_BUSY)) = wire::number(error, &["code"]) {
                return Err(Busy(code).into());
            }
            bail!("deezer api {path} refused the call: {error}");
        }
        Ok(answer)
    }

    /// A `page.get` call for one of the layout pages the web app is built from: `home` for
    /// the listener's own feed, `channels/<slug>` for a curated hub. The page spec rides in
    /// the `gateway_input` url parameter as JSON, which is the one call that takes its
    /// arguments that way rather than in the posted body.
    async fn page(&self, page: &str) -> Result<Value> {
        let token = self.inner.session.read().await.api_token.clone();
        let input = json!({
            "PAGE": page,
            "VERSION": "2.3",
            "SUPPORT": {
                "grid": ["channel", "album", "playlist", "artist", "smarttracklist"],
                "horizontal-grid": ["album", "playlist", "channel", "track", "smarttracklist"],
                "slider": ["album", "channel"],
            },
            "LANG": "en",
        });
        let url = format!(
            "{GATEWAY}?method=page.get&input=3&api_version=1.0&api_token={}&gateway_input={}",
            escape::component(&token),
            escape::component(&input.to_string())
        );
        let sid = self.inner.session.read().await.sid.clone();
        let text = self
            .inner
            .http
            .post(url)
            .header("Cookie", format!("arl={}; sid={sid}", self.inner.arl))
            .header("Content-Type", "text/plain;charset=UTF-8")
            .body("{}")
            .send()
            .await
            .with_context(|| format!("cannot reach the deezer gateway for page {page}"))?
            .text()
            .await
            .context("cannot read the deezer gateway answer")?;
        let answer: Value = serde_json::from_str(&text)
            .with_context(|| format!("the deezer gateway answered page {page} with no json"))?;
        if let Some(error) = answer.get("error") {
            let trouble = match error {
                Value::Object(map) if !map.is_empty() => Some(error.to_string()),
                Value::Array(list) if !list.is_empty() => Some(error.to_string()),
                _ => None,
            };
            if let Some(trouble) = trouble {
                bail!("deezer page.get {page} refused the call: {trouble}");
            }
        }
        Ok(answer.get("results").cloned().unwrap_or(Value::Null))
    }

    /// One `deezer.pageProfile` tab: the user's favorite albums, artists or playlists,
    /// including the private ones the public api hides.
    async fn profile_tab(&self, tab: &str) -> Result<Vec<Value>> {
        let user_id = self.user_id().await;
        let results = self
            .gw(
                "deezer.pageProfile",
                json!({ "user_id": user_id, "tab": tab, "nb": LIBRARY_PAGE }),
            )
            .await?;
        Ok(results
            .get("TAB")
            .and_then(|tabs| tabs.get(tab))
            .and_then(|section| section.get("data"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    async fn favorite(&self, method: &str, key: &str, id: &str) -> Result<()> {
        self.gw(method, json!({ key: id }))
            .await
            .with_context(|| format!("deezer {method} refused the change"))?;
        Ok(())
    }

    /// Up to `SUGGESTIONS` of the artist's own albums without the album the page is already
    /// showing. One more is asked for, since the page's album may be among them.
    async fn more_from_artist(&self, album_id: &str, artist_id: &str) -> Result<Vec<Album>> {
        let page = self
            .public(&format!(
                "/artist/{}/albums?limit={}",
                escape::component(artist_id),
                SUGGESTIONS + 1
            ))
            .await
            .with_context(|| format!("cannot load more from artist {artist_id}"))?;
        Ok(page
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(wire::album)
            .filter(|album| album.id != album_id)
            .take(SUGGESTIONS)
            .collect())
    }

    /// Up to `SUGGESTIONS` artists Deezer lists as related, for the rail's artists tab.
    async fn similar_artists(&self, artist_id: &str) -> Result<Vec<SavedArtist>> {
        let page = self
            .public(&format!(
                "/artist/{}/related?limit={SUGGESTIONS}",
                escape::component(artist_id)
            ))
            .await
            .with_context(|| format!("cannot load artists related to {artist_id}"))?;
        Ok(page
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|artist| {
                Some(SavedArtist {
                    id: artist.get("id").and_then(wire::id)?,
                    name: artist.get("name").and_then(Value::as_str)?.to_owned(),
                    cover: artist
                        .get("picture_big")
                        .or_else(|| artist.get("picture_medium"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    added_at: None,
                })
            })
            .take(SUGGESTIONS)
            .collect())
    }

    /// One `deezer.pageArtist` call: the artist's own overview as the web app draws it, with
    /// biography, fan count, top tracks and the first discography page. Unlike the public
    /// api's albums list, the sections here hold only what belongs to the artist itself.
    async fn artist_page(&self, artist_id: &str) -> Result<Value> {
        self.gw(
            "deezer.pageArtist",
            json!({
                "art_id": artist_id,
                "lang": "en",
                "header": true,
                "tab": 0,
            }),
        )
        .await
        .with_context(|| format!("cannot load the deezer artist {artist_id}"))
    }

    /// The whole of an artist's own discography, paged through `album.getDiscography` since
    /// `deezer.pageArtist` lists only the first stretch. An album that keeps showing up
    /// under another page's artist credit never lands here, which is what keeps a page free
    /// of other artists' releases.
    async fn discography(&self, artist_id: &str) -> Result<Vec<Album>> {
        let mut albums = Vec::new();
        let mut start = 0;
        loop {
            let page = self
                .gw(
                    "album.getDiscography",
                    json!({
                        "art_id": artist_id,
                        "discography_mode": "all",
                        "nb": DISCOGRAPHY_PAGE,
                        "nb_songs": 0,
                        "start": start,
                    }),
                )
                .await
                .with_context(|| format!("cannot load the discography of {artist_id}"))?;
            let batch: Vec<Album> = page
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(wire::album)
                .collect();
            let fetched = batch.len() as u32;
            albums.extend(batch);
            let total = wire::number(&page, &["total"]).unwrap_or(albums.len() as u64) as u32;
            start += DISCOGRAPHY_PAGE;
            if fetched < DISCOGRAPHY_PAGE || start >= total || start >= DISCOGRAPHY_MAX {
                break;
            }
        }
        Ok(albums)
    }

    /// The account's Flow: Deezer's endless personal mix, as a track listing.
    async fn flow_tracks(&self) -> Result<Vec<Track>> {
        let user_id = self.user_id().await;
        let path = format!("/user/{}/flow?limit=40", escape::component(&user_id));
        let page = self
            .public(&path)
            .await
            .context("cannot load the deezer flow")?;
        Ok(wire::track_list(&page))
    }

    /// Flow as a playlist page, for the `flow` synthetic id.
    async fn flow_playlist(&self) -> Result<PlaylistDetail> {
        let tracks = self.flow_tracks().await?;
        Ok(PlaylistDetail {
            playlist: Playlist {
                id: FLOW_ID.to_owned(),
                name: "Flow".to_owned(),
                owner: "Deezer".to_owned(),
                owner_id: String::new(),
                owned: false,
                collaborative: false,
                blend: false,
                public: true,
                cover: tracks.iter().find_map(|track| track.cover.clone()),
                track_count: tracks.len() as u32,
                modified_at: None,
            },
            tracks,
            continuation: None,
        })
    }

    /// A genre radio as a playlist page, for `radio:` synthetic ids. The radio is endless,
    /// so the page holds one stretch of it.
    async fn radio_playlist(&self, radio_id: &str) -> Result<PlaylistDetail> {
        let radio_id = escape::component(radio_id);
        let detail_path = format!("/radio/{radio_id}");
        let tracks_path = format!("/radio/{radio_id}/tracks?limit=100");
        let (detail, tracks) = tokio::join!(self.public(&detail_path), self.public(&tracks_path));
        let mut playlist = wire::radio(&detail.context("cannot load the deezer radio")?)
            .with_context(|| format!("cannot read the deezer radio {radio_id}"))?;
        let tracks = tracks
            .map(|page| wire::track_list(&page))
            .context("cannot load the deezer radio tracks")?;
        playlist.track_count = tracks.len() as u32;
        Ok(PlaylistDetail {
            playlist,
            tracks,
            continuation: None,
        })
    }

    /// One smart tracklist as a playlist page, for `smarttracklist:` synthetic ids:
    /// `deezer.pageSmartTracklist` answers the mix's own title, blurb and cover along its
    /// tracks.
    async fn smart_playlist(&self, tracklist_id: &str) -> Result<PlaylistDetail> {
        let results = self
            .gw(
                "deezer.pageSmartTracklist",
                json!({
                    "SMARTTRACKLIST_ID": tracklist_id,
                    "nb": 100,
                    "start": 0,
                    "lang": "en",
                }),
            )
            .await
            .with_context(|| format!("cannot load the deezer mix {tracklist_id}"))?;
        let tracks = results
            .get("SONGS")
            .map(wire::track_list)
            .unwrap_or_default();
        let data = &results["DATA"];
        let title = wire::text(data, &["AUTO_GENERATED_TITLE"])
            .filter(|title| !title.is_empty())
            .or_else(|| wire::text(data, &["TITLE"]))
            .unwrap_or("Mix")
            .to_owned();
        let cover = wire::image(
            "cover",
            wire::text(&data["COVER"], &["MD5"]).or_else(|| {
                data["PICTURES"]
                    .as_array()
                    .and_then(|list| list.first())
                    .and_then(|first| wire::text(first, &["MD5"]))
            }),
            500,
        )
        .or_else(|| tracks.iter().find_map(|track| track.cover.clone()));
        let track_count = results
            .get("SONGS")
            .and_then(|songs| wire::number(songs, &["total"]))
            .unwrap_or(tracks.len() as u64) as u32;
        Ok(PlaylistDetail {
            playlist: Playlist {
                id: format!("{SMART_PREFIX}{tracklist_id}"),
                name: title,
                owner: "Deezer".to_owned(),
                owner_id: String::new(),
                owned: false,
                collaborative: false,
                blend: false,
                public: true,
                cover,
                track_count,
                modified_at: None,
            },
            tracks,
            continuation: None,
        })
    }

    /// A card for every smart tracklist the account has, named the way Deezer names them.
    /// The lookups run together, paced by the slot like every gateway call, and a tracklist
    /// the gateway does not know, or one that names nothing back, is left out.
    async fn mix_cards(&self) -> Vec<GenreItem> {
        let lookups = SMART_TRACKLISTS.iter().map(|tracklist_id| async move {
            let results = self
                .gw(
                    "deezer.pageSmartTracklist",
                    json!({
                        "SMARTTRACKLIST_ID": tracklist_id,
                        "nb": 1,
                        "start": 0,
                        "lang": "en",
                    }),
                )
                .await;
            (tracklist_id, results)
        });
        let mut items = Vec::new();
        for (tracklist_id, results) in futures::future::join_all(lookups).await {
            let data = match results {
                Ok(results) => results["DATA"].clone(),
                Err(error) => {
                    log::debug!("deezer: no {tracklist_id} mix card: {error:#}");
                    continue;
                }
            };
            let title = wire::text(&data, &["AUTO_GENERATED_TITLE"])
                .filter(|title| !title.is_empty())
                .or_else(|| wire::text(&data, &["TITLE"]))
                .unwrap_or("Mix")
                .to_owned();
            let cover = wire::image(
                "cover",
                wire::text(&data["COVER"], &["MD5"]).or_else(|| {
                    data["PICTURES"]
                        .as_array()
                        .and_then(|list| list.first())
                        .and_then(|first| wire::text(first, &["MD5"]))
                }),
                300,
            );
            items.push(GenreItem::Playlist(Playlist {
                id: format!("{SMART_PREFIX}{tracklist_id}"),
                name: title,
                owner: "Deezer".to_owned(),
                owner_id: String::new(),
                owned: false,
                collaborative: false,
                blend: false,
                public: true,
                cover,
                track_count: 0,
                modified_at: None,
            }));
        }
        items
    }

    /// The artist overview through the public api, for the odd id `deezer.pageArtist` will
    /// not answer. The albums listing it uses mixes in releases the artist only guests on,
    /// so it is strictly the fallback.
    async fn artist_public(&self, artist_id: &str) -> Result<Artist> {
        let artist = escape::component(artist_id);
        let detail_path = format!("/artist/{artist}");
        let top_path = format!("/artist/{artist}/top?limit=20");
        let albums_path = format!("/artist/{artist}/albums?limit=50");
        let (detail, top, albums) = tokio::join!(
            self.public(&detail_path),
            self.public(&top_path),
            self.public(&albums_path),
        );
        let detail = detail.context("cannot load the artist")?;
        Ok(Artist {
            name: wire::text(&detail, &["name"])
                .unwrap_or_default()
                .to_owned(),
            cover_large: detail
                .get("picture_xl")
                .or_else(|| detail.get("picture_big"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            biography: None,
            monthly_listeners: wire::number(&detail, &["nb_fan"]),
            top_tracks: top.map(|page| wire::track_list(&page)).unwrap_or_default(),
            albums: albums
                .map(|page| {
                    page.get("data")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .filter_map(wire::album)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// A home built from the public api when `page.get` brings nothing back: the service's
    /// charts plus its genre radios as the mixes shelf.
    async fn chart_sections(&self) -> Result<Vec<GenreSection>> {
        let (albums, artists, playlists, radios) = tokio::join!(
            self.public("/chart/0/albums?limit=15"),
            self.public("/chart/0/artists?limit=15"),
            self.public("/chart/0/playlists?limit=15"),
            self.public("/radio/genres"),
        );
        let mut sections = Vec::new();
        if let Ok(page) = albums {
            let items: Vec<GenreItem> = page
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(wire::album)
                .map(GenreItem::Album)
                .collect();
            if !items.is_empty() {
                sections.push(GenreSection {
                    title: "home-top-albums".to_owned(),
                    items,
                });
            }
        }
        if let Ok(page) = artists {
            let items: Vec<GenreItem> = page
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(wire::saved_artist)
                .map(GenreItem::Artist)
                .collect();
            if !items.is_empty() {
                sections.push(GenreSection {
                    title: "home-top-artists".to_owned(),
                    items,
                });
            }
        }
        if let Ok(page) = playlists {
            let items: Vec<GenreItem> = page
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|value| wire::playlist(value, ""))
                .map(GenreItem::Playlist)
                .collect();
            if !items.is_empty() {
                sections.push(GenreSection {
                    title: "home-top-playlists".to_owned(),
                    items,
                });
            }
        }
        if let Ok(page) = radios {
            let items: Vec<GenreItem> = page
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(wire::radio)
                .map(GenreItem::Playlist)
                .collect();
            if !items.is_empty() {
                sections.push(GenreSection {
                    title: "nav-mixes".to_owned(),
                    items,
                });
            }
        }
        Ok(sections)
    }

    /// The full gateway record of one track: metadata plus the `TRACK_TOKEN` playback needs.
    /// A region-locked track comes with a `FALLBACK` record of another release of the same
    /// song, and that one is what plays.
    async fn track_data(&self, track_id: &str) -> Result<Value> {
        let results = self
            .gw("song.getData", json!({ "SNG_ID": track_id }))
            .await
            .with_context(|| format!("cannot load the deezer track {track_id}"))?;
        match results
            .get("FALLBACK")
            .filter(|fallback| !fallback.is_null())
        {
            Some(fallback) => Ok(fallback.clone()),
            None => Ok(results),
        }
    }

    /// Opens the audio of a track at `quality`, or the best below it the license covers: track
    /// token, then a stream url from `get_url`, then the GET itself.
    pub async fn open_stream(&self, track_id: &str, quality: Quality) -> Result<Opened> {
        let data = self.track_data(track_id).await?;
        let effective_id = wire::id(&data["SNG_ID"])
            .with_context(|| format!("the deezer track {track_id} names no id"))?;
        let token = wire::text(&data, &["TRACK_TOKEN"])
            .filter(|token| !token.is_empty())
            .with_context(|| format!("the deezer track {track_id} is not playable"))?;
        let duration = wire::number(&data, &["DURATION"]).map(Duration::from_secs);
        let loudness = wire::decimal(&data, &["GAIN"])
            .filter(|lufs| (-70.0..0.0).contains(lufs))
            .map(|lufs| Loudness {
                lufs: lufs as f32,
                peak: None,
            });

        let license = self.inner.session.read().await.license_token.clone();
        let formats: &[&str] = match effective_id.starts_with('-') {
            true => &[UPLOAD_FORMAT],
            false => formats_from(quality),
        };
        let mut last = anyhow::anyhow!("no format was offered");
        for format in formats {
            match self.stream_url(&license, token, format).await {
                Ok(Some(url)) => {
                    log::debug!("deezer: {effective_id} streams as {format}");
                    let response = self
                        .inner
                        .http
                        .get(&url)
                        .send()
                        .await
                        .context("cannot stream the deezer track")?
                        .error_for_status()
                        .context("the deezer cdn refused the stream")?;
                    let key = self.track_key(&effective_id);
                    return Ok(Opened {
                        id: effective_id.clone(),
                        response,
                        key,
                        effective_id,
                        duration,
                        loudness,
                    });
                }
                Ok(None) => last = anyhow::anyhow!("format {format} is not licensed"),
                Err(error) => last = error,
            }
        }
        Err(last.context("the deezer track has no playable stream"))
    }

    /// The Blowfish key of the stream with id `effective_id`, which needs nothing from the network.
    pub fn track_key(&self, effective_id: &str) -> decrypt::Secret {
        let secret = self.inner.secret.get().copied().unwrap_or_default();
        decrypt::track_key(effective_id, &secret)
    }

    /// One `get_url` attempt for one format. None when the license does not cover it.
    async fn stream_url(&self, license: &str, token: &str, format: &str) -> Result<Option<String>> {
        let payload = json!({
            "license_token": license,
            "media": [{ "type": "FULL", "formats": [{ "cipher": "BF_CBC_STRIPE", "format": format }] }],
            "track_tokens": [token],
        });
        let answer: Value = self
            .inner
            .http
            .post(MEDIA)
            .json(&payload)
            .send()
            .await
            .context("cannot reach the deezer media api")?
            .json()
            .await
            .context("cannot read the deezer media api answer")?;
        let entry = &answer["data"][0];
        if let Some(errors) = entry.get("errors").and_then(Value::as_array)
            && !errors.is_empty()
        {
            return Ok(None);
        }
        Ok(entry["media"][0]["sources"][0]["url"]
            .as_str()
            .map(str::to_owned))
    }
}

#[async_trait]
impl MusicApi for DeezerClient {
    fn share_url(&self, kind: MediaKind, id: &str) -> Option<String> {
        match kind {
            MediaKind::Playlist
                if id == FLOW_ID
                    || id.starts_with(SMART_PREFIX)
                    || id.starts_with(RADIO_PREFIX) =>
            {
                None
            }
            MediaKind::Playlist => Some(format!(
                "https://www.deezer.com/playlist/{}",
                escape::component(id)
            )),
            MediaKind::Track => Some(format!(
                "https://www.deezer.com/track/{}",
                escape::component(id)
            )),
            MediaKind::Album => Some(format!(
                "https://www.deezer.com/album/{}",
                escape::component(id)
            )),
            MediaKind::Artist => Some(format!(
                "https://www.deezer.com/artist/{}",
                escape::component(id)
            )),
        }
    }

    async fn profile(&self) -> Result<UserProfile> {
        Ok(self.inner.session.read().await.profile.clone())
    }

    async fn user(&self, user_id: &str) -> Result<UserDetail> {
        let user_id = escape::component(user_id);
        let detail_path = format!("/user/{user_id}");
        let playlists_path = format!("/user/{user_id}/playlists?limit=50");
        let (detail, playlists) =
            tokio::join!(self.public(&detail_path), self.public(&playlists_path));
        let mut user = wire::user(&detail.context("cannot load the deezer user")?)
            .context("cannot read the deezer user")?;
        user.playlists = playlists
            .map(|page| {
                page.get("data")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|value| wire::playlist(value, ""))
                    .collect()
            })
            .unwrap_or_default();
        Ok(user)
    }

    async fn artist(&self, artist_id: &str) -> Result<Artist> {
        let page = match self.artist_page(artist_id).await {
            Ok(page) => page,
            Err(error) => {
                log::debug!(
                    "deezer: no page for artist {artist_id}, public api instead: {error:#}"
                );
                return self.artist_public(artist_id).await;
            }
        };
        let data = &page["DATA"];
        Ok(Artist {
            name: wire::text(data, &["ART_NAME"])
                .unwrap_or_default()
                .to_owned(),
            cover_large: wire::image("artist", wire::text(data, &["ART_PICTURE"]), 1000),
            biography: wire::text(&page["BIO"], &["BIO"]).and_then(wire::biography),
            monthly_listeners: wire::number(data, &["NB_FAN"]),
            top_tracks: page.get("TOP").map(wire::track_list).unwrap_or_default(),
            albums: page
                .get("ALBUMS")
                .and_then(|albums| albums.get("data"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(wire::album)
                .collect(),
        })
    }

    async fn artist_catalogue(&self, artist_id: &str, known: &[Track]) -> Result<ArtistCatalogue> {
        let (albums, top) = tokio::join!(
            self.discography(artist_id),
            self.gw(
                "artist.getTopTrack",
                json!({ "ART_ID": artist_id, "nb": 100 }),
            ),
        );
        let albums = albums?;
        let top_tracks = top
            .map(|page| {
                page.get("data")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(wire::track)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|_| known.to_vec());
        Ok(ArtistCatalogue {
            albums,
            top_tracks,
            appears_on: Vec::new(),
        })
    }

    async fn artist_profile(&self, artist_id: &str) -> Result<ArtistProfile> {
        let page = self.artist_page(artist_id).await?;
        let data = &page["DATA"];
        Ok(ArtistProfile {
            name: wire::text(data, &["ART_NAME"])
                .unwrap_or_default()
                .to_owned(),
            cover_large: wire::image("artist", wire::text(data, &["ART_PICTURE"]), 1000),
            biography: wire::text(&page["BIO"], &["BIO"]).and_then(wire::biography),
        })
    }

    /// Answers from the portrait cache first and looks the rest up one at a time, so a
    /// batch never holds more than one slot of the quota while a search waits for its own.
    /// An artist the api will not answer for is left out.
    async fn artist_images(&self, ids: Vec<String>) -> Result<HashMap<String, String>> {
        let mut images = HashMap::new();
        let mut missing = Vec::new();
        {
            let known = &self.inner.portraits;
            for id in ids.into_iter().take(PORTRAIT_LIMIT) {
                match known.get(&id) {
                    Some(cover) => {
                        images.insert(id, cover.clone());
                    }
                    None => missing.push(id),
                }
            }
        }
        for id in missing {
            let Ok(detail) = self
                .public(&format!("/artist/{}", escape::component(&id)))
                .await
            else {
                continue;
            };
            let Some(cover) = detail
                .get("picture_big")
                .and_then(Value::as_str)
                .map(str::to_owned)
            else {
                continue;
            };
            self.inner.portraits.insert(id.clone(), cover.clone());
            images.insert(id, cover);
        }
        Ok(images)
    }

    async fn saved_tracks(&self) -> Result<Vec<Track>> {
        let user_id = self.user_id().await;
        let results = self
            .gw(
                "favorite_song.getList",
                json!({ "user_id": user_id, "start": 0, "nb": LIBRARY_PAGE }),
            )
            .await
            .context("cannot load the favorite tracks")?;
        Ok(wire::track_list(&results))
    }

    async fn set_track_saved(&self, track_id: &str, saved: bool) -> Result<()> {
        let method = match saved {
            true => "favorite_song.add",
            false => "favorite_song.remove",
        };
        self.favorite(method, "SNG_ID", track_id).await
    }

    async fn track(&self, track_id: &str) -> Result<Track> {
        let data = self.track_data(track_id).await?;
        wire::track(&data).with_context(|| format!("cannot read the deezer track {track_id}"))
    }

    async fn track_playcount(&self, _track_id: &str) -> Result<Option<u64>> {
        Ok(None)
    }

    async fn playlists(&self) -> Result<Vec<Playlist>> {
        let user_id = self.user_id().await;
        Ok(self
            .profile_tab("playlists")
            .await
            .context("cannot load the playlists")?
            .iter()
            .filter_map(|value| wire::playlist(value, &user_id))
            .collect())
    }

    async fn create_playlist(&self, name: &str) -> Result<String> {
        let results = self
            .gw(
                "playlist.create",
                json!({ "title": name, "status": 0, "description": "", "songs": [] }),
            )
            .await
            .context("cannot create the playlist")?;
        wire::id(&results)
            .or_else(|| wire::id(&results["PLAYLIST_ID"]))
            .context("the created playlist came back with no id")
    }

    async fn rename_playlist(&self, playlist_id: &str, name: &str) -> Result<()> {
        self.gw(
            "playlist.update",
            json!({ "playlist_id": playlist_id, "title": name }),
        )
        .await
        .context("cannot rename the playlist")?;
        Ok(())
    }

    async fn delete_playlist(&self, playlist_id: &str) -> Result<()> {
        self.gw("playlist.delete", json!({ "playlist_id": playlist_id }))
            .await
            .context("cannot delete the playlist")?;
        Ok(())
    }

    async fn remove_playlist_from_library(&self, playlist_id: &str) -> Result<()> {
        self.gw(
            "playlist.deleteFavorite",
            json!({ "PLAYLIST_ID": playlist_id }),
        )
        .await
        .context("cannot unfollow the playlist")?;
        Ok(())
    }

    async fn add_playlist_to_library(&self, playlist_id: &str) -> Result<()> {
        self.gw(
            "playlist.addFavorite",
            json!({ "PARENT_PLAYLIST_ID": playlist_id }),
        )
        .await
        .context("cannot follow the playlist")?;
        Ok(())
    }

    async fn set_playlist_public(&self, playlist_id: &str, public: bool) -> Result<()> {
        self.gw(
            "playlist.update",
            json!({ "playlist_id": playlist_id, "status": match public {
                true => 1,
                false => 0,
            } }),
        )
        .await
        .context("cannot change the playlist's visibility")?;
        Ok(())
    }

    async fn add_track_to_playlist(&self, playlist_id: &str, track_id: &str) -> Result<()> {
        self.gw(
            "playlist.addSongs",
            json!({ "playlist_id": playlist_id, "songs": [[track_id, 0]] }),
        )
        .await
        .context("cannot add the track to the playlist")?;
        Ok(())
    }

    async fn remove_track_from_playlist(&self, playlist_id: &str, track_id: &str) -> Result<()> {
        self.gw(
            "playlist.deleteSongs",
            json!({ "playlist_id": playlist_id, "songs": [[track_id, 0]] }),
        )
        .await
        .context("cannot remove the track from the playlist")?;
        Ok(())
    }

    async fn saved_albums(&self) -> Result<Vec<Album>> {
        Ok(self
            .profile_tab("albums")
            .await
            .context("cannot load the favorite albums")?
            .iter()
            .filter_map(wire::album)
            .collect())
    }

    async fn set_album_saved(&self, album_id: &str, saved: bool) -> Result<()> {
        let method = match saved {
            true => "favorite_album.add",
            false => "favorite_album.remove",
        };
        self.favorite(method, "ALB_ID", album_id).await
    }

    async fn saved_artists(&self) -> Result<Vec<SavedArtist>> {
        Ok(self
            .profile_tab("artists")
            .await
            .context("cannot load the favorite artists")?
            .iter()
            .filter_map(wire::saved_artist)
            .collect())
    }

    async fn set_artist_saved(&self, artist_id: &str, saved: bool) -> Result<()> {
        let method = match saved {
            true => "artist.addFavorite",
            false => "artist.deleteFavorite",
        };
        self.favorite(method, "ART_ID", artist_id).await
    }

    async fn album(&self, album_id: &str) -> Result<AlbumDetail> {
        let detail = self
            .public(&format!("/album/{}", escape::component(album_id)))
            .await
            .with_context(|| format!("cannot load the album {album_id}"))?;
        let tracks = detail
            .get("tracks")
            .map(wire::track_list)
            .unwrap_or_default();
        let album =
            wire::album(&detail).with_context(|| format!("cannot read the album {album_id}"))?;
        Ok(AlbumDetail { album, tracks })
    }

    async fn album_tracks(&self, album_id: &str) -> Result<Vec<Track>> {
        Ok(self.album(album_id).await?.tracks)
    }

    async fn album_catalogue(
        &self,
        album_id: &str,
        artist_id: Option<&str>,
    ) -> Result<AlbumCatalogue> {
        let Some(artist_id) = artist_id else {
            return Ok(AlbumCatalogue::default());
        };
        let detail_path = format!("/artist/{}", escape::component(artist_id));
        let (detail, more_by, similar) = tokio::join!(
            self.public(&detail_path),
            self.more_from_artist(album_id, artist_id),
            self.similar_artists(artist_id),
        );
        let own = ArtistRef {
            name: detail
                .ok()
                .and_then(|detail| wire::text(&detail, &["name"]).map(str::to_owned))
                .unwrap_or_default(),
            id: Some(artist_id.to_owned()),
        };
        // Nothing read at all is an error rather than an empty rail, so the catalog does not
        // keep the empty answer for the rest of the session.
        let (more_by, similar) = match (more_by, similar) {
            (Err(error), Err(_)) => return Err(error.context("cannot read any recommendations")),
            pair => pair,
        };
        if let Err(error) = &more_by {
            log::warn!("deezer: cannot read more from this artist: {error:#}");
        }
        if let Err(error) = &similar {
            log::warn!("deezer: cannot read related artists: {error:#}");
        }
        let (more_by, similar) = (more_by.unwrap_or_default(), similar.unwrap_or_default());
        let mut seen = HashSet::new();
        let mut liked: Vec<Album> = more_by
            .into_iter()
            .map(|album| album.credit(&own))
            .filter(|album| seen.insert(album.id.clone()))
            .collect();
        // One artist at a time, so a thin rail never holds more than one slot of the
        // quota while a search waits for its own.
        if liked.len() < SUGGESTIONS && !similar.is_empty() {
            for artist in similar.iter().take(SIMILAR_ARTISTS) {
                if liked.len() >= SUGGESTIONS {
                    break;
                }
                match self.more_from_artist(album_id, &artist.id).await {
                    Ok(releases) => {
                        let theirs = ArtistRef {
                            name: artist.name.clone(),
                            id: Some(artist.id.clone()),
                        };
                        for album in releases.into_iter().take(SIMILAR_RELEASES) {
                            if liked.len() >= SUGGESTIONS {
                                break;
                            }
                            if seen.insert(album.id.clone()) {
                                liked.push(album.credit(&theirs));
                            }
                        }
                    }
                    Err(error) => {
                        log::warn!(
                            "deezer: cannot load albums by similar artist {}: {error:#}",
                            artist.id
                        );
                    }
                }
            }
        }
        Ok(AlbumCatalogue {
            also_like: liked,
            similar,
        })
    }

    async fn playlist(&self, playlist_id: &str) -> Result<PlaylistDetail> {
        if playlist_id == FLOW_ID {
            return self.flow_playlist().await;
        }
        if let Some(radio_id) = playlist_id.strip_prefix(RADIO_PREFIX) {
            return self.radio_playlist(radio_id).await;
        }
        if let Some(tracklist_id) = playlist_id.strip_prefix(SMART_PREFIX) {
            return self.smart_playlist(tracklist_id).await;
        }
        // the gateway, not the public api: only it can see the user's private playlists
        let results = self
            .gw(
                "deezer.pagePlaylist",
                json!({
                    "playlist_id": playlist_id,
                    "start": 0,
                    "nb": LIBRARY_PAGE,
                    "lang": "en",
                    "tab": 0,
                    "tags": true,
                    "header": true,
                }),
            )
            .await
            .with_context(|| format!("cannot load the playlist {playlist_id}"))?;
        let user_id = self.user_id().await;
        let data = &results["DATA"];
        let tracks = results
            .get("SONGS")
            .map(wire::track_list)
            .unwrap_or_default();
        let mut playlist = wire::playlist(data, &user_id)
            .with_context(|| format!("cannot read the playlist {playlist_id}"))?;
        if playlist.track_count == 0 {
            playlist.track_count = tracks.len() as u32;
        }
        if playlist.cover.is_none() {
            playlist.cover = tracks.iter().find_map(|track| track.cover.clone());
        }
        Ok(PlaylistDetail {
            playlist,
            tracks,
            continuation: None,
        })
    }

    async fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<Track>> {
        Ok(self.playlist(playlist_id).await?.tracks)
    }

    async fn playlist_covers(&self, playlist_id: &str, wanted: usize) -> Result<Vec<String>> {
        let tracks = self.playlist_tracks(playlist_id).await?;
        Ok(distinct_covers(&tracks, wanted))
    }

    async fn track_radio(
        &self,
        track_id: &str,
        _from: Option<&str>,
    ) -> Result<(Vec<Track>, Option<String>)> {
        let results = self
            .gw(
                "song.getSearchTrackMix",
                json!({ "SNG_ID": track_id, "start": 0, "nb": 25 }),
            )
            .await;
        match results {
            Ok(results) => Ok((wire::track_list(&results), None)),
            Err(error) => {
                log::warn!("deezer: no track radio for {track_id}: {error:#}");
                Ok((Vec::new(), None))
            }
        }
    }

    async fn search(&self, query: &str) -> Result<Vec<Track>> {
        let page = self
            .public(&format!("/search?q={}&limit=50", escape::component(query)))
            .await
            .context("cannot search deezer")?;
        Ok(wire::track_list(&page))
    }

    async fn search_albums(&self, query: &str) -> Result<Vec<Album>> {
        let page = self
            .public(&format!(
                "/search/album?q={}&limit=30",
                escape::component(query)
            ))
            .await
            .context("cannot search deezer albums")?;
        Ok(page
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(wire::album)
            .collect())
    }

    async fn search_playlists(&self, query: &str) -> Result<Vec<Playlist>> {
        let page = self
            .public(&format!(
                "/search/playlist?q={}&limit=30",
                escape::component(query)
            ))
            .await
            .context("cannot search deezer playlists")?;
        Ok(page
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|value| wire::playlist(value, ""))
            .collect())
    }

    /// The account's own recently played, newest first. Deezer lists them under the
    /// profile's `history` tab, which only the account itself can read.
    async fn recently_played(&self) -> Result<Vec<Track>> {
        match self.profile_tab("history").await {
            Ok(items) => Ok(items.iter().filter_map(wire::track).collect()),
            Err(error) => {
                log::debug!("deezer: no listening history: {error:#}");
                Ok(Vec::new())
            }
        }
    }

    /// The home feed the web app draws: `page.get` answers the listener's own sections
    /// (made-for-you mixes, charts, new releases, genre rows), Flow stands in as Quick
    /// picks, and the smart tracklists land as one Mixes shelf. When the layout call brings
    /// nothing back, the public charts fill in instead.
    async fn home(&self) -> Result<HomeFeed> {
        let (page, flow, mixes) =
            tokio::join!(self.page("home"), self.flow_tracks(), self.mix_cards(),);
        let mut sections = match page {
            Ok(results) => {
                let user_id = self.user_id().await;
                wire::page_sections(&results, &user_id)
            }
            Err(error) => {
                log::warn!("deezer: cannot load the home page: {error:#}");
                Vec::new()
            }
        };
        if sections.is_empty() {
            sections = self.chart_sections().await.unwrap_or_else(|error| {
                log::warn!("deezer: cannot load the charts: {error:#}");
                Vec::new()
            });
        }
        if !mixes.is_empty() {
            sections.push(GenreSection {
                title: "nav-mixes".to_owned(),
                items: mixes,
            });
        }
        Ok(HomeFeed {
            listen_again: Vec::new(),
            quick_picks: flow.ok().filter(|tracks| !tracks.is_empty()),
            sections,
        })
    }

    /// Deezer's whole genre list, as the search page's browse grid.
    async fn genres(&self) -> Result<Vec<Genre>> {
        let page = self
            .public("/genre?limit=100")
            .await
            .context("cannot load the deezer genres")?;
        Ok(page
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(wire::genre)
            .filter(|genre| genre.id != "0")
            .collect())
    }

    /// One genre's page. A `channel:` id opens the curated channel of that slug; a numeric
    /// id is a public-api genre, drawn from its chart, artists and radios.
    async fn genre(&self, genre_id: &str) -> Result<GenreDetail> {
        if let Some(slug) = genre_id.strip_prefix(CHANNEL_PREFIX) {
            let results = self
                .page(&format!("channels/{slug}"))
                .await
                .with_context(|| format!("cannot load the deezer channel {slug}"))?;
            let user_id = self.user_id().await;
            return Ok(GenreDetail {
                name: wire::text(&results, &["title", "TITLE"])
                    .unwrap_or_default()
                    .to_owned(),
                sections: wire::page_sections(&results, &user_id),
            });
        }

        let genre_id = escape::component(genre_id);
        let genre_path = format!("/genre/{genre_id}");
        let albums_path = format!("/chart/{genre_id}/albums?limit=15");
        let tracks_path = format!("/chart/{genre_id}/tracks?limit=15");
        let artists_path = format!("/genre/{genre_id}/artists?limit=15");
        let playlists_path = format!("/chart/{genre_id}/playlists?limit=15");
        let radios_path = format!("/genre/{genre_id}/radios");
        let (genre, albums, tracks, artists, playlists, radios) = tokio::join!(
            self.public(&genre_path),
            self.public(&albums_path),
            self.public(&tracks_path),
            self.public(&artists_path),
            self.public(&playlists_path),
            self.public(&radios_path),
        );
        let name = genre
            .ok()
            .and_then(|detail| wire::text(&detail, &["name"]).map(str::to_owned))
            .unwrap_or_default();
        let mut sections = Vec::new();
        let mut shelf =
            |title: &str, page: Result<Value>, item: fn(&Value) -> Option<GenreItem>| {
                let items: Vec<GenreItem> = page
                    .map(|page| {
                        page.get("data")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default()
                    })
                    .unwrap_or_default()
                    .iter()
                    .filter_map(item)
                    .collect();
                if !items.is_empty() {
                    sections.push(GenreSection {
                        title: title.to_owned(),
                        items,
                    });
                }
            };
        shelf("nav-albums", albums, |value| {
            wire::album(value).map(GenreItem::Album)
        });
        shelf("nav-songs", tracks, |value| {
            wire::track(value).map(GenreItem::Track)
        });
        shelf("nav-artists", artists, |value| {
            wire::saved_artist(value).map(GenreItem::Artist)
        });
        shelf("home-playlists", playlists, |value| {
            wire::playlist(value, "").map(GenreItem::Playlist)
        });
        shelf("nav-mixes", radios, |value| {
            wire::radio(value).map(GenreItem::Playlist)
        });
        Ok(GenreDetail { name, sections })
    }
}

/// Locks a mutex whose guarded value is still sound after a panic elsewhere.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Where in `FORMATS` the best format a plan covers sits, from the `OPTIONS` of
/// `deezer.getUserData`. A level the options do not mention counts as covered, so an account
/// that says nothing about its plan is asked for every format.
fn plan(options: &Value) -> usize {
    let covers = |keys: [&str; 2]| {
        let flags = keys
            .iter()
            .filter_map(|key| options.get(key))
            .filter_map(|value| value.as_bool().or_else(|| value.as_u64().map(|on| on != 0)))
            .collect::<Vec<_>>();
        flags.is_empty() || flags.contains(&true)
    };
    match (
        covers(["web_lossless", "mobile_lossless"]),
        covers(["web_hq", "mobile_hq"]),
    ) {
        (true, _) => 0,
        (false, true) => 1,
        (false, false) => 2,
    }
}

/// The formats to try for a level, from its own down to the lowest.
fn formats_from(quality: Quality) -> &'static [&'static str] {
    let first = match quality.within(QUALITIES) {
        Quality::HiRes | Quality::Lossless => 0,
        Quality::High => 1,
        Quality::Normal | Quality::Low => 2,
    };
    &FORMATS[first..]
}
