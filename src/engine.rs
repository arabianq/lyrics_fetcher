use crate::{
    cache::Cache,
    lyrics::{Lyrics, MAX_BYTES, parse_lrc, parse_lyricsfile, parse_ttml},
    sources::{LyricsSource, Track, http::Uncached, normalized},
};
use anyhow::{Context, Result, bail};
use futures::{StreamExt, stream::FuturesUnordered};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::{
    sync::{Mutex, Semaphore},
    time::{Instant, sleep_until, timeout},
};

#[derive(Clone, Debug)]
pub struct Found {
    pub source: String,
    pub lyrics: Lyrics,
    pub cached: bool,
}
#[derive(Clone)]
enum Outcome {
    Found(Found),
    Missing,
    Error(String),
}
type Flight = Mutex<Option<Outcome>>;
struct Provider {
    source: Arc<dyn LyricsSource>,
    gate: Semaphore,
}
pub struct Engine {
    providers: Vec<Provider>,
    cache: Cache,
    offline: bool,
    flights: Mutex<HashMap<Track, Weak<Flight>>>,
    grace: Duration,
}
impl Engine {
    pub fn new(sources: Vec<Arc<dyn LyricsSource>>, cache: Cache, offline: bool) -> Self {
        Self {
            providers: sources
                .into_iter()
                .map(|source| Provider {
                    source,
                    gate: Semaphore::new(1),
                })
                .collect(),
            cache,
            offline,
            flights: Mutex::new(HashMap::new()),
            grace: Duration::from_secs(2),
        }
    }
    pub async fn lookup(&self, track: &Track, path: &Path) -> Result<Option<Found>> {
        let path = path.to_owned();
        if let Some(lyrics) = tokio::task::spawn_blocking(move || sidecar(&path)).await?? {
            return Ok(Some(Found {
                source: "local".into(),
                lyrics,
                cached: false,
            }));
        }
        let flight = {
            let mut flights = self.flights.lock().await;
            flights.retain(|_, v| v.strong_count() > 0);
            if let Some(flight) = flights.get(track).and_then(Weak::upgrade) {
                flight
            } else {
                let flight = Arc::new(Mutex::new(None));
                flights.insert(track.clone(), Arc::downgrade(&flight));
                flight
            }
        };
        let mut result = flight.lock().await;
        if result.is_none() {
            *result = Some(match self.lookup_providers(track).await {
                Ok(Some(found)) => Outcome::Found(found),
                Ok(None) => Outcome::Missing,
                Err(e) => Outcome::Error(format!("{e:#}")),
            });
        }
        match result.as_ref().unwrap() {
            Outcome::Found(found) => Ok(Some(found.clone())),
            Outcome::Missing => Ok(None),
            Outcome::Error(e) => bail!("{e}"),
        }
    }
    async fn provider(&self, provider: &Provider, track: &Track) -> Result<Option<Found>> {
        // Queue before starting the timeout: a large batch must not consume its request budget waiting.
        let _permit = provider.gate.acquire().await?;
        let name = provider.source.name();
        let cache = self.cache.clone();
        let request = track.clone();
        let cached = tokio::task::spawn_blocking(move || cache.load(name, &request)).await?;
        if let Some(cached) = &cached
            && (cached.fresh || self.offline)
        {
            return Ok(cached.lyrics.clone().map(|lyrics| Found {
                source: name.into(),
                lyrics,
                cached: true,
            }));
        }
        if self.offline {
            return Ok(None);
        }
        let fetched = match timeout(Duration::from_secs(45), provider.source.fetch(track)).await {
            Ok(result) => result,
            Err(error) => Err(error).context("provider lookup timed out"),
        };
        let mut fetched = match fetched {
            Ok(found) => found,
            Err(e) => {
                if let Some(lyrics) = cached.and_then(|c| c.lyrics) {
                    return Ok(Some(Found {
                        source: name.into(),
                        lyrics,
                        cached: true,
                    }));
                }
                if e.downcast_ref::<Uncached>().is_some() {
                    return Ok(None);
                }
                return Err(e);
            }
        };
        if let Some(l) = &mut fetched {
            l.validate()?;
            if l.empty() {
                fetched = None
            }
        }
        if let Some(old) = cached.and_then(|c| c.lyrics)
            && fetched.as_ref().is_none_or(|new| new.tier() < old.tier())
        {
            fetched = Some(old)
        }
        let cache = self.cache.clone();
        let request = track.clone();
        let data = fetched.clone();
        if let Err(e) =
            tokio::task::spawn_blocking(move || cache.store(name, &request, data.as_ref())).await?
        {
            eprintln!("[WARNING] Cache write failed: {e:#}");
        }
        Ok(fetched.map(|lyrics| Found {
            source: name.into(),
            lyrics,
            cached: false,
        }))
    }
    async fn lookup_providers(&self, track: &Track) -> Result<Option<Found>> {
        let mut pending = FuturesUnordered::new();
        for (index, provider) in self.providers.iter().enumerate() {
            pending.push(async move { (index, self.provider(provider, track).await) });
        }
        let mut results = Vec::new();
        let mut errors = Vec::new();
        let mut successful = 0usize;
        let mut deadline = None;
        loop {
            let response = if let Some(at) = deadline {
                tokio::select! {response=pending.next()=>response,_=sleep_until(at)=>break}
            } else {
                pending.next().await
            };
            let Some((index, response)) = response else {
                break;
            };
            match response {
                Ok(Some(found)) => {
                    successful += 1;
                    results.push((index, found));
                    if deadline.is_none() {
                        deadline = Some(Instant::now() + self.grace);
                    }
                }
                Ok(None) => successful += 1,
                Err(e) => {
                    eprintln!("[WARNING] {}: {e:#}", self.providers[index].source.name());
                    errors.push(format!("{}: {e:#}", self.providers[index].source.name()));
                }
            }
        }
        if !results.is_empty() {
            return Ok(pick(track, results));
        }
        if successful == 0 && !errors.is_empty() {
            bail!("all sources failed: {}", errors.join("; "))
        }
        Ok(None)
    }
}

fn plausible(track: &Track, lyrics: &Lyrics) -> bool {
    if track.duration == 0 {
        return true;
    }
    let slack = (track.duration / 100).max(2000);
    lyrics
        .duration
        .is_none_or(|d| d.abs_diff(track.duration) <= slack)
        && lyrics
            .lines
            .last()
            .is_none_or(|l| l.start <= track.duration + 3000)
}
fn agreement(a: &Lyrics, b: &Lyrics) -> Option<u64> {
    if a.lines.len() * b.lines.len() > 250000 {
        return None;
    }
    let aa = a
        .lines
        .iter()
        .filter(|l| !l.background && !l.text.trim().is_empty())
        .collect::<Vec<_>>();
    let bb = b
        .lines
        .iter()
        .filter(|l| !l.background && !l.text.trim().is_empty())
        .collect::<Vec<_>>();
    let mut gaps = Vec::new();
    let mut j = 0;
    for a in aa {
        if let Some(k) = bb[j..]
            .iter()
            .position(|b| normalized(&a.text) == normalized(&b.text))
        {
            j += k;
            gaps.push(a.start.abs_diff(bb[j].start));
            j += 1;
        }
    }
    if gaps.len() < 3 {
        return None;
    }
    gaps.sort_unstable();
    Some(gaps[gaps.len() / 2])
}
fn pick(track: &Track, mut results: Vec<(usize, Found)>) -> Option<Found> {
    let agreement = results
        .iter()
        .map(|(_, a)| {
            results
                .iter()
                .filter(|(_, b)| {
                    a.source != b.source
                        && plausible(track, &a.lyrics)
                        && plausible(track, &b.lyrics)
                })
                .filter_map(|(_, b)| agreement(&a.lyrics, &b.lyrics))
                .min()
                .unwrap_or(u64::MAX)
        })
        .collect::<Vec<_>>();
    let band = (track.duration / 1000).max(250);
    let best = (0..results.len()).max_by_key(|&i| {
        let (priority, found) = &results[i];
        let lyrics = &found.lyrics;
        (
            plausible(track, lyrics),
            !lyrics.lines.is_empty(),
            lyrics.duration.is_some() && track.duration > 0,
            std::cmp::Reverse(agreement[i] / band),
            lyrics.tier(),
            std::cmp::Reverse(*priority),
        )
    })?;
    Some(results.swap_remove(best).1)
}
fn sidecar(path: &Path) -> Result<Option<Lyrics>> {
    use std::io::Read;
    for extension in ["lyricsfile.yaml", "ttml", "lrc", "LRC"] {
        let file = path.with_extension(extension);
        let mut handle = match std::fs::File::open(&file) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", file.display())),
        };
        let mut bytes = Vec::new();
        handle
            .by_ref()
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        let parsed = match extension {
            "lyricsfile.yaml" => parse_lyricsfile(&bytes),
            "ttml" => parse_ttml(&bytes),
            _ => parse_lrc(&bytes),
        };
        match parsed {
            Ok(lyrics) if !lyrics.empty() => return Ok(Some(lyrics)),
            Ok(_) => {}
            Err(e) => eprintln!(
                "[WARNING] Ignoring invalid sidecar '{}': {e:#}",
                file.display()
            ),
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::Line;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Mock {
        calls: AtomicUsize,
        lyrics: Lyrics,
    }
    #[async_trait]
    impl LyricsSource for Mock {
        fn name(&self) -> &'static str {
            "mock"
        }
        async fn fetch(&self, _: &Track) -> Result<Option<Lyrics>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok(Some(self.lyrics.clone()))
        }
    }
    fn track() -> Track {
        Track {
            title: "song".into(),
            artist: "artist".into(),
            album: String::new(),
            duration: 180000,
            allow_inaccurate: false,
        }
    }
    fn found(source: &str, start: u64, duration: u64) -> Found {
        Found {
            source: source.into(),
            cached: false,
            lyrics: Lyrics {
                lines: (0..3)
                    .map(|i| Line {
                        start: start + i * 1000,
                        text: format!("line {i}"),
                        ..Line::default()
                    })
                    .collect(),
                duration: Some(duration),
                ..Lyrics::default()
            },
        }
    }
    #[test]
    fn ranking_rejects_wrong_cut_and_prefers_agreeing_timing() {
        let result = pick(
            &track(),
            vec![
                (0, found("wrong", 1000, 230000)),
                (1, found("offset", 10000, 180000)),
                (2, found("a", 1000, 180000)),
                (3, found("b", 1100, 180000)),
            ],
        )
        .unwrap();
        assert_eq!(result.source, "a");
    }
    #[tokio::test]
    async fn deduplicates_concurrent_tracks_and_supports_offline_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache {
            directory: Some(dir.path().to_owned()),
        };
        let mock = Arc::new(Mock {
            calls: AtomicUsize::new(0),
            lyrics: Lyrics {
                plain: "hello".into(),
                ..Lyrics::default()
            },
        });
        let engine = Engine::new(vec![mock.clone()], cache.clone(), false);
        let track = track();
        let (a, b) = tokio::join!(
            engine.lookup(&track, Path::new("/nonexistent/a.mp3")),
            engine.lookup(&track, Path::new("/nonexistent/b.mp3"))
        );
        assert!(a.unwrap().is_some() && b.unwrap().is_some());
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
        let offline = Engine::new(vec![mock.clone()], cache, true);
        assert!(
            offline
                .lookup(&track, Path::new("/nonexistent/a.mp3"))
                .await
                .unwrap()
                .unwrap()
                .cached
        );
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn local_sidecar_wins_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.flac");
        std::fs::write(path.with_extension("lrc"), "[00:01.000]local").unwrap();
        let engine = Engine::new(Vec::new(), Cache { directory: None }, true);
        assert_eq!(
            engine
                .lookup(&track(), &path)
                .await
                .unwrap()
                .unwrap()
                .source,
            "local"
        );
    }
}
