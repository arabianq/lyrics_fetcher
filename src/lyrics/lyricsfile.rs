use super::*;

#[derive(Deserialize)]
struct Document {
    version: String,
    #[serde(default)]
    metadata: Metadata,
    #[serde(default)]
    lines: Vec<Entry>,
    #[serde(default)]
    plain: String,
}
#[derive(Default, Deserialize)]
struct Metadata {
    #[serde(default)]
    instrumental: bool,
    duration_ms: Option<u64>,
}
#[derive(Deserialize)]
struct Span {
    #[serde(default)]
    text: String,
    start_ms: u64,
    end_ms: Option<u64>,
}
#[derive(Deserialize)]
struct Entry {
    #[serde(flatten)]
    span: Span,
    #[serde(default)]
    words: Vec<Span>,
}

pub fn parse(data: &[u8]) -> Result<Lyrics> {
    let text = check_input(data)?;
    let document: Document = serde_yaml_ng::from_str(text)?;
    ensure!(document.version == "1.0", "unsupported Lyricsfile version");
    ensure!(
        document.lines.len() <= MAX_LINES,
        "too many Lyricsfile lines"
    );
    let mut lyrics = Lyrics {
        plain: document.plain,
        instrumental: document.metadata.instrumental,
        duration: document.metadata.duration_ms,
        ..Lyrics::default()
    };
    for entry in document.lines {
        let words = entry
            .words
            .into_iter()
            .map(|w| Word {
                start: w.start_ms,
                end: w.end_ms,
                text: w.text,
            })
            .collect::<Vec<_>>();
        let text = if entry.span.text.is_empty() {
            words.iter().map(|w| w.text.as_str()).collect()
        } else {
            entry.span.text
        };
        lyrics.lines.push(Line {
            start: entry.span.start_ms,
            end: entry.span.end_ms,
            text,
            words,
            ..Line::default()
        });
    }
    lyrics.validate()?;
    Ok(lyrics)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_words_and_rejects_negative_times() {
        let data = "version: '1.0'\nlines:\n- text: Hello\n  start_ms: 1000\n  words:\n  - text: Hello\n    start_ms: 1000\n    end_ms: 2000\n";
        assert_eq!(
            parse(data.as_bytes()).unwrap().lines[0].words[0].end,
            Some(2000)
        );
        assert!(parse(data.replace("start_ms: 1000", "start_ms: -1").as_bytes()).is_err());
    }
}
