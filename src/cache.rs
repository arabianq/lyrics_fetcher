use crate::{lyrics::Lyrics, sources::Track};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const DAY: u64 = 86400;
#[derive(Clone)]
pub struct Cache {
    pub directory: Option<PathBuf>,
}
#[derive(Serialize, Deserialize)]
struct Entry {
    version: u8,
    saved: u64,
    lyrics: Option<Lyrics>,
}
pub struct Cached {
    pub lyrics: Option<Lyrics>,
    pub fresh: bool,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
impl Cache {
    fn path(&self, provider: &str, track: &Track) -> Option<PathBuf> {
        let digest = Sha256::digest(serde_json::to_vec(&(provider, track)).ok()?);
        Some(self.directory.as_ref()?.join(format!("{digest:x}.json")))
    }
    pub fn load(&self, provider: &str, track: &Track) -> Option<Cached> {
        let mut data = Vec::new();
        std::fs::File::open(self.path(provider, track)?)
            .ok()?
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut data)
            .ok()?;
        if data.len() > 4 * 1024 * 1024 {
            return None;
        }
        let mut entry: Entry = serde_json::from_slice(&data).ok()?;
        if entry.version != 1 {
            return None;
        }
        let age = now().saturating_sub(entry.saved);
        if age
            > if entry.lyrics.is_some() {
                90 * DAY
            } else {
                DAY
            }
        {
            return None;
        }
        if let Some(lyrics) = &mut entry.lyrics {
            lyrics.validate().ok()?;
            if lyrics.empty() {
                return None;
            }
        }
        let fresh = age < 14 * DAY
            || entry
                .lyrics
                .as_ref()
                .is_none_or(|l| l.tier() == 2 || l.instrumental);
        Some(Cached {
            lyrics: entry.lyrics,
            fresh,
        })
    }
    pub fn store(&self, provider: &str, track: &Track, lyrics: Option<&Lyrics>) -> Result<()> {
        let Some(path) = self.path(provider, track) else {
            return Ok(());
        };
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir)?;
        let entry = Entry {
            version: 1,
            saved: now(),
            lyrics: lyrics.cloned(),
        };
        let mut file = tempfile::NamedTempFile::new_in(dir)?;
        serde_json::to_writer(&mut file, &entry)?;
        file.flush()?;
        file.persist(path)?;
        Ok(())
    }
    pub fn prune(&self) -> Result<()> {
        let Some(dir) = &self.directory else {
            return Ok(());
        };
        if !dir.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let meta = entry.metadata()?;
            if meta.is_file() && meta.modified()?.elapsed().unwrap_or_default().as_secs() > 90 * DAY
            {
                std::fs::remove_file(path)?;
            }
        }
        Ok(())
    }
}

pub fn default_directory() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|p| p.join("lyrics_fetcher"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Some(dir) = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
        {
            return Some(dir.join("lyrics_fetcher"));
        }
        std::env::var_os("HOME").map(PathBuf::from).map(|p| {
            p.join(if cfg!(target_os = "macos") {
                "Library/Caches/lyrics_fetcher"
            } else {
                ".cache/lyrics_fetcher"
            })
        })
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8], overwrite: bool) -> Result<()> {
    ensure!(
        overwrite || !path.exists(),
        "sidecar already exists: {}",
        path.display()
    );
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    if overwrite {
        file.persist(path)?;
    } else {
        file.persist_noclobber(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn track() -> Track {
        Track {
            title: "song".into(),
            artist: "artist".into(),
            album: String::new(),
            duration: 180000,
            allow_inaccurate: false,
        }
    }
    #[test]
    fn separates_track_versions_and_ignores_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache {
            directory: Some(dir.path().to_owned()),
        };
        let track = track();
        cache
            .store(
                "lrclib",
                &track,
                Some(&Lyrics {
                    plain: "hello".into(),
                    ..Lyrics::default()
                }),
            )
            .unwrap();
        assert_eq!(
            cache.load("lrclib", &track).unwrap().lyrics.unwrap().plain,
            "hello"
        );
        let mut other = track.clone();
        other.duration += 30000;
        assert!(cache.load("lrclib", &other).is_none());
        std::fs::write(cache.path("lrclib", &track).unwrap(), b"broken").unwrap();
        assert!(cache.load("lrclib", &track).is_none());
    }
    #[test]
    fn expired_misses_are_retried_and_sidecars_are_not_clobbered() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache {
            directory: Some(dir.path().to_owned()),
        };
        let track = track();
        let path = cache.path("lrclib", &track).unwrap();
        std::fs::write(
            &path,
            serde_json::to_vec(&Entry {
                version: 1,
                saved: now() - DAY - 1,
                lyrics: None,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(cache.load("lrclib", &track).is_none());
        atomic_write(&path, b"original", true).unwrap();
        assert!(atomic_write(&path, b"new", false).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"original");
    }
}
