use std::collections::{HashMap, HashSet};

use music::Track;

/// How many artist mixes the Mixes page lists at most.
pub(crate) const MIXES: usize = 12;

/// How many tracks a mix lines up behind its seed at most.
pub(crate) const MIX_LIMIT: usize = 30;

/// How many tracks the forever queue lines up once it runs short, past the lookahead that
/// asks for them, so one fill buys a stretch of playback.
pub(crate) const FOREVER_BATCH: usize = 20;

/// How far back listening history still counts as already heard for the forever queue, and
/// how deep a play can sit before it stops weighing on taste and recency.
pub(crate) const FOREVER_HISTORY: usize = 200;

/// How many artists a mix gathers at most. The lead fronts it and the rest widen what it
/// scores for; past a handful a cluster stops meaning anything.
const MEMBERS: usize = 4;

/// The affinity that pulls an artist into a lead's mix instead of anchoring its own: a
/// feature and a couple of shared albums, or a run of overlapping rare tags, reaches it.
const JOIN: f32 = 7.;

/// What a favorite is worth in an artist's taste score.
const TASTE_STARRED: f32 = 6.;
/// What a play is worth at its freshest; rank halves it every `RECENT_SPAN` plays.
const TASTE_RECENT: f32 = 5.;
const RECENT_SPAN: f32 = 40.;
/// A deep shelf presence counts a little, on a log scale so it cannot run away.
const TASTE_SIZE: f32 = 2.;

/// Two artists on one track; a feature is the strongest hint they belong in one mix.
const FEATURE: f32 = 10.;
/// Two artists on one album; looser company than a feature.
const APPEAR: f32 = 5.;
/// The most shared releases can ever add to an affinity edge.
const APPEAR_CAP: f32 = 15.;
/// The most a tag overlap can add to an affinity edge.
const TAG_EDGE_CAP: f32 = 6.;
/// Each credit two artists share, capped so a staff writer cannot weld a cluster together.
const CREDIT_EDGE: f32 = 2.;
const CREDIT_EDGE_CAP: f32 = 4.;

/// A track by one of the mix's own artists.
const ARTIST: f32 = 10.;
/// A track by an artist related to the mix, scaled by its strongest edge and capped.
const RELATED: f32 = 7.;
/// Off the seed's own album; an album is shorter company than an artist.
const ALBUM: f32 = 4.;
/// A track as long as the cluster's average, fading to nothing `DURATION_FADE` seconds apart.
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
/// Near the cluster's mean year, fading to nothing `ERA_SPAN` years apart.
const ERA: f32 = 3.;
const ERA_SPAN: f32 = 10.;
/// A starred track is the listener's own anchor inside a mix.
const STARRED: f32 = 3.;
/// What a track loses for being played recently, scaled by how recent; keeps a rebuilt mix
/// from opening with whatever just finished.
const RECENT_PENALTY: f32 = 3.;
/// A track never played and never starred gets a small discovery boost, so a mix is not
/// all reruns.
const FRESH: f32 = 1.5;
/// A stable per-track nudge, so mixes on the same shelf still read differently.
const JITTER: f32 = 0.5;
/// What each track an artist or an album already placed costs their next one, so a mix
/// spreads across the library instead of settling on whoever scored highest.
const ARTIST_REPEAT: f32 = 6.;
const ALBUM_REPEAT: f32 = 2.;

/// The affinity an unheard artist earns per pick while the forever queue walks, scaled by
/// its strongest edge to the last artist picked.
const ROAM: f32 = 6.;
/// The chance a forever pick ignores affinity and jumps anywhere taste points, so the walk
/// wanders the library instead of circling one corner of it.
const HOP: f32 = 0.25;
/// The taste floor and scale of a forever pick, so an unplayed artist can still be chosen
/// but a loved one leads.
const TASTE_BASE: f32 = 0.5;
const TASTE_SCALE: f32 = 2.;

/// A ready-made mix on the Mixes page: the artists it is named for, and the track
/// `Playback::play_mix` opens with and scores the shelf against.
#[derive(Clone, Debug)]
pub struct Mix {
    /// The member artist names, lead first, joined for the card's "{name} Mix" title.
    pub name: String,
    /// The track the mix opens with; its album, cover and flags also anchor scoring.
    pub seed: Track,
    /// The artist keys the mix scores for, lead first.
    pub(crate) members: Vec<String>,
}

impl Mix {
    /// A one-artist mix around `seed`, for a track asked to start a mix of its own.
    pub fn around(seed: &Track) -> Self {
        Self {
            name: lead_name(seed).to_owned(),
            seed: seed.clone(),
            members: vec![artist_key(seed).to_owned()],
        }
    }
}

/// What the shelf knows about one of its artists, gathered once so mixes can be seeded,
/// clustered and scored without walking the track list again.
#[derive(Clone)]
struct Artist<'a> {
    /// The name a mix lists it by.
    name: &'a str,
    /// Where the artist's tracks sit in the index's pool.
    tracks: Vec<usize>,
    /// How many of them are favorites.
    starred: u32,
    /// Recent plays, rank-weighted so last night's binge outranks last year's.
    recent: f32,
    /// Genre tag counts, normalized; sharing a rare one is what pulls artists into a mix.
    tags: HashMap<String, f32>,
    /// Credit names, normalized.
    credits: HashSet<String>,
    /// Artists sharing a track or an album, weighted by how and how often.
    neighbors: HashMap<&'a str, f32>,
    year_total: i64,
    year_count: u32,
    duration_total: f64,
    /// The artist's track best suited to front a mix, and the rank that picked it.
    best: usize,
    best_rank: f32,
}

impl<'a> Artist<'a> {
    fn of(track: &'a Track) -> Self {
        Self {
            name: lead_name(track),
            tracks: Vec::new(),
            starred: 0,
            recent: 0.,
            tags: HashMap::new(),
            credits: HashSet::new(),
            neighbors: HashMap::new(),
            year_total: 0,
            year_count: 0,
            duration_total: 0.,
            best: 0,
            best_rank: f32::MIN,
        }
    }

    /// How much of the artist the listener keeps around: favorites first, recent plays
    /// close behind, sheer shelf presence last.
    fn taste(&self) -> f32 {
        TASTE_STARRED * self.starred as f32
            + TASTE_RECENT * self.recent
            + TASTE_SIZE * (self.tracks.len() as f32 + 1.).ln()
    }
}

/// A shelf folded into what mixes need: each artist's tags, credits, neighbors and taste,
/// plus the favorites, album years and play recency that scoring reads per track. Building
/// it is one pass over the pool, and every later lookup is a map hit.
pub(crate) struct Index<'a> {
    tracks: Vec<&'a Track>,
    artists: HashMap<&'a str, Artist<'a>>,
    /// How rare each tag is across the shelf's artists; a shared rare tag binds tighter
    /// than a genre everyone carries.
    tag_rarity: HashMap<String, f32>,
    favorites: &'a HashSet<String>,
    years: &'a HashMap<&'a str, i32>,
    /// What a track id's most recent play still counts for: 1 just played, fading out over
    /// `FOREVER_HISTORY` plays of history.
    recency: HashMap<String, f32>,
    /// The highest taste score in the index, for normalising weights.
    top_taste: f32,
}

impl<'a> Index<'a> {
    /// Folds `tracks` into per-artist entries. `favorites` and `years` come from the same
    /// shelf; `recent` is listening history newest first and only marks what it names.
    pub(crate) fn new(
        tracks: impl IntoIterator<Item = &'a Track>,
        favorites: &'a HashSet<String>,
        years: &'a HashMap<&'a str, i32>,
        recent: impl IntoIterator<Item = &'a Track>,
    ) -> Self {
        let tracks: Vec<&'a Track> = tracks.into_iter().collect();
        let mut artists: HashMap<&'a str, Artist<'a>> = HashMap::new();
        // Co-appearances, gathered while the artists are built and applied after, since
        // one track can name artists on both ends of the edge.
        let mut pairs: Vec<(&'a str, &'a str, f32)> = Vec::new();
        let mut albums: HashMap<&'a str, Vec<&'a str>> = HashMap::new();

        for (index, track) in tracks.iter().enumerate() {
            if !track.playable || track.id.is_none() {
                continue;
            }
            let key = artist_key(track);
            let artist = artists.entry(key).or_insert_with(|| Artist::of(track));
            artist.tracks.push(index);
            let starred = track
                .id
                .as_ref()
                .is_some_and(|id| favorites.contains(id.as_str()));
            if starred {
                artist.starred += 1;
            }
            for tag in &track.tags {
                *artist.tags.entry(normalize(tag)).or_default() += 1.;
            }
            artist
                .credits
                .extend(track.credits.iter().map(|credit| normalize(&credit.name)));
            if let Some(year) = track.album_id.as_deref().and_then(|album| years.get(album)) {
                artist.year_total += *year as i64;
                artist.year_count += 1;
            }
            artist.duration_total += track.duration.as_secs_f64();
            // A favorite fronts its artist's mix before anything, and among them the most
            // played and most popular does.
            let rank = POPULARITY * track.popularity as f32 / 100.
                + track.playcount.map_or(0., |count| (count as f32 + 1.).ln())
                + match starred {
                    true => 100.,
                    false => 0.,
                };
            if rank > artist.best_rank {
                artist.best = index;
                artist.best_rank = rank;
            }
            for featured in track.artist_refs.iter().skip(1) {
                let other = featured.id.as_deref().unwrap_or(&featured.name);
                if other != key {
                    pairs.push((key, other, FEATURE));
                }
            }
            if let Some(album) = track.album_id.as_deref() {
                let keys = albums.entry(album).or_default();
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }

        // A compilation crediting dozens of artists says little about any pair of them, so
        // only the ones a band could actually share count.
        for keys in albums.values().filter(|keys| keys.len() <= MEMBERS * 2) {
            for (i, &a) in keys.iter().enumerate() {
                for &b in &keys[i + 1..] {
                    pairs.push((a, b, APPEAR));
                }
            }
        }
        for (a, b, weight) in pairs {
            if let Some(artist) = artists.get_mut(a) {
                *artist.neighbors.entry(b).or_default() += weight;
            }
            if let Some(artist) = artists.get_mut(b) {
                *artist.neighbors.entry(a).or_default() += weight;
            }
        }

        let mut recency: HashMap<String, f32> = HashMap::new();
        for (rank, track) in recent.into_iter().enumerate().take(FOREVER_HISTORY) {
            let weight = 1. / (1. + rank as f32 / RECENT_SPAN);
            if let Some(id) = track.id.as_deref() {
                recency.entry(id.to_owned()).or_insert(weight);
            }
            if let Some(artist) = artists.get_mut(artist_key(track)) {
                artist.recent += weight;
            }
        }

        let mut counts: HashMap<&str, u32> = HashMap::new();
        for artist in artists.values() {
            for tag in artist.tags.keys() {
                *counts.entry(tag.as_str()).or_default() += 1;
            }
        }
        let tag_rarity = counts
            .into_iter()
            .map(|(tag, held)| {
                (
                    tag.to_owned(),
                    (1. + artists.len() as f32 / held as f32).ln(),
                )
            })
            .collect();

        let top_taste = artists
            .values()
            .map(Artist::taste)
            .fold(0., f32::max)
            .max(1.);

        Self {
            tracks,
            artists,
            tag_rarity,
            favorites,
            years,
            recency,
            top_taste,
        }
    }

    /// How strongly two artists belong in one mix: shared releases capped, then the tags
    /// they share weighed by rarity, then a little for each credit they share.
    fn edge(&self, a: &Artist, b_key: &str, b: &Artist) -> f32 {
        let mut edge = a
            .neighbors
            .get(b_key)
            .copied()
            .unwrap_or(0.)
            .min(APPEAR_CAP);
        let shared: f32 = a
            .tags
            .iter()
            .map(|(tag, count)| {
                b.tags.get(tag).copied().unwrap_or(0.).min(*count)
                    * self.tag_rarity.get(tag).copied().unwrap_or(1.)
            })
            .sum();
        edge += shared.min(TAG_EDGE_CAP);
        let shared = a
            .credits
            .iter()
            .filter(|credit| b.credits.contains(*credit))
            .count() as f32;
        edge + (shared * CREDIT_EDGE).min(CREDIT_EDGE_CAP)
    }
}

/// The mixes a shelf's tracks make, best first and `limit` at most. Artists rank by taste,
/// and each in turn joins the cluster it is closest to or starts its own, the way a daily
/// mix gathers the artists that sound alike. A mix's seed is its lead's best track:
/// favorites first, then the most played.
pub(crate) fn seeds(index: &Index, limit: usize) -> Vec<Mix> {
    let mut ranked: Vec<(&str, &Artist)> = index
        .artists
        .iter()
        .map(|(key, artist)| (*key, artist))
        .collect();
    ranked.sort_by(|(a_key, a), (b_key, b)| {
        b.taste()
            .total_cmp(&a.taste())
            .then_with(|| a_key.cmp(b_key))
    });

    let mut clusters: Vec<Vec<(&str, &Artist)>> = Vec::new();
    for &(key, artist) in &ranked {
        let mut best: Option<(usize, f32)> = None;
        for (slot, cluster) in clusters.iter().enumerate() {
            if cluster.len() >= MEMBERS {
                continue;
            }
            let affinity: f32 = cluster
                .iter()
                .map(|&(member_key, member)| index.edge(artist, member_key, member))
                .sum();
            if affinity >= JOIN && best.is_none_or(|(_, points)| affinity > points) {
                best = Some((slot, affinity));
            }
        }
        match best {
            Some((slot, _)) => clusters[slot].push((key, artist)),
            // Once the page is full, only artists with somewhere to belong still land.
            None if clusters.len() < limit => clusters.push(vec![(key, artist)]),
            None => {}
        }
    }

    clusters.sort_by(|a, b| {
        let taste = |cluster: &[(&str, &Artist)]| {
            cluster
                .iter()
                .map(|(_, artist)| artist.taste())
                .sum::<f32>()
        };
        taste(b).total_cmp(&taste(a))
    });
    clusters
        .into_iter()
        .map(|mut cluster| {
            cluster.sort_by(|(_, a), (_, b)| b.taste().total_cmp(&a.taste()));
            Mix {
                name: cluster
                    .iter()
                    .map(|(_, artist)| artist.name)
                    .collect::<Vec<_>>()
                    .join(", "),
                seed: index.tracks[cluster[0].1.best].clone(),
                members: cluster.iter().map(|(key, _)| (*key).to_owned()).collect(),
            }
        })
        .collect()
}

/// What a candidate is matched against, gathered from the mix's members once so scoring it
/// is a handful of set lookups. The seed's own artist is added when it is not one already,
/// so a mix started from a lone track still carries a tag and credit profile.
struct Profile<'a> {
    members: Vec<(&'a str, Artist<'a>)>,
    /// The member keys, for telling the mix's own artists from related ones.
    keys: HashSet<&'a str>,
    /// The members' normalized names, for catching a keyed artist named another way.
    names: HashSet<String>,
    /// The members' mean album year and track length, where the shelf knows them.
    era: Option<f32>,
    duration: f32,
}

impl<'a> Profile<'a> {
    fn of(index: &'a Index<'a>, mix: &'a Mix, seed: &'a Track) -> Self {
        let mut keys = HashSet::new();
        let mut names = HashSet::new();
        let mut members: Vec<(&'a str, Artist<'a>)> = Vec::new();
        for key in &mix.members {
            if let Some(artist) = index.artists.get(key.as_str()) {
                keys.insert(key.as_str());
                names.insert(normalize(artist.name));
                members.push((key.as_str(), artist.clone()));
            }
        }

        let seed_key = artist_key(seed);
        if !keys.contains(seed_key) {
            let mut artist = Artist::of(seed);
            for tag in &seed.tags {
                *artist.tags.entry(normalize(tag)).or_default() += 1.;
            }
            artist
                .credits
                .extend(seed.credits.iter().map(|credit| normalize(&credit.name)));
            if let Some(year) = seed
                .album_id
                .as_deref()
                .and_then(|album| index.years.get(album))
            {
                artist.year_total = *year as i64;
                artist.year_count = 1;
            }
            artist.duration_total = seed.duration.as_secs_f64();
            artist.tracks.push(0);
            names.insert(normalize(artist.name));
            keys.insert(seed_key);
            members.push((seed_key, artist));
        }

        let (mut year_total, mut year_count, mut duration_total, mut tracks) =
            (0_i64, 0_u32, 0_f64, 0_usize);
        for (_, artist) in &members {
            year_total += artist.year_total;
            year_count += artist.year_count;
            duration_total += artist.duration_total;
            tracks += artist.tracks.len();
        }
        Self {
            members,
            keys,
            names,
            era: (year_count > 0).then(|| year_total as f32 / year_count as f32),
            duration: match tracks {
                0 => seed.duration.as_secs_f32(),
                _ => (duration_total / tracks as f64) as f32,
            },
        }
    }
}

/// The `MIX_LIMIT` best tracks of the index's pool for `mix`, best first. `heard` holds
/// the ids the queue already knows; they never come back.
pub(crate) fn score(index: &Index, mix: &Mix, heard: &HashSet<String>) -> Vec<Track> {
    let seed = &mix.seed;
    let profile = Profile::of(index, mix, seed);

    let mut ranked: Vec<(usize, f32)> = index
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, track)| {
            track.playable
                && track
                    .id
                    .as_ref()
                    .is_some_and(|id| !heard.contains(id) && seed.id.as_ref() != Some(id))
        })
        .map(|(slot, track)| (slot, points(index, &profile, seed, track)))
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
        for (slot, &(index_slot, base)) in ranked.iter().enumerate() {
            if taken[slot] {
                continue;
            }
            let track = index.tracks[index_slot];
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
        let track = index.tracks[ranked[slot].0];
        *placed_artists.entry(artist_key(track)).or_default() += 1;
        if let Some(album) = track.album_id.as_deref() {
            *placed_albums.entry(album).or_default() += 1;
        }
        picked.push(track.clone());
    }
    picked
}

/// `count` unheard tracks out of the pool, walked rather than picked at random: each pick
/// leans toward artists near the last one by taste and affinity, with the odd hop
/// elsewhere so a fill wanders the library instead of circling one corner of it. `anchor`
/// is what is playing now, and starts the walk where the listener already is.
pub(crate) fn batch(
    index: &Index,
    heard: &HashSet<String>,
    anchor: Option<&Track>,
    count: usize,
) -> Vec<Track> {
    // Each artist's unheard tracks by pool position, so a pick never repeats.
    let mut eligible: Vec<(&str, &Artist, Vec<usize>)> = index
        .artists
        .iter()
        .filter_map(|(key, artist)| {
            let open: Vec<usize> = artist
                .tracks
                .iter()
                .copied()
                .filter(|&slot| {
                    index.tracks[slot]
                        .id
                        .as_ref()
                        .is_some_and(|id| !heard.contains(id))
                })
                .collect();
            (!open.is_empty()).then_some((*key, artist, open))
        })
        .collect();

    let mut anchor = anchor.map(artist_key);
    let mut picked = Vec::with_capacity(count.min(eligible.len()));
    while picked.len() < count && !eligible.is_empty() {
        let anchored = anchor.and_then(|key| index.artists.get(key).map(|a| (key, a)));
        let hop = anchored.is_none() || fastrand::f32() < HOP;
        let weight = |artist: &Artist| {
            let taste = TASTE_BASE + TASTE_SCALE * artist.taste() / index.top_taste;
            match (hop, anchored) {
                (false, Some((anchor_key, anchor_artist))) => {
                    taste + ROAM * (index.edge(artist, anchor_key, anchor_artist) / JOIN).min(1.)
                }
                _ => taste,
            }
        };
        let total: f32 = eligible.iter().map(|(_, artist, _)| weight(artist)).sum();

        let mut roll = fastrand::f32() * total;
        let mut chosen = eligible.len() - 1;
        for (slot, (_, artist, _)) in eligible.iter().enumerate() {
            roll -= weight(artist);
            if roll <= 0. {
                chosen = slot;
                break;
            }
        }

        let key = eligible[chosen].0;
        let open = &mut eligible[chosen].2;
        let slot = open.swap_remove(fastrand::usize(..open.len()));
        if open.is_empty() {
            eligible.swap_remove(chosen);
        }
        picked.push(index.tracks[slot].clone());
        anchor = Some(key);
    }
    picked
}

/// The score a candidate earns against the mix, before the spread across picks adjusts it.
/// A member's own track leads; a related artist's follows by its strongest edge to the
/// cluster, and the rest is familiarity weighed against freshness.
fn points(index: &Index, profile: &Profile, seed: &Track, track: &Track) -> f32 {
    let mut points = 0.;
    let own = match track.artist_refs.is_empty() {
        true => {
            profile.keys.contains(artist_key(track))
                || profile.names.contains(&normalize(&track.artists))
        }
        false => track.artist_refs.iter().any(|artist| {
            let key = artist.id.as_deref().unwrap_or(&artist.name);
            profile.keys.contains(key) || profile.names.contains(&normalize(&artist.name))
        }),
    };
    if own {
        points += ARTIST;
    } else {
        let mut relatedness: f32 = 0.;
        let keys = track
            .artist_refs
            .iter()
            .map(|artist| artist.id.as_deref().unwrap_or(&artist.name))
            .chain(std::iter::once(artist_key(track)));
        for key in keys {
            let Some(candidate) = index.artists.get(key) else {
                continue;
            };
            for (member_key, member) in &profile.members {
                relatedness = relatedness.max(index.edge(candidate, member_key, member));
            }
        }
        points += RELATED * (relatedness / JOIN).min(1.);
    }

    if track.album_id.is_some() && track.album_id == seed.album_id {
        points += ALBUM;
    }
    let gap = (profile.duration - track.duration.as_secs_f32()).abs();
    points += DURATION * (1. - gap / DURATION_FADE).max(0.);
    points += POPULARITY * track.popularity as f32 / 100.;
    if let Some(count) = track.playcount {
        points +=
            PLAYCOUNT * (count as f32 + 1.).ln().min(PLAYCOUNT_SATURATION) / PLAYCOUNT_SATURATION;
    }
    if track.explicit == seed.explicit {
        points += EXPLICIT;
    }
    if let Some(era) = profile.era
        && let Some(year) = track
            .album_id
            .as_deref()
            .and_then(|album| index.years.get(album))
    {
        points += ERA * (1. - (era - *year as f32).abs() / ERA_SPAN).max(0.);
    }

    let id = track.id.as_deref().unwrap_or_default();
    let starred = index.favorites.contains(id);
    if starred {
        points += STARRED;
    }
    match index.recency.get(id) {
        Some(&recency) => points -= RECENT_PENALTY * recency,
        None if !starred => points += FRESH,
        None => {}
    }
    points + JITTER * jitter(id)
}

/// The artist a track belongs to for counting: its first credited id, else its names line.
fn artist_key(track: &Track) -> &str {
    track
        .artist_refs
        .first()
        .map(|artist| artist.id.as_deref().unwrap_or(&artist.name))
        .unwrap_or(&track.artists)
}

/// The name a mix lists an artist by: its first credited name, else its artists line.
fn lead_name(track: &Track) -> &str {
    track
        .artist_refs
        .first()
        .map(|artist| artist.name.as_str())
        .unwrap_or(&track.artists)
}

fn normalize(name: &str) -> String {
    name.trim().to_lowercase()
}

/// A stable per-id value in [0, 1), so a track's nudge never changes between rebuilds.
fn jitter(id: &str) -> f32 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash % 1000) as f32 / 1000.
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::time::Duration;

    use music::{ArtistRef, Track};

    use super::{Index, seeds};

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

    fn index<'a>(
        tracks: &'a [Track],
        favorites: &'a HashSet<String>,
        years: &'a HashMap<&'a str, i32>,
    ) -> Index<'a> {
        Index::new(tracks.iter(), favorites, years, std::iter::empty())
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

        let mixes = seeds(&index(&tracks, &favorites, &HashMap::new()), 10);

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

        assert!(seeds(&index(&tracks, &HashSet::new(), &HashMap::new()), 10,).is_empty());
    }

    #[test]
    fn seeds_honor_the_limit() {
        let tracks: Vec<Track> = (0..20)
            .map(|index| track(index, &format!("Artist {index}"), &format!("a-{index}")))
            .collect();

        let mixes = seeds(&index(&tracks, &HashSet::new(), &HashMap::new()), 5);

        assert_eq!(mixes.len(), 5);
    }

    #[test]
    fn seeds_fall_back_to_the_artists_line() {
        let mut plain = track(0, "Duo Band", "");
        plain.artist_refs = Vec::new();
        let mixes = seeds(&index(&[plain], &HashSet::new(), &HashMap::new()), 10);

        assert_eq!(mixes[0].name, "Duo Band");
    }
}
