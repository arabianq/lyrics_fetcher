use super::{LyricsSource, Track, http::Http};
use crate::lyrics::{Lyrics, parse_lrc, parse_lyricsfile};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;

pub struct LrcLibSource {
    pub http: Http,
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(default)]
    track_name: String,
    #[serde(default)]
    artist_name: String,
    duration: Option<f64>,
    #[serde(default)]
    instrumental: bool,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
    lyricsfile: Option<String>,
}
impl Record {
    fn decode(&self) -> Result<Lyrics> {
        if self.instrumental {
            return Ok(Lyrics {
                instrumental: true,
                ..Lyrics::default()
            });
        }
        let mut l = if let Some(raw) = self.lyricsfile.as_deref().filter(|s| !s.is_empty()) {
            parse_lyricsfile(raw.as_bytes())
                .or_else(|_| parse_lrc(self.synced_lyrics.as_deref().unwrap_or("").as_bytes()))?
        } else {
            parse_lrc(self.synced_lyrics.as_deref().unwrap_or("").as_bytes())?
        };
        if l.plain.is_empty() {
            l.plain = self.plain_lyrics.clone().unwrap_or_default();
        }
        l.duration = self
            .duration
            .filter(|d| d.is_finite() && *d >= 0.0)
            .map(|d| (d * 1000.0).round() as u64);
        l.validate()?;
        Ok(l)
    }
    fn matches(&self, track: &Track) -> bool {
        track.matches(
            &self.track_name,
            &self.artist_name,
            self.duration
                .filter(|d| d.is_finite() && *d >= 0.0)
                .map(|d| (d * 1000.0) as u64),
        )
    }
}
fn best(records: Vec<Record>, track: &Track) -> Option<Lyrics> {
    records
        .into_iter()
        .filter(|r| r.matches(track) || track.allow_inaccurate)
        .filter_map(|r| {
            r.decode()
                .ok()
                .filter(|l| !l.empty())
                .map(|l| (r.matches(track), l))
        })
        .max_by_key(|(matched, l)| {
            (
                *matched,
                l.tier(),
                std::cmp::Reverse(l.duration.map_or(u64::MAX, |d| d.abs_diff(track.duration))),
            )
        })
        .map(|(_, l)| l)
}
#[async_trait]
impl LyricsSource for LrcLibSource {
    fn name(&self) -> &'static str {
        "lrclib"
    }
    async fn fetch(&self, track: &Track) -> Result<Option<Lyrics>> {
        let exact: Option<Record> = self
            .http
            .json(
                self.http
                    .client
                    .get("https://lrclib.net/api/get")
                    .query(&track.query("track_name", "artist_name", "album_name", "duration")),
            )
            .await?;
        let exact = exact
            .filter(|r| r.matches(track))
            .map(|r| r.decode())
            .transpose()?
            .filter(|l| !l.empty());
        if exact
            .as_ref()
            .is_some_and(|l| l.instrumental || l.tier() == 2)
        {
            return Ok(exact);
        }
        let records: Result<Option<Vec<Record>>> = self
            .http
            .json(
                self.http
                    .client
                    .get("https://lrclib.net/api/search")
                    .query(&[("track_name", &track.title)]),
            )
            .await;
        let found = match records {
            Ok(records) => best(records.unwrap_or_default(), track),
            Err(e) => {
                if exact.is_some() {
                    return Ok(exact);
                } else {
                    return Err(e);
                }
            }
        };
        Ok(match (exact, found) {
            (Some(a), Some(b)) => Some(if b.tier() > a.tier() { b } else { a }),
            (a, b) => a.or(b),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn track() -> Track {
        Track {
            title: "song".into(),
            artist: "artist".into(),
            album: "album".into(),
            duration: 180000,
            allow_inaccurate: false,
        }
    }
    fn record(text: Option<&str>, duration: f64) -> Record {
        Record {
            track_name: "song".into(),
            artist_name: "artist".into(),
            duration: Some(duration),
            plain_lyrics: text.map(str::to_owned),
            ..Record::default()
        }
    }
    #[test]
    fn empty_candidate_does_not_hide_lyrics() {
        assert_eq!(
            best(
                vec![record(None, 180.0), record(Some("text"), 180.0)],
                &track()
            )
            .unwrap()
            .plain,
            "text"
        );
    }
    #[test]
    fn strict_matching_rejects_another_cut() {
        assert!(best(vec![record(Some("text"), 220.0)], &track()).is_none());
    }
    #[test]
    fn synced_candidate_outranks_plain() {
        let mut synced = record(None, 181.0);
        synced.synced_lyrics = Some("[00:01.00]text".into());
        assert_eq!(
            best(vec![record(Some("plain"), 180.0), synced], &track())
                .unwrap()
                .tier(),
            1
        );
    }
    #[test]
    fn reads_word_synced_lyricsfile() {
        let record=Record{ lyricsfile:Some("version: '1.0'\nlines:\n- text: hello\n  start_ms: 1000\n  words:\n  - text: hello\n    start_ms: 1000\n    end_ms: 2000\n".into()),..Record::default()};
        assert_eq!(record.decode().unwrap().tier(), 2);
    }
}
