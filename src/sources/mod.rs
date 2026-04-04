pub mod genius;
pub mod lrclib;

pub use genius::GeniusSource;
pub use lrclib::LrcLibSource;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use id3::Tag;
use std::sync::Arc;

#[async_trait]
pub trait LyricsSource: Send + Sync {
    fn name(&self) -> &'static str;
    async fn fetch_lyrics(&self, tag: &Tag, allow_inaccurate: bool) -> Result<String>;
}

/// Фабрика для создания источников по их строковому названию
pub async fn create_source(name: &str) -> Result<Arc<dyn LyricsSource>> {
    match name {
        "lrclib" => Ok(Arc::new(LrcLibSource::new().await?)),
        "genius" => Ok(Arc::new(GeniusSource::new().await?)),
        _ => Err(anyhow!("Unknown source type: {}", name)),
    }
}
