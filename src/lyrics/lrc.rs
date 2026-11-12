use super::*;
use regex::Regex;
use std::sync::LazyLock;

static WORD_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([0-9]+:[0-9]+(?::[0-9]+)?(?:[.,][0-9]+)?)>").unwrap());

pub fn parse(data: &[u8]) -> Result<Lyrics> {
    let text = check_input(data)?;
    let mut lyrics = Lyrics::default();
    let mut plain = Vec::new();
    let mut offset = 0i64;
    let mut expanded = 0usize;
    for raw in text.lines() {
        let mut rest = raw.trim_start();
        let mut stamps = Vec::new();
        while let Some(tail) = rest.strip_prefix('[') {
            let Some((tag, next)) = tail.split_once(']') else {
                break;
            };
            if !tag.contains(':') {
                break;
            }
            let Some(at) = timestamp(tag) else { break };
            stamps.push(at);
            ensure!(
                stamps.len() + lyrics.lines.len() <= MAX_LINES,
                "too many lyric lines"
            );
            rest = next.trim_start_matches([' ', '\t']);
        }
        if stamps.is_empty() {
            if let Some(tag) = rest.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
                && let Some((key, value)) = tag.split_once(':')
            {
                match key.trim().to_lowercase().as_str() {
                    "offset" => {
                        offset = value
                            .trim()
                            .parse::<i64>()
                            .unwrap_or(0)
                            .clamp(-(MAX_TIME as i64), MAX_TIME as i64)
                    }
                    "length" => lyrics.duration = timestamp(value),
                    _ => {}
                }
                continue;
            }
            plain.push(raw.to_owned());
            continue;
        }
        let body = WORD_TAG.replace_all(rest, "").trim().to_owned();
        let words = split_words(rest, stamps[0]);
        expanded += body.len() * stamps.len()
            + words.iter().map(|w| w.text.len()).sum::<usize>() * stamps.len();
        ensure!(expanded <= MAX_BYTES, "expanded lyrics exceed size limit");
        for at in stamps.iter().copied() {
            let shift = |time: u64| at.saturating_add(time.saturating_sub(stamps[0]));
            lyrics.lines.push(Line {
                start: at,
                text: body.clone(),
                words: words
                    .iter()
                    .map(|w| Word {
                        start: shift(w.start),
                        end: w.end.map(shift),
                        text: w.text.clone(),
                    })
                    .collect(),
                ..Line::default()
            });
        }
        ensure!(
            lyrics.lines.iter().map(|l| l.words.len()).sum::<usize>() <= MAX_WORDS,
            "too many lyric words"
        );
    }
    for line in &mut lyrics.lines {
        let shift = |time: u64| time.saturating_add_signed(-offset);
        line.start = shift(line.start);
        for word in &mut line.words {
            word.start = shift(word.start);
            word.end = word.end.map(shift);
        }
    }
    if lyrics.lines.is_empty() {
        lyrics.plain = plain.join("\n").trim().to_owned();
    }
    lyrics.validate()?;
    Ok(lyrics)
}

fn split_words(text: &str, start: u64) -> Vec<Word> {
    let mut words = Vec::new();
    let mut previous = 0;
    let mut at = start;
    for tag in WORD_TAG.captures_iter(text) {
        let Some(next) = timestamp(&tag[1]) else {
            return Vec::new();
        };
        if next < at {
            return Vec::new();
        }
        let span = tag.get(0).unwrap();
        if span.start() > previous {
            words.push(Word {
                start: at,
                end: Some(next),
                text: text[previous..span.start()].to_owned(),
            });
        }
        at = next;
        previous = span.end();
    }
    if previous == 0 {
        return Vec::new();
    }
    if previous < text.len() {
        words.push(Word {
            start: at,
            end: None,
            text: text[previous..].to_owned(),
        });
    }
    trim_words(&mut words);
    words
}

pub(super) fn trim_words(words: &mut Vec<Word>) {
    while let Some(first) = words.first_mut() {
        first.text = first.text.trim_start().to_owned();
        if !first.text.is_empty() {
            break;
        }
        words.remove(0);
    }
    while let Some(last) = words.last_mut() {
        last.text = last.text.trim_end().to_owned();
        if !last.text.is_empty() {
            break;
        }
        words.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enhanced_roundtrip_keeps_spacing_gaps_and_word_ends() {
        let lyrics =
            parse(b"[00:01.000]<00:01.000>Hello <00:01.500><00:02.000>world<00:03.000>").unwrap();
        assert_eq!(lyrics.lines[0].text, "Hello world");
        assert_eq!(lyrics.lines[0].words[0].end, Some(1500));
        assert_eq!(parse(lyrics.lrc(true).as_bytes()).unwrap(), lyrics);
    }
    #[test]
    fn multiple_stamps_and_offset_shift_words() {
        let l = parse(b"[offset:500]\n[00:01.000][00:05.000]<00:01.000>hello<00:02.000>").unwrap();
        assert_eq!(
            l.lines.iter().map(|l| l.start).collect::<Vec<_>>(),
            [500, 4500]
        );
        assert_eq!(l.lines[1].words[0].end, Some(5500));
    }
    #[test]
    fn invalid_word_timing_falls_back_to_line_sync() {
        let l = parse(b"[00:02.000]<00:01.000>hello").unwrap();
        assert!(l.lines[0].words.is_empty());
        assert_eq!(l.lines[0].text, "hello");
        assert!(timestamp("NaN").is_none());
    }
}
