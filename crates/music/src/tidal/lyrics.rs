//! Tidal lyrics: the account's own synced and plain sheets, read over the same api the
//! players use, with the track's provider id rather than a name guess.

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Mutex;

use crate::lyrics::lrc;
use crate::tidal::{self, TidalClient, auth, client};
use crate::{Lyrics, LyricsHit, LyricsProvider, LyricsQuery};

/// Lyrics that arrive signed to the account rank like a file's own tags.
const TRUST: u32 = 100;

type Identity = String;

/// Asks Tidal for the lyrics of one of its tracks. Any other track, or no stored sign-in,
/// gets no answer.
pub struct TidalLyrics {
    client: Mutex<Option<(Identity, TidalClient)>>,
}

impl TidalLyrics {
    pub fn new() -> Self {
        Self {
            client: Mutex::new(None),
        }
    }

    /// A client for the stored sign-in, kept until the refresh token changes. This provider
    /// has no session handle, so it reads the same store `offline_playback` does.
    fn client(&self) -> Result<Option<TidalClient>> {
        let Some((stored, generation)) = auth::restore() else {
            return Ok(None);
        };
        let identity = stored.refresh_token.clone();
        let mut held = self
            .client
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((known, client)) = held.as_ref()
            && *known == identity
        {
            return Ok(Some(client.clone()));
        }
        let client = TidalClient::new(client::http()?, stored, Some(generation));
        *held = Some((identity, client.clone()));
        Ok(Some(client))
    }
}

impl Default for TidalLyrics {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LyricsProvider for TidalLyrics {
    fn name(&self) -> &'static str {
        tidal::NAME
    }

    async fn search(&self, query: &LyricsQuery) -> Result<Vec<LyricsHit>> {
        let Some(id) = query.id_for(tidal::SLUG) else {
            return Ok(Vec::new());
        };
        let Some(client) = self.client()? else {
            return Ok(Vec::new());
        };
        let lyrics = client.lyrics(id).await?;
        Ok(lyrics
            .into_iter()
            .map(|lyrics| LyricsHit {
                source: tidal::NAME,
                trust: TRUST,
                lyrics,
                instrumental: false,
                title: query.title.clone(),
                artist: query.artist.clone(),
                album: query.album.clone(),
                duration: (!query.duration.is_zero()).then_some(query.duration),
                writers: Vec::new(),
            })
            .collect())
    }
}

/// One sheet of a lyrics document: synced lines when `lrc` parses, else the plain text.
pub(crate) fn sheet(text: &str, lrc_text: &str) -> Option<Lyrics> {
    let lines = lrc::parse(lrc_text.trim_start_matches('\u{feff}'));
    if !lines.is_empty() {
        return Some(Lyrics::Synced {
            lines: lines.into(),
        });
    }
    (!text.trim().is_empty()).then(|| Lyrics::plain(text))
}

/// Parses the OpenAPI lyrics resources of an `included` array: `text` for the plain sheet,
/// `lrcText` for the synced one. An entry with neither is skipped.
pub(crate) fn sheets(included: &[serde_json::Value]) -> Vec<Lyrics> {
    included
        .iter()
        .filter(|entry| crate::tidal::wire::text(entry, "type") == Some("lyrics"))
        .filter_map(|entry| {
            let attributes = entry.get("attributes")?;
            sheet(
                crate::tidal::wire::text(attributes, "text")?,
                crate::tidal::wire::text(attributes, "lrcText").unwrap_or(""),
            )
        })
        .collect()
}

/// Parses the v1 lyrics answer: `subtitles` is LRC when it is not empty, `lyrics` the plain
/// text, `isRightToLeft` marks the direction the lyrics model does not carry, so a right to
/// left sheet is still given as plain text in its written order.
pub(crate) fn legacy(answer: &serde_json::Value) -> Option<Lyrics> {
    let text = crate::tidal::wire::text(answer, "lyrics").unwrap_or("");
    let lrc = crate::tidal::wire::text(answer, "subtitles").unwrap_or("");
    sheet(text, lrc)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn openapi_lyrics_prefer_the_synced_sheet() {
        let included = vec![json!({
            "type": "lyrics",
            "id": "1",
            "attributes": {
                "text": "first line\nsecond line",
                "lrcText": "[00:01.00]first line\n[00:02.00]second line",
                "direction": "ltr",
                "provider": "Musixmatch",
                "technicalStatus": "OK"
            }
        })];
        let sheets = sheets(&included);
        assert_eq!(sheets.len(), 1);
        assert!(sheets[0].synced());
    }

    #[test]
    fn openapi_lyrics_without_synced_text_stay_plain() {
        let included = vec![json!({
            "type": "lyrics",
            "attributes": { "text": "plain words\nanother line" }
        })];
        let sheets = sheets(&included);
        assert_eq!(sheets.len(), 1);
        assert!(!sheets[0].synced());
    }

    #[test]
    fn openapi_lyrics_skip_other_resources_and_empty_sheets() {
        let included = vec![
            json!({ "type": "tracks", "attributes": { "text": "not lyrics" } }),
            json!({ "type": "lyrics", "attributes": { "text": "  ", "lrcText": "" } }),
        ];
        assert!(sheets(&included).is_empty());
    }

    #[test]
    fn legacy_answer_reads_subtitles_as_synced() {
        let answer = json!({
            "trackId": 1,
            "lyricsProvider": "Musixmatch",
            "lyrics": "first line\nsecond line",
            "subtitles": "[00:01.00]first line\n[00:02.00]second line",
            "isRightToLeft": false
        });
        let sheet = legacy(&answer).unwrap();
        assert!(sheet.synced());
    }

    #[test]
    fn legacy_answer_falls_back_to_plain_text() {
        let answer = json!({ "lyrics": "plain words", "subtitles": "" });
        let sheet = legacy(&answer).unwrap();
        assert!(!sheet.synced());
    }

    #[test]
    fn legacy_answer_with_nothing_is_empty() {
        assert!(legacy(&json!({ "lyrics": "", "subtitles": "" })).is_none());
        assert!(legacy(&json!({})).is_none());
    }
}
