---
name: easymusic
description: Search music, download original audio files, and prepare audio playback streams using the easymusic CLI or MCP server. Use for song or artist lookup, music downloads, playback through easymusic, and easymusic runtime troubleshooting.
---

# easymusic

Use easymusic's source registry to find tracks and resolve their audio. Built-in
sources are `youtube`, `netease`, and `kuwo`; configured executable plugins can
add others.

## Install

If `easymusic` is not installed, use Cargo with Rust 1.88 or newer:

```bash
cargo install --git https://github.com/caftxx/easymusic --locked
```

Alternatively, install from the root of a local easymusic checkout:

```bash
cargo install --path . --locked
```

Ensure Cargo's installation bin directory (`$CARGO_HOME/bin`, or
`~/.cargo/bin` by default) is on PATH. Then verify the installed command:

```bash
easymusic --version
easymusic --help
```

Cargo installs only easymusic. The `netease` and `kuwo` sources need no extra
executables; YouTube needs yt-dlp and a JavaScript runtime such as QuickJS or
Deno. MCP audio streaming additionally needs ffmpeg. See
[CLI usage](references/cli.md) for dependency diagnostics and runtime overrides.

## Choose the interface

- If easymusic MCP is configured and its tools are available, prefer
  `search_music` for music lookup and `prepare_stream` for playback preparation.
  Read [MCP usage](references/mcp.md).
- If MCP is not configured or its tools are unavailable, use the CLI directly
  for search and download; MCP setup is not a prerequisite. Read
  [CLI usage](references/cli.md). The CLI does not provide playback, so a
  playback request still requires a compatible player or playback integration.
- For saving an original audio file, searching across sources, or diagnosing
  the runtime, use the CLI. Read [CLI usage](references/cli.md).
- MCP has no download tool. The CLI has only `search`, `download`, `mcp`, and
  `doctor`; do not invent `play`, `select`, `resolve`, or `stream` subcommands.
- Preparing a stream does not play sound. Playback requires a compatible
  consumer that can reach the MCP server's loopback address. Check that this
  consumer exists before preparing a stream, and report playback only after
  the consumer actually starts it.

Use the installed executable and live MCP schema as the authority if the
deployed version differs from these instructions. Invoke `easymusic` from PATH.

## Search and select

Keep song title and artist separate when known; both are used for search and
ranking. Preserve requested versions such as live, cover, or instrumental.
Do not treat ranking as proof of identity or availability of playable audio.

For MCP requests, inspect `ok` and `needs_confirmation` in the search result.
When `needs_confirmation` is true, present the selected track and relevant
alternatives and ask the user to choose before calling `prepare_stream`, unless
the user has already confirmed that exact track. When false, continue within
the user's requested action without an extra confirmation step.

For CLI downloads where the intended track is unclear, search first, resolve
the ambiguity, then download by ID. `download --title/--artist` immediately
downloads the best match; its returned `needs_confirmation` is informational
and does **not** pause the download.

## Preserve source identity

- Pass returned track IDs unchanged, including prefixes such as `netease:` or
  `kuwo:`. Do not substitute a title, webpage URL, or guessed native ID.
- Bare input IDs resolve through the first enabled source (normally YouTube).
  Only that first source can emit bare IDs, if it supports doing so. Keep the
  enabled source order consistent between search and use.
  For storage across configurations, retain the originating source as well;
  do not reinterpret a bare ID after reordering sources.
- `--source` pins search; `--sources` sets the enabled sources and their order.
  Without a pin, search falls back in priority order. Resolution routes by
  track ID, so changing `--source` does not move a selected ID to another source.
- Resolve IDs close to download/playback time. Upstream media URLs can expire;
  keep the track ID for subsequent requests rather than caching a media URL.

## Handle failures

Read the returned error before retrying. Correct invalid arguments, missing
dependencies, or source configuration first. If a track is unavailable, search
another enabled source when consistent with the user's request and select its
returned ID; do not merely replace the old ID's prefix. Stop repeated attempts
when the same error persists without a changed input or environment.

Report the actual outcome: selected track, saved file path, prepared stream, or
playback started. Include a blocking error when the requested action could not
complete.
