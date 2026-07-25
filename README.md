# easy-music

Agent-friendly music search, selection, downloading, and streaming audio
transcoding CLI.

`easy-music` keeps metadata operations and binary streaming separate:

- `search`, `select`, `resolve`, `prepare`, and `download` write stable JSON to
  stdout.
- `stream` writes only audio bytes to stdout; optional lifecycle JSONL goes to
  stderr.
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

- Rust 1.85 or newer to build.
- `ffmpeg` on `PATH` for `stream`.

## Build

```bash
cargo build --release
cargo install --path .
```

## Agent-facing metadata commands

```bash
easy-music search --keyword "晴天" --artist "周杰伦" --pretty
easy-music select --title "晴天" --artist "周杰伦" --pretty
easy-music resolve --id "MTEyNjE3ODA=" --pretty
easy-music prepare --title "晴天" --artist "周杰伦" --target xiaozhi --pretty
```

Do not concatenate title and artist into one upstream keyword. Search by title,
then use `--artist` for local ranking. This handles providers that return no
results for combined queries such as `晴天 周杰伦`.

## Downloading

Download the original remote audio bytes without invoking ffmpeg:

```bash
# Automatically named "周杰伦 - 晴天.mp3" in the current directory
easy-music download --title "晴天" --artist "周杰伦" --pretty

# Download by search-result ID to an exact path
easy-music download --id "MTEyNjE3ODA=" --output "./music/sunny.mp3" --pretty

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
  --id "MTEyNjE3ODA=" \
  --format pcm-s16le \
  --sample-rate 24000 \
  --channels 1 \
  --output - > song.pcm
```

Streaming Ogg Opus:

```bash
easy-music stream --id "MTEyNjE3ODA=" --format opus-ogg --output song.opus
```

xiaozhi-compatible raw Opus packets:

```bash
easy-music stream \
  --id "MTEyNjE3ODA=" \
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

- `EASY_MUSIC_API_BASE_URL`: compatible API origin; defaults to
  `https://buguyy.top`.
- `EASY_MUSIC_FFMPEG`: ffmpeg executable path.

Run diagnostics:

```bash
easy-music doctor --online --pretty
```

The default music source is a third-party service. Its availability, catalog,
and returned media URLs are outside this project's control. Use audio only
where you have the right to access and play it.
