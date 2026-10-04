use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use music::{Lyrics, LyricsHit, LyricsProvider, LyricsQuery, kugou, lrclib, musixmatch, netease};

const LISTED: usize = 4;

struct Probe {
    source: &'static str,
    elapsed: Duration,
    hits: Vec<LyricsHit>,
    error: Option<String>,
}

fn providers() -> Vec<Arc<dyn LyricsProvider>> {
    vec![
        Arc::new(musixmatch::Musixmatch::new()),
        Arc::new(lrclib::LrcLib::new()),
        Arc::new(kugou::Kugou::new()),
        Arc::new(netease::NetEase::new()),
    ]
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let Some(lookup) = std::env::args().nth(1) else {
        bail!("usage: lyrics-prober <search query>");
    };

    let query = resolve(&lookup).await?;

    println!(
        "{} - {} [{}] {}",
        query.title,
        query.artist,
        query.album.as_deref().unwrap_or(""),
        clock(query.duration),
    );
    println!();

    let mut probes = probe(&query).await;
    probes.sort_by_key(|probe| probe.elapsed);

    println!(
        "{:<12} {:>6} {:>5}  {:>6} {:>4} {:<8} matched title / artist",
        "provider", "ms", "hits", "score", "keep", "kind"
    );
    for found in &probes {
        report(&query, found);
    }
    println!();

    let all: Vec<LyricsHit> = probes
        .iter()
        .flat_map(|found| found.hits.iter().cloned())
        .collect();
    let ranked = music::lyrics::rank(&query, all);
    match ranked.is_empty() {
        true => println!(
            "nothing survived ranking{}",
            match music::lyrics::instrumental(&query, &ranked) {
                true => ", the track reads as instrumental",
                false => "",
            }
        ),
        false => {
            println!("ranked:");
            for (place, hit) in ranked.iter().enumerate() {
                println!(
                    "  {:>2}. {:<12} {:>6} {:<8} {}",
                    place + 1,
                    hit.source,
                    music::lyrics::score(&query, hit),
                    kind(&hit.lyrics),
                    shape(&hit.lyrics),
                );
            }

            let mut reshaped = ranked.clone();
            music::lyrics::reshape(&mut reshaped);
            let winner = &reshaped[0];
            println!();
            println!(
                "winner: {} ({}, {}){}",
                winner.source,
                kind(&winner.lyrics),
                shape(&winner.lyrics),
                match winner.lyrics == ranked[0].lyrics {
                    true => String::new(),
                    false => format!(" reshaped onto {}", ranked[0].source),
                }
            );
        }
    }

    Ok(())
}

/// Resolves a free-form query into the canonical title, artist, album and duration LrcLib
/// knows it by, so the probes run against real metadata rather than the words as typed.
async fn resolve(lookup: &str) -> Result<LyricsQuery> {
    let hit = lrclib::LrcLib::new()
        .search(&LyricsQuery {
            title: lookup.to_owned(),
            artist: String::new(),
            album: None,
            duration: Duration::ZERO,
            track: None,
        })
        .await
        .context("cannot look the song up on lrclib")?
        .into_iter()
        .next()
        .context("nothing found for that query")?;
    Ok(LyricsQuery {
        title: hit.title,
        artist: hit.artist,
        album: hit.album,
        duration: hit.duration.unwrap_or_default(),
        track: None,
    })
}

async fn probe(query: &LyricsQuery) -> Vec<Probe> {
    let mut tasks = tokio::task::JoinSet::new();
    for provider in providers() {
        let query = query.clone();
        tasks.spawn(async move {
            let started = Instant::now();
            let found = provider.search(&query).await;
            Probe {
                source: provider.name(),
                elapsed: started.elapsed(),
                hits: found.as_ref().map(Vec::clone).unwrap_or_default(),
                error: found.err().map(|error| format!("{error:#}")),
            }
        });
    }

    let mut probes = Vec::new();
    while let Some(found) = tasks.join_next().await {
        if let Ok(found) = found {
            probes.push(found);
        }
    }
    probes
}

fn report(query: &LyricsQuery, found: &Probe) {
    let millis = found.elapsed.as_millis();
    if let Some(error) = &found.error {
        println!("{:<12} {millis:>6} {:>5}  {error}", found.source, "-");
        return;
    }
    if found.hits.is_empty() {
        println!("{:<12} {millis:>6} {:>5}", found.source, 0);
        return;
    }

    let mut scored: Vec<(i64, &LyricsHit)> = found
        .hits
        .iter()
        .map(|hit| (music::lyrics::score(query, hit), hit))
        .collect();
    scored.sort_by(|(left, _), (right, _)| right.cmp(left));

    for (place, (score, hit)) in scored.iter().take(LISTED).enumerate() {
        let head = match place {
            0 => format!("{:<12} {millis:>6} {:>5}", found.source, found.hits.len()),
            _ => format!("{:<12} {:>6} {:>5}", "", "", ""),
        };
        println!(
            "{head}  {score:>6} {:>4} {:<8} {} - {}{}",
            match music::lyrics::eligible(query, hit) {
                true => "yes",
                false => "no",
            },
            kind(&hit.lyrics),
            hit.title,
            hit.artist,
            hit.duration
                .map(|duration| format!(" ({})", clock(duration)))
                .unwrap_or_default(),
        );
    }
    if let Some(rest) = scored.len().checked_sub(LISTED).filter(|rest| *rest > 0) {
        println!("{:<12} {:>6} {:>5}  {rest:>6} more", "", "", "");
    }
}

fn kind(lyrics: &Lyrics) -> &'static str {
    match (lyrics.worded(), lyrics.synced()) {
        (true, _) => "worded",
        (false, true) => "synced",
        (false, false) => "plain",
    }
}

fn shape(lyrics: &Lyrics) -> String {
    let Lyrics::Synced { lines } = lyrics else {
        return "unsynced".to_owned();
    };
    let words: usize = lines
        .iter()
        .map(|line| line.words.as_ref().map_or(0, Vec::len))
        .sum();
    let voices = lines.iter().filter(|line| !line.voice.lead()).count();
    let secondary: usize = lines.iter().map(|line| line.secondary.len()).sum();

    format!(
        "{} lines, {words} words, {voices} background, {secondary} lanes, spans {}",
        lines.len(),
        lyrics.span().map(clock).unwrap_or_else(|| "-".to_owned()),
    )
}

fn clock(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
