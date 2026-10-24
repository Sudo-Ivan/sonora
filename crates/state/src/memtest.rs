//! Heap-growth checks for the queue entity under dhat's testing profiler. An 11,000 track
//! library is queued, shuffled and skipped through a few thousand times, then the session
//! signs out and the queue must give it all back. Live heap after the last round is compared
//! to the post-warmup baseline; growth past the tolerance is a leak.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use dhat::HeapStats;
use gpui::prelude::*;
use gpui::{Entity, TestAppContext};
use music::{SignIn, Track};

use crate::{AppSettings, Io, Session, queue::Queue};

/// Live-heap headroom a churn round may keep before it counts as leaked. A few hundred KiB
/// covers deferred drops inside the dispatcher; steady growth past it is not a pool.
const TOLERANCE: usize = 2 * 1024 * 1024;

/// A provider that signs nothing in and plays nothing, enough for a `Session` to exist.
struct Fake;
#[async_trait]
impl music::MusicProvider for Fake {
    fn name(&self) -> &'static str {
        "Fake"
    }
    fn slug(&self) -> &'static str {
        "fake"
    }
    fn sign_in_options(&self) -> Vec<SignIn> {
        Vec::new()
    }
    fn stored(&self) -> bool {
        false
    }
    async fn restore(&self) -> Result<Option<music::ProviderSession>> {
        Ok(None)
    }
    async fn sign_in(
        &self,
        _method: SignIn,
        _prompt: music::PromptSink,
        _input: music::InputSource,
    ) -> Result<music::ProviderSession> {
        anyhow::bail!("fake cannot sign in")
    }
    fn sign_out(&self) {}
}

fn track(i: usize) -> Track {
    Track {
        id: Some(format!("fake:track:{i}")),
        name: format!("Track number {i} of the soak library"),
        playable: true,
        artists: format!("Artist {}", i / 12),
        artist_refs: Vec::new(),
        album: format!("Album {}", i / 12),
        album_id: Some(format!("album-{}", i / 12)),
        cover: Some(format!("https://art.local/covers/{:04}.jpg", i / 12)),
        duration: Duration::from_secs(180 + (i % 240) as u64),
        added_at: None,
        added_by: None,
        playcount: Some((i % 100) as u64),
        popularity: (i % 100) as u32,
        explicit: false,
        track_number: (i % 12 + 1) as u32,
        disc_number: 1,
        tags: Vec::new(),
        languages: Vec::new(),
        credits: Vec::new(),
    }
}

#[test]
fn queue_churn() {
    let _profiler = dhat::Profiler::builder().testing().build();

    let cx = TestAppContext::single();
    let root = std::env::temp_dir().join("sonora-state-mem");
    let _ = std::fs::remove_dir_all(&root);
    let database = storage::Database::at(root.join("state.sqlite"));

    let (_settings, session, queue): (Entity<AppSettings>, Entity<Session>, Entity<Queue>) = cx
        .update(|cx| {
            let settings = cx.new(|_| AppSettings::load(database));
            let session = cx.new(|cx| {
                Session::new(
                    Vec::new(),
                    Arc::new(Fake),
                    settings.clone(),
                    Io::new().unwrap(),
                    cx,
                )
            });
            let queue = cx.new(|cx| Queue::new(session.clone(), settings.clone(), cx));
            (settings, session, queue)
        });

    let library: Vec<Track> = (0..11_000).map(track).collect();

    let run = |cx: &TestAppContext, shuffle: bool| {
        cx.update(|app| {
            queue.update(app, |queue, cx| {
                queue.set_shuffle(shuffle, cx);
                let _ = queue.start(library.clone(), 0, None, cx);
                for _ in 0..2_000 {
                    let _ = queue.next(cx);
                }
                for _ in 0..500 {
                    let _ = queue.previous(cx);
                }
            });
        });
    };

    run(&cx, false);
    run(&cx, true);
    let base = HeapStats::get();
    run(&cx, false);
    run(&cx, true);
    cx.executor().run_until_parked();
    let last = HeapStats::get();
    println!(
        "ALLOC queue_base={} queue_last={} queue_kept={}",
        base.curr_bytes,
        last.curr_bytes,
        last.curr_bytes.saturating_sub(base.curr_bytes)
    );

    // sign the session out: the queue must empty, per the entity rules
    cx.update(|app| {
        session.update(app, |session, cx| session.sign_out(cx));
    });
    cx.executor().run_until_parked();
    let lengths = cx.read(|app| {
        let queue = queue.read(app);
        (
            queue.past().len(),
            queue.upcoming().len() + queue.similar().len(),
            queue.current().is_some(),
        )
    });
    println!("ALLOC queue_after_signout={lengths:?}");
    assert_eq!(
        lengths,
        (0, 0, false),
        "queue kept tracks after sign-out: {lengths:?}"
    );

    let kept = last.curr_bytes.saturating_sub(base.curr_bytes);
    assert!(kept <= TOLERANCE, "queue churn leaked {kept} bytes");

    // mix scoring over the same 11,000 tracks: the allocation hot spot
    let favorites: HashSet<String> = (0..50).map(|i| format!("fake:track:{}", i * 7)).collect();
    let years: HashMap<&str, i32> = library
        .iter()
        .filter_map(|track| {
            track
                .album_id
                .as_deref()
                .map(|id| (id, 1980 + (id.len() % 45) as i32))
        })
        .collect();
    let before_mix = HeapStats::get();
    let index = crate::mix::Index::new(
        library.iter(),
        &favorites,
        &years,
        library.iter().take(200),
        1,
    );
    let mixes = crate::mix::seeds(&index, 12);
    let themed = crate::mix::themed(&index);
    let mut dealt = 0usize;
    let heard: HashSet<String> = HashSet::new();
    for mix in &mixes {
        dealt += crate::mix::score(&index, mix, &heard).len();
    }
    let after_mix = HeapStats::get();
    println!(
        "ALLOC mix_seeds={} mix_themed={} mix_scored={} mix_blocks={} mix_kept={}",
        mixes.len(),
        themed.len(),
        dealt,
        after_mix
            .total_blocks
            .saturating_sub(before_mix.total_blocks),
        after_mix.curr_bytes.saturating_sub(before_mix.curr_bytes)
    );
    drop(index);
    drop(mixes);
    drop(themed);

    let _ = std::fs::remove_dir_all(&root);
}
