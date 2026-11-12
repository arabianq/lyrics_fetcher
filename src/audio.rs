use crate::{lyrics::Lyrics, sources::Track};
use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use id3::{TagLike, Version};
use lofty::{
    config::WriteOptions,
    file::{AudioFile, FileType, TaggedFileExt},
    tag::{ItemKey, TagExt, TagType},
};
use std::{path::Path, time::SystemTime};

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum LyricsFormat {
    #[default]
    Auto,
    Plain,
    Lrc,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    length: u64,
    modified: SystemTime,
}
pub struct AudioInfo {
    pub track: Track,
    pub has_lyrics: bool,
    pub snapshot: Snapshot,
}
fn snapshot(path: &Path) -> Result<Snapshot> {
    let m = std::fs::metadata(path)?;
    Ok(Snapshot {
        length: m.len(),
        modified: m.modified()?,
    })
}
fn unchanged(path: &Path, original: &Snapshot) -> Result<()> {
    let current = snapshot(path)?;
    ensure!(
        current.length == original.length && current.modified == original.modified,
        "audio file changed during lookup; refusing to overwrite it"
    );
    Ok(())
}

pub fn inspect(path: &Path, allow_inaccurate: bool) -> Result<AudioInfo> {
    let snapshot = snapshot(path)?;
    let file = lofty::probe::Probe::open(path)?
        .guess_file_type()?
        .read()
        .with_context(|| format!("cannot read audio metadata: {}", path.display()))?;
    let track = file
        .primary_tag()
        .into_iter()
        .chain(file.tags())
        .find_map(|tag| Track::from_tags(tag, file.properties(), allow_inaccurate).ok())
        .context("missing track title or artist")?;
    let mut has_lyrics = file.tags().iter().any(|tag| {
        tag.items().any(|item| {
            matches!(item.key(), ItemKey::Lyrics | ItemKey::UnsyncLyrics)
                && item.value().text().is_some_and(|s| !s.trim().is_empty())
        })
    });
    if file.file_type() == FileType::Mpeg
        && let Ok(tag) = id3::Tag::read_from_path(path)
    {
        has_lyrics |= tag.lyrics().any(|l| !l.text.trim().is_empty())
            || tag.synchronised_lyrics().any(|l| !l.content.is_empty());
    }
    Ok(AudioInfo {
        track,
        has_lyrics,
        snapshot,
    })
}

pub fn save(path: &Path, original: &Snapshot, lyrics: &Lyrics, format: LyricsFormat) -> Result<()> {
    unchanged(path, original)?;
    ensure!(
        !std::fs::symlink_metadata(path)?.file_type().is_symlink(),
        "refusing to replace a symlink"
    );
    let mut tagged = lofty::probe::Probe::open(path)?.guess_file_type()?.read()?;
    let text = match format {
        LyricsFormat::Plain => lyrics.plain.clone(),
        LyricsFormat::Lrc => lyrics.lrc(false),
        LyricsFormat::Auto => lyrics.lrc(true),
    };
    ensure!(!text.trim().is_empty(), "refusing to write empty lyrics");
    let mut copy = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))?;
    std::fs::copy(path, copy.path())?;
    if tagged.file_type() == FileType::Mpeg {
        let mut tag = match id3::Tag::read_from_path(path) {
            Ok(t) => t,
            Err(e) if matches!(e.kind, id3::ErrorKind::NoTag) => id3::Tag::new(),
            Err(e) => return Err(e).context("refusing to replace unreadable MP3 tags"),
        };
        replace_mp3_lyrics(&mut tag, lyrics, &text, format);
        tag.write_to_path(copy.path(), Version::Id3v24)?;
    } else {
        let tag_type = match tagged.file_type() {
            FileType::Wav | FileType::Aiff => TagType::Id3v2,
            _ => tagged.primary_tag_type(),
        };
        if tagged.tag(tag_type).is_none() {
            tagged.insert_tag(lofty::tag::Tag::new(tag_type));
        }
        let tag = tagged
            .tag_mut(tag_type)
            .context("unsupported lyrics tag type")?;
        tag.remove_key(ItemKey::Lyrics);
        tag.remove_key(ItemKey::UnsyncLyrics);
        let key = if tag_type == TagType::Id3v2 {
            ItemKey::UnsyncLyrics
        } else {
            ItemKey::Lyrics
        };
        ensure!(
            tag.insert_text(key, text.clone()),
            "audio tag type cannot store lyrics"
        );
        tag.save_to(copy.as_file_mut(), WriteOptions::default())?;
    }
    copy.as_file().sync_all()?;
    let written = if tagged.file_type() == FileType::Mpeg {
        id3::Tag::read_from_path(copy.path())?
            .lyrics()
            .any(|l| l.text == text)
    } else {
        let written =
            lofty::probe::Probe::new(std::io::BufReader::new(std::fs::File::open(copy.path())?))
                .set_file_type(tagged.file_type())
                .read()?;
        written.tags().iter().any(|t| {
            [ItemKey::Lyrics, ItemKey::UnsyncLyrics]
                .iter()
                .any(|key| t.get_string(*key) == Some(text.as_str()))
        })
    };
    ensure!(
        written,
        "audio format did not retain lyrics; original file has been preserved"
    );
    unchanged(path, original)?;
    copy.persist(path)?;
    Ok(())
}
fn replace_mp3_lyrics(tag: &mut id3::Tag, lyrics: &Lyrics, text: &str, format: LyricsFormat) {
    tag.remove("USLT");
    tag.remove("SYLT");
    tag.add_frame(id3::frame::Lyrics {
        lang: "und".into(),
        description: String::new(),
        text: text.to_owned(),
    });
    if matches!(format, LyricsFormat::Plain) || lyrics.lines.is_empty() {
        return;
    }
    let mut content = Vec::new();
    for line in &lyrics.lines {
        if matches!(format, LyricsFormat::Auto) && !line.words.is_empty() {
            for (i, word) in line.words.iter().enumerate() {
                content.push((
                    word.start as u32,
                    format!(
                        "{}{}",
                        if i == 0 && !content.is_empty() {
                            "\n"
                        } else {
                            ""
                        },
                        word.text
                    ),
                ));
            }
        } else {
            content.push((
                line.start as u32,
                format!(
                    "{}{}",
                    if content.is_empty() { "" } else { "\n" },
                    line.text
                ),
            ));
        }
    }
    content.sort_by_key(|(time, _)| *time);
    tag.add_frame(id3::frame::SynchronisedLyrics {
        lang: "und".into(),
        timestamp_format: id3::frame::TimestampFormat::Ms,
        content_type: id3::frame::SynchronisedLyricsType::Lyrics,
        description: String::new(),
        content,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overwrite_removes_all_languages_and_preserves_metadata() {
        let mut tag = id3::Tag::new();
        tag.set_title("title");
        for lang in ["eng", "rus"] {
            tag.add_frame(id3::frame::Lyrics {
                lang: lang.into(),
                description: String::new(),
                text: "old".into(),
            });
        }
        let lyrics =
            crate::lyrics::parse_lrc(b"[00:01.000]<00:01.000>hello <00:02.000>world<00:03.000>")
                .unwrap();
        replace_mp3_lyrics(&mut tag, &lyrics, "new", LyricsFormat::Auto);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tag.mp3");
        std::fs::write(&file, []).unwrap();
        tag.write_to_path(&file, Version::Id3v24).unwrap();
        let tag = id3::Tag::read_from_path(&file).unwrap();
        assert_eq!(tag.lyrics().count(), 1);
        assert_eq!(tag.lyrics().next().unwrap().text, "new");
        assert_eq!(tag.title(), Some("title"));
        let synced = tag.synchronised_lyrics().next().unwrap();
        assert_eq!(synced.content[1].0, 2000);
        assert_eq!(synced.content[1].1, "world");
    }
    #[test]
    fn wav_writes_id3_lyrics_and_preserves_riff_metadata_and_pcm() {
        use lofty::tag::Accessor;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audio.wav");
        let pcm = vec![128u8; 8000];
        let mut wav = Vec::new();
        wav.extend(b"RIFF");
        wav.extend(8036u32.to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(8000u32.to_le_bytes());
        wav.extend(8000u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(8u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(8000u32.to_le_bytes());
        wav.extend(&pcm);
        std::fs::write(&path, wav).unwrap();
        let mut riff = lofty::tag::Tag::new(TagType::RiffInfo);
        riff.set_title("Title".into());
        riff.set_artist("Artist".into());
        riff.insert_text(ItemKey::Comment, "Keep me".into());
        riff.save_to_path(&path, WriteOptions::default()).unwrap();
        let info = inspect(&path, false).unwrap();
        let lyrics = crate::lyrics::parse_lrc(b"[00:00.500]hello").unwrap();
        save(&path, &info.snapshot, &lyrics, LyricsFormat::Auto).unwrap();
        let read = inspect(&path, false).unwrap();
        assert!(read.has_lyrics);
        assert_eq!(read.track.title, "Title");
        let tagged = lofty::read_from_path(&path).unwrap();
        assert_eq!(
            tagged
                .tag(TagType::RiffInfo)
                .unwrap()
                .get_string(ItemKey::Comment),
            Some("Keep me")
        );
        assert_eq!(
            tagged
                .tag(TagType::Id3v2)
                .unwrap()
                .get_string(ItemKey::UnsyncLyrics),
            Some("[00:00.500]hello")
        );
        let bytes = std::fs::read(path).unwrap();
        let start = bytes.windows(4).position(|b| b == b"data").unwrap() + 8;
        assert_eq!(&bytes[start..start + 8000], pcm);
    }
    #[test]
    fn detects_changes_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audio");
        std::fs::write(&path, b"before").unwrap();
        let original = snapshot(&path).unwrap();
        std::fs::write(&path, b"changed audio").unwrap();
        assert!(
            save(
                &path,
                &original,
                &Lyrics {
                    plain: "text".into(),
                    ..Lyrics::default()
                },
                LyricsFormat::Auto
            )
            .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), b"changed audio");
    }
}
