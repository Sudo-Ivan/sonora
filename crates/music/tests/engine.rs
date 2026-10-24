//! Heap and resource growth checks for the shared playback engine, under dhat's testing
//! profiler. Skips, preloads, seeks and downloads churn through a fake provider for a few
//! hundred rounds; live heap, RSS, fds and threads are compared against the post-warmup
//! baseline, and growth past the tolerance is a leak.
//!
//! The engine opens a real output device. Run it where one exists:
//! `cargo test -p music --test engine -- --ignored --nocapture`

use std::fs;
use std::time::Duration;

use bytes::Bytes;
use dhat::HeapStats;
use rodio::Source as _;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Live-heap headroom a churn round may keep before it counts as leaked.
const TOLERANCE: usize = 256 * 1024;

/// Runs `workload` `iterations` times and answers the heap and process snapshots after the
/// warmup iteration and after the last one.
fn settled(
    iterations: usize,
    warmup: usize,
    mut workload: impl FnMut(usize),
) -> (HeapStats, Proc, HeapStats, Proc) {
    let mut base = HeapStats::get();
    let mut before = proc();
    for i in 0..iterations {
        workload(i);
        if i == warmup {
            base = HeapStats::get();
            before = proc();
        }
    }
    std::thread::sleep(Duration::from_millis(500));
    (base, before, HeapStats::get(), proc())
}

/// A /proc/self snapshot: RSS, anonymous data, mappings, fds and threads. dhat only sees the
/// Rust heap, so growth that lives in C libraries and mmap shows up here instead.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
struct Proc {
    rss: u64,
    data: u64,
    maps: usize,
    fds: usize,
    threads: usize,
}

#[cfg(target_os = "linux")]
fn proc() -> Proc {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = |key: &str| {
        status
            .lines()
            .find(|line| line.starts_with(key))
            .and_then(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
            .unwrap_or(0)
    };
    Proc {
        rss: kb("VmRSS"),
        data: kb("VmData"),
        maps: fs::read_to_string("/proc/self/maps").map_or(0, |m| m.lines().count()),
        fds: fs::read_dir("/proc/self/fd").map_or(0, |d| d.count()),
        threads: kb("Threads") as usize,
    }
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug, Clone, Copy, Default)]
struct Proc {
    rss: u64,
    data: u64,
    maps: usize,
    fds: usize,
    threads: usize,
}
#[cfg(not(target_os = "linux"))]
fn proc() -> Proc {
    Proc::default()
}

/// A generated 16 KiB-per-chunk track served one chunk at a time, like a provider's chunked
/// encrypted body, so a skip lands mid-download.
struct Chunks {
    left: usize,
}
impl music::stream::Source for Chunks {
    async fn chunk(&mut self) -> anyhow::Result<Option<Bytes>> {
        match self.left {
            0 => Ok(None),
            left => {
                self.left = left - 1;
                tokio::time::sleep(Duration::from_millis(15)).await;
                Ok(Some(Bytes::from(vec![3u8; 16 * 1024])))
            }
        }
    }
}
struct Plain;
impl music::stream::Body for Plain {
    fn feed(&mut self, chunk: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(chunk);
    }
}

/// The fake provider: `load` starts a slow spool download, `open` answers a silent sine so
/// the whole fetch-to-output path runs exactly as for a real provider.
struct Fake;

#[async_trait::async_trait]
impl music::engine::Fetch for Fake {
    type Loaded = music::stream::Stream<Plain>;
    type Source = rodio::source::TakeDuration<rodio::source::SineWave>;

    fn name(&self) -> &'static str {
        "fake"
    }

    async fn load(&self, _id: &str) -> anyhow::Result<Self::Loaded> {
        Ok(music::stream::Stream::pulling(
            Chunks { left: 48 },
            Some(48 * 16 * 1024),
            Plain,
        ))
    }

    fn open(&self, _id: &str, _loaded: &Self::Loaded, _at: Duration) -> Option<Self::Source> {
        Some(rodio::source::SineWave::new(220.0).take_duration(Duration::from_secs(2)))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "opens a real audio output"]
async fn engine_churn() {
    let _profiler = dhat::Profiler::builder().testing().build();

    let config = music::PlaybackConfig {
        normalisation: false,
        gapless: true,
        position_interval: Duration::from_millis(100),
        gain: 0.0,
        equalizer: music::equalizer::EqualizerWriter::new(
            false,
            music::equalizer::Curve::new(music::equalizer::Preamp::Auto, vec![]).unwrap(),
        )
        .reader(),
        transcode: Default::default(),
        quality: Default::default(),
        meter: Default::default(),
        analyzer: Default::default(),
        vault: None,
    };
    let (player, mut events) = music::engine::start(Fake, config);

    // drain events so the channel never backs up
    let drain = tokio::spawn(async move { while events.next().await.is_some() {} });

    let mut skips = 0usize;
    let (base, before, last, after) = settled(150, 10, |i| {
        let id = i.to_string();
        match i % 6 {
            // skip before the load even lands
            0 => {
                let _ = player.load(&id, Duration::ZERO, false);
                let _ = player.load(&(i + 1).to_string(), Duration::ZERO, false);
            }
            // load, let the download run a moment, skip
            1 => {
                let _ = player.load(&id, Duration::ZERO, false);
                std::thread::sleep(Duration::from_millis(200));
                let _ = player.load(&(i + 1).to_string(), Duration::ZERO, true);
            }
            // preload then skip
            2 => {
                let _ = player.preload(&id, true);
                std::thread::sleep(Duration::from_millis(50));
                let _ = player.load(&(i + 1).to_string(), Duration::ZERO, false);
            }
            // seek while playing
            3 => {
                let _ = player.load(&id, Duration::ZERO, false);
                std::thread::sleep(Duration::from_millis(200));
                player.seek(Duration::from_millis(500));
            }
            // pause and resume
            4 => {
                let _ = player.load(&id, Duration::ZERO, false);
                std::thread::sleep(Duration::from_millis(100));
                player.pause();
                player.play();
            }
            // a natural 2 s track ends, the gapless join runs
            _ => {
                let _ = player.load(&id, Duration::ZERO, false);
                std::thread::sleep(Duration::from_millis(2100));
            }
        }
        skips += 1;
        std::thread::sleep(Duration::from_millis(20));
    });
    // a download then cancel
    for i in 0..30 {
        let progress = music::Fraction::default();
        let _ = player.download(&i.to_string(), progress).await;
    }
    std::thread::sleep(Duration::from_secs(1));

    println!(
        "ALLOC engine_skips={skips} base={} last={} kept={} rss={}->{} data={}->{} maps={}->{} fds={}->{} threads={}->{}",
        base.curr_bytes,
        last.curr_bytes,
        last.curr_bytes.saturating_sub(base.curr_bytes),
        before.rss,
        after.rss,
        before.data,
        after.data,
        before.maps,
        after.maps,
        before.fds,
        after.fds,
        before.threads,
        after.threads,
    );
    let kept = last.curr_bytes.saturating_sub(base.curr_bytes);
    assert!(kept <= TOLERANCE, "engine churn leaked {kept} heap bytes");
    assert!(
        after.rss <= before.rss + 32 * 1024,
        "RSS grew {} KiB across churn",
        after.rss.saturating_sub(before.rss)
    );
    assert!(
        after.threads <= before.threads + 6,
        "threads grew {} -> {}",
        before.threads,
        after.threads
    );
    assert!(
        after.fds <= before.fds + 8,
        "fds grew {} -> {}",
        before.fds,
        after.fds
    );

    drain.abort();
}
