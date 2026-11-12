mod genius;
pub mod http;
mod kugou;
mod lrclib;
mod remote;
mod youtube;

use crate::lyrics::Lyrics;
use anyhow::{Result, anyhow, ensure};
use async_trait::async_trait;
use lofty::{
    properties::FileProperties,
    tag::{Accessor, Tag},
};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};

pub const DEFAULT_SOURCES: &str = "betterlyrics,unison,lyricsplus,kugou,lrclib,youtubemusic";

#[derive(Clone, Debug, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: u64,
    pub allow_inaccurate: bool,
}

pub fn normalized(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

impl Track {
    pub fn from_tags(
        tag: &Tag,
        properties: &FileProperties,
        allow_inaccurate: bool,
    ) -> Result<Self> {
        let title = tag.title().unwrap_or_default().trim().to_owned();
        let artist = tag.artist().unwrap_or_default().trim().to_owned();
        ensure!(
            !title.is_empty() && !artist.is_empty(),
            "missing track title or artist"
        );
        Ok(Self {
            title,
            artist,
            album: tag.album().unwrap_or_default().trim().to_owned(),
            duration: properties.duration().as_millis().try_into().unwrap_or(0),
            allow_inaccurate,
        })
    }
    pub fn matches(&self, title: &str, artist: &str, duration: Option<u64>) -> bool {
        let artists = normalized(&self.artist);
        let artist = normalized(artist.trim_end_matches(" - Topic"));
        let artist_matches = !artist.is_empty()
            && (artists == artist
                || format!(" {artists} ").contains(&format!(" {artist} "))
                || format!(" {artist} ").contains(&format!(" {artists} ")));
        normalized(&self.title) == normalized(title)
            && artist_matches
            && self.matches_duration(duration)
    }
    pub fn matches_duration(&self, duration: Option<u64>) -> bool {
        self.allow_inaccurate
            || self.duration == 0
            || duration.is_none_or(|d| self.duration.abs_diff(d) <= 8000)
    }
    pub fn query(
        &self,
        title: &str,
        artist: &str,
        album: &str,
        duration: &str,
    ) -> Vec<(String, String)> {
        let mut q = vec![
            (title.into(), self.title.clone()),
            (artist.into(), self.artist.clone()),
        ];
        if !self.album.is_empty() {
            q.push((album.into(), self.album.clone()));
        }
        if self.duration > 0 {
            q.push((duration.into(), ((self.duration + 500) / 1000).to_string()));
        }
        q
    }
}

#[async_trait]
pub trait LyricsSource: Send + Sync {
    fn name(&self) -> &'static str;
    async fn fetch(&self, track: &Track) -> Result<Option<Lyrics>>;
    async fn fetch_lyrics(
        &self,
        tag: &Tag,
        properties: &FileProperties,
        allow_inaccurate: bool,
    ) -> Result<String> {
        let track = Track::from_tags(tag, properties, allow_inaccurate)?;
        Ok(self
            .fetch(&track)
            .await?
            .map(|l| l.lrc(true))
            .unwrap_or_default())
    }
}

pub fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION"),
            " (+",
            env!("CARGO_PKG_REPOSITORY"),
            ")"
        ))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?)
}

pub async fn create_source(name: &str) -> Result<Arc<dyn LyricsSource>> {
    create_with_http(name, http::Http::new(client()?, Duration::from_millis(500)))
}

pub fn create_with_http(name: &str, http: http::Http) -> Result<Arc<dyn LyricsSource>> {
    Ok(match name {
        "lrclib" => Arc::new(lrclib::LrcLibSource { http }),
        "genius" => Arc::new(genius::GeniusSource::new(http)?),
        "betterlyrics" => Arc::new(remote::RemoteSource::new(remote::Kind::Better, http)?),
        "unison" => Arc::new(remote::RemoteSource::new(remote::Kind::Unison, http)?),
        "lyricsplus" => Arc::new(remote::RemoteSource::new(remote::Kind::Plus, http)?),
        "kugou" => Arc::new(kugou::KugouSource { http }),
        "youtubemusic" => Arc::new(youtube::YoutubeSource { http }),
        _ => return Err(anyhow!("unknown lyrics source: {name}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matching_keeps_versions_and_unicode() {
        let t = Track {
            title: "Песня (Live)".into(),
            artist: "Исполнитель".into(),
            album: String::new(),
            duration: 180000,
            allow_inaccurate: false,
        };
        assert!(t.matches("ПЕСНЯ (Live)", "Исполнитель - Topic", Some(181000)));
        assert!(!t.matches("Песня", "Исполнитель", Some(180000)));
        assert!(!t.matches("Песня (Live)", "Исполнитель", Some(220000)));
    }
}
