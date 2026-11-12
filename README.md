# Lyrics Fetcher

Fetch and embed plain, line-synced and word-synced lyrics in a local music library.
Supports MP3, FLAC, Ogg Vorbis, Opus, AAC, WAV, M4A/ALAC, AIFF and APE through
[Lofty](https://github.com/Serial-ATA/lofty-rs).

## Sources

The default providers, in priority order, are:

| CLI name | Timing / format |
| --- | --- |
| `betterlyrics` | Word/syllable timing from TTML |
| `unison` | TTML, LRC or plain text |
| `lyricsplus` | Word/syllable and line timing from JSON |
| `kugou` | Word timing from KRC, with an LRC fallback |
| `lrclib` | Line timing from LRC; word timing from Lyricsfile |
| `youtubemusic` | Line timing or plain text |

`genius` is also available as a plain-text fallback, using `GENIUS_API_KEY`.
BetterLyrics can serve cached tracks without a key; set `BETTERLYRICS_API_KEY`
for access to uncached tracks if you have one. Keys can be supplied through the
environment or a `.env` file in the working directory or its parents.

Providers and parsers are adapted from the MIT-licensed
[Dux lyrics engine](https://github.com/duxlabs-dev/dux-go/tree/26b745ee0afd6ea68584427309d154d4612cc57b/lyrics).
See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Install

Download a binary from the repository's Releases page, or build with stable Rust:

```sh
git clone https://github.com/arabianq/lyrics_fetcher.git
cd lyrics_fetcher
cargo build --release --locked
```

## Usage

```sh
lyrics_fetcher ~/Music
```

Files with nonempty embedded lyrics are skipped unless `--overwrite-existing` is
set. Instrumental tracks are reported without writing empty lyrics. A missing
result leaves the audio unchanged. File, directory and write failures produce a
nonzero exit status and appear in the final summary.

Preview the matches before writing:

```sh
lyrics_fetcher ~/Music --dry-run
```

Process four files concurrently and also save enhanced `.lrc` files:

```sh
lyrics_fetcher ~/Music --threads 4 --sidecar
```

Select providers and their priority:

```sh
lyrics_fetcher ~/Music --sources betterlyrics,kugou,lrclib
lyrics_fetcher ~/Music --sources lrclib,genius
```

Use local sidecars only, or use sidecars and cached online results:

```sh
lyrics_fetcher ~/Music --sources none --no-cache
lyrics_fetcher ~/Music --offline
```

### Matching and timing

Title, artist and duration come from audio metadata. Strict matching normalizes
case and punctuation, keeps version notes such as `(Live)`, and allows up to eight
seconds of duration difference where the provider supplies a duration.
`--allow-inaccurate` permits approximate provider matches; review these with
`--dry-run` before overwriting a library.

Providers run concurrently. The first successful result starts a two-second
window for other providers to supply a better match. Ranking considers plausible
track length, synchronization, agreement between providers' line timings, word
timing, known duration and finally provider priority. A fast plain-text answer
therefore does not immediately end the search.

Timing is supplied by the providers; this tool does not generate word alignment
from audio. Invalid word timing falls back to line timing. LyricsPlus responses
that claim synchronization but contain only zero timestamps are rejected.

| `--lyrics-format` | Embedded output |
| --- | --- |
| `auto` (default) | Enhanced LRC with word timing when available, otherwise LRC or plain text |
| `lrc` | Line-level LRC, or plain text if no timing exists |
| `plain` | Text without timestamps |

For MP3, `auto` and `lrc` also write an ID3 SYLT frame with word or line start
positions. `--sidecar` exports enhanced LRC regardless of the embedded format.
Player support for enhanced LRC and SYLT varies. LRC export does not preserve
separate singer identities or the explicit backing-vocal flag.

### Local lyrics

Before any provider or cache lookup, a track such as `song.flac` checks these files
in the same directory:

1. `song.lyricsfile.yaml` (Lyricsfile 1.0)
2. `song.ttml`
3. `song.lrc`
4. `song.LRC`

Invalid files are reported and the search continues. Local lyrics have priority.
An existing local `.lrc` is preserved when embedding from a local sidecar.

### Options

| Option | Default / behavior |
| --- | --- |
| `-t, --threads N` | `1`; concurrent files, from 1 to 256 |
| `-o, --overwrite-existing` | Replace embedded lyrics and allow replacing exported sidecars |
| `-a, --allow-inaccurate` | Allow approximate matches |
| `-s, --sources LIST` | Six default providers above; comma-separated; `none` disables online providers |
| `-e, --extensions LIST` | `flac,mp3,ogg,opus,aac,wav,m4a,alac,aiff,ape` |
| `--lyrics-format FORMAT` | `auto`, `lrc` or `plain` |
| `--sidecar` | Also export enhanced `.lrc` |
| `--offline` | Never request lyrics over the network |
| `--no-cache` | Disable reading and writing the disk cache |
| `--cache-dir PATH` | Override the platform cache directory |
| `--request-interval MS` | `500`; minimum interval between requests to each provider |
| `--dry-run` | Search without changing audio or sidecars; cache may still be populated |

### Cache and request limits

The default cache is `$XDG_CACHE_HOME/lyrics_fetcher` or
`~/.cache/lyrics_fetcher` on Linux, `~/Library/Caches/lyrics_fetcher` on macOS,
and `%LOCALAPPDATA%/lyrics_fetcher` on Windows.

Results are cached separately for each provider and track signature, including
duration and matching mode. Successful results expire after 90 days. Plain and
line-synced results are refreshed after 14 days to look for word timing; a failed
refresh retains the cached result. Misses expire after one day. Request failures
and BetterLyrics uncached-track responses are not cached as misses.

Each provider processes one track lookup at a time. Other files wait in a queue
before the 45-second lookup timeout starts. Individual HTTP requests have a
five-second connection timeout and a 15-second total timeout. HTTP 429/503
responses are retried up to twice, respecting a numeric `Retry-After` up to 30
seconds. Identical concurrent track lookups share their result.

Audio is updated through a temporary copy in the same directory. Lyrics are read
back before replacing the original, and a file changed during lookup is refused.
This requires enough free space for a copy of each concurrently processed file.
A sidecar write failure after saving audio is reported explicitly.

## Development

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Tests cover timing parsers, matching, ranking, cache behavior, duplicate lookups,
HTTP retry/size limits, and metadata-preserving audio writes. The HTTP test uses a
local server; the test suite does not require access to lyrics providers.
