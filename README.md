# easymusic

Agent-friendly music search and downloading CLI, plus an MCP server for search
and audio streaming. Music discovery and playable URL resolution are
implemented exclusively through `yt-dlp`; the old site-specific provider
adapters have been removed.

The metadata and audio paths remain separate:

- `search` calls `ytsearchN:` with a flat playlist and returns stable JSON.
- `download` searches and ranks by title/artist or accepts an ID/URL, then
  streams the original audio format directly to disk.
- `mcp` exposes search and one-time streaming preparation tools over stdio.

No browser, browser engine, page DOM parser, or Python installation is needed
when the standalone release binaries are used.

## Runtime layout

Official easymusic release archives include `yt-dlp` and a JavaScript runtime:

```text
easymusic       # easymusic.exe on Windows
yt-dlp           # yt-dlp.exe on Windows
qjs              # qjs.exe on Windows; Deno is used on Windows ARM64
```

easymusic first looks for `yt-dlp` beside its own executable, then falls back
to `PATH`. When the selected `yt-dlp` has an adjacent `deno` or `qjs`, it is
automatically passed as the YouTube JavaScript runtime. Deno is preferred if
both exist.

This JavaScript runtime is required by current yt-dlp releases for full YouTube
support; it is a small command-line engine, not a browser. The official
standalone yt-dlp executable already contains its Python runtime and EJS solver
scripts.

When building or installing easymusic yourself, provide:

- Rust 1.88 or newer to build.
- A current standalone `yt-dlp` executable.
- QuickJS-NG 0.12+ or Deno 2.3+ for current YouTube extraction.
- `ffmpeg` on `PATH` only for MCP audio output and the library streaming API.
  CLI search and download do not invoke ffmpeg.

Paths can be overridden without relying on machine-wide installations:

```bash
easymusic \
  --yt-dlp /opt/easymusic/yt-dlp \
  --js-runtime quickjs:/opt/easymusic/qjs \
  search --keyword "晴天" --artist "周杰伦" --pretty
```

Equivalent environment variables are `EASYMUSIC_YT_DLP` and
`EASYMUSIC_JS_RUNTIME`.

Some server/datacenter IPs are challenged by YouTube. Export a Netscape-format
`cookies.txt` on an authorized machine and mount it read-only on the server:

```bash
easymusic --cookies /run/secrets/youtube-cookies.txt \
  search --keyword "晴天" --artist "周杰伦"
```

`EASYMUSIC_YT_DLP_COOKIES` is the environment-variable equivalent. The server
does not need the browser that exported the file. Treat the file as a secret;
easymusic passes its path to yt-dlp and never includes its contents in output.

## Build

```bash
cargo build --release
cargo install --path .
```

`cargo install` installs only easymusic itself. For a self-contained server
deployment, use a release archive or place standalone yt-dlp and QuickJS/Deno
beside the installed easymusic executable.

## Searching

```bash
easymusic search --keyword "天地龙鳞" --artist "王力宏" --limit 10 --pretty
```

When both title and artist are supplied, easymusic searches for both (for
example `天地龙鳞 王力宏`) and then uses the separate values for local ranking.
Search IDs are YouTube video IDs. Download resolves the selected ID immediately
before transfer because resolved media URLs are short-lived.

The library API uses the same backend:

```rust,no_run
use easymusic::{MusicClient, YtDlpConfig};
use std::path::PathBuf;

# async fn example() -> easymusic::Result<()> {
let client = MusicClient::with_config(YtDlpConfig {
    executable: PathBuf::from("/opt/easymusic/yt-dlp"),
    js_runtime: Some("quickjs:/opt/easymusic/qjs".to_owned()),
    cookies: Some(PathBuf::from("/run/secrets/youtube-cookies.txt")),
});
let results = client.search("晴天 周杰伦", 10).await?;
let audio = client.resolve(&results.tracks[0].id).await?;
# Ok(())
# }
```

## Downloading

Download the selected best audio-only format without transcoding:

```bash
# Uses yt-dlp metadata to retain the selected format extension, such as .m4a or .webm
easymusic download --title "天地龙鳞" --artist "王力宏" --pretty

# Resolve a search-result video ID, then save to an exact path
easymusic download --id "DYptgVvkVLQ" --output "./music/song.webm" --pretty

# A previously resolved URL can still be downloaded directly
easymusic download --url "https://example.com/song.mp3" --output-dir "./music"
```

Downloads use a temporary sibling file and are installed atomically after
completion. Existing files are preserved unless `--force` is supplied.

## Library streaming

Audio transcoding remains available to Rust integrations and the MCP server.

Available profiles:

| Profile | Output |
| --- | --- |
| `web-voice` | 48 kHz stereo Ogg Opus |
| `xiaozhi` | 24 kHz mono raw Opus packets with `len32be` framing |
| `pcm16k` | 16 kHz mono PCM s16le |
| `pcm24k` | 24 kHz mono PCM s16le |

The `len32be` format is a four-byte unsigned big-endian packet length followed
by one raw Opus packet, repeated until end-of-stream.

For an in-process device integration, consume chunks directly:

```rust,ignore
# async fn example(config: easymusic::StreamConfig) -> easymusic::Result<()> {
let mut audio = easymusic::spawn_audio_stream(&config).await?;
while let Some(chunk) = audio.next_chunk().await {
    // `opus-packets` yields one raw Opus packet per chunk.
    device.send_binary(chunk.data).await?;
}
audio.finish().await?;
# Ok(())
# }
```

## MCP server

```bash
easymusic mcp
```

The MCP server exposes:

- `search_music`: combine title/artist, search through yt-dlp, rank candidates,
  and return the selected track plus alternatives.
- `prepare_stream`: resolve a confirmed video ID, start ffmpeg, prebuffer the
  first audio chunk, and return a short-lived one-time loopback URL.

Streaming profiles are `xiaozhi-v1`, `web-opus`, `pcm-s16le-16k`, and
`pcm-s16le-24k`. Prepared streams are served only on loopback, expire after 60
seconds by default, can be consumed once, and are limited to eight pending
streams.

`EASYMUSIC_FFMPEG` overrides ffmpeg and `EASYMUSIC_STREAM_BIND` overrides the
MCP loopback listener. The corresponding CLI flags are also available.

## Diagnostics

```bash
easymusic doctor --pretty
easymusic doctor --online --pretty
```

The result reports the exact yt-dlp command, detected adjacent JS runtime,
ffmpeg version, and optionally the result of a one-item online YouTube search.

## Releases

Tags matching `v*` publish self-contained search/runtime archives for:

| Platform | Targets | Archive |
| --- | --- | --- |
| Linux (glibc) | `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` | `.tar.gz` |
| Linux (musl) | `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` | `.tar.gz` |
| macOS | `x86_64-apple-darwin`, `aarch64-apple-darwin` | `.tar.gz` |
| Windows | `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc` | `.zip` |

Every archive includes easymusic, yt-dlp, QuickJS (or Deno on Windows ARM64),
documentation, third-party notices, and a SHA-256 checksum. ffmpeg remains a
separate optional runtime because it is used only by MCP and library
transcoding/streaming.

## Safety and usage

Only HTTP and HTTPS audio sources are accepted. Direct-URL downloads and
library streaming reject private, loopback, link-local, and documentation
addresses unless explicitly allowed by their flag or configuration.

yt-dlp search and extraction depend on YouTube behavior and may require regular
yt-dlp updates. Use media only where you have the right to access, download,
and play it, and comply with the source site's terms and applicable law.
