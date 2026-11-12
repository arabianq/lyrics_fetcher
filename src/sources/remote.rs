use super::{LyricsSource, Track, http::Http};
use crate::lyrics::{Line, Lyrics, MAX_LINES, MAX_WORDS, Word, parse_lrc, parse_ttml};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde::Deserialize;

pub enum Kind {
    Better,
    Unison,
    Plus,
}
pub struct RemoteSource {
    kind: Kind,
    http: Http,
    key: Option<reqwest::header::HeaderValue>,
}
impl RemoteSource {
    pub fn new(kind: Kind, http: Http) -> Result<Self> {
        let key = std::env::var("BETTERLYRICS_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                let mut v = reqwest::header::HeaderValue::from_str(&s)?;
                v.set_sensitive(true);
                Ok::<_, anyhow::Error>(v)
            })
            .transpose()?;
        Ok(Self { kind, http, key })
    }
}
#[async_trait]
impl LyricsSource for RemoteSource {
    fn name(&self) -> &'static str {
        match self.kind {
            Kind::Better => "betterlyrics",
            Kind::Unison => "unison",
            Kind::Plus => "lyricsplus",
        }
    }
    async fn fetch(&self, track: &Track) -> Result<Option<Lyrics>> {
        let lyrics = match self.kind {
            Kind::Better => {
                #[derive(Deserialize)]
                struct Response {
                    #[serde(default)]
                    ttml: String,
                }
                let mut request = self
                    .http
                    .client
                    .get("https://lyrics-api.boidu.dev/getLyrics")
                    .query(&track.query("s", "a", "al", "d"));
                if let Some(key) = &self.key {
                    request = request.header("X-API-Key", key);
                }
                let Some(response) = self.http.json::<Response>(request).await? else {
                    return Ok(None);
                };
                if response.ttml.is_empty() {
                    return Ok(None);
                }
                parse_ttml(response.ttml.as_bytes())?
            }
            Kind::Unison => {
                #[derive(Deserialize)]
                struct Data {
                    lyrics: String,
                    #[serde(default)]
                    format: String,
                }
                #[derive(Deserialize)]
                struct Response {
                    data: Data,
                }
                let Some(response) = self
                    .http
                    .json::<Response>(
                        self.http
                            .client
                            .get("https://unison.betterlyrics.org/lyrics")
                            .query(&track.query("song", "artist", "album", "duration")),
                    )
                    .await?
                else {
                    return Ok(None);
                };
                if response.data.format == "ttml" {
                    parse_ttml(response.data.lyrics.as_bytes())?
                } else {
                    parse_lrc(response.data.lyrics.as_bytes())?
                }
            }
            Kind::Plus => {
                let Some(response) = self
                    .http
                    .json::<PlusResponse>(
                        self.http
                            .client
                            .get("https://lyricsplus.binimum.org/v2/lyrics/get")
                            .query(&track.query("title", "artist", "album", "duration")),
                    )
                    .await?
                else {
                    return Ok(None);
                };
                decode_plus(response)?
            }
        };
        Ok((!lyrics.empty()).then_some(lyrics))
    }
}
#[derive(Deserialize)]
struct PlusResponse {
    #[serde(rename = "type")]
    kind: String,
    lyrics: Vec<PlusLine>,
    #[serde(default)]
    metadata: PlusMetadata,
}
#[derive(Default, Deserialize)]
struct PlusMetadata {
    #[serde(rename = "totalDuration")]
    total_duration: Option<String>,
}
#[derive(Deserialize)]
struct PlusLine {
    time: Option<i64>,
    #[serde(default)]
    duration: i64,
    text: String,
    #[serde(default, rename = "syllabus")]
    words: Vec<PlusWord>,
    #[serde(default)]
    element: PlusElement,
}
#[derive(Default, Deserialize)]
struct PlusElement {
    #[serde(default)]
    singer: String,
}
#[derive(Deserialize)]
struct PlusWord {
    time: Option<i64>,
    #[serde(default)]
    duration: i64,
    text: String,
    #[serde(default, rename = "isBackground")]
    background: bool,
}
fn decode_plus(response: PlusResponse) -> Result<Lyrics> {
    ensure!(
        response.lyrics.len() <= MAX_LINES
            && response.lyrics.iter().map(|l| l.words.len()).sum::<usize>() <= MAX_WORDS,
        "LyricsPlus response too large"
    );
    let mut lyrics = Lyrics {
        duration: response
            .metadata
            .total_duration
            .as_deref()
            .and_then(crate::lyrics::timestamp),
        ..Lyrics::default()
    };
    lyrics.plain = response
        .lyrics
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if response.kind != "Word" && response.kind != "Line" {
        lyrics.validate()?;
        return Ok(lyrics);
    }
    for source in response.lyrics {
        let Some(start) = source.time.and_then(|t| u64::try_from(t).ok()) else {
            continue;
        };
        let end = (source.duration > 0).then(|| start.saturating_add(source.duration as u64));
        let valid = response.kind == "Word"
            && !source.words.is_empty()
            && source
                .words
                .iter()
                .all(|w| w.time.is_some_and(|t| t >= start as i64))
            && source
                .words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<String>()
                == source.text;
        if !valid {
            lyrics.lines.push(Line {
                start,
                end,
                text: source.text,
                voice: source.element.singer,
                ..Line::default()
            });
            continue;
        }
        for background in [false, true] {
            let words = source
                .words
                .iter()
                .filter(|w| w.background == background)
                .map(|w| Word {
                    start: w.time.unwrap() as u64,
                    end: (w.duration > 0)
                        .then(|| (w.time.unwrap() as u64).saturating_add(w.duration as u64)),
                    text: w.text.clone(),
                })
                .collect::<Vec<_>>();
            if words.is_empty() {
                continue;
            }
            lyrics.lines.push(Line {
                start: words[0].start,
                end: words.iter().filter_map(|w| w.end).max(),
                text: words.iter().map(|w| w.text.as_str()).collect(),
                words,
                voice: source.element.singer.clone(),
                background,
            });
        }
    }
    ensure!(
        lyrics.lines.is_empty()
            || lyrics
                .lines
                .iter()
                .any(|l| l.start > 0 || l.end.is_some_and(|e| e > 0)),
        "LyricsPlus returned zero-only timestamps"
    );
    lyrics.validate()?;
    Ok(lyrics)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decodes_syllables_and_background() {
        let data = r#"{"type":"Word","lyrics":[{"time":1000,"duration":2000,"text":"hello hey","syllabus":[{"time":1000,"duration":1000,"text":"hello "},{"time":2000,"duration":1000,"text":"hey","isBackground":true}]}]}"#;
        let l = decode_plus(serde_json::from_str(data).unwrap()).unwrap();
        assert_eq!(l.lines.len(), 2);
        assert_eq!(l.tier(), 2);
        assert!(l.lines[1].background);
    }
    #[test]
    fn rejects_fake_sync() {
        let data = r#"{"type":"Line","lyrics":[{"time":0,"text":"hello"}]}"#;
        assert!(decode_plus(serde_json::from_str(data).unwrap()).is_err());
    }
}
