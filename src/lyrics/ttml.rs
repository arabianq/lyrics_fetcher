use super::*;
use roxmltree::{Document, Node, ParsingOptions};

fn attr<'a>(node: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.name() == name)
        .map(|a| a.value())
}
fn inherited<'a>(node: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.ancestors().find_map(|n| attr(n, name))
}
fn timing(node: Node<'_, '_>) -> Result<(u64, Option<u64>)> {
    let start = inherited(node, "begin")
        .map(|v| timestamp(v).ok_or_else(|| anyhow::anyhow!("invalid TTML begin")))
        .transpose()?
        .unwrap_or(0);
    let end = if let Some(value) = attr(node, "end") {
        Some(timestamp(value).ok_or_else(|| anyhow::anyhow!("invalid TTML end"))?)
    } else if let Some(value) = attr(node, "dur") {
        Some(
            start
                .checked_add(
                    timestamp(value).ok_or_else(|| anyhow::anyhow!("invalid TTML duration"))?,
                )
                .ok_or_else(|| anyhow::anyhow!("TTML overflow"))?,
        )
    } else {
        inherited(node, "end").and_then(timestamp)
    };
    ensure!(
        end.is_none_or(|e| e >= start && e <= MAX_TIME),
        "invalid TTML timing range"
    );
    Ok((start, end))
}

pub fn parse(data: &[u8]) -> Result<Lyrics> {
    let text = check_input(data)?;
    ensure!(!text.contains("<!DOCTYPE"), "TTML DTD is unsupported");
    let doc = Document::parse_with_options(
        text,
        ParsingOptions {
            nodes_limit: 64000,
            ..Default::default()
        },
    )?;
    ensure!(
        doc.root_element().tag_name().name() == "tt",
        "invalid TTML root"
    );
    let mut lyrics = Lyrics::default();
    for p in doc.descendants().filter(|n| n.has_tag_name("p")) {
        ensure!(
            p.ancestors().any(|n| n.has_tag_name("body")),
            "TTML paragraph outside body"
        );
        timing(p)?;
        let mut parts: Vec<Line> = Vec::new();
        for node in p
            .descendants()
            .filter(|n| n.is_text() || n.has_tag_name("br"))
        {
            if node
                .ancestors()
                .any(|a| matches!(attr(a, "role"), Some("x-translation" | "x-transliteration")))
            {
                continue;
            }
            let parent = node.parent().unwrap_or(p);
            let background = parent.ancestors().any(|n| attr(n, "role") == Some("x-bg"));
            let voice = inherited(parent, "agent").unwrap_or("").to_owned();
            let index = parts
                .iter()
                .position(|l| l.background == background && l.voice == voice)
                .unwrap_or_else(|| {
                    parts.push(Line {
                        background,
                        voice,
                        ..Line::default()
                    });
                    parts.len() - 1
                });
            let text = if node.has_tag_name("br") {
                "\n".to_owned()
            } else {
                node.text().unwrap_or("").replace(['\n', '\r', '\t'], " ")
            };
            let timed_span = parent
                .ancestors()
                .take_while(|n| *n != p)
                .find(|n| n.has_tag_name("span") && attr(*n, "begin").is_some());
            let (start, end) = timing(timed_span.unwrap_or(p))?;
            let line = &mut parts[index];
            if timed_span.is_none() && text.trim().is_empty() {
                if let Some(last) = line.words.last_mut() {
                    last.text.push_str(&text);
                    line.text.push_str(&text);
                }
                continue;
            }
            if line.text.is_empty() {
                line.start = start;
            }
            line.start = line.start.min(start);
            line.end = line.end.max(end);
            line.text.push_str(&text);
            if let Some(last) = line
                .words
                .last_mut()
                .filter(|w| w.start == start && w.end == end)
            {
                last.text.push_str(&text);
            } else {
                line.words.push(Word { start, end, text });
            }
        }
        for mut line in parts {
            lrc::trim_words(&mut line.words);
            line.text = line.text.trim().to_owned();
            if line.text.is_empty() {
                continue;
            }
            if !p
                .descendants()
                .any(|n| n.has_tag_name("span") && attr(n, "begin").is_some())
            {
                line.words.clear();
            }
            lyrics.lines.push(line);
            ensure!(lyrics.lines.len() <= MAX_LINES, "too many TTML lines");
        }
    }
    lyrics.duration = doc
        .descendants()
        .find(|n| n.has_tag_name("body"))
        .and_then(|n| attr(n, "end"))
        .and_then(timestamp);
    lyrics.validate()?;
    Ok(lyrics)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn words_background_and_translation() {
        let l = parse(br#"<tt xmlns:ttm="http://www.w3.org/ns/ttml#metadata"><body><div><p begin="1s" end="4s" ttm:agent="v1"><span begin="1s" end="2s">Hello </span><span begin="2s" end="3s">world</span><span ttm:role="x-translation">translated</span><span ttm:role="x-bg" begin="3s" end="4s">hey</span></p></div></body></tt>"#).unwrap();
        assert_eq!(l.lines.len(), 2);
        assert_eq!(l.lines[0].text, "Hello world");
        assert_eq!(l.lines[0].words[1].start, 2000);
        assert!(l.lines[1].background);
        assert!(!l.plain.contains("translated"));
    }
    #[test]
    fn whitespace_between_spans_keeps_word_sync() {
        let lyrics = parse(b"<tt><body><p begin='1s' end='3s'>\n  <span begin='1s' end='2s'>Hello</span> <span begin='2s' end='3s'>world</span>\n</p></body></tt>").unwrap();
        assert_eq!(lyrics.lines[0].text, "Hello world");
        assert_eq!(lyrics.lines[0].words.len(), 2);
    }
    #[test]
    fn rejects_invalid_xml_and_times() {
        for text in [
            "<tt><body>",
            "<!DOCTYPE tt><tt/>",
            "<tt><body><p begin='NaNs'>x</p></body></tt>",
        ] {
            assert!(parse(text.as_bytes()).is_err());
        }
    }
}
