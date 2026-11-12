use super::{LyricsSource, Track, http::Http};
use crate::lyrics::Lyrics;
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use scraper::{ElementRef, Html, Node, Selector};
use serde::Deserialize;

pub struct GeniusSource {
    key: String,
    http: Http,
}
#[derive(Deserialize)]
struct Search {
    response: SearchData,
}
#[derive(Deserialize)]
struct SearchData {
    hits: Vec<Hit>,
}
#[derive(Deserialize)]
struct Hit {
    #[serde(rename = "type")]
    kind: String,
    result: Song,
}
#[derive(Deserialize)]
struct Song {
    title: String,
    primary_artist: Artist,
    url: String,
}
#[derive(Deserialize)]
struct Artist {
    name: String,
}
impl GeniusSource {
    pub fn new(http: Http) -> Result<Self> {
        let key =
            std::env::var("GENIUS_API_KEY").context("GENIUS_API_KEY is required for Genius")?;
        anyhow::ensure!(!key.trim().is_empty(), "GENIUS_API_KEY is empty");
        Ok(Self { key, http })
    }
}
#[async_trait]
impl LyricsSource for GeniusSource {
    fn name(&self) -> &'static str {
        "genius"
    }
    async fn fetch(&self, track: &Track) -> Result<Option<Lyrics>> {
        let query = format!("{} {}", track.artist, track.title);
        let Some(search) = self
            .http
            .json::<Search>(
                self.http
                    .client
                    .get("https://api.genius.com/search")
                    .query(&[("q", query)])
                    .bearer_auth(&self.key),
            )
            .await?
        else {
            return Ok(None);
        };
        let selected = search
            .response
            .hits
            .iter()
            .filter(|hit| hit.kind == "song")
            .find(|hit| track.matches(&hit.result.title, &hit.result.primary_artist.name, None))
            .or_else(|| {
                track
                    .allow_inaccurate
                    .then(|| search.response.hits.iter().find(|hit| hit.kind == "song"))
                    .flatten()
            });
        let Some(hit) = selected else { return Ok(None) };
        let url = reqwest::Url::parse(&hit.result.url)?;
        anyhow::ensure!(
            url.scheme() == "https"
                && url
                    .host_str()
                    .is_some_and(|h| h == "genius.com" || h.ends_with(".genius.com")),
            "invalid Genius lyrics URL"
        );
        let Some(bytes) = self
            .http
            .bytes(self.http.client.get(url), 2 * 1024 * 1024)
            .await?
        else {
            return Ok(None);
        };
        let page_html = String::from_utf8(bytes)?;
        let title = &track.title;
        let text = extract(&page_html, title)?;
        let mut lyrics = Lyrics {
            plain: text,
            ..Lyrics::default()
        };
        lyrics.validate()?;
        Ok((!lyrics.empty()).then_some(lyrics))
    }
}
fn extract(page_html: &str, title: &str) -> Result<String> {
    let document = Html::parse_document(page_html);
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

    if let Some(idx) = lyrics.find("Lyrics")
        && idx < 150
    {
        let prefix = &lyrics[..idx];
        let suffix = &lyrics[idx + "Lyrics".len()..];
        if prefix.contains("Contributor")
            || suffix.starts_with('[')
            || prefix.to_lowercase().contains(&title.to_lowercase())
        {
            lyrics = suffix.trim_start().to_string();
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn html_keeps_line_breaks_and_ignores_controls() {
        let html = r#"<div data-lyrics-container="true">one<br>two <a>three</a><span data-exclude-from-selection="true">junk</span></div>"#;
        assert_eq!(extract(html, "song").unwrap(), "one\ntwo three");
    }
}
