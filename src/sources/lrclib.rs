use crate::sources::LyricsSource;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use id3::{Tag, TagLike};
use reqwest::{Client, StatusCode};
use serde::Deserialize;

pub struct LrcLibSource {
    client: Client,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrcLibGetResponse {
    _id: i64,
    track_name: String,
    artist_name: String,
    album_name: String,
    duration: Option<f64>,
    _instrumental: bool,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}

impl LrcLibGetResponse {
    fn extract_lyrics(&self) -> Option<String> {
        if let Some(synced) = &self.synced_lyrics {
            if !synced.trim().is_empty() {
                return Some(synced.clone());
            }
        }

        if let Some(plain) = &self.plain_lyrics {
            if !plain.trim().is_empty() {
                return Some(plain.clone());
            }
        }

        None
    }
}

impl LrcLibSource {
    pub async fn new() -> Result<Self> {
        let client = Client::builder()
            .user_agent(format!(
                "{name}/{version} (+{repo_url})",
                name = env!("CARGO_PKG_NAME"),
                version = env!("CARGO_PKG_VERSION"),
                repo_url = env!("CARGO_PKG_REPOSITORY")
            ))
            .build()
            .context("Failed to build HTTP client for LrcLib")?;

        Ok(Self { client })
    }
}

#[async_trait]
impl LyricsSource for LrcLibSource {
    fn name(&self) -> &'static str {
        "LrcLib"
    }

    async fn fetch_lyrics(&self, tag: &Tag, allow_inaccurate: bool) -> Result<String> {
        let base_url = "https://lrclib.net/api";

        let track_name = tag
            .title()
            .ok_or_else(|| anyhow!("Missing track title in ID3 tag"))?;
        let artist_name = tag
            .artist()
            .ok_or_else(|| anyhow!("Missing artist name in ID3 tag"))?;
        let album_name = tag.album().unwrap_or_default();
        let target_duration = tag.duration().unwrap_or_default() / 1000;

        let mut get_req = self
            .client
            .get(format!("{base_url}/get"))
            .query(&[("track_name", track_name), ("artist_name", artist_name)]);

        if !album_name.is_empty() {
            get_req = get_req.query(&[("album_name", album_name)]);
        }

        if target_duration > 0 {
            get_req = get_req.query(&[("duration", target_duration.to_string())]);
        }

        let response = get_req.send().await?;

        if response.status() == StatusCode::OK {
            let data: LrcLibGetResponse = response.json().await?;
            if let Some(lyrics) = data.extract_lyrics() {
                return Ok(lyrics);
            }
        }

        let mut search_req = self
            .client
            .get(format!("{base_url}/search"))
            .query(&[("track_name", track_name), ("artist_name", artist_name)]);

        if !album_name.is_empty() {
            search_req = search_req.query(&[("album_name", album_name)]);
        }

        let response = search_req.send().await?;

        if response.status() == StatusCode::OK {
            let data: Vec<LrcLibGetResponse> = response.json().await?;
            let track_name_lower = track_name.to_lowercase();
            let artist_name_lower = artist_name.to_lowercase();
            let album_name_lower = album_name.to_lowercase();

            let best_match = data
                .iter()
                .find(|item| {
                    item.track_name.to_lowercase() == track_name_lower
                        && item.artist_name.to_lowercase() == artist_name_lower
                        && item.album_name.to_lowercase() == album_name_lower
                        && item
                            .duration
                            .map_or(false, |d| (d.round() as u32).abs_diff(target_duration) <= 2)
                })
                .or_else(|| {
                    data.iter().find(|item| {
                        item.track_name.to_lowercase() == track_name_lower
                            && item.artist_name.to_lowercase() == artist_name_lower
                            && item.album_name.to_lowercase() == album_name_lower
                    })
                })
                .or_else(|| {
                    data.iter().find(|item| {
                        item.track_name.to_lowercase() == track_name_lower
                            && item.artist_name.to_lowercase() == artist_name_lower
                    })
                });

            if let Some(best) = best_match {
                if let Some(lyrics) = best.extract_lyrics() {
                    return Ok(lyrics);
                }
            } else if allow_inaccurate {
                if let Some(first) = data.first() {
                    if let Some(lyrics) = first.extract_lyrics() {
                        return Ok(lyrics);
                    }
                }
            }
        } else {
            return Err(anyhow!(
                "LrcLib search API request failed with status {}: {}",
                response.status(),
                response.text().await.unwrap_or_default()
            ));
        }

        Ok("".to_string())
    }
}
