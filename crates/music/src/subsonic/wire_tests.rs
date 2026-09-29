//! Contract and fault tests for the Subsonic client against a stub server. The mocks feed
//! it every shape a real server can produce, including the bad ones: error statuses, failed
//! responses, cut and malformed bodies, and fields of the wrong type.
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::auth;
use super::client::SubsonicClient;
use crate::{MusicApi, Report};

fn client(server_uri: &str) -> SubsonicClient {
    let signature = auth::sign("ivan", "secret");
    SubsonicClient::new(
        server_uri.to_owned(),
        "ivan".to_owned(),
        "secret".to_owned(),
        &signature,
        false,
    )
    .expect("a client against the stub")
}

/// The `subsonic-response` envelope every JSON endpoint returns, with the endpoint's own
/// payload merged in.
fn ok(data: Value) -> ResponseTemplate {
    let mut envelope = json!({
        "status": "ok",
        "version": "1.16.1",
        "type": "navidrome",
        "serverVersion": "0.52.5",
        "openSubsonic": true,
    });
    for (key, value) in data.as_object().expect("an object") {
        envelope[key] = value.clone();
    }
    ResponseTemplate::new(200).set_body_json(json!({ "subsonic-response": envelope }))
}

fn song(id: &str) -> Value {
    json!({
        "id": id,
        "title": format!("Track {id}"),
        "album": "Album",
        "albumId": "al1",
        "artist": "Artist",
        "artistId": "ar1",
        "duration": 183,
        "track": 2,
        "discNumber": 1,
        "coverArt": "cv1",
        "genres": [{"name": "rock"}, {"name": "indie"}],
        "playCount": 7,
    })
}

#[tokio::test]
async fn playlists_map_the_opensubsonic_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/getPlaylists"))
        .respond_with(ok(json!({
            "playlists": {
                "playlist": [{
                    "id": "pl1",
                    "name": "Long drives",
                    "owner": "ivan",
                    "public": true,
                    "songCount": 42,
                    "coverArt": "cv1",
                    "created": "2024-01-01T10:00:00Z",
                    "changed": "2024-06-01T12:00:00Z",
                }],
            }
        })))
        .mount(&server)
        .await;

    let playlists = client(&server.uri())
        .playlists()
        .await
        .expect("playlists load");

    assert_eq!(playlists.len(), 1);
    let playlist = &playlists[0];
    assert_eq!(playlist.id, "pl1");
    assert_eq!(playlist.name, "Long drives");
    assert_eq!(playlist.track_count, 42);
    assert!(
        playlist.modified_at.is_some(),
        "changed lands on modified_at"
    );
    assert!(playlist.cover.is_some(), "a cover id becomes a cover url");
}

#[tokio::test]
async fn album_tracks_map_the_full_child() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/getAlbum"))
        .respond_with(ok(json!({
            "album": {
                "id": "al1",
                "name": "Album",
                "songCount": 1,
                "song": [song("s1")],
            }
        })))
        .mount(&server)
        .await;

    let tracks = client(&server.uri())
        .album_tracks("al1")
        .await
        .expect("tracks load");

    assert_eq!(tracks.len(), 1);
    let track = &tracks[0];
    assert_eq!(track.id.as_deref(), Some("s1"));
    assert_eq!(track.name, "Track s1");
    assert_eq!(track.artists, "Artist");
    assert_eq!(track.album, "Album");
    assert_eq!(track.album_id.as_deref(), Some("al1"));
    assert_eq!(track.tags, ["rock", "indie"]);
    assert_eq!(track.track_number, 2);
    assert_eq!(track.playcount, Some(7));
    assert!(track.cover.is_some());
}

#[tokio::test]
async fn played_posts_a_real_scrobble() {
    let server = MockServer::start().await;
    let mock = Mock::given(method("GET"))
        .and(path("/rest/scrobble"))
        .and(query_param("submission", "true"))
        .respond_with(ok(json!({})))
        .expect(1)
        .mount_as_scoped(&server)
        .await;

    client(&server.uri())
        .played(
            "s1",
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        )
        .await
        .expect("the scrobble lands");

    drop(mock);
}

#[tokio::test]
async fn report_falls_back_to_now_playing_without_the_extension() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/getOpenSubsonicExtensions"))
        .respond_with(ok(json!({ "openSubsonicExtensions": [] })))
        .mount(&server)
        .await;
    let scrobbled = Mock::given(method("GET"))
        .and(path("/rest/scrobble"))
        .and(query_param("submission", "false"))
        .respond_with(ok(json!({})))
        .expect(1)
        .mount_as_scoped(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/reportPlayback"))
        .respond_with(ok(json!({})))
        .expect(0)
        .mount(&server)
        .await;

    client(&server.uri())
        .report("s1", Report::Playing, Duration::from_secs(30))
        .await
        .expect("now playing lands");

    drop(scrobbled);
}

#[tokio::test]
async fn report_uses_playback_report_when_the_server_offers_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/getOpenSubsonicExtensions"))
        .respond_with(ok(json!({
            "openSubsonicExtensions": [{ "name": "playbackReport", "versions": [1] }],
        })))
        .mount(&server)
        .await;
    let reported = Mock::given(method("GET"))
        .and(path("/rest/reportPlayback"))
        .and(query_param("state", "playing"))
        .respond_with(ok(json!({})))
        .expect(1)
        .mount_as_scoped(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/scrobble"))
        .respond_with(ok(json!({})))
        .expect(0)
        .mount(&server)
        .await;

    client(&server.uri())
        .report("s1", Report::Playing, Duration::from_secs(30))
        .await
        .expect("playbackReport lands");

    drop(reported);
}

#[tokio::test]
async fn a_server_error_fails_the_call() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/getPlaylists"))
        .respond_with(ResponseTemplate::new(500).set_body_string("broken"))
        .mount(&server)
        .await;

    assert!(client(&server.uri()).playlists().await.is_err());
}

#[tokio::test]
async fn a_failed_response_fails_the_call() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/getPlaylists"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "subsonic-response": {
                "status": "failed",
                "version": "1.16.1",
                "error": { "code": 40, "message": "wrong credentials" },
            }
        })))
        .mount(&server)
        .await;

    let error = client(&server.uri())
        .playlists()
        .await
        .expect_err("a failed status must error");
    assert!(format!("{error:#}").contains("wrong credentials"));
}

#[tokio::test]
async fn malformed_json_fails_the_call() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/getPlaylists"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json at all"))
        .mount(&server)
        .await;

    assert!(client(&server.uri()).playlists().await.is_err());
}

#[tokio::test]
async fn a_cut_body_fails_the_call() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/getPlaylists"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"subsonic-response": {"status": "ok", "playli"#),
        )
        .mount(&server)
        .await;

    assert!(client(&server.uri()).playlists().await.is_err());
}

#[tokio::test]
async fn an_html_login_page_fails_the_call() {
    // A reverse proxy that has lost its auth sends HTML, not JSON.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/getPlaylists"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("<html><body>please sign in</body></html>"),
        )
        .mount(&server)
        .await;

    assert!(client(&server.uri()).playlists().await.is_err());
}

#[tokio::test]
async fn wrongly_typed_fields_fail_the_call() {
    // A duration that arrives as a string is not something the wire model can stand in for.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/getAlbum"))
        .respond_with(ok(json!({
            "album": {
                "id": "al1",
                "name": "Album",
                "songCount": 1,
                "song": [{
                    "id": "s1",
                    "title": "Track s1",
                    "duration": "183",
                }],
            }
        })))
        .mount(&server)
        .await;

    assert!(client(&server.uri()).album_tracks("al1").await.is_err());
}

#[tokio::test]
async fn missing_optional_fields_still_map() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/getAlbum"))
        .respond_with(ok(json!({
            "album": {
                "id": "al1",
                "name": "Album",
                "songCount": 1,
                "song": [{ "id": "s1", "title": "Track s1" }],
            }
        })))
        .mount(&server)
        .await;

    let tracks = client(&server.uri())
        .album_tracks("al1")
        .await
        .expect("sparse songs still map");

    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].name, "Track s1");
}
