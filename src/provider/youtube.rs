//! YouTube music source backed by the external `yt-dlp` process.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;
use tokio::time::timeout;

use crate::config::YtDlpConfig;
use crate::error::{EasyMusicError, ErrorCode, Result};
use crate::network::validate_http_url;
use crate::provider::{MusicSource, SourceDiagnostics};
use crate::provider::{SourceResolvedTrack, SourceTrack};
use crate::track_id::NativeTrackId;

const SEARCH_TIMEOUT: Duration = Duration::from_secs(45);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(90);
const YOUTUBE_WATCH_URL: &str = "https://www.youtube.com/watch?v=";

/// `MusicSource` implementation that shells out to `yt-dlp`.
#[derive(Debug, Clone)]
pub struct YouTubeSource {
    executable: PathBuf,
    js_runtime: Option<String>,
    cookies: Option<PathBuf>,
}

impl YouTubeSource {
    pub fn new(mut config: YtDlpConfig) -> Self {
        if config.js_runtime.is_none() {
            config.js_runtime = discover_adjacent_js_runtime(&config.executable);
        }
        Self {
            executable: config.executable,
            js_runtime: config.js_runtime,
            cookies: config.cookies,
        }
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn js_runtime(&self) -> Option<&str> {
        self.js_runtime.as_deref()
    }

    async fn run(
        &self,
        operation_args: &[&str],
        deadline: Duration,
        failure_code: ErrorCode,
        operation: &str,
    ) -> Result<Vec<u8>> {
        let mut command = Command::new(&self.executable);
        command
            .args([
                "--ignore-config",
                "--no-warnings",
                "--no-progress",
                "--no-update",
            ])
            .args(
                self.js_runtime
                    .as_ref()
                    .map(|runtime| ["--js-runtimes", runtime.as_str()])
                    .into_iter()
                    .flatten(),
            )
            .args(
                self.cookies
                    .as_ref()
                    .map(|cookies| ["--cookies".as_ref(), cookies.as_os_str()])
                    .into_iter()
                    .flatten(),
            )
            .args(operation_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let output = timeout(deadline, command.output())
            .await
            .map_err(|_| {
                EasyMusicError::new(
                    failure_code,
                    format!(
                        "yt-dlp {operation} timed out after {} seconds",
                        deadline.as_secs()
                    ),
                )
            })?
            .map_err(|error| start_error(&self.executable, error))?;

        if !output.status.success() {
            let detail = concise_stderr(&output.stderr);
            let message = if detail.is_empty() {
                format!("yt-dlp {operation} failed with {}", output.status)
            } else {
                format!("yt-dlp {operation} failed: {detail}")
            };
            return Err(EasyMusicError::new(failure_code, message));
        }
        Ok(output.stdout)
    }
}

#[async_trait]
impl MusicSource for YouTubeSource {
    fn name(&self) -> &'static str {
        "youtube"
    }

    fn display_name(&self) -> &'static str {
        "YouTube (yt-dlp)"
    }

    fn diagnostics(&self) -> SourceDiagnostics<'_> {
        SourceDiagnostics {
            executable: Some(&self.executable),
            js_runtime: self.js_runtime.as_deref(),
        }
    }

    async fn search(&self, keyword: &str, limit: usize) -> Result<Vec<SourceTrack>> {
        let search = format!("ytsearch{limit}:{keyword}");
        let output = self
            .run(
                &[
                    "--flat-playlist",
                    "--skip-download",
                    "--dump-single-json",
                    &search,
                ],
                SEARCH_TIMEOUT,
                ErrorCode::UpstreamApi,
                "search",
            )
            .await?;
        parse_search_output(&output)
    }

    async fn resolve(&self, native: &NativeTrackId) -> Result<SourceResolvedTrack> {
        let id = native.as_str();
        let id = id.trim();
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(EasyMusicError::invalid(
                "YouTube track id is not a valid video ID",
            ));
        }
        let page_url = format!("{YOUTUBE_WATCH_URL}{id}");
        let output = self
            .run(
                &[
                    "--skip-download",
                    "--no-playlist",
                    "--format",
                    "bestaudio/best",
                    "--dump-single-json",
                    &page_url,
                ],
                RESOLVE_TIMEOUT,
                ErrorCode::AudioSource,
                "audio URL resolution",
            )
            .await?;
        parse_resolve_output(id, &output)
    }
}

#[derive(Debug, Deserialize)]
struct SearchPage {
    #[serde(default)]
    entries: Vec<Option<SearchEntry>>,
}

#[derive(Debug, Deserialize)]
struct SearchEntry {
    id: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    #[serde(default)]
    artists: Vec<String>,
    channel: Option<String>,
    uploader: Option<String>,
    thumbnail: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResolvedEntry {
    id: Option<String>,
    title: Option<String>,
    url: Option<String>,
    ext: Option<String>,
}

fn parse_search_output(output: &[u8]) -> Result<Vec<SourceTrack>> {
    let page: SearchPage = serde_json::from_slice(output).map_err(|error| {
        EasyMusicError::upstream(format!("yt-dlp returned invalid search JSON: {error}"))
    })?;
    let tracks = page
        .entries
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let id = non_empty(entry.id?)?;
            let title = non_empty(entry.title?)?;
            let artist = entry
                .artist
                .and_then(non_empty)
                .or_else(|| entry.artists.into_iter().find_map(non_empty))
                .or_else(|| entry.channel.and_then(non_empty))
                .or_else(|| entry.uploader.and_then(non_empty))
                .unwrap_or_default();
            Some(SourceTrack {
                id: NativeTrackId::new(id),
                title,
                artist,
                artwork_url: entry.thumbnail.and_then(non_empty),
            })
        })
        .collect::<Vec<_>>();
    Ok(tracks)
}

fn parse_resolve_output(requested_id: &str, output: &[u8]) -> Result<SourceResolvedTrack> {
    let entry: ResolvedEntry = serde_json::from_slice(output).map_err(|error| {
        EasyMusicError::source(format!("yt-dlp returned invalid audio JSON: {error}"))
    })?;
    let url = entry
        .url
        .and_then(non_empty)
        .ok_or_else(|| EasyMusicError::source("yt-dlp did not return a playable audio URL"))?;
    validate_http_url(&url)?;
    Ok(SourceResolvedTrack {
        id: NativeTrackId::new(
            entry
                .id
                .and_then(non_empty)
                .unwrap_or_else(|| requested_id.to_owned()),
        ),
        title: entry
            .title
            .and_then(non_empty)
            .unwrap_or_else(|| requested_id.to_owned()),
        url,
        extension: entry.ext.and_then(valid_extension),
    })
}

pub(crate) fn discover_adjacent_js_runtime(executable: &Path) -> Option<String> {
    let parent = executable
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())?;
    let deno = parent.join(if cfg!(windows) { "deno.exe" } else { "deno" });
    if deno.is_file() {
        return Some(format!("deno:{}", deno.display()));
    }
    let quickjs = parent.join(if cfg!(windows) { "qjs.exe" } else { "qjs" });
    quickjs
        .is_file()
        .then(|| format!("quickjs:{}", quickjs.display()))
}

fn start_error(executable: &Path, error: io::Error) -> EasyMusicError {
    if error.kind() == io::ErrorKind::NotFound {
        EasyMusicError::new(
            ErrorCode::DependencyMissing,
            format!(
                "yt-dlp executable not found at {}; bundle it next to easymusic or set EASYMUSIC_YT_DLP",
                executable.display()
            ),
        )
    } else {
        EasyMusicError::new(
            ErrorCode::DependencyMissing,
            format!(
                "failed to start yt-dlp at {}: {error}",
                executable.display()
            ),
        )
    }
}

fn concise_stderr(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let mut joined = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join(" | ");
    if joined.chars().count() > 600 {
        joined = joined.chars().take(597).collect::<String>() + "...";
    }
    joined
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn valid_extension(value: String) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    ((1..=10).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        .then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flat_youtube_search_results() {
        let output = r#"{
            "entries": [
                {
                    "id": "DYptgVvkVLQ",
                    "title": "周杰伦 Jay Chou【晴天 Sunny Day】",
                    "channel": "杰威尔音乐 JVR Music",
                    "thumbnail": "https://i.ytimg.com/vi/DYptgVvkVLQ/hqdefault.jpg"
                },
                {
                    "id": "second_id-1",
                    "title": "晴天",
                    "artist": "周杰伦",
                    "artists": ["ignored"],
                    "uploader": "ignored"
                },
                null,
                {"id": "missing-title"}
            ]
        }"#;

        let result = parse_search_output(output.as_bytes()).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id.as_str(), "DYptgVvkVLQ");
        assert_eq!(result[0].artist, "杰威尔音乐 JVR Music");
        assert_eq!(result[1].artist, "周杰伦");
    }

    #[test]
    fn parses_selected_audio_format_url() {
        let output = r#"{
            "id": "DYptgVvkVLQ",
            "title": "周杰伦 Jay Chou【晴天 Sunny Day】",
            "url": "https://rr1---sn.example.googlevideo.com/videoplayback?expire=1",
            "ext": "webm"
        }"#;

        let resolved = parse_resolve_output("DYptgVvkVLQ", output.as_bytes()).unwrap();
        assert_eq!(resolved.id.as_str(), "DYptgVvkVLQ");
        assert!(resolved.url.starts_with("https://"));
        assert_eq!(resolved.extension.as_deref(), Some("webm"));
    }

    #[test]
    fn rejects_missing_or_non_http_audio_urls() {
        let missing = parse_resolve_output("id", br#"{"id":"id"}"#).unwrap_err();
        assert!(matches!(missing.code, ErrorCode::AudioSource));

        let non_http =
            parse_resolve_output("id", br#"{"id":"id","url":"file:///tmp/audio"}"#).unwrap_err();
        assert!(matches!(non_http.code, ErrorCode::AudioSource));
    }

    #[test]
    fn trims_and_limits_subprocess_errors() {
        let stderr = format!("\n  first line  \nsecond line\n{}", "x".repeat(1000));
        let message = concise_stderr(stderr.as_bytes());
        assert!(message.starts_with("first line | second line | "));
        assert!(message.ends_with("..."));
        assert!(message.chars().count() <= 600);
    }
}
