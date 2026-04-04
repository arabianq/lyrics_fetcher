# Lyrics Fetcher

A fast, asynchronous command-line utility written in Rust to automatically fetch and embed lyrics into your local music library. It traverses your directories, reads audio metadata, and writes the fetched lyrics directly back into the ID3 tags of your audio files.

## Features

- **Asynchronous Processing**: Speed up lyrics fetching by processing multiple files concurrently.
- **Multiple Sources**: Fetch from multiple configurable lyrics sources (default: `lrclib`).
- **Smart Skipping**: Automatically skips files that already have lyrics, with an option to force overwrite.
- **Configurable Matching**: Option to allow inaccurate matches for hard-to-find lyrics.

## Installation

### Pre-built Binaries

You can download the latest pre-compiled binaries for Linux, macOS, and Windows from the Releases section (generated via the project's Development Release workflow). Both `x86_64` and `aarch64` architectures are supported.

### Build from Source

To build from source, ensure you have Rust and Cargo installed:

```bash
git clone https://github.com/your-username/lyrics_fetcher.git
cd lyrics_fetcher
cargo build --release
```

The compiled binary will be available at `target/release/lyrics_fetcher` (or `target/release/lyrics_fetcher.exe` on Windows).

## Usage

At a minimum, provide the path to your music directory:

```bash
lyrics_fetcher /path/to/your/music
```

### Options

```text
Usage: lyrics_fetcher [OPTIONS] <MUSIC_DIR_PATH>

Arguments:
  <MUSIC_DIR_PATH>                 The root directory containing your music library.

Options:
  -t, --threads <THREADS>          Number of concurrent threads to use [default: 1]
  -o, --overwrite-existing         Overwrite existing lyrics in audio files [default: false]
  -a, --allow-inaccurate           Allow inaccurate matches when searching [default: false]
  -s, --sources <SOURCES>          Comma-separated list of lyrics sources to query [default: lrclib]
  -e, --extensions <EXTENSIONS>    Comma-separated list of file extensions to process 
                                   [default: flac,mp3,ogg,opus,aac,wav,m4a,alac,aiff,ape]
  -h, --help                       Print help
```

### Examples

**Fetch lyrics using 8 concurrent threads:**
```bash
lyrics_fetcher --threads 8 ~/Music
```

**Force overwrite of existing lyrics and allow inaccurate matches:**
```bash
lyrics_fetcher --overwrite-existing --allow-inaccurate ~/Music
```

**Process only MP3 and FLAC files using a specific source:**
```bash
lyrics_fetcher --extensions mp3,flac --sources lrclib ~/Music
```

## Configuration

This project supports `dotenv`. Depending on the lyrics sources you configure (if using sources other than `lrclib`), you may be able to provide API keys or other configuration options via a `.env` file in your working directory.