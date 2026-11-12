use crate::sources::LyricsSource;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use lofty::{
    properties::FileProperties,
    tag::{Accessor, Tag},
};
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
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(15))
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

    async fn fetch_lyrics(
        &self,
        tag: &Tag,
        properties: &FileProperties,
        allow_inaccurate: bool,
    ) -> Result<String> {
        let base_url = "https://lrclib.net/api";

        let track_name = tag
            .title()
            .ok_or_else(|| anyhow!("Missing track title in ID3 tag"))?
            .to_string();
        let artist_name = tag
            .artist()
            .ok_or_else(|| anyhow!("Missing artist name in ID3 tag"))?
            .to_string();
        let album_name = tag.album().unwrap_or_default().to_string();
        let target_duration = properties.duration().as_secs() as u32;

        let mut get_req = self
            .client
            .get(format!("{base_url}/get"))
            .query(&[("track_name", &track_name), ("artist_name", &artist_name)]);

        if !album_name.is_empty() {
            get_req = get_req.query(&[("album_name", &album_name)]);
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
            .query(&[("track_name", &track_name), ("artist_name", &artist_name)]);

        if !album_name.is_empty() {
            search_req = search_req.query(&[("album_name", &album_name)]);
        }

        let response = search_req.send().await?;

        if response.status() == StatusCode::OK {
            let data: Vec<LrcLibGetResponse> = response.json().await?;
            let track_name_lower = track_name.to_lowercase();
            let artist_name_lower = artist_name.to_lowercase();
            let album_name_lower = album_name.to_lowercase();

            if let Some(best) = select_record(
                &data,
                &track_name_lower,
                &artist_name_lower,
                &album_name_lower,
                target_duration,
                allow_inaccurate,
            ) {
                return Ok(best.extract_lyrics().unwrap());
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

fn select_record<'a>(
    records: &'a [LrcLibGetResponse],
    title: &str,
    artist: &str,
    album: &str,
    duration: u32,
    allow_inaccurate: bool,
) -> Option<&'a LrcLibGetResponse> {
    records
        .iter()
        .filter(|item| item.extract_lyrics().is_some())
        .filter(|item| {
            allow_inaccurate
                || (item.track_name.to_lowercase() == title
                    && item.artist_name.to_lowercase() == artist
                    && (duration == 0
                        || item
                            .duration
                            .is_some_and(|d| (d - f64::from(duration)).abs() <= 8.0)))
        })
        .min_by_key(|item| {
            (
                item.track_name.to_lowercase() != title
                    || item.artist_name.to_lowercase() != artist,
                item.synced_lyrics
                    .as_ref()
                    .is_none_or(|s| s.trim().is_empty()),
                item.duration
                    .map_or(u32::MAX, |d| (d.round() as u32).abs_diff(duration)),
                item.album_name.to_lowercase() != album,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(text: Option<&str>, duration: f64) -> LrcLibGetResponse {
        LrcLibGetResponse {
            _id: 1,
            track_name: "song".into(),
            artist_name: "artist".into(),
            album_name: "album".into(),
            duration: Some(duration),
            _instrumental: false,
            plain_lyrics: text.map(str::to_owned),
            synced_lyrics: None,
        }
    }
    #[test]
    fn empty_candidate_does_not_hide_lyrics() {
        let records = [record(None, 180.0), record(Some("text"), 180.0)];
        assert_eq!(
            select_record(&records, "song", "artist", "album", 180, false)
                .unwrap()
                .extract_lyrics()
                .as_deref(),
            Some("text")
        );
    }
    #[test]
    fn strict_matching_rejects_another_cut() {
        let records = [record(Some("text"), 210.0)];
        assert!(select_record(&records, "song", "artist", "album", 180, false).is_none());
        assert!(select_record(&records, "song", "artist", "album", 180, true).is_some());
    }
    #[test]
    fn synced_candidate_outranks_plain() {
        let mut synced = record(None, 181.0);
        synced.synced_lyrics = Some("[00:01.00]text".into());
        let records = [record(Some("plain"), 180.0), synced];
        assert!(
            select_record(&records, "song", "artist", "album", 180, false)
                .unwrap()
                .synced_lyrics
                .is_some()
        );
    }
}
