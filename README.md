# easy-music

Agent-friendly music search, selection, downloading, and streaming audio
transcoding CLI and MCP server.

`easy-music` keeps metadata operations and binary streaming separate:

- `search`, `select`, `resolve`, `prepare`, and `download` write stable JSON to
  stdout.
- `stream` writes only audio bytes to stdout; optional lifecycle JSONL goes to
  stderr.
- `mcp` exposes search and one-time streaming preparation tools over MCP stdio.
- The Rust library exposes the same API for in-process integrations.

For a device integration, consume chunks directly instead of parsing CLI
stdout:

```rust,no_run
let mut audio = easy_music::spawn_audio_stream(&config).await?;
while let Some(chunk) = audio.next_chunk().await {
    // `opus-packets` yields exactly one raw Opus packet per chunk.
    device.send_binary(chunk.data).await?;
}
audio.finish().await?;
```

## Requirements

- Rust 1.88 or newer to build.
- `ffmpeg` on `PATH` for `stream`.

## Build

```bash
cargo build --release
cargo install --path .
```

## Provider architecture

`MusicClient` is a provider-independent facade. It validates public inputs and
delegates to an injected `MusicProvider`:

```text
src/api.rs                         MusicClient facade
src/provider/mod.rs                MusicProvider interface + built-in factory
src/provider/buguyy.rs             Buguyy implementation and DTOs
src/provider/qianqian.rs           91Q implementation, signing, and DTOs
src/provider/yymp3.rs              YYMP3 implementation and HTML parsing
```

Each provider owns its endpoints, transport details, response DTOs, parsers,
and focused tests. Adding another built-in source requires a new provider file
and one factory registration; CLI, MCP, downloading, and streaming code remain
unchanged.

Library users can inject a custom implementation directly:

```rust,no_run
use async_trait::async_trait;
use easy_music::{
    MusicClient, MusicProvider, ResolvedTrack, Result, SearchResult,
};

struct MyProvider;

#[async_trait]
impl MusicProvider for MyProvider {
    fn name(&self) -> &str {
        "my-provider"
    }

    async fn search(&self, keyword: &str) -> Result<SearchResult> {
        todo!("search {keyword}")
    }

    async fn resolve(&self, id: &str) -> Result<ResolvedTrack> {
        todo!("resolve {id}")
    }
}

let client = MusicClient::with_provider(MyProvider);
```

Tags matching `v*` publish release archives for:

| Platform | Targets | Archive |
| --- | --- | --- |
| Linux | `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` | `.tar.gz` |
| macOS | `x86_64-apple-darwin`, `aarch64-apple-darwin` | `.tar.gz` |
| Windows | `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc` | `.zip` |

Each archive includes a SHA-256 checksum. The Linux binaries are portable
across common glibc- and musl-based systems. `ffmpeg` remains a separate
runtime requirement on every platform.

## Agent-facing metadata commands

```bash
easy-music search --keyword "天地龙鳞" --artist "王力宏" --pretty
easy-music select --title "天地龙鳞" --artist "王力宏" --pretty
easy-music resolve --id "T10062480746" --pretty
easy-music prepare --title "天地龙鳞" --artist "王力宏" --target xiaozhi --pretty
```

Do not concatenate title and artist into one upstream keyword. Search by title,
then use `--artist` for local ranking. This handles providers that return no
results for combined queries such as `晴天 周杰伦`.

### Provider selection

List all registered providers:

```bash
easy-music provider --list --pretty
```

Select a provider by name with the global `--provider` option. The default is
`qianqian`:

```bash
easy-music \
  --provider yymp3 \
  search --keyword "刚好遇见你" --artist "李玉刚" --pretty
```

### 91Q / 千千音乐

Search-result `TSID` values are accepted by `resolve`, `stream`, `prepare`,
`download`, and the MCP server. The adapter resolves the 320 kbit/s source when
available. Media URLs are signed and short-lived, so resolve immediately before
playback or downloading. Unauthenticated VIP results are searchable, but
resolving them returns an `audio_source` error instead of using a preview or
attempting to bypass account access.

## MCP server

Start the local stdio MCP server:

```bash
easy-music mcp
```

It exposes:

- `search_music`: search, rank, and return a selected track plus alternatives.
- `prepare_stream`: resolve a selected ID and return a short-lived, one-time
  loopback URL using a terminal output profile.

`prepare_stream` defaults to `xiaozhi-v1` for backward compatibility and
supports these profiles:

- `xiaozhi-v1`: 24 kHz mono Opus packets, 60 ms, `len32be`.
- `web-opus`: 48 kHz stereo Opus in an Ogg stream.
- `pcm-s16le-16k`: raw 16 kHz mono signed 16-bit PCM.
- `pcm-s16le-24k`: raw 24 kHz mono signed 16-bit PCM.

For example, pass `"profile": "web-opus"` for browser-oriented playback.
The result reports `profile`, `content_type`, `codec`, `framing`, sample rate,
channel count, and nominal frame duration so clients can validate compatibility.
It also reports `ready`, `prebuffered_bytes`, and `prepare_latency_ms`.
`ready: true` means ffmpeg has already started and at least one audio chunk is
buffered, reducing the delay between the terminal's HTTP request and its first
audio frame.

MCP is the control plane only. Prepared audio is served over an ephemeral HTTP
listener bound to `127.0.0.1`; the URL expires after 60 seconds and can be
consumed once. Up to eight unconsumed prepared streams are retained; expired or
evicted streams terminate their ffmpeg process. Override the ffmpeg executable
with `EASY_MUSIC_FFMPEG`, the listener with `EASY_MUSIC_STREAM_BIND`, or use the
corresponding `mcp` flags.

Remote HTTP audio reads have a 15-second I/O timeout. ffmpeg automatically
reconnects interrupted seekable and streaming responses with a maximum
two-second reconnect delay, while normal end-of-file still completes playback.

## Downloading

Download the original remote audio bytes without invoking ffmpeg:

```bash
# Automatically named "王力宏 - 天地龙鳞（…）.mp3" in the current directory
easy-music download --title "天地龙鳞" --artist "王力宏" --pretty

# Download by search-result ID to an exact path
easy-music download --id "T10062480746" --output "./music/song.mp3" --pretty

# Download an already resolved URL into a directory
easy-music download --url "https://example.com/song.mp3" --output-dir "./music"
```

Downloads are streamed through a temporary sibling file and moved into place
only after completion. Existing files are preserved unless `--force` is
specified. The JSON result includes the final absolute path, byte count,
content type, selected track, and selection confidence when available.

## Streaming

Raw mono 24 kHz signed 16-bit little-endian PCM:

```bash
easy-music stream \
  --id "T10062480746" \
  --format pcm-s16le \
  --sample-rate 24000 \
  --channels 1 \
  --output - > song.pcm
```

Streaming Ogg Opus:

```bash
easy-music stream --id "T10062480746" --format opus-ogg --output song.opus
```

xiaozhi-compatible raw Opus packets:

```bash
easy-music stream \
  --id "T10062480746" \
  --profile xiaozhi \
  --output -
```

`opus-packets` stdout uses `len32be` framing:

```text
4-byte unsigned big-endian packet length
N-byte raw Opus packet
4-byte unsigned big-endian packet length
N-byte raw Opus packet
...
```

The `xiaozhi` profile defaults to mono, 24 kHz, 64 kbit/s, and 60 ms Opus
packets. Explicit flags override profile defaults.

Available profiles:

| Profile | Output |
| --- | --- |
| `web-voice` | 48 kHz stereo Ogg Opus |
| `xiaozhi` | length-framed raw Opus packets |
| `pcm16k` | 16 kHz mono PCM s16le |
| `pcm24k` | 24 kHz mono PCM s16le |

## Safety and configuration

Only HTTP and HTTPS audio sources are accepted. Private, loopback, link-local,
and documentation addresses are rejected unless
`--allow-private-network` is explicitly provided.

Environment variables:

- `EASY_MUSIC_PROVIDER`: provider name; defaults to `qianqian`. Run
  `easy-music provider --list` for the supported names.
- `EASY_MUSIC_FFMPEG`: ffmpeg executable path.

Run diagnostics:

```bash
easy-music doctor --online --pretty
```

The default music source is a third-party service. Its HTML, internal
JavaScript endpoints, availability, catalog, and returned media URLs are
outside this project's control. Use audio only where you have the right to
access and play it.
