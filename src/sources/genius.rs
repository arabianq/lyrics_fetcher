use crate::sources::LyricsSource;

use anyhow::{Context, Ok, Result, anyhow};
use async_trait::async_trait;
use lofty::{
    properties::FileProperties,
    tag::{Accessor, Tag},
};
use reqwest::Client;
use scraper::{ElementRef, Html, Node, Selector};
use serde::Deserialize;

pub struct GeniusSource {
    pub api_key: String,
    client: Client,
}

#[derive(Deserialize)]
struct GeniusSearchResponse {
    response: GeniusSearchData,
}

#[derive(Deserialize)]
struct GeniusSearchData {
    hits: Vec<GeniusHit>,
}

#[derive(Deserialize)]
struct GeniusHit {
    #[serde(rename = "type")]
    hit_type: String,
    result: GeniusHitResult,
}

#[derive(Deserialize)]
struct GeniusHitResult {
    title: String,
    primary_artist: GeniusArtist,
    url: String,
}

#[derive(Deserialize)]
struct GeniusArtist {
    name: String,
}

impl GeniusSource {
    pub async fn new() -> Result<Self> {
        let api_key = std::env::var("GENIUS_API_KEY")
            .context("GENIUS_API_KEY environment variable is required for Genius source")?;

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
            .context("Failed to build HTTP client for Genius")?;

        Ok(Self { api_key, client })
    }
}

#[async_trait]
impl LyricsSource for GeniusSource {
    fn name(&self) -> &'static str {
        "Genius"
    }

    async fn fetch_lyrics(
        &self,
        tag: &Tag,
        _properties: &FileProperties,
        allow_inaccurate: bool,
    ) -> Result<String> {
        let title = tag
            .title()
            .ok_or_else(|| anyhow!("Missing track title in ID3 tag"))?
            .to_string();
        let artist = tag
            .artist()
            .ok_or_else(|| anyhow!("Missing artist name in ID3 tag"))?
            .to_string();

        let query = format!("{} {}", artist, title);

        let search_res = self
            .client
            .get("https://api.genius.com/search")
            .query(&[("q", &query)])
            .bearer_auth(&self.api_key)
            .send()
            .await?;

        if !search_res.status().is_success() {
            return Err(anyhow!(
                "Genius API search failed with status {}: {}",
                search_res.status(),
                search_res.text().await.unwrap_or_default()
            ));
        }

        let data: GeniusSearchResponse = search_res.json().await?;

        let target_title = title.to_lowercase();
        let target_artist = artist.to_lowercase();

        let best_match = data.response.hits.iter().find(|hit| {
            if hit.hit_type != "song" {
                return false;
            }
            let hit_title = hit.result.title.to_lowercase();
            let hit_artist = hit.result.primary_artist.name.to_lowercase();

            (hit_title.contains(&target_title) || target_title.contains(&hit_title))
                && (hit_artist.contains(&target_artist) || target_artist.contains(&hit_artist))
        });

        let selected = if let Some(hit) = best_match {
            hit
        } else if allow_inaccurate {
            data.response
                .hits
                .iter()
                .find(|hit| hit.hit_type == "song")
                .ok_or_else(|| anyhow!("No results found on Genius"))?
        } else {
            return Ok("".to_string());
        };

        let lyrics_url = &selected.result.url;

        let page_html = self
            .client
            .get(lyrics_url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;

        let document = Html::parse_document(&page_html);
        let selector = Selector::parse(r#"div[data-lyrics-container="true"]"#)
            .map_err(|_| anyhow!("Failed to create CSS selector for lyrics extraction"))?;

        let mut lyrics = String::new();

        fn extract_text(node: ElementRef, out: &mut String) {
            for child in node.children() {
                match child.value() {
                    Node::Text(text) => {
                        out.push_str(text);
                    }
                    Node::Element(element) => {
                        if let Some(child_elem) = ElementRef::wrap(child) {
                            if child_elem.attr("data-exclude-from-selection").is_some() {
                                continue;
                            }
                            if element.name() == "br" {
                                out.push('\n');
                            } else if element.name() == "script" || element.name() == "style" {
                                continue;
                            } else {
                                extract_text(child_elem, out);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        for container in document.select(&selector) {
            extract_text(container, &mut lyrics);
            lyrics.push_str("\n\n");
        }

        let mut lyrics = lyrics.trim().to_string();

        if let Some(idx) = lyrics.find("Lyrics") {
            if idx < 150 {
                let prefix = &lyrics[..idx];
                let suffix = &lyrics[idx + "Lyrics".len()..];
                if prefix.contains("Contributor")
                    || suffix.starts_with('[')
                    || prefix.to_lowercase().contains(&title.to_lowercase())
                {
                    lyrics = suffix.trim_start().to_string();
                }
            }
        }

        if lyrics.ends_with("Embed") {
            let mut idx = lyrics.len() - "Embed".len();
            while idx > 0 && lyrics.as_bytes()[idx - 1].is_ascii_digit() {
                idx -= 1;
            }
            lyrics = lyrics[..idx].trim_end().to_string();
        }

        if lyrics.is_empty() {
            return Err(anyhow!(
                "Could not extract lyrics text from the Genius page"
            ));
        }

        Ok(lyrics)
    }
}
