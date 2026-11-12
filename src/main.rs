mod sources;

use lofty::{
    file::{AudioFile, TaggedFileExt},
    tag::TagExt,
};
use sources::*;

use anyhow::Result;
use async_walkdir::WalkDir;
use itertools::Itertools;

use futures::StreamExt;
use std::{
    path::{Path, PathBuf},
    process::exit,
    sync::Arc,
};
use tokio::sync::Semaphore;

use clap::Parser;

use id3::{TagLike, Version};
use lofty::{config::WriteOptions, tag::ItemKey};

#[derive(Parser)]
struct Args {
    #[arg(required = true)]
    music_dir_path: PathBuf,

    /// Number of concurrent threads to use
    #[arg(short, long, default_value_t = 1)]
    threads: usize,

    /// Overwrite existing lyrics in audio files
    #[arg(short, long, default_value_t = false)]
    overwrite_existing: bool,

    /// Allow inaccurate matches when searching
    #[arg(short, long, default_value_t = false)]
    allow_inaccurate: bool,

    /// Comma-separated list of lyrics sources to query
    #[arg(short, long, default_value = "lrclib", value_delimiter = ',')]
    sources: Vec<String>,

    /// Comma-separated list of file extensions to process
    #[arg(
        short,
        long,
        default_value = "flac,mp3,ogg,opus,aac,wav,m4a,alac,aiff,ape",
        value_delimiter = ','
    )]
    extensions: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let args = Args::parse();

    let music_dir_path = args.music_dir_path;
    let threads = args.threads;
    let overwrite = args.overwrite_existing;
    let allow_inaccurate = args.allow_inaccurate;

    if !music_dir_path.exists() {
        eprintln!("[ERROR] The specified music directory does not exist.");
        exit(1);
    }

    if threads < 1 {
        eprintln!("[ERROR] The number of threads must be at least 1.");
        exit(1);
    }

    if args.sources.is_empty() {
        eprintln!("[ERROR] At least one source must be specified.");
        exit(1);
    }

    if args.extensions.is_empty() {
        eprintln!("[ERROR] At least one file extension must be specified.");
        exit(1);
    }

    let lyrics_sources_raw: Vec<String> = args
        .sources
        .into_iter()
        .unique()
        .map(|s| s.to_lowercase())
        .collect();

    let extensions: Vec<String> = args
        .extensions
        .into_iter()
        .map(|s| s.to_lowercase())
        .collect();

    println!("[INFO] Music directory: {}", music_dir_path.display());
    println!("[INFO] Number of threads: {}", threads);
    println!("[INFO] Sources: {}", lyrics_sources_raw.join(", "));

    let mut sources: Vec<Arc<dyn LyricsSource>> = vec![];
    for source_name in lyrics_sources_raw {
        let source_name = source_name.trim().to_lowercase();

        match create_source(&source_name).await {
            Ok(source) => sources.push(source),
            Err(e) => {
                eprintln!(
                    "[WARNING] Failed to initialize source '{}', skipping: {}",
                    source_name, e
                );
            }
        }
    }

    if sources.is_empty() {
        eprintln!("[ERROR] No valid sources were initialized. Exiting.");
        exit(1);
    }

    let sources = Arc::new(sources);
    let semaphore = Arc::new(Semaphore::new(threads));
    let mut entries = WalkDir::new(music_dir_path);
    let mut tasks = Vec::new();

    while let Some(entry) = entries.next().await {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("[ERROR] Failed to read directory entry: {}", e);
                continue;
            }
        };

        let is_file = entry
            .file_type()
            .await
            .map(|ft| ft.is_file())
            .unwrap_or(false);
        if !is_file {
            continue;
        }

        let ext = entry
            .path()
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if !extensions.contains(&ext) {
            continue;
        }

        let file_path = entry.path().to_path_buf();
        let sources_clone = Arc::clone(&sources);

        let permit = Arc::clone(&semaphore).acquire_owned().await.unwrap();

        let task = tokio::spawn(async move {
            let _permit = permit;
            process_file(&file_path, sources_clone, overwrite, allow_inaccurate).await;
        });
        tasks.push(task);
    }

    for task in tasks {
        let _ = task.await;
    }

    Ok(())
}

async fn process_file(
    file_path: &Path,
    sources: Arc<Vec<Arc<dyn LyricsSource>>>,
    overwrite: bool,
    allow_inaccurate: bool,
) {
    let mut tagged_file = match lofty::read_from_path(&file_path) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("[ERROR] Failed to read '{}': {}", file_path.display(), e);
            return;
        }
    };

    let search_tag = match tagged_file.primary_tag().or(tagged_file.first_tag()) {
        Some(t) => t.clone(),
        None => {
            eprintln!(
                "[ERROR] No tags in '{}', don't know what to search for. Skipping",
                file_path.display()
            );
            return;
        }
    };

    let properties = tagged_file.properties();

    let has_lyrics = search_tag.items().any(|item| {
        matches!(item.key(), ItemKey::Lyrics) || format!("{:?}", item.key()).contains("Lyric")
    });

    if !overwrite && has_lyrics {
        println!(
            "[INFO] Lyrics already exist in '{}', skipping",
            file_path.display()
        );
        return;
    }

    for source in sources.iter() {
        match source
            .fetch_lyrics(&search_tag, properties, allow_inaccurate)
            .await
        {
            Ok(lyrics) => {
                let lyrics = lyrics.trim();
                if lyrics.is_empty() {
                    println!(
                        "[INFO] Source '{}' did not return any lyrics for file '{}'",
                        source.name(),
                        file_path.display()
                    );
                    continue;
                }

                println!(
                    "[INFO] Found lyrics for '{}' via '{}'",
                    file_path.display(),
                    source.name()
                );

                let ext = file_path
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();

                if ext == "mp3" {
                    let mut mp3_tag = match id3::Tag::read_from_path(file_path) {
                        Ok(tag) => tag,
                        Err(e) if matches!(e.kind, id3::ErrorKind::NoTag) => id3::Tag::new(),
                        Err(e) => {
                            eprintln!(
                                "[ERROR] Refusing to replace unreadable MP3 tags in '{}': {}",
                                file_path.display(),
                                e
                            );
                            return;
                        }
                    };
                    replace_mp3_lyrics(&mut mp3_tag, lyrics);

                    if let Err(e) = mp3_tag.write_to_path(&file_path, Version::Id3v24) {
                        eprintln!(
                            "[ERROR] Failed to save MP3 tag in {}: {}",
                            file_path.display(),
                            e
                        );
                    } else {
                        println!(
                            "[INFO] Successfully wrote lyrics for '{}' via id3 (MP3)",
                            file_path.display()
                        );
                    }
                } else {
                    if tagged_file.primary_tag().is_none() {
                        let tag_type = tagged_file.primary_tag_type();
                        tagged_file.insert_tag(lofty::tag::Tag::new(tag_type));
                    }

                    let tag_to_save = tagged_file.primary_tag_mut().unwrap();

                    tag_to_save.remove_key(ItemKey::Lyrics);
                    tag_to_save.insert_text(ItemKey::Lyrics, lyrics.to_string());

                    if let Err(e) = tag_to_save.save_to_path(&file_path, WriteOptions::default()) {
                        eprintln!(
                            "[ERROR] Failed to save tags in {}: {}",
                            file_path.display(),
                            e
                        );
                    } else {
                        println!(
                            "[INFO] Successfully wrote lyrics for '{}' via lofty ({})",
                            file_path.display(),
                            ext.to_uppercase()
                        );
                    }
                }
                break;
            }
            Err(e) => {
                eprintln!(
                    "[ERROR] Source '{}' error for '{}': {}",
                    source.name(),
                    file_path.display(),
                    e
                );
            }
        }
    }
}

fn replace_mp3_lyrics(tag: &mut id3::Tag, lyrics: &str) {
    tag.remove("USLT");
    tag.remove("SYLT");
    tag.add_frame(id3::frame::Lyrics {
        lang: "und".into(),
        description: String::new(),
        text: lyrics.to_owned(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overwrite_removes_all_languages_and_preserves_other_metadata() {
        let mut tag = id3::Tag::new();
        tag.set_title("title");
        for lang in ["eng", "rus"] {
            tag.add_frame(id3::frame::Lyrics {
                lang: lang.into(),
                description: String::new(),
                text: "old".into(),
            });
        }
        replace_mp3_lyrics(&mut tag, "new");
        assert_eq!(tag.lyrics().count(), 1);
        assert_eq!(tag.lyrics().next().unwrap().text, "new");
        assert_eq!(tag.title(), Some("title"));
    }
}
