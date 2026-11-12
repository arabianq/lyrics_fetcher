use super::{LyricsSource, Track, http::Http, normalized};
use crate::lyrics::{Lyrics, decode_krc, parse_lrc};
use anyhow::Result;
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;

pub struct KugouSource {
    pub http: Http,
}
#[derive(Deserialize)]
struct Songs {
    data: SongData,
}
#[derive(Deserialize)]
struct SongData {
    #[serde(default)]
    info: Vec<Song>,
}
#[derive(Deserialize)]
struct Song {
    songname: String,
    singername: String,
    hash: String,
    duration: u64,
}
#[derive(Deserialize)]
struct Candidates {
    #[serde(default)]
    candidates: Vec<Candidate>,
}
#[derive(Deserialize)]
struct Candidate {
    id: String,
    accesskey: String,
    song: String,
    singer: String,
    #[serde(default)]
    duration: u64,
}
#[derive(Deserialize)]
struct Download {
    content: String,
}
fn matches(track: &Track, title: &str, artist: &str, duration: Option<u64>) -> bool {
    artist
        .split('、')
        .any(|a| track.matches(title, a, duration))
}
impl KugouSource {
    async fn candidate(
        &self,
        track: &Track,
        mut query: Vec<(String, String)>,
    ) -> Result<Option<Candidate>> {
        query.extend([
            ("ver".into(), "1".into()),
            ("man".into(), "yes".into()),
            ("client".into(), "pc".into()),
        ]);
        let response: Option<Candidates> = self
            .http
            .json(
                self.http
                    .client
                    .get("https://lyrics.kugou.com/search")
                    .query(&query),
            )
            .await?;
        Ok(response
            .into_iter()
            .flat_map(|r| r.candidates)
            .filter(|c| {
                matches(
                    track,
                    &c.song,
                    &c.singer,
                    (c.duration > 0).then_some(c.duration),
                )
            })
            .min_by_key(|c| c.duration.abs_diff(track.duration)))
    }
    async fn download(&self, candidate: &Candidate, format: &str) -> Result<Option<Lyrics>> {
        let query = [
            ("id", candidate.id.as_str()),
            ("accesskey", candidate.accesskey.as_str()),
            ("fmt", format),
            ("charset", "utf8"),
            ("client", "pc"),
            ("ver", "1"),
        ];
        let response: Option<Download> = self
            .http
            .json(
                self.http
                    .client
                    .get("https://lyrics.kugou.com/download")
                    .query(&query),
            )
            .await?;
        response
            .map(|r| {
                let bytes = STANDARD.decode(r.content)?;
                if format == "krc" {
                    decode_krc(&bytes)
                } else {
                    parse_lrc(&bytes)
                }
            })
            .transpose()
    }
}
#[async_trait]
impl LyricsSource for KugouSource {
    fn name(&self) -> &'static str {
        "kugou"
    }
    async fn fetch(&self, track: &Track) -> Result<Option<Lyrics>> {
        let keyword = format!("{} {}", track.title, track.artist);
        let songs: Option<Songs> = self
            .http
            .json(
                self.http
                    .client
                    .get("https://mobileservice.kugou.com/api/v3/search/song")
                    .query(&[
                        ("version", "9108"),
                        ("plat", "0"),
                        ("pagesize", "8"),
                        ("showtype", "0"),
                        ("keyword", &keyword),
                    ]),
            )
            .await?;
        let mut songs = songs
            .into_iter()
            .flat_map(|s| s.data.info)
            .filter(|s| {
                matches(
                    track,
                    &s.songname,
                    &s.singername,
                    Some(s.duration.saturating_mul(1000)),
                )
            })
            .collect::<Vec<_>>();
        songs.sort_by_key(|s| s.duration.saturating_mul(1000).abs_diff(track.duration));
        let mut candidate = None;
        for song in songs.into_iter().take(8) {
            candidate = self
                .candidate(track, vec![("hash".into(), song.hash)])
                .await?;
            if candidate.is_some() {
                break;
            }
        }
        if candidate.is_none() {
            let mut query = vec![("keyword".into(), keyword)];
            if track.duration > 0 {
                query.push(("duration".into(), track.duration.to_string()));
            }
            candidate = self.candidate(track, query).await?;
        }
        let Some(candidate) = candidate else {
            return Ok(None);
        };
        let mut lyrics = match self.download(&candidate, "krc").await {
            Ok(Some(l)) if !l.empty() => Some(l),
            _ => self.download(&candidate, "lrc").await?,
        };
        if let Some(l) = &mut lyrics {
            for line in &mut l.lines {
                line.text = unescape(&line.text);
                for word in &mut line.words {
                    word.text = unescape(&word.text);
                }
            }
            if l.lines.len() == 1 && l.lines[0].text.contains("纯音乐") {
                return Ok(Some(Lyrics {
                    instrumental: true,
                    ..Lyrics::default()
                }));
            }
            let mut credits = 0;
            for line in &l.lines {
                let text = line.text.trim();
                let header = normalized(text)
                    == normalized(&format!("{} {}", track.title, track.artist))
                    || text.contains(" - ");
                let credit = text.split_once([':', '：']).is_some_and(|(role, _)| {
                    [
                        "作词",
                        "作曲",
                        "编曲",
                        "词",
                        "曲",
                        "制作人",
                        "混音",
                        "录音",
                        "演唱",
                        "词曲",
                    ]
                    .contains(&role.trim())
                        || ["lyrics", "composer", "arranger", "written by"]
                            .contains(&role.trim().to_lowercase().as_str())
                });
                if credit || (credits == 0 && header) {
                    credits += 1
                } else {
                    break;
                }
            }
            l.lines.drain(..credits);
            l.plain = l
                .lines
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            l.duration = (candidate.duration > 0).then_some(candidate.duration);
            l.validate()?;
        }
        Ok(lyrics.filter(|l| !l.empty()))
    }
}
fn unescape(text: &str) -> String {
    scraper::Html::parse_fragment(text)
        .root_element()
        .text()
        .collect()
}
