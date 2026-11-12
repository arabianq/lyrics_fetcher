mod audio;
mod cache;
mod engine;
mod lyrics;
mod sources;

use anyhow::{Context, Result, bail, ensure};
use async_walkdir::WalkDir;
use audio::LyricsFormat;
use cache::Cache;
use clap::Parser;
use engine::Engine;
use futures::StreamExt;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::task::JoinSet;

#[derive(Parser)]
#[command(
    version,
    about = "Fetch and embed plain, line-synced and word-synced lyrics"
)]
struct Args {
    /// Root directory of the music library
    music_dir_path: PathBuf,
    /// Number of files processed concurrently
    #[arg(short,long,default_value_t=1,value_parser=clap::value_parser!(u16).range(1..=256))]
    threads: u16,
    /// Replace existing embedded lyrics and sidecars
    #[arg(short, long)]
    overwrite_existing: bool,
    /// Permit approximate track and duration matches
    #[arg(short, long)]
    allow_inaccurate: bool,
    /// Providers in priority order; use "none" for local sidecars only
    #[arg(short,long,default_value=sources::DEFAULT_SOURCES,value_delimiter=',')]
    sources: Vec<String>,
    /// File extensions to process
    #[arg(
        short,
        long,
        default_value = "flac,mp3,ogg,opus,aac,wav,m4a,alac,aiff,ape",
        value_delimiter = ','
    )]
    extensions: Vec<String>,
    /// Embedded format: auto keeps word timing, lrc keeps line timing
    #[arg(long,value_enum,default_value_t=LyricsFormat::Auto)]
    lyrics_format: LyricsFormat,
    /// Also save an enhanced .lrc next to the audio file
    #[arg(long)]
    sidecar: bool,
    /// Use local sidecars and disk cache without network requests
    #[arg(long)]
    offline: bool,
    /// Disable disk cache
    #[arg(long, conflicts_with = "cache_dir")]
    no_cache: bool,
    /// Override the platform cache directory
    #[arg(long)]
    cache_dir: Option<PathBuf>,
    /// Minimum milliseconds between HTTP requests to each provider
    #[arg(long,default_value_t=500,value_parser=clap::value_parser!(u64).range(0..=10000))]
    request_interval: u64,
    /// Search and report results without modifying audio or sidecars
    #[arg(long)]
    dry_run: bool,
}

#[derive(Default)]
struct Summary {
    written: usize,
    skipped: usize,
    missing: usize,
    instrumental: usize,
    failed: usize,
}
enum Outcome {
    Written,
    Skipped,
    Missing,
    Instrumental,
}
impl Summary {
    fn record(&mut self, result: Result<(PathBuf, Result<Outcome>), tokio::task::JoinError>) {
        match result {
            Ok((_, Ok(Outcome::Written))) => self.written += 1,
            Ok((_, Ok(Outcome::Skipped))) => self.skipped += 1,
            Ok((_, Ok(Outcome::Missing))) => self.missing += 1,
            Ok((_, Ok(Outcome::Instrumental))) => self.instrumental += 1,
            Ok((path, Err(e))) => {
                self.failed += 1;
                eprintln!("[ERROR] '{}': {e:#}", path.display());
            }
            Err(e) => {
                self.failed += 1;
                eprintln!("[ERROR] Worker failed: {e}");
            }
        }
    }
}
fn normalize(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty() && seen.insert(s.clone()))
        .collect()
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let mut args = Args::parse();
    ensure!(
        args.music_dir_path.is_dir(),
        "music directory does not exist or is not a directory"
    );
    args.sources = normalize(args.sources);
    args.extensions = normalize(args.extensions)
        .into_iter()
        .map(|s| s.trim_start_matches('.').to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    ensure!(
        !args.sources.is_empty(),
        "at least one source is required; use --sources none for local sidecars"
    );
    ensure!(
        !args.extensions.is_empty(),
        "at least one extension is required"
    );
    let client = sources::client()?;
    let mut providers = Vec::new();
    if args.sources == ["none"] {
    } else {
        ensure!(
            !args.sources.iter().any(|s| s == "none"),
            "source 'none' cannot be combined with other sources"
        );
        for name in &args.sources {
            if name == "genius" && !args.offline {
                ensure!(
                    std::env::var("GENIUS_API_KEY").is_ok_and(|key| !key.trim().is_empty()),
                    "GENIUS_API_KEY is required for Genius"
                );
            }
            providers.push(sources::create_with_http(
                name,
                sources::http::Http::new(
                    client.clone(),
                    Duration::from_millis(args.request_interval),
                ),
            )?);
        }
    }
    let cache = Cache {
        directory: if args.no_cache {
            None
        } else {
            args.cache_dir.clone().or_else(cache::default_directory)
        },
    };
    let pruning = cache.clone();
    if let Err(e) = tokio::task::spawn_blocking(move || pruning.prune()).await? {
        eprintln!("[WARNING] Cache cleanup failed: {e:#}");
    }
    let engine = Arc::new(Engine::new(providers, cache, args.offline));
    println!(
        "[INFO] Directory: {}; concurrent files: {}; sources: {}",
        args.music_dir_path.display(),
        args.threads,
        args.sources.join(", ")
    );
    let args = Arc::new(args);
    let mut entries = WalkDir::new(&args.music_dir_path);
    let mut jobs = JoinSet::new();
    let mut summary = Summary::default();
    while let Some(entry) = entries.next().await {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                summary.failed += 1;
                eprintln!("[ERROR] Directory traversal: {e}");
                continue;
            }
        };
        match entry.file_type().await {
            Ok(t) if t.is_file() => {}
            Ok(_) => continue,
            Err(e) => {
                summary.failed += 1;
                eprintln!("[ERROR] '{}': {e}", entry.path().display());
                continue;
            }
        };
        let path = entry.path().to_owned();
        let ext = path
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if !args.extensions.contains(&ext) {
            continue;
        }
        while jobs.len() >= usize::from(args.threads) {
            if let Some(result) = jobs.join_next().await {
                summary.record(result)
            }
        }
        let engine = engine.clone();
        let args = args.clone();
        jobs.spawn(async move {
            let result = process(&path, &engine, &args).await;
            (path, result)
        });
    }
    while let Some(result) = jobs.join_next().await {
        summary.record(result)
    }
    println!(
        "[INFO] {}: {}; skipped: {}; not found: {}; instrumental: {}; failed: {}",
        if args.dry_run {
            "Found (dry run)"
        } else {
            "Written"
        },
        summary.written,
        summary.skipped,
        summary.missing,
        summary.instrumental,
        summary.failed
    );
    if summary.failed > 0 {
        bail!("{} file(s) or directory entries failed", summary.failed)
    }
    Ok(())
}

async fn process(path: &Path, engine: &Engine, args: &Args) -> Result<Outcome> {
    let read_path = path.to_owned();
    let inaccurate = args.allow_inaccurate;
    let info =
        tokio::task::spawn_blocking(move || audio::inspect(&read_path, inaccurate)).await??;
    if info.has_lyrics && !args.overwrite_existing {
        println!("[INFO] Existing lyrics, skipping '{}'", path.display());
        return Ok(Outcome::Skipped);
    }
    let Some(found) = engine.lookup(&info.track, path).await? else {
        println!("[INFO] No lyrics: '{}'", path.display());
        return Ok(Outcome::Missing);
    };
    if found.lyrics.instrumental {
        println!("[INFO] Instrumental: '{}'", path.display());
        return Ok(Outcome::Instrumental);
    }
    println!(
        "[INFO] '{}' via {}{} ({})",
        path.display(),
        found.source,
        if found.cached { " cache" } else { "" },
        match found.lyrics.tier() {
            2 => "word sync",
            1 => "line sync",
            _ => "plain",
        }
    );
    if found.source == "unison" {
        println!("[INFO] Lyrics from Unison (https://unison.boidu.dev)");
    }
    if args.dry_run {
        return Ok(Outcome::Written);
    }
    let write_path = path.to_owned();
    let format = args.lyrics_format;
    let sidecar = args.sidecar;
    let overwrite = args.overwrite_existing;
    tokio::task::spawn_blocking(move || -> Result<()> {
        let lrc = write_path.with_extension("lrc");
        let write_sidecar = sidecar && (found.source != "local" || !lrc.exists());
        if write_sidecar && lrc.exists() && !overwrite {
            bail!(
                "sidecar '{}' already exists; use --overwrite-existing to replace it",
                lrc.display()
            )
        }
        audio::save(&write_path, &info.snapshot, &found.lyrics, format)
            .context("failed to save audio lyrics")?;
        if write_sidecar {
            cache::atomic_write(&lrc, found.lyrics.lrc(true).as_bytes(), overwrite)
                .context("audio saved, but sidecar write failed")?;
        }
        Ok(())
    })
    .await??;
    Ok(Outcome::Written)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_cli_and_normalizes_sources_before_deduplication() {
        assert_eq!(
            normalize(vec!["LRCLIB".into(), " lrclib ".into()]),
            ["lrclib"]
        );
        assert!(Args::try_parse_from(["lyrics_fetcher", "/tmp", "--threads", "0"]).is_err());
        assert!(
            Args::try_parse_from([
                "lyrics_fetcher",
                "/tmp",
                "--no-cache",
                "--cache-dir",
                "/tmp"
            ])
            .is_err()
        );
    }
}
