mod sources;

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

use id3::{Tag, TagLike, Version, frame::Lyrics};

#[derive(Parser)]
struct Args {
    #[arg(required = true)]
    music_dir_path: PathBuf,

    #[arg(short, long, default_value_t = 1)]
    threads: usize,

    #[arg(short, long, default_value_t = false)]
    overwrite_existing: bool,

    #[arg(short, long, default_value_t = false)]
    allow_inaccurate: bool,

    #[arg(short, long, default_value = "lrclib", value_delimiter = ',')]
    sources: Vec<String>,

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
        let semaphore_clone = Arc::clone(&semaphore);
        let sources_clone = Arc::clone(&sources);

        let task = tokio::spawn(async move {
            let _permit = semaphore_clone.acquire().await.unwrap();
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
    let mut tag = match Tag::async_read_from_path(file_path).await {
        Ok(tag) => tag,
        Err(e) => {
            eprintln!(
                "[ERROR] Failed to read ID3 tag for file '{}': {}",
                file_path.display(),
                e
            );
            return;
        }
    };

    if !overwrite && tag.lyrics().next().is_some() {
        println!(
            "[INFO] File '{}' already has lyrics, skipping (use --overwrite to force)",
            file_path.display()
        );
        return;
    }

    for source in sources.iter() {
        match source.fetch_lyrics(&tag, allow_inaccurate).await {
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
                    "[INFO] Successfully fetched lyrics for file '{}' from source '{}'",
                    file_path.display(),
                    source.name()
                );

                tag.remove_all_lyrics();
                tag.remove_all_synchronised_lyrics();

                tag.add_frame(Lyrics {
                    lang: "XXX".to_string(),
                    description: format!("Fetched from {}", source.name()),
                    text: lyrics.to_string(),
                });

                if let Err(e) = tag.write_to_path(file_path, Version::Id3v24) {
                    eprintln!(
                        "[ERROR] Failed to write tags into {}: {}",
                        file_path.display(),
                        e
                    );
                }
                break;
            }
            Err(e) => {
                eprintln!(
                    "[ERROR] Failed to fetch lyrics for file '{}' from source '{}': {}",
                    file_path.display(),
                    source.name(),
                    e
                );
            }
        }
    }
}
