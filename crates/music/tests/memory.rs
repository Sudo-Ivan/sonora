//! Heap-growth checks for the library scan and the stream spool, under dhat's testing
//! profiler. A workload runs a few warmup iterations, then the assertion compares live heap
//! after the last one against the post-warmup baseline: bytes or blocks that grow past a small
//! per-iteration tolerance are a leak.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use dhat::HeapStats;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Live-heap headroom a workload may keep per iteration before it counts as leaked. A few
/// KiB covers deferred frees inside pools; steady growth past it does not happen by accident.
const TOLERANCE: usize = 64 * 1024;

/// Runs `workload` `iterations` times and answers the live-heap snapshots after the warmup
/// iteration `warmup` and after the last one. A pause between runs lets worker pools and
/// deferred drops quiesce, so the baseline is the workload's real footprint.
fn settled(
    iterations: usize,
    warmup: usize,
    mut workload: impl FnMut(usize),
) -> (HeapStats, HeapStats) {
    let mut base = HeapStats::get();
    for i in 0..iterations {
        workload(i);
        std::thread::sleep(std::time::Duration::from_millis(20));
        if i == warmup {
            base = HeapStats::get();
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(150));
    (base, HeapStats::get())
}

/// How many fds the process holds, for the spool-leak check on Linux.
#[cfg(target_os = "linux")]
fn fds() -> usize {
    fs::read_dir("/proc/self/fd").map_or(0, |dir| dir.count())
}

/// How many threads the process runs, for the spool-thread leak check on Linux.
#[cfg(target_os = "linux")]
fn threads() -> usize {
    fs::read_dir("/proc/self/task").map_or(0, |dir| dir.count())
}

/// A tag-free PCM WAV written by hand: a short run of silence is all the scanner needs a
/// real audio frame for, and lofty reads its metadata-less form fine.
fn wav(samples: u16) -> Vec<u8> {
    let data = (samples as usize) * 4;
    let mut out = Vec::with_capacity(44 + data);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36u32 + data as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt \x10\x00\x00\x00\x01\x00\x02\x00");
    out.extend_from_slice(&44100u32.to_le_bytes());
    out.extend_from_slice(&(44100u32 * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data as u32).to_le_bytes());
    out.resize(44 + data, 0);
    out
}

/// Writes `count` tiny audio files under `dir/albumNNN/`, twelve to an album, so albums,
/// artists and the index all materialise. Tags stay absent; the scan still indexes every
/// file.
fn make_library(dir: &Path, count: usize) -> Vec<PathBuf> {
    let pcm = wav(1024);
    let mut files = Vec::with_capacity(count);
    for i in 0..count {
        let album = i / 12;
        let dest = dir.join(format!("album{album:03}"));
        fs::create_dir_all(&dest).unwrap();
        let path = dest.join(format!("track{:02}.wav", i % 12 + 1));
        fs::write(&path, &pcm).unwrap();
        files.push(path);
    }
    files
}

#[test]
fn memory() {
    let _profiler = dhat::Profiler::builder().testing().build();

    local_library_scan();
    stream_spool();
}

/// Scans a synthetic library, rescans it with files touched, added and removed, then scans
/// once empty. The index, the scanned model and the sqlite rows must not grow across runs.
fn local_library_scan() {
    let root = std::env::temp_dir().join("sonora-mem-lib");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let files = make_library(&root, 2_000);

    let cache = storage::Cache::at(root.join("cache.sqlite"));
    let index = music::local::index::Index::new(cache);

    let mut seen = 0usize;
    let (base, last) = settled(6, 1, |i| {
        match i {
            // a rescan: touch a file, add one, drop one
            2 => {
                fs::write(&files[5], wav(2048)).unwrap();
                fs::write(root.join("album000/newtrack.wav"), wav(1024)).unwrap();
                let _ = fs::remove_file(&files[20]);
            }
            // an empty-library scan
            5 => {
                for f in &files {
                    let _ = fs::remove_file(f);
                }
                let _ = fs::remove_file(root.join("album000/newtrack.wav"));
            }
            _ => {}
        }
        let scanned = music::local::scan::scan(&[root.clone()], &root, &index);
        seen = seen.max(scanned.tracks.len());
        drop(scanned);
    });
    let kept = last.curr_bytes.saturating_sub(base.curr_bytes);
    println!(
        "ALLOC scan_tracks={seen} scan_base={} scan_last={} scan_kept={kept}",
        base.curr_bytes, last.curr_bytes
    );
    assert!(
        kept <= TOLERANCE,
        "library scan leaked {kept} bytes over the baseline"
    );

    let _ = fs::remove_dir_all(&root);
}

/// Generated chunks over a stream spool, dropped mid-download like a skip. The spool file
/// must be unlinked and every backing byte freed when the last handle goes.
fn stream_spool() {
    struct Chunks {
        left: usize,
    }
    impl music::stream::Source for Chunks {
        async fn chunk(&mut self) -> anyhow::Result<Option<Bytes>> {
            match self.left {
                0 => Ok(None),
                left => {
                    self.left = left - 1;
                    Ok(Some(Bytes::from(vec![7u8; 64 * 1024])))
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

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let fd_start = fds();
    let (base, last) = settled(60, 5, |_i| {
        rt.block_on(async {
            let stream =
                music::stream::Stream::pulling(Chunks { left: 8 }, Some(512 * 1024), Plain);
            let mut reader = stream.reader();
            tokio::task::spawn_blocking(move || {
                let mut buf = [0u8; 8192];
                // read part way only, like a skip mid-download
                for _ in 0..8 {
                    let _ = reader.read(&mut buf);
                }
                drop(reader);
            })
            .await
            .unwrap();
            drop(stream);
        });
    });
    let kept = last.curr_bytes.saturating_sub(base.curr_bytes);
    let fd_end = fds();
    println!(
        "ALLOC spool_base={} spool_last={} spool_kept={kept} spool_fds={fd_start}->{fd_end} threads={}",
        base.curr_bytes,
        last.curr_bytes,
        threads()
    );
    assert!(kept <= TOLERANCE, "stream spool leaked {kept} bytes");
    assert!(
        fd_end <= fd_start + 8,
        "open fds grew {fd_start} -> {fd_end}"
    );
}
