mod krc;
mod lrc;
mod lyricsfile;
mod ttml;

pub use krc::decode as decode_krc;
pub use lrc::parse as parse_lrc;
pub use lyricsfile::parse as parse_lyricsfile;
pub use ttml::parse as parse_ttml;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const MAX_BYTES: usize = 512 * 1024;
pub const MAX_LINES: usize = 2000;
pub const MAX_WORDS: usize = 16000;
pub const MAX_TIME: u64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Word {
    pub start: u64,
    pub end: Option<u64>,
    pub text: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Line {
    pub start: u64,
    pub end: Option<u64>,
    pub text: String,
    #[serde(default)]
    pub words: Vec<Word>,
    #[serde(default)]
    pub voice: String,
    #[serde(default)]
    pub background: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Lyrics {
    pub plain: String,
    pub lines: Vec<Line>,
    pub instrumental: bool,
    pub duration: Option<u64>,
}

impl Lyrics {
    pub fn empty(&self) -> bool {
        !self.instrumental
            && self.plain.trim().is_empty()
            && self.lines.iter().all(|l| l.text.trim().is_empty())
    }

    pub fn tier(&self) -> u8 {
        if self.lines.iter().any(|l| !l.words.is_empty()) {
            2
        } else if !self.lines.is_empty() {
            1
        } else {
            0
        }
    }

    pub fn validate(&mut self) -> Result<()> {
        ensure!(
            self.lines.len() <= MAX_LINES && self.plain.lines().count() <= MAX_LINES,
            "too many lyric lines"
        );
        ensure!(
            self.lines.iter().map(|l| l.words.len()).sum::<usize>() <= MAX_WORDS,
            "too many lyric words"
        );
        let size = self.plain.len()
            + self
                .lines
                .iter()
                .map(|l| l.text.len() + l.words.iter().map(|w| w.text.len()).sum::<usize>())
                .sum::<usize>();
        ensure!(size <= MAX_BYTES, "lyrics exceed size limit");
        ensure!(
            self.duration.is_none_or(|d| d <= MAX_TIME),
            "invalid lyrics duration"
        );
        for line in &mut self.lines {
            ensure!(
                line.start <= MAX_TIME && line.end.is_none_or(|e| e >= line.start && e <= MAX_TIME),
                "invalid line timing"
            );
            let mut previous = line.start;
            let valid = line.words.iter().all(|w| {
                let ok = w.start >= previous
                    && w.start <= MAX_TIME
                    && w.end.is_none_or(|e| e >= w.start && e <= MAX_TIME);
                previous = w.start;
                ok
            });
            if !valid
                || line
                    .words
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<String>()
                    != line.text
            {
                line.words.clear();
            }
        }
        self.lines.sort_by_key(|l| l.start);
        if self.plain.trim().is_empty() {
            self.plain = self
                .lines
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
        }
        Ok(())
    }

    pub fn lrc(&self, enhanced: bool) -> String {
        if self.lines.is_empty() {
            return self.plain.clone();
        }
        let mut out = String::new();
        for line in &self.lines {
            out.push_str(&format!("[{}]", stamp(line.start)));
            if enhanced && !line.words.is_empty() {
                for word in &line.words {
                    out.push_str(&format!("<{}>{}", stamp(word.start), word.text));
                    if let Some(end) = word.end {
                        out.push_str(&format!("<{}>", stamp(end)));
                    }
                }
            } else {
                out.push_str(&line.text);
            }
            out.push('\n');
        }
        out.trim_end_matches('\n').to_owned()
    }
}

pub fn stamp(ms: u64) -> String {
    format!("{:02}:{:02}.{:03}", ms / 60000, ms / 1000 % 60, ms % 1000)
}

pub fn timestamp(value: &str) -> Option<u64> {
    let value = value.trim().replace(',', ".");
    let (value, scale) = if let Some(s) = value.strip_suffix("ms") {
        (s, 0.001)
    } else if let Some(s) = value.strip_suffix('s') {
        (s, 1.0)
    } else if let Some(s) = value.strip_suffix('m') {
        (s, 60.0)
    } else if let Some(s) = value.strip_suffix('h') {
        (s, 3600.0)
    } else {
        (value.as_str(), 1.0)
    };
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() > 3 {
        return None;
    }
    let mut seconds = 0.0;
    for part in parts {
        let n = part.parse::<f64>().ok()?;
        if !n.is_finite() || n < 0.0 {
            return None;
        }
        seconds = seconds * 60.0 + n;
    }
    let ms = seconds * scale * 1000.0;
    (ms.is_finite() && ms <= MAX_TIME as f64).then(|| ms.round() as u64)
}

pub fn check_input(data: &[u8]) -> Result<&str> {
    ensure!(data.len() <= MAX_BYTES, "lyrics exceed size limit");
    Ok(std::str::from_utf8(data)?.trim_start_matches('\u{feff}'))
}
