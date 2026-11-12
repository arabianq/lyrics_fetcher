use super::*;
use flate2::read::ZlibDecoder;
use regex::Regex;
use std::{io::Read, sync::LazyLock};

static LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[(\d+),(\d+)\](.*)").unwrap());
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<(\d+),(\d+),\d+>([^<]*)").unwrap());
const KEY: [u8; 16] = [
    64, 71, 97, 119, 94, 50, 116, 71, 81, 54, 49, 45, 206, 210, 110, 105,
];

pub fn decode(data: &[u8]) -> Result<Lyrics> {
    ensure!(
        data.len() <= MAX_BYTES && data.starts_with(b"krc1"),
        "invalid KRC header"
    );
    let compressed = data[4..]
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ KEY[i % KEY.len()])
        .collect::<Vec<_>>();
    let mut data = Vec::new();
    ZlibDecoder::new(compressed.as_slice())
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut data)?;
    parse(&data)
}

fn parse(data: &[u8]) -> Result<Lyrics> {
    let text = check_input(data)?;
    let mut lyrics = Lyrics::default();
    for raw in text.lines() {
        let Some(line) = LINE.captures(raw) else {
            continue;
        };
        let start = line[1].parse::<u64>()?;
        let duration = line[2].parse::<u64>()?;
        let mut words = Vec::new();
        for word in WORD.captures_iter(&line[3]) {
            let at = start
                .checked_add(word[1].parse()?)
                .ok_or_else(|| anyhow::anyhow!("KRC overflow"))?;
            let end = at
                .checked_add(word[2].parse()?)
                .ok_or_else(|| anyhow::anyhow!("KRC overflow"))?;
            words.push(Word {
                start: at,
                end: Some(end),
                text: word[3].to_owned(),
            });
            ensure!(words.len() <= MAX_WORDS, "too many KRC words");
        }
        lrc::trim_words(&mut words);
        let text = words.iter().map(|w| w.text.as_str()).collect();
        lyrics.lines.push(Line {
            start,
            end: Some(
                start
                    .checked_add(duration)
                    .ok_or_else(|| anyhow::anyhow!("KRC overflow"))?,
            ),
            text,
            words,
            ..Line::default()
        });
        ensure!(lyrics.lines.len() <= MAX_LINES, "too many KRC lines");
    }
    lyrics.validate()?;
    Ok(lyrics)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decrypts_compressed_word_timing() {
        use std::io::Write;
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(b"[1000,2000]<0,500,0>Hello <1000,1000,0>world")
            .unwrap();
        let mut data = b"krc1".to_vec();
        data.extend(
            encoder
                .finish()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(i, b)| b ^ KEY[i % 16]),
        );
        let l = decode(&data).unwrap();
        assert_eq!(l.lines[0].text, "Hello world");
        assert_eq!(l.lines[0].words[1].start, 2000);
        assert!(decode(b"bad").is_err());
    }
}
