# CLI usage

## Discover and diagnose

```bash
easymusic --help
easymusic search --help
easymusic download --help
easymusic doctor --pretty
# Contacts each enabled source with a small search:
easymusic doctor --online --pretty
```

Successful commands print JSON on stdout. Runtime errors print
`{"ok":false,"error":{"code":"...","message":"..."}}` on stderr and exit
nonzero. Argument-parser errors and help are plain text. Inspect both the exit
status and JSON `ok`: `doctor` can exit successfully while reporting `ok:false`.
`--pretty` changes JSON formatting only.

Search and download from `netease`/`kuwo` need no external executable. YouTube
uses yt-dlp and a JavaScript runtime; release bundles include them. Streaming
needs ffmpeg; CLI search/download do not. Doctor currently checks ffmpeg as a
required dependency even for CLI-only use, so inspect individual dependency
results when its overall `ok` is false. `yt_dlp.required` depends on whether
YouTube is enabled. Online doctor passes its search check if any source works;
inspect per-source results for a pinned-source failure.

## Search

```bash
easymusic search --keyword "晴天" --artist "周杰伦" --limit 5
easymusic --source netease search --keyword "晴天" --artist "周杰伦"
easymusic --sources netease,kuwo search --keyword "晴天" --all-sources
```

`--keyword` is required; `--artist` is optional. `--limit` is 1–50 (default 10).
The response has `ok`, `keyword`, `count`, `source`, and `tracks`. Each track has
`id`, `title`, `artist`, and optional `artwork_url`. Results are ranked, but CLI
search does not return `confidence` or `needs_confirmation`.

`--all-sources` merges and reranks enabled sources; the response source is
`"all"`. A simultaneous `--source` (including `EASYMUSIC_SOURCE`) restricts that
merge to the pinned source. To merge all sources, omit the pin and remove an
inherited `EASYMUSIC_SOURCE` from that command's environment.

## Download

Choose exactly one input mode: `--id`, `--url`, or the query group
`--title`/`--artist` (either or both).

```bash
# Replace the example ID with the exact selected search-result ID.
easymusic download --id "netease:186016" --output-dir ./music
# Immediately selects and downloads the best match:
easymusic --source netease download --title "晴天" --artist "周杰伦" --output-dir ./music
# For an already resolved direct HTTP(S) audio URL:
easymusic download --url "https://example.com/song.mp3" --output ./music/song.mp3
```

`--output` is an exact path; `--output-dir` generates a filename from metadata
and the source format. They are mutually exclusive. Without either, the output
directory is the current directory. Prefer automatic naming when the source
format is unknown: writing a `.mp3` filename does not transcode WebM or M4A.
`--url` expects audio bytes, not a music-service webpage.

Files are written through a temporary sibling and installed atomically.
Existing destinations are preserved unless `--force` is supplied. Use `--force`
only when replacement is intended. Private/loopback/link-local source URLs are
blocked by default; use `--allow-private-network` only for an intended private
source, not as a generic retry flag.

Success includes `ok:true` and `file.path`, `file.source_url`,
`file.bytes_written`, and optional `file.content_type`. ID/query downloads also
include `track`; query downloads include `confidence` and `needs_confirmation`.
Use `file.path` as the saved artifact path.

## Runtime overrides

| CLI option | Environment variable | Purpose |
| --- | --- | --- |
| `--source NAME` | `EASYMUSIC_SOURCE` | Pin searches to one enabled source |
| `--sources NAMES` | `EASYMUSIC_SOURCES` | Comma-separated enabled sources in priority order |
| `--plugins-dir PATH` | `EASYMUSIC_PLUGINS_DIR` | Discover `easymusic-source-*` executables |
| `--yt-dlp PATH` | `EASYMUSIC_YT_DLP` | Override YouTube extractor |
| `--js-runtime RUNTIME[:PATH]` | `EASYMUSIC_JS_RUNTIME` | For example `quickjs:/opt/easymusic/qjs` |
| `--cookies PATH` | `EASYMUSIC_YT_DLP_COOKIES` | Existing Netscape-format YouTube cookie file |

Plugins are also discovered in `plugins/` beside the executable and directories
in `EASYMUSIC_SOURCE_PATH`. Use configured sources rather than installing new
plugins implicitly. Treat cookie contents as secrets; pass the file path.

Runtime error codes and exit statuses: `invalid_arguments` 2, `no_results` 3,
`ambiguous_selection` 4, `upstream_api` 5, `audio_source` 6, `transcode` 7,
`dependency_missing` 8, `interrupted` 130, and `io` 1. For upstream/source errors,
inspect the message for unavailable tracks, network failures, or YouTube
challenges. Source fallback applies to search, not automatic recovery of an
already selected ID's audio resolution.
