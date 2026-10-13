//! A disk cache for streamed tracks.
//!
//! What lands on disk is whatever the provider sent over the wire, before its `Body` did
//! anything to it: for Deezer that is still the encrypted audio, so nothing readable ever
//! reaches a file and `Body::confidential` stays true of every byte the cache holds. A
//! replay feeds the stored file through the same body a live download would, which keeps
//! exactly one decoding path.
//!
//! A stored track is two files: `<id>` for the bytes and `<id>.json` for what the provider
//! learned while fetching, so a replay skips the metadata call too. The cache evicts the
//! least recently played tracks once it grows past the shared limit, and a store that never
//! completes leaves only a `.part` file, which is never served and is swept on sight.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek as _, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use bytes::Bytes;
use tokio::io::AsyncReadExt;

use crate::engine::Loudness;
use crate::stream::Source;

/// How much of a file a replay hands over per chunk.
const READ_CHUNK: usize = 64 * 1024;

const PART: &str = "part";
const META: &str = "json";

/// The folder every provider's cache lives under, inside the app cache directory.
fn root() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("sonora")
        .join("audio")
}

/// A filename-safe form of a track id. Provider ids are mostly digits, but a `:` or `/`
/// would not survive on every filesystem.
fn file_name(id: &str) -> String {
    id.chars()
        .map(
            |c| match c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                true => c,
                false => '_',
            },
        )
        .collect()
}

/// What a stored track remembers besides its bytes. `track` is the id the bytes answer
/// for, which a region-fallback may not share with the requested id; the size and the CRC
/// are what `get` verifies before serving them.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct CachedMeta {
    pub track: String,
    pub duration_secs: Option<f64>,
    /// The track's loudness as its provider measured it, in LUFS.
    pub lufs: Option<f32>,
    pub bytes: u64,
    pub crc: u32,
}

impl CachedMeta {
    pub fn duration(&self) -> Option<Duration> {
        self.duration_secs.map(Duration::from_secs_f64)
    }

    pub fn loudness(&self) -> Option<Loudness> {
        self.lufs.map(|lufs| Loudness { lufs, peak: None })
    }
}

/// A shared handle on one provider's cache. `limit` is an atomic so a setting change
/// reaches a running engine without a restart; zero keeps the cache read-only and stops
/// new stores.
#[derive(Clone, Debug)]
pub struct StreamCache {
    folder: PathBuf,
    limit: Arc<AtomicU64>,
}

impl StreamCache {
    /// The cache for `provider` under the shared cache folder, capped at `limit` bytes.
    /// The atomic is read at store time, so callers can share it with the setting.
    pub fn new(provider: &str, limit: Arc<AtomicU64>) -> Self {
        Self {
            folder: root().join(provider),
            limit,
        }
    }

    /// Removes every provider's cached tracks. Call it when the setting goes off, so the
    /// space comes back at once rather than waiting for the tracks to age out.
    pub fn clear() {
        let folder = root();
        if let Err(error) = fs::remove_dir_all(&folder)
            && error.kind() != io::ErrorKind::NotFound
        {
            log::warn!("cache: cannot clear {}: {error}", folder.display());
        }
    }

    fn limit(&self) -> u64 {
        self.limit.load(Ordering::Relaxed)
    }

    /// The stored bytes of `id`, verified against its sidecar, or `None` when the track is
    /// not kept or what is kept fails the check. A hit touches the file, so eviction drops
    /// what was played longest ago.
    pub fn get(&self, id: &str) -> Option<Cached> {
        if self.limit() == 0 {
            return None;
        }
        let path = self.folder.join(file_name(id));
        let meta_path = path.with_extension(META);
        let meta: Option<CachedMeta> = fs::read_to_string(&meta_path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok());
        let Some(meta) = meta else {
            // No sidecar means an unfinished or foreign file, which must never be served.
            discard(&path);
            return None;
        };
        let mut file = File::open(&path).ok()?;
        let bytes = file.metadata().ok()?.len();
        if bytes == 0 || bytes != meta.bytes || crc(&file) != meta.crc {
            discard(&path);
            return None;
        }
        file.seek(io::SeekFrom::Start(0)).ok()?;
        // LRU runs on last played, which is the file's mtime.
        file.set_modified(SystemTime::now()).ok();
        Some(Cached {
            file: tokio::fs::File::from_std(file),
            left: bytes,
            bytes,
            meta,
        })
    }

    /// Where to send the bytes of `id` while they download. `None` when the cache is off,
    /// the announced size is already over the limit, or the folder will not take a file:
    /// the track then streams uncached exactly as before.
    pub fn store(&self, id: &str, meta: CachedMeta) -> Option<Caching> {
        if self.limit() == 0 {
            return None;
        }
        fs::create_dir_all(&self.folder).ok()?;
        let name = file_name(id);
        let path = self.folder.join(&name);
        let part = path.with_extension(PART);
        let file = File::create(&part)
            .with_context(|| format!("cache: cannot open {}", part.display()))
            .ok()?;
        let meta_path = path.with_extension(META);
        Some(Caching {
            file: BufWriter::new(file),
            part,
            path,
            meta_path,
            meta,
            written: 0,
            hasher: crc32fast::Hasher::new(),
            limit: self.limit.clone(),
            folder: self.folder.clone(),
            finished: false,
            broke: false,
        })
    }
}

/// A stored track, served as a `Source` over its file.
pub struct Cached {
    file: tokio::fs::File,
    left: u64,
    /// The file length, which is also what the stream reports as its total.
    pub bytes: u64,
    pub meta: CachedMeta,
}

impl Source for Cached {
    async fn chunk(&mut self) -> Result<Option<Bytes>> {
        if self.left == 0 {
            return Ok(None);
        }
        let wanted = self.left.min(READ_CHUNK as u64) as usize;
        let mut out = vec![0u8; wanted];
        self.file.read_exact(&mut out).await?;
        self.left -= out.len() as u64;
        Ok(Some(Bytes::from(out)))
    }
}

/// The write half of a `store`, teed into the download by [`Tee`]. Bytes land in a `.part`
/// file and only `finish` renames it onto the track's name, so a read never serves a partial
/// track. Dropping an unfinished one removes the partial file.
pub struct Caching {
    file: BufWriter<File>,
    part: PathBuf,
    path: PathBuf,
    meta_path: PathBuf,
    meta: CachedMeta,
    written: u64,
    hasher: crc32fast::Hasher,
    limit: Arc<AtomicU64>,
    folder: PathBuf,
    finished: bool,
    broke: bool,
}

impl Caching {
    /// Appends one wire chunk. A write that fails, or a track outgrowing the limit, stops
    /// the store quietly: the download itself is unaffected.
    pub fn write(&mut self, chunk: &[u8]) {
        if self.broke {
            return;
        }
        self.written += chunk.len() as u64;
        if self.written > self.limit.load(Ordering::Relaxed) {
            self.broke = true;
            return;
        }
        if let Err(error) = self.file.write_all(chunk) {
            log::debug!(
                "cache: cannot keep writing {}: {error}",
                self.part.display()
            );
            self.broke = true;
            return;
        }
        self.hasher.update(chunk);
    }

    /// Seals the store once the download has ended cleanly. Nothing before this is visible
    /// to `get`, and a track that never finishes never was.
    pub fn finish(&mut self) {
        if self.broke || self.finished {
            return;
        }
        if let Err(error) = self.file.flush() {
            log::debug!("cache: cannot finish {}: {error}", self.part.display());
            return;
        }
        self.finished = true;
        self.meta.crc = self.hasher.clone().finalize();
        if let Err(error) = fs::rename(&self.part, &self.path) {
            log::debug!("cache: cannot keep {}: {error}", self.path.display());
            return;
        }
        self.meta.bytes = self.written;
        match serde_json::to_string(&self.meta) {
            Ok(text) => {
                fs::write(&self.meta_path, text).ok();
            }
            Err(error) => log::warn!("cache: cannot describe {}: {error}", self.path.display()),
        }
        evict(&self.folder, self.limit.load(Ordering::Relaxed));
    }
}

impl Drop for Caching {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        fs::remove_file(&self.part).ok();
    }
}

/// A `Source` that tees every chunk into a `Caching` store while it passes it on. Finished
/// the moment the source answers `None`, so a broken download never seals a partial file.
pub struct Tee<S> {
    inner: S,
    store: Caching,
}

impl<S> Tee<S> {
    pub fn new(inner: S, store: Caching) -> Self {
        Self { inner, store }
    }
}

impl<S: Source> Source for Tee<S> {
    async fn chunk(&mut self) -> Result<Option<Bytes>> {
        let chunk = self.inner.chunk().await?;
        match &chunk {
            Some(bytes) => self.store.write(bytes),
            None => self.store.finish(),
        }
        Ok(chunk)
    }
}

/// The CRC32 of `file`, read back from the start. This is what stands between a replay and
/// a file the disk damaged: a mismatch evicts the track and the next load fetches it anew.
fn crc(file: &File) -> u32 {
    let mut file = file;
    file.seek(std::io::SeekFrom::Start(0)).ok();
    let mut hasher = crc32fast::Hasher::new();
    let mut out = [0u8; READ_CHUNK];
    loop {
        match file.read(&mut out) {
            Ok(0) => break,
            Ok(n) => hasher.update(&out[..n]),
            Err(_) => break,
        }
    }
    hasher.finalize()
}

/// Removes a track's files, whichever of them exist.
fn discard(path: &Path) {
    fs::remove_file(path).ok();
    fs::remove_file(path.with_extension(META)).ok();
}

/// Drops the least recently played tracks until the folder fits `limit`. Parts and stale
/// sidecars are swept on sight, since nothing will ever finish or serve them.
fn evict(folder: &PathBuf, limit: u64) {
    let mut kept: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let mut total = 0u64;
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == PART) {
            fs::remove_file(&path).ok();
            continue;
        }
        if path.extension().is_some_and(|ext| ext == META) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        total += meta.len();
        kept.push((
            path,
            meta.len(),
            meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        ));
    }
    kept.sort_by_key(|(_, _, touched)| *touched);
    let mut at = 0;
    while total > limit && at < kept.len() {
        let (path, bytes, _) = &kept[at];
        total -= *bytes;
        discard(path);
        at += 1;
    }
    // A sidecar whose track file is gone is swept too.
    if let Ok(entries) = fs::read_dir(folder) {
        for path in entries.flatten().map(|entry| entry.path()) {
            let is_meta = path.extension().is_some_and(|ext| ext == META);
            let orphan = path
                .file_stem()
                .is_some_and(|stem| !folder.join(stem).exists());
            if is_meta && orphan {
                fs::remove_file(&path).ok();
            }
        }
    }
}
