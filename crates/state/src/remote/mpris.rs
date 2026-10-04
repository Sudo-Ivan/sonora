use std::ffi::c_void;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

use anyhow::Result;
use futures::future;
use gpui::{App, Task};
use mpris_server::{LoopStatus, Metadata, PlaybackStatus, Player, Time, TrackId};
use music::{LOCAL_TRACK_PREFIX, Track};
use tokio::sync::mpsc;

use super::{BUS_NAME, Command, DISPLAY_NAME};
use crate::{PlaybackState, Repeat};

/// The desktop entry a native install ships, `sonora.desktop`.
const DESKTOP_ENTRY: &str = "sonora";

/// A change to publish on the MPRIS player, applied in the order it was sent.
enum Update {
    Metadata(Metadata),
    Playback(PlaybackStatus, Time),
    Seeked(Time),
    Volume(f64),
    Shuffle(bool),
    Loop(LoopStatus),
}

/// The MPRIS player on the session bus. It lives on the main thread because `mpris_server`
/// hands out a `!Send` player, and every setter only queues an update for it.
pub struct Controls {
    updates: mpsc::UnboundedSender<Update>,
    _server: Task<()>,
}

impl Controls {
    /// Claims the bus name in the background. A failure there is logged and leaves the
    /// setters as no-ops, since the bus is only reached once the player is built.
    pub fn new(
        _hwnd: Option<*mut c_void>,
        commands: mpsc::UnboundedSender<Command>,
        cx: &mut App,
    ) -> Result<Self> {
        let (updates, mut receiver) = mpsc::unbounded_channel();
        let _server = cx.spawn(async move |_| {
            let player = match build(commands).await {
                Ok(player) => player,
                Err(error) => {
                    return log::warn!("remote: cannot register the mpris player: {error}");
                }
            };
            let apply = async {
                while let Some(update) = receiver.recv().await {
                    if let Err(error) = apply(&player, update).await {
                        log::warn!("remote: cannot publish to mpris: {error}");
                    }
                }
            };
            future::join(player.run(), apply).await;
        });
        Ok(Self { updates, _server })
    }

    pub fn describe(&mut self, track: Option<&Track>, cover: Option<&str>) {
        let metadata = match track {
            Some(track) => metadata(track, cover),
            None => Metadata::builder().trackid(TrackId::NO_TRACK).build(),
        };
        self.send(Update::Metadata(metadata));
    }

    pub fn set_playback(&mut self, state: &PlaybackState, at: Duration) {
        let status = match state {
            PlaybackState::Playing | PlaybackState::Loading => PlaybackStatus::Playing,
            PlaybackState::Paused => PlaybackStatus::Paused,
            PlaybackState::Idle | PlaybackState::Failed(_) => PlaybackStatus::Stopped,
        };
        self.send(Update::Playback(status, time(at)));
    }

    pub fn seeked(&mut self, at: Duration) {
        self.send(Update::Seeked(time(at)));
    }

    pub fn set_volume(&mut self, level: f64) {
        self.send(Update::Volume(level));
    }

    pub fn set_shuffle(&mut self, on: bool) {
        self.send(Update::Shuffle(on));
    }

    pub fn set_repeat(&mut self, repeat: Repeat) {
        let status = match repeat {
            Repeat::Off => LoopStatus::None,
            Repeat::All => LoopStatus::Playlist,
            Repeat::One => LoopStatus::Track,
        };
        self.send(Update::Loop(status));
    }

    fn send(&self, update: Update) {
        self.updates.send(update).ok();
    }
}

/// Registers `org.mpris.MediaPlayer2.sonora` and routes every request the player accepts
/// into `commands`.
async fn build(commands: mpsc::UnboundedSender<Command>) -> mpris_server::zbus::Result<Player> {
    let desktop_entry = std::env::var("FLATPAK_ID").unwrap_or_else(|_| DESKTOP_ENTRY.to_owned());
    let player = Player::builder(BUS_NAME)
        .identity(DISPLAY_NAME)
        .desktop_entry(desktop_entry)
        .can_play(true)
        .can_pause(true)
        .can_go_next(true)
        .can_go_previous(true)
        .can_seek(true)
        .can_control(true)
        .can_quit(true)
        .can_raise(true)
        .build()
        .await?;

    let send = move |command| {
        commands.send(command).ok();
    };
    player.connect_play({
        let send = send.clone();
        move |_| send(Command::Play)
    });
    player.connect_pause({
        let send = send.clone();
        move |_| send(Command::Pause)
    });
    player.connect_stop({
        let send = send.clone();
        move |_| send(Command::Pause)
    });
    player.connect_play_pause({
        let send = send.clone();
        move |_| send(Command::Toggle)
    });
    player.connect_next({
        let send = send.clone();
        move |_| send(Command::Next)
    });
    player.connect_previous({
        let send = send.clone();
        move |_| send(Command::Previous)
    });
    player.connect_seek({
        let send = send.clone();
        move |_, offset| {
            let step = Duration::from_micros(offset.as_micros().unsigned_abs());
            match offset.is_negative() {
                true => send(Command::Back(step)),
                false => send(Command::Forward(step)),
            }
        }
    });
    // the spec has a position for another track dropped as stale, and a negative one ignored
    player.connect_set_position({
        let send = send.clone();
        move |player, track, at| {
            let current = player.metadata().trackid();
            if current.as_ref() == Some(track) && !at.is_negative() {
                send(Command::Seek(Duration::from_micros(
                    at.as_micros().unsigned_abs(),
                )));
            }
        }
    });
    player.connect_set_volume({
        let send = send.clone();
        move |_, level| send(Command::Volume(level))
    });
    player.connect_set_shuffle({
        let send = send.clone();
        move |_, on| send(Command::Shuffle(on))
    });
    player.connect_set_loop_status({
        let send = send.clone();
        move |_, status| {
            send(Command::Repeat(match status {
                LoopStatus::None => Repeat::Off,
                LoopStatus::Playlist => Repeat::All,
                LoopStatus::Track => Repeat::One,
            }))
        }
    });
    player.connect_raise({
        let send = send.clone();
        move |_| send(Command::Raise)
    });
    player.connect_quit(move |_| send(Command::Quit));
    Ok(player)
}

async fn apply(player: &Player, update: Update) -> mpris_server::zbus::Result<()> {
    match update {
        Update::Metadata(metadata) => player.set_metadata(metadata).await,
        Update::Playback(status, at) => {
            player.set_position(at);
            player.set_playback_status(status).await
        }
        Update::Seeked(at) => player.seeked(at).await,
        Update::Volume(level) => player.set_volume(level).await,
        Update::Shuffle(on) => player.set_shuffle(on).await,
        Update::Loop(status) => player.set_loop_status(status).await,
    }
}

fn metadata(track: &Track, cover: Option<&str>) -> Metadata {
    let artists = match track.artist_refs.is_empty() {
        true => vec![track.artists.clone()],
        false => track
            .artist_refs
            .iter()
            .map(|artist| artist.name.clone())
            .collect(),
    };
    let mut metadata = Metadata::builder()
        .trackid(track_id(track))
        .title(track.name.clone())
        .artist(artists)
        .album(track.album.clone())
        .length(time(track.duration))
        .build();
    metadata.set_art_url(cover);
    metadata.set_url(location(track));
    metadata.set_genre((!track.tags.is_empty()).then(|| track.tags.clone()));
    metadata.set_track_number(
        (track.track_number > 0).then(|| track.track_number.min(i32::MAX as u32) as i32),
    );
    metadata.set_disc_number(
        (track.disc_number > 0).then(|| track.disc_number.min(i32::MAX as u32) as i32),
    );
    metadata.set_use_count(
        track
            .playcount
            .map(|count| i32::try_from(count).unwrap_or(i32::MAX)),
    );
    metadata
}

/// The file a widget can open for the track. Only a local file has one; a streamed track
/// carries no public url, so it is left out rather than publishing a link that 404s.
fn location(track: &Track) -> Option<String> {
    let path = track.id.as_deref()?.strip_prefix(LOCAL_TRACK_PREFIX)?;
    Some(format!("file://{path}"))
}

/// An object path standing for the track, which MPRIS needs to match a seek to the track it
/// was aimed at. Provider ids carry characters a path cannot, so the path holds their hash.
fn track_id(track: &Track) -> TrackId {
    let mut hasher = DefaultHasher::new();
    track.id.as_ref().unwrap_or(&track.name).hash(&mut hasher);
    TrackId::try_from(format!("/app/sonora/track/t{:016x}", hasher.finish()))
        .unwrap_or(TrackId::NO_TRACK)
}

fn time(at: Duration) -> Time {
    Time::from_micros(at.as_micros().try_into().unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mpris_server::zbus;
    use mpris_server::zbus::zvariant::OwnedValue;

    use super::*;

    fn track() -> Track {
        Track {
            id: Some("track".to_owned()),
            name: "track".to_owned(),
            playable: true,
            artists: "artist".to_owned(),
            artist_refs: Vec::new(),
            album: "album".to_owned(),
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
    fn metadata_carries_the_cover() {
        let published = metadata(&track(), Some("file:///tmp/cover.jpg"));
        assert_eq!(
            published.art_url().as_deref(),
            Some("file:///tmp/cover.jpg")
        );
        let bare = metadata(&track(), None);
        assert_eq!(bare.art_url(), None);
    }

    #[test]
    fn metadata_carries_the_track_details() {
        let mut detailed = track();
        detailed.tags = vec!["shoegaze".to_owned(), "dream pop".to_owned()];
        detailed.track_number = 4;
        detailed.disc_number = 2;
        detailed.playcount = Some(37);
        let published = metadata(&detailed, None);
        assert_eq!(
            published.genre(),
            Some(vec!["shoegaze".to_owned(), "dream pop".to_owned()])
        );
        assert_eq!(published.track_number(), Some(4));
        assert_eq!(published.disc_number(), Some(2));
        assert_eq!(published.use_count(), Some(37));

        // an unset number or count publishes nothing rather than a zero a widget would show
        let bare = metadata(&track(), None);
        assert_eq!(bare.genre(), None);
        assert_eq!(bare.track_number(), None);
        assert_eq!(bare.disc_number(), None);
        assert_eq!(bare.use_count(), None);
    }

    #[test]
    fn metadata_links_a_local_track_to_its_file() {
        let mut local = track();
        local.id = Some(format!("{LOCAL_TRACK_PREFIX}/music/a.flac"));
        assert_eq!(
            metadata(&local, None).url().as_deref(),
            Some("file:///music/a.flac")
        );
        // a streamed track has no file a widget could open
        assert_eq!(metadata(&track(), None).url(), None);
    }

    /// Builds the real player on the session bus, publishes a track with a cover and reads the
    /// Metadata property back over D-Bus. Skipped where no session bus answers.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn publishes_the_cover_over_dbus() {
        let Ok(connection) = zbus::Connection::session().await else {
            return;
        };
        let (commands, _inbox) = mpsc::unbounded_channel();
        let player = build(commands).await.expect("cannot build the player");
        let check = async {
            apply(
                &player,
                Update::Metadata(metadata(&track(), Some("file:///tmp/cover.jpg"))),
            )
            .await
            .expect("cannot publish the metadata");
            let properties = zbus::fdo::PropertiesProxy::builder(&connection)
                .destination("org.mpris.MediaPlayer2.sonora")
                .expect("bad destination")
                .path("/org/mpris/MediaPlayer2")
                .expect("bad path")
                .build()
                .await
                .expect("cannot reach the player");
            let value = properties
                .get(
                    "org.mpris.MediaPlayer2.Player"
                        .try_into()
                        .expect("bad interface"),
                    "Metadata",
                )
                .await
                .expect("cannot read the metadata");
            let fields: HashMap<String, OwnedValue> =
                value.try_into().expect("metadata is not a dictionary");
            assert_eq!(
                fields["mpris:artUrl"]
                    .downcast_ref::<zbus::zvariant::Str>()
                    .map(|url| url.to_string()),
                Ok("file:///tmp/cover.jpg".to_owned())
            );
            assert_eq!(
                fields["xesam:title"]
                    .downcast_ref::<zbus::zvariant::Str>()
                    .map(|title| title.to_string()),
                Ok("track".to_owned())
            );
        };
        future::select(std::pin::pin!(player.run()), std::pin::pin!(check)).await;
    }
}
