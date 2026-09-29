use std::time::Duration;

use opensubsonic::data::{AlbumId3, ArtistId3, Child, ItemDate, RecordLabel};

use crate::{Album, ArtistRef, Playlist, ReleaseType, SavedArtist, Track, UserProfile};

pub fn track(song: Child, cover: Option<String>) -> Track {
    let (artists, artist_refs) = artists_of(
        song.artist,
        song.artist_id,
        song.artists.as_ref(),
        song.display_artist,
    );
    // The star date is when the listener added the song to their library and wins over
    // created, which is when the file reached the server.
    let added_at = song
        .starred
        .as_deref()
        .or(song.created.as_deref())
        .and_then(iso8601);
    Track {
        id: Some(song.id.clone()),
        name: song.title,
        playable: !song.is_video.unwrap_or(false),
        artists,
        artist_refs,
        album: song.album.unwrap_or_default(),
        album_id: song.album_id.filter(|id| !id.is_empty()),
        cover,
        duration: Duration::from_secs(song.duration.unwrap_or(0).max(0) as u64),
        added_at,
        added_by: None,
        playcount: song.play_count.map(|count| count as u64),
        popularity: 0,
        explicit: song.explicit_status.as_deref() == Some("explicit"),
        track_number: song.track.unwrap_or(0).max(0) as u32,
        disc_number: song.disc_number.unwrap_or(1).max(1) as u32,
        // The OpenSubsonic genres list wins; a plain Subsonic server sends only `genre`.
        tags: song
            .genres
            .map(|genres| {
                genres
                    .into_iter()
                    .map(|genre| genre.name.trim().to_lowercase())
                    .filter(|name| !name.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|genres| !genres.is_empty())
            .unwrap_or_else(|| {
                song.genre
                    .map(|genre| genre.trim().to_lowercase())
                    .filter(|genre| !genre.is_empty())
                    .into_iter()
                    .collect()
            }),
        languages: Vec::new(),
        credits: Vec::new(),
    }
}

pub fn album(source: AlbumId3, cover: Option<String>, cover_large: Option<String>) -> Album {
    let (artists, artist_refs) = artists_of(
        source.artist,
        source.artist_id,
        source.artists.as_ref(),
        source.display_artist,
    );
    let year = source.year.unwrap_or(0);
    let label = labels(source.record_labels.as_deref());
    Album {
        id: source.id,
        name: source.name,
        artists,
        artist_refs,
        cover,
        cover_large,
        release_type: release_type(source.release_types.as_deref(), source.is_compilation),
        year,
        track_count: source.song_count.unwrap_or(0).max(0) as u32,
        release_date: release_date(
            source.release_date.as_ref(),
            source.original_release_date.as_ref(),
            year,
        ),
        label,
        copyrights: Vec::new(),
        added_at: source
            .starred
            .as_deref()
            .or(source.created.as_deref())
            .and_then(iso8601),
    }
}

/// The record labels an OpenSubsonic server lists for an album, joined into one credit. A
/// server without the extension lists none, which leaves the label empty.
pub fn labels(labels: Option<&[RecordLabel]>) -> String {
    labels
        .unwrap_or_default()
        .iter()
        .map(|label| label.name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The kind of release an OpenSubsonic server lists for an album. A server without the extension
/// lists no types, which leaves every album an album unless it is flagged as a compilation.
pub fn release_type(types: Option<&[String]>, compilation: Option<bool>) -> ReleaseType {
    ReleaseType::from_musicbrainz(
        types.unwrap_or_default().iter().map(String::as_str),
        compilation.unwrap_or(false),
    )
}

/// The fields both OpenSubsonic playlist shapes carry, bundled so `playlist` does not take
/// them one argument each.
pub struct PlaylistSource<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub owner: Option<&'a str>,
    pub public: bool,
    pub track_count: u32,
    pub cover: Option<String>,
    /// The server's ISO 8601 stamp of the last edit, stored as `modified_at`.
    pub changed: Option<&'a str>,
}

/// One playlist row. The owner is only a username no `user` lookup can resolve, so the id
/// stays empty rather than pointing a `Destination::User` link at nothing; `owned` still
/// comes from the name matching the signed-in user.
pub fn playlist(source: PlaylistSource<'_>, username: &str) -> Playlist {
    let owner = source.owner.unwrap_or_default().to_owned();
    Playlist {
        id: source.id.to_owned(),
        name: source.name.to_owned(),
        owner: owner.clone(),
        owner_id: String::new(),
        owned: !owner.is_empty() && owner == username,
        collaborative: false,
        blend: false,
        public: source.public,
        cover: source.cover,
        track_count: source.track_count,
        modified_at: source.changed.and_then(iso8601),
    }
}

pub fn saved_artist(source: &ArtistId3, cover: Option<String>) -> SavedArtist {
    SavedArtist {
        id: source.id.clone(),
        name: source.name.clone(),
        cover,
        added_at: source.starred.as_deref().and_then(iso8601),
    }
}

pub fn profile(username: String) -> UserProfile {
    UserProfile {
        id: username.clone(),
        display_name: username,
        avatar: None,
    }
}

pub(crate) fn artists_of(
    artist: Option<String>,
    artist_id: Option<String>,
    many: Option<&Vec<ArtistId3>>,
    display: Option<String>,
) -> (String, Vec<ArtistRef>) {
    if let Some(list) = many.filter(|list| !list.is_empty()) {
        let refs = list
            .iter()
            .map(|item| ArtistRef {
                name: item.name.clone(),
                id: Some(item.id.clone()),
            })
            .collect();
        return match display {
            Some(name) => (name, refs),
            None => {
                let joined = list
                    .iter()
                    .map(|item| item.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                (joined, refs)
            }
        };
    }
    let name = display.or(artist).unwrap_or_default();
    let id = artist_id.filter(|id| !id.is_empty());
    let refs = match (name.is_empty(), id) {
        (true, _) => Vec::new(),
        (false, id) => vec![ArtistRef {
            name: name.clone(),
            id,
        }],
    };
    (name, refs)
}

/// The release date string an album shows: the OpenSubsonic `releaseDate` triple first, its
/// `originalReleaseDate` next, and the bare year when a server lists neither.
pub(crate) fn release_date(
    date: Option<&ItemDate>,
    original: Option<&ItemDate>,
    year: i32,
) -> String {
    date.or(original)
        .and_then(item_date)
        .unwrap_or_else(|| match year {
            0 => String::new(),
            _ => year.to_string(),
        })
}

/// An OpenSubsonic date triple as `YYYY-MM-DD`, shortening to `YYYY-MM` or `YYYY` when the
/// server leaves the finer parts out. None when there is not even a year.
fn item_date(date: &ItemDate) -> Option<String> {
    let year = date.year?;
    Some(match (date.month, date.day) {
        (Some(month), Some(day)) => format!("{year:04}-{month:02}-{day:02}"),
        (Some(month), None) => format!("{year:04}-{month:02}"),
        _ => format!("{year:04}"),
    })
}

/// An ISO 8601 stamp such as `2024-05-12T10:23:44.1Z` as seconds since the epoch. A
/// `+02:00`-style offset is applied, a fraction of a second is dropped and a bare date
/// counts as midnight UTC. A stamp that does not fit the shape is None.
pub(crate) fn iso8601(stamp: &str) -> Option<i64> {
    let stamp = stamp.trim();
    let (date, time) = stamp
        .split_once('T')
        .or_else(|| stamp.split_once(' '))
        .unwrap_or((stamp, ""));
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // The clock may end in Z or a +HH:MM / -HH:MM offset, which the first sign marks.
    let (time, offset) = match time.find(['+', '-']) {
        Some(at) => {
            let (time, zone) = time.split_at(at);
            let sign = match zone.as_bytes()[0] {
                b'+' => -1i64,
                _ => 1,
            };
            let mut parts = zone[1..].split(':');
            let hours: i64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
            let minutes: i64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
            (time, sign * (hours * 3_600 + minutes * 60))
        }
        None => (time, 0),
    };
    let mut clock = time.trim_end_matches('Z').split(':');
    let hour: i64 = clock
        .next()
        .filter(|part| !part.is_empty())
        .and_then(|hour| hour.parse().ok())
        .unwrap_or(0);
    let minute: i64 = clock.next().unwrap_or("0").parse().unwrap_or(0);
    let second: i64 = clock
        .next()
        .and_then(|second| second.split('.').next())
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    Some(days_since_epoch(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second + offset)
}

/// The days a civil date sits from 1970-01-01.
fn days_since_epoch(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
