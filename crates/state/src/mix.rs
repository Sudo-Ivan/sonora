use std::collections::{HashMap, HashSet};

use music::Track;

/// How many artist mixes the Mixes page lists at most.
pub(crate) const MIXES: usize = 12;

/// How many tracks a mix lines up behind its seed at most.
pub(crate) const MIX_LIMIT: usize = 30;

/// How many tracks the forever queue lines up once it runs short, past the lookahead that
/// asks for them, so one fill buys a stretch of playback.
pub(crate) const FOREVER_BATCH: usize = 20;

/// How far back listening history still counts as already heard for the forever queue.
pub(crate) const FOREVER_HISTORY: usize = 200;

/// A shared artist is the strongest hint that two tracks belong in one mix.
const ARTIST: f32 = 10.;
/// Off the seed's own album; an album is shorter company than an artist.
const ALBUM: f32 = 4.;
/// Each genre tag a track shares with the seed, up to `TAG_CAP` in all.
const TAG: f32 = 3.;
const TAG_CAP: f32 = 6.;
/// Each of the seed's credits a track shares, up to `CREDIT_CAP` in all.
const CREDIT: f32 = 1.5;
const CREDIT_CAP: f32 = 3.;
/// A track as long as the seed, fading to nothing `DURATION_FADE` seconds apart.
const DURATION: f32 = 2.;
const DURATION_FADE: f32 = 60.;
/// The provider's own 0-100 popularity, as a mild boost.
const POPULARITY: f32 = 1.5;
/// A play count lifts a track a little, on a log scale saturating at `PLAYCOUNT_SATURATION`
/// so a huge one cannot run away with the mix.
const PLAYCOUNT: f32 = 1.;
const PLAYCOUNT_SATURATION: f32 = 8.;
/// Matching the seed on explicit content.
const EXPLICIT: f32 = 0.5;
/// From the seed's decade, where the shelf knows both albums' years.
const ERA: f32 = 3.;
const ERA_SPAN: i32 = 10;
/// What each track an artist or an album already placed costs their next one, so a mix
/// spreads across the library instead of settling on whoever scored highest.
const ARTIST_REPEAT: f32 = 6.;
const ALBUM_REPEAT: f32 = 2.;

/// A ready-made mix on the Mixes page: the artist it is named for, and the track
/// `Playback::play_mix` scores the shelf against to build it.
#[derive(Clone, Debug)]
pub struct Mix {
    pub name: String,
    pub seed: Track,
}

/// The mixes a shelf's tracks make, one per artist, best first and `limit` at most. An
/// artist with more of the shelf's favorites comes first, then one with more tracks, then
/// alphabetical, so the page leads with whoever the listener keeps around. A mix's seed is
/// the artist's first favorite in library order, else its first playable track there.
pub(crate) fn seeds(tracks: &[Track], favorites: &HashSet<String>, limit: usize) -> Vec<Mix> {
    struct Group<'a> {
        name: &'a str,
        starred: usize,
        seed: &'a Track,
        size: usize,
    }

    let mut groups: HashMap<&str, Group> = HashMap::new();
    for track in tracks {
        let Some(id) = track.id.as_deref() else {
            continue;
        };
        if !track.playable {
            continue;
        }
        let name = track
            .artist_refs
            .first()
            .map(|artist| artist.name.as_str())
            .unwrap_or(&track.artists);
        let group = groups.entry(artist_key(track)).or_insert_with(|| Group {
            name,
            starred: 0,
            seed: track,
            size: 0,
        });
        group.size += 1;
        if favorites.contains(id) {
            group.starred += 1;
            if !favorites.contains(group.seed.id.as_deref().unwrap_or_default()) {
                group.seed = track;
            }
        }
    }

    let mut groups: Vec<Group> = groups.into_values().collect();
    groups.sort_by(|a, b| {
        b.starred
            .cmp(&a.starred)
            .then_with(|| b.size.cmp(&a.size))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    groups
        .into_iter()
        .take(limit)
        .map(|group| Mix {
            name: group.name.to_owned(),
            seed: group.seed.clone(),
        })
        .collect()
}

/// What a candidate is matched against, gathered from the seed once so scoring it is a
/// handful of set lookups.
struct SeedRefs<'a> {
    artist_ids: HashSet<&'a str>,
    artist_names: HashSet<String>,
    tags: HashSet<String>,
    credits: HashSet<String>,
}

/// The `MIX_LIMIT` best tracks of `candidates` for a mix on `seed`, best first. `heard`
/// holds the ids the queue already knows; they never come back. `year` is the seed album's
/// year where the shelf knows it, and `years` maps the candidates' album ids to theirs.
pub(crate) fn score(
    seed: &Track,
    candidates: &[Track],
    heard: &HashSet<String>,
    year: Option<i32>,
    years: &HashMap<&str, i32>,
) -> Vec<Track> {
    let refs = SeedRefs {
        artist_ids: seed
            .artist_refs
            .iter()
            .filter_map(|artist| artist.id.as_deref())
            .collect(),
        artist_names: seed
            .artist_refs
            .iter()
            .map(|artist| normalize(&artist.name))
            .collect(),
        tags: seed.tags.iter().map(|tag| normalize(tag)).collect(),
        credits: seed
            .credits
            .iter()
            .map(|credit| normalize(&credit.name))
            .collect(),
    };

    let mut ranked: Vec<(usize, f32)> = candidates
        .iter()
        .enumerate()
        .filter(|(_, track)| {
            track.playable
                && track
                    .id
                    .as_ref()
                    .is_some_and(|id| !heard.contains(id) && seed.id.as_ref() != Some(id))
        })
        .map(|(index, track)| (index, points(seed, &refs, track, year, years)))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));

    // Picks are greedy with a running price on the artists and albums already placed: a
    // library of one artist still fills the mix, but a varied one never settles on it.
    let mut picked = Vec::with_capacity(MIX_LIMIT.min(ranked.len()));
    let mut taken = vec![false; ranked.len()];
    let mut placed_artists: HashMap<&str, usize> = HashMap::new();
    let mut placed_albums: HashMap<&str, usize> = HashMap::new();
    while picked.len() < MIX_LIMIT {
        let mut best: Option<(usize, f32)> = None;
        for (slot, &(index, base)) in ranked.iter().enumerate() {
            if taken[slot] {
                continue;
            }
            let track = &candidates[index];
            let adjusted = base
                - ARTIST_REPEAT
                    * placed_artists.get(artist_key(track)).copied().unwrap_or(0) as f32
                - ALBUM_REPEAT
                    * track
                        .album_id
                        .as_deref()
                        .and_then(|id| placed_albums.get(id))
                        .copied()
                        .unwrap_or(0) as f32;
            if best.is_none_or(|(_, points)| adjusted > points) {
                best = Some((slot, adjusted));
            }
        }
        let Some((slot, _)) = best else {
            break;
        };
        taken[slot] = true;
        let track = &candidates[ranked[slot].0];
        *placed_artists.entry(artist_key(track)).or_default() += 1;
        if let Some(album) = track.album_id.as_deref() {
            *placed_albums.entry(album).or_default() += 1;
        }
        picked.push(track.clone());
    }
    picked
}

/// `count` unheard tracks out of `pool`, shuffled and spread so one artist cannot take two
/// picks while the pool still has another to give.
pub(crate) fn batch(pool: &[&Track], heard: &HashSet<String>, count: usize) -> Vec<Track> {
    let mut fresh: Vec<&Track> = pool
        .iter()
        .copied()
        .filter(|track| track.playable && track.id.as_ref().is_some_and(|id| !heard.contains(id)))
        .collect();
    fastrand::shuffle(&mut fresh);

    let mut artists: HashSet<&str> = HashSet::new();
    let mut rest: Vec<&Track> = Vec::new();
    let mut picks: Vec<&Track> = Vec::with_capacity(count.min(fresh.len()));
    for track in fresh {
        match artists.insert(artist_key(track)) {
            true => picks.push(track),
            false => rest.push(track),
        }
    }
    picks.extend(rest);
    picks.truncate(count);
    picks.into_iter().cloned().collect()
}

/// The score a candidate earns against the seed, before the spread across picks adjusts it.
fn points(
    seed: &Track,
    refs: &SeedRefs,
    track: &Track,
    year: Option<i32>,
    years: &HashMap<&str, i32>,
) -> f32 {
    let mut points = 0.;
    let same_artist = track.artist_refs.iter().any(|artist| {
        artist
            .id
            .as_deref()
            .is_some_and(|id| refs.artist_ids.contains(id))
            || refs.artist_names.contains(&normalize(&artist.name))
    }) || (!track.artists.is_empty()
        && normalize(&track.artists) == normalize(&seed.artists));
    if same_artist {
        points += ARTIST;
    }
    if track.album_id.is_some() && track.album_id == seed.album_id {
        points += ALBUM;
    }
    let shared = track
        .tags
        .iter()
        .filter(|tag| refs.tags.contains(&normalize(tag)))
        .count();
    points += (shared as f32 * TAG).min(TAG_CAP);
    let shared = track
        .credits
        .iter()
        .filter(|credit| refs.credits.contains(&normalize(&credit.name)))
        .count();
    points += (shared as f32 * CREDIT).min(CREDIT_CAP);
    let gap = seed.duration.abs_diff(track.duration).as_secs_f32();
    points += DURATION * (1. - gap / DURATION_FADE).max(0.);
    points += POPULARITY * track.popularity as f32 / 100.;
    if let Some(count) = track.playcount {
        points +=
            PLAYCOUNT * (count as f32 + 1.).ln().min(PLAYCOUNT_SATURATION) / PLAYCOUNT_SATURATION;
    }
    if track.explicit == seed.explicit {
        points += EXPLICIT;
    }
    if let Some(year) = year
        && let Some(&other) = track.album_id.as_deref().and_then(|id| years.get(id))
        && (year - other).abs() <= ERA_SPAN
    {
        points += ERA;
    }
    points
}

/// The artist a track belongs to for counting: its first credited id, else its names line.
fn artist_key(track: &Track) -> &str {
    track
        .artist_refs
        .first()
        .map(|artist| artist.id.as_deref().unwrap_or(&artist.name))
        .unwrap_or(&track.artists)
}

fn normalize(name: &str) -> String {
    name.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Duration;

    use music::{ArtistRef, Track};

    use super::seeds;

    fn track(index: usize, artist: &str, artist_id: &str) -> Track {
        Track {
            id: Some(format!("track-{index}")),
            name: format!("Track {index}"),
            playable: true,
            artists: artist.to_owned(),
            artist_refs: vec![ArtistRef {
                name: artist.to_owned(),
                id: Some(artist_id.to_owned()),
            }],
            album: String::new(),
            album_id: None,
            cover: None,
            duration: Duration::from_secs(180),
            added_at: None,
            added_by: None,
            playcount: None,
            popularity: 0,
            explicit: false,
            track_number: 0,
            disc_number: 0,
            tags: Vec::new(),
            languages: Vec::new(),
            credits: Vec::new(),
        }
    }

    #[test]
    fn seeds_group_by_artist_and_prefer_favorites() {
        let tracks = vec![
            track(0, "Solo", "a-solo"),
            track(1, "Big", "a-big"),
            track(2, "Big", "a-big"),
            track(3, "Solo", "a-solo"),
        ];
        // Big has more tracks, but Solo holds the only favorite and leads.
        let favorites = HashSet::from(["track-3".to_owned()]);

        let mixes = seeds(&tracks, &favorites, 10);

        assert_eq!(mixes.len(), 2);
        assert_eq!(mixes[0].name, "Solo");
        assert_eq!(mixes[0].seed.id.as_deref(), Some("track-3"));
        assert_eq!(mixes[1].name, "Big");
        assert_eq!(mixes[1].seed.id.as_deref(), Some("track-1"));
    }

    #[test]
    fn seeds_skip_tracks_that_cannot_play_or_be_named() {
        let mut unplayable = track(0, "Quiet", "a-quiet");
        unplayable.playable = false;
        let mut anonymous = track(1, "Unnamed", "a-unnamed");
        anonymous.id = None;
        let tracks = vec![unplayable, anonymous];

        assert!(seeds(&tracks, &HashSet::new(), 10).is_empty());
    }

    #[test]
    fn seeds_honor_the_limit() {
        let tracks: Vec<Track> = (0..20)
            .map(|index| track(index, &format!("Artist {index}"), &format!("a-{index}")))
            .collect();

        let mixes = seeds(&tracks, &HashSet::new(), 5);

        assert_eq!(mixes.len(), 5);
    }

    #[test]
    fn seeds_fall_back_to_the_artists_line() {
        let mut plain = track(0, "Duo Band", "");
        plain.artist_refs = Vec::new();
        let mixes = seeds(&[plain], &HashSet::new(), 10);

        assert_eq!(mixes[0].name, "Duo Band");
    }
}
