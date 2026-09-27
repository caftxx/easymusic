# MCP usage

## Connection

The server uses MCP over stdio. A host launches executable `easymusic` with
arguments `["mcp"]`; use an absolute executable path if the host's PATH differs.
For source configuration, an example argument array is
`["--sources", "netease,kuwo", "--source", "netease", "mcp"]`.
Use the host's existing MCP configuration format. Starting a shell process
alone does not connect its tools to an agent.

```bash
easymusic mcp --help
easymusic mcp --ffmpeg /opt/bin/ffmpeg --stream-bind 127.0.0.1:0 --stream-ttl-seconds 60
```

`--ffmpeg` also reads `EASYMUSIC_FFMPEG`; `--stream-bind` also reads
`EASYMUSIC_STREAM_BIND`. The default listener is `127.0.0.1:0` (dynamic port),
and the default TTL is 60 seconds. TTL must be positive and the listener must
be loopback. Keep the server running while consuming its stream.

Discover the connected tools under their host-provided names; wrappers may
prefix `search_music` and `prepare_stream`. Results contain JSON text. Parse
that JSON and check `ok`, even when the MCP transport call itself succeeded.
Errors have `ok:false` and `error.code`/`error.message`.

## Search music

Call `search_music` with title and/or artist; at least one must be nonempty:

```json
{"title":"晴天","artist":"周杰伦","source":"netease"}
```

All three parameters are optional strings; omit unused fields. A supplied
`source` overrides the server's search default and must name an enabled source.
Without it, the server uses its `--source`/`EASYMUSIC_SOURCE` default or falls
back in configured priority order. There is no MCP `limit`, `keyword`, or
`all_sources` parameter.

Success returns `ok`, `selected`, `confidence`, `needs_confirmation`, and
`alternatives`. `selected` and each alternative have flat fields `id`, `title`,
`artist`, `score`, and optional `artwork_url`; the ID is `selected.id`, not
`selected.track.id`. If `needs_confirmation` is true and the exact track has
not already been confirmed, obtain the user's choice before preparing audio.
Do not replace this flag with a self-chosen confidence threshold.

## Prepare and consume audio

After selection, call `prepare_stream` with the selected ID. This example uses
an illustrative ID; substitute the ID returned by search:

```json
{"id":"netease:186016","profile":"web-opus","start_seconds":30,"duration_seconds":20}
```

`id` is required. Optional `start_seconds` must be non-negative;
`duration_seconds` must be positive. Omit them to stream from the beginning
without a duration cap. These fields use seconds, not milliseconds.
`prepare_stream` takes no `source`, URL, output path, or private-network flag.

Choose the profile for the actual consumer; always specify it when the
consumer needs something other than the default:

| MCP profile | Audio | Framing / content type |
| --- | --- | --- |
| `xiaozhi-v1` (default) | 24 kHz mono Opus, 60 ms frames | `len32be` / `application/x-opus-packets` |
| `web-opus` | 48 kHz stereo Opus | `ogg` / `audio/ogg` |
| `pcm-s16le-16k` | 16 kHz mono signed 16-bit little-endian PCM | `raw` / `audio/pcm` |
| `pcm-s16le-24k` | 24 kHz mono signed 16-bit little-endian PCM | `raw` / `audio/pcm` |

`len32be` is a four-byte unsigned big-endian packet length followed by a raw
Opus packet, repeated. It is not Ogg. Raw PCM has no WAV header. Do not use the
Rust library profile names (`web-voice`, `xiaozhi`, `pcm16k`, `pcm24k`) as MCP
arguments.

Success includes `stream_url`, `track_id`, `title`, `profile`, `content_type`,
`codec`, `framing`, `sample_rate`, `channels`, `frame_duration_ms`, `ready`,
`prebuffered_bytes`, `prepare_latency_ms`, and `expires_in_seconds`. The server
starts ffmpeg and waits for the first audio chunk before returning `ready:true`.
This indicates buffered audio, not playback success or a completed transfer.

Immediately pass `stream_url` and its format metadata to the compatible playback
consumer. The URL is loopback-only and usable from the server's machine/network
namespace; a remote browser, speaker, or sandbox cannot reach it merely because
the agent can see the URL. Use an available local playback integration or report
the missing integration instead of presenting the URL as a public audio link.

Each URL is single-use and unused URLs expire after the returned TTL. Do not
probe it with HEAD/GET, prefetch it, or download it for inspection before handing
it to the consumer: a request can consume the lease. Prepare only when the
consumer is ready. At most eight pending streams are retained; preparing more
evicts the oldest. For a consumed, expired, or evicted URL, prepare a fresh stream
for the same confirmed ID instead of retrying the old URL. Investigate recurring
consumer failures before creating further streams.
