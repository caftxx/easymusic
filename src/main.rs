use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use clap::{ArgGroup, Args, Parser, Subcommand};
use easymusic::mcp_server::{McpServerConfig, serve_mcp};
use easymusic::model::PlaybackHint;
use easymusic::{
    AudioFormat, AudioProfile, DownloadConfig, DownloadedFile, EasyMusicError, ErrorCode, Framing,
    MusicClient, PreparedTrack, Result, StreamConfig, Track, YtDlpConfig, default_search_limit,
    download_audio, rank_tracks, select_track, stream_audio,
};
use serde::Serialize;
use serde_json::json;
use tokio::process::Command;

#[derive(Debug, Parser)]
#[command(name = "easymusic", version, about)]
struct Cli {
    /// yt-dlp executable path. A bundled executable next to easymusic is preferred by default.
    #[arg(long, global = true, env = "EASYMUSIC_YT_DLP", value_name = "PATH")]
    yt_dlp: Option<PathBuf>,

    /// yt-dlp JavaScript runtime spec, for example quickjs:/app/qjs or deno:/app/deno.
    #[arg(
        long,
        global = true,
        env = "EASYMUSIC_JS_RUNTIME",
        value_name = "RUNTIME[:PATH]"
    )]
    js_runtime: Option<String>,

    /// Netscape-format cookies file for YouTube requests; no server browser is required.
    #[arg(
        long,
        global = true,
        env = "EASYMUSIC_YT_DLP_COOKIES",
        value_name = "PATH"
    )]
    cookies: Option<PathBuf>,

    /// Pretty-print metadata JSON. Never applies to binary stream output.
    #[arg(long, global = true)]
    pretty: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Search YouTube through yt-dlp by song title or artist keyword.
    Search(SearchArgs),
    /// Rank search results and select the best candidate.
    Select(SelectArgs),
    /// Resolve a search-result ID to a temporary playable URL.
    Resolve(ResolveArgs),
    /// Select and resolve a track in one Agent-friendly operation.
    Prepare(PrepareArgs),
    /// Stream a remote track through ffmpeg as PCM or Opus.
    Stream(StreamArgs),
    /// Download the original remote audio file without transcoding.
    Download(DownloadArgs),
    /// Serve Agent tools over MCP stdio with loopback streaming URLs.
    Mcp(McpArgs),
    /// Check yt-dlp, ffmpeg, and optionally online search.
    Doctor(DoctorArgs),
}

#[derive(Debug, Args)]
struct SearchArgs {
    /// Song title or free-form music search keyword.
    #[arg(long)]
    keyword: String,

    /// Optional artist hint appended to the yt-dlp query and used for local ranking.
    #[arg(long)]
    artist: Option<String>,

    #[arg(long, default_value_t = 10)]
    limit: usize,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("query")
        .required(true)
        .multiple(true)
        .args(["title", "artist"])
))]
struct SelectArgs {
    #[arg(long)]
    title: Option<String>,
    #[arg(long)]
    artist: Option<String>,
}

#[derive(Debug, Args)]
struct ResolveArgs {
    #[arg(long)]
    id: String,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("query")
        .required(true)
        .multiple(true)
        .args(["title", "artist"])
))]
struct PrepareArgs {
    #[arg(long)]
    title: Option<String>,
    #[arg(long)]
    artist: Option<String>,
    #[arg(long, value_enum, default_value = "web-voice")]
    target: AudioProfile,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("source")
        .required(true)
        .multiple(false)
        .args(["id", "url"])
))]
struct StreamArgs {
    /// Resolve this track ID immediately before streaming.
    #[arg(long)]
    id: Option<String>,

    /// Stream an already resolved HTTP(S) URL.
    #[arg(long)]
    url: Option<String>,

    /// Apply terminal defaults. Explicit format/rate/channel flags override it.
    #[arg(long, value_enum)]
    profile: Option<AudioProfile>,

    #[arg(long, value_enum)]
    format: Option<AudioFormat>,

    #[arg(long)]
    sample_rate: Option<u32>,

    #[arg(long)]
    channels: Option<u8>,

    /// Opus bitrate in bits per second.
    #[arg(long)]
    bitrate: Option<u32>,

    /// Opus frame duration in milliseconds.
    #[arg(long)]
    frame_ms: Option<f32>,

    #[arg(long, value_enum, default_value = "len32be")]
    framing: Framing,

    /// Destination path, or '-' for binary stdout.
    #[arg(long, default_value = "-")]
    output: String,

    #[arg(long, env = "EASYMUSIC_FFMPEG", default_value = "ffmpeg")]
    ffmpeg: PathBuf,

    /// Permit localhost, private, and link-local source URLs.
    #[arg(long)]
    allow_private_network: bool,

    #[arg(long)]
    start_seconds: Option<f64>,

    #[arg(long)]
    duration_seconds: Option<f64>,

    /// Emit stream lifecycle JSON lines on stderr.
    #[arg(long)]
    events_json: bool,
}

#[derive(Debug, Args)]
struct DownloadArgs {
    /// Resolve this track ID before downloading.
    #[arg(long)]
    id: Option<String>,

    /// Select a track by song title before downloading.
    #[arg(long)]
    title: Option<String>,

    /// Artist hint used with --title, or as the search keyword by itself.
    #[arg(long)]
    artist: Option<String>,

    /// Download an already resolved HTTP(S) URL.
    #[arg(long)]
    url: Option<String>,

    /// Exact destination file path.
    #[arg(long, conflicts_with = "output_dir")]
    output: Option<PathBuf>,

    /// Directory for an automatically named file. Defaults to the current directory.
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Replace an existing destination file.
    #[arg(long)]
    force: bool,

    /// Permit localhost, private, and link-local source URLs.
    #[arg(long)]
    allow_private_network: bool,
}

#[derive(Debug, Args)]
struct McpArgs {
    /// Loopback address for one-time audio stream URLs.
    #[arg(long, env = "EASYMUSIC_STREAM_BIND", default_value = "127.0.0.1:0")]
    stream_bind: SocketAddr,

    /// Seconds before an unused one-time stream URL expires.
    #[arg(long, default_value_t = 60)]
    stream_ttl_seconds: u64,

    #[arg(long, env = "EASYMUSIC_FFMPEG", default_value = "ffmpeg")]
    ffmpeg: PathBuf,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    /// Also issue a small YouTube search through yt-dlp.
    #[arg(long)]
    online: bool,

    #[arg(long, env = "EASYMUSIC_FFMPEG", default_value = "ffmpeg")]
    ffmpeg: PathBuf,
}

#[derive(Debug, Serialize)]
struct DoctorResult {
    ok: bool,
    yt_dlp: DependencyStatus,
    ffmpeg: DependencyStatus,
    js_runtime: Option<String>,
    search: Option<OnlineSearchStatus>,
}

#[derive(Debug, Serialize)]
struct DependencyStatus {
    ok: bool,
    command: String,
    version: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct OnlineSearchStatus {
    ok: bool,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct DownloadCommandResult {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    track: Option<Track>,
    #[serde(skip_serializing_if = "Option::is_none")]
    confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    needs_confirmation: Option<bool>,
    file: DownloadedFile,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let pretty = cli.pretty;
    match run(cli).await {
        Ok(output) => {
            if let Some(value) = output
                && let Err(error) = print_json(&value, pretty)
            {
                return print_error(error);
            }
            ExitCode::SUCCESS
        }
        Err(error) => print_error(error),
    }
}

async fn run(cli: Cli) -> Result<Option<serde_json::Value>> {
    let mut config = YtDlpConfig::default();
    if let Some(executable) = cli.yt_dlp {
        config.executable = executable;
    }
    if let Some(js_runtime) = cli.js_runtime {
        config.js_runtime = Some(js_runtime);
    }
    config.cookies = cli.cookies;
    let client = MusicClient::with_config(config);
    match cli.command {
        Commands::Search(args) => {
            let query = music_query(Some(&args.keyword), args.artist.as_deref())?;
            let mut result = client.search(&query, args.limit).await?;
            let ranked = rank_tracks(&result.tracks, Some(&args.keyword), args.artist.as_deref());
            result.tracks = ranked
                .into_iter()
                .take(args.limit)
                .map(|item| item.track)
                .collect();
            result.count = result.tracks.len();
            Ok(Some(serde_json::to_value(result).expect("serializable")))
        }
        Commands::Select(args) => {
            let result =
                select_from_query(&client, args.title.as_deref(), args.artist.as_deref()).await?;
            Ok(Some(serde_json::to_value(result).expect("serializable")))
        }
        Commands::Resolve(args) => {
            let result = client.resolve(&args.id).await?;
            Ok(Some(json!({"ok": true, "track": result})))
        }
        Commands::Prepare(args) => {
            let selection =
                select_from_query(&client, args.title.as_deref(), args.artist.as_deref()).await?;
            let resolved = client.resolve(&selection.selected.track.id).await?;
            let playback = match args.target {
                AudioProfile::WebVoice => PlaybackHint {
                    mode: "remote_url".to_owned(),
                    profile: None,
                },
                profile => PlaybackHint {
                    mode: "stream".to_owned(),
                    profile: Some(profile),
                },
            };
            let result = PreparedTrack {
                ok: true,
                track: selection.selected.track,
                url: resolved.url,
                confidence: selection.confidence,
                needs_confirmation: selection.needs_confirmation,
                playback,
                alternatives: selection.alternatives,
            };
            Ok(Some(serde_json::to_value(result).expect("serializable")))
        }
        Commands::Stream(args) => {
            let url = match (&args.id, &args.url) {
                (Some(id), None) => client.resolve(id).await?.url,
                (None, Some(url)) => url.clone(),
                _ => {
                    return Err(EasyMusicError::invalid(
                        "provide exactly one of --id or --url",
                    ));
                }
            };
            let config = stream_config(url, args);
            stream_audio(&config).await?;
            Ok(None)
        }
        Commands::Download(args) => {
            let result = run_download(&client, args).await?;
            Ok(Some(serde_json::to_value(result).expect("serializable")))
        }
        Commands::Mcp(args) => {
            serve_mcp(
                client,
                McpServerConfig {
                    stream_bind: args.stream_bind,
                    stream_ttl: Duration::from_secs(args.stream_ttl_seconds),
                    ffmpeg: args.ffmpeg,
                },
            )
            .await?;
            Ok(None)
        }
        Commands::Doctor(args) => {
            let yt_dlp = check_dependency(client.yt_dlp_executable(), &["--version"]).await;
            let ffmpeg = check_ffmpeg(&args.ffmpeg).await;
            let search = if args.online {
                Some(match client.search("晴天 周杰伦", 1).await {
                    Ok(result) if !result.tracks.is_empty() => OnlineSearchStatus {
                        ok: true,
                        error: None,
                    },
                    Ok(_) => OnlineSearchStatus {
                        ok: false,
                        error: Some("yt-dlp search returned no tracks".to_owned()),
                    },
                    Err(error) => OnlineSearchStatus {
                        ok: false,
                        error: Some(error.message),
                    },
                })
            } else {
                None
            };
            let ok = yt_dlp.ok && ffmpeg.ok && search.as_ref().is_none_or(|status| status.ok);
            let result = DoctorResult {
                ok,
                yt_dlp,
                ffmpeg,
                js_runtime: client.js_runtime().map(str::to_owned),
                search,
            };
            Ok(Some(serde_json::to_value(result).expect("serializable")))
        }
    }
}

async fn run_download(client: &MusicClient, args: DownloadArgs) -> Result<DownloadCommandResult> {
    let has_query = args.title.is_some() || args.artist.is_some();
    let source_count =
        usize::from(args.id.is_some()) + usize::from(args.url.is_some()) + usize::from(has_query);
    if source_count != 1 {
        return Err(EasyMusicError::invalid(
            "provide exactly one source: --id, --url, or --title/--artist",
        ));
    }

    let (url, extension, track, confidence, needs_confirmation) = if let Some(id) = &args.id {
        let resolved = client.resolve(id).await?;
        (
            resolved.url,
            resolved.extension,
            Some(Track {
                id: resolved.id,
                title: resolved.title,
                artist: String::new(),
                artwork_url: None,
            }),
            None,
            None,
        )
    } else if let Some(url) = &args.url {
        (url.clone(), None, None, None, None)
    } else {
        let selection =
            select_from_query(client, args.title.as_deref(), args.artist.as_deref()).await?;
        let track = selection.selected.track;
        let resolved = client.resolve(&track.id).await?;
        (
            resolved.url,
            resolved.extension,
            Some(track),
            Some(selection.confidence),
            Some(selection.needs_confirmation),
        )
    };

    let output = download_output_path(
        args.output,
        args.output_dir,
        track.as_ref(),
        &url,
        extension.as_deref(),
    )?;
    let file = download_audio(&DownloadConfig {
        url,
        output,
        overwrite: args.force,
        allow_private_network: args.allow_private_network,
    })
    .await?;

    Ok(DownloadCommandResult {
        ok: true,
        track,
        confidence,
        needs_confirmation,
        file,
    })
}

async fn select_from_query(
    client: &MusicClient,
    title: Option<&str>,
    artist: Option<&str>,
) -> Result<easymusic::SelectResult> {
    let keyword = music_query(title, artist)?;
    let search = client.search(&keyword, default_search_limit()).await?;
    select_track(&search.tracks, title, artist)
}

fn music_query(title: Option<&str>, artist: Option<&str>) -> Result<String> {
    let parts = [title, artist]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return Err(EasyMusicError::new(
            ErrorCode::InvalidArguments,
            "title or artist is required",
        ));
    }
    Ok(parts.join(" "))
}

fn download_output_path(
    output: Option<PathBuf>,
    output_dir: Option<PathBuf>,
    track: Option<&Track>,
    url: &str,
    preferred_extension: Option<&str>,
) -> Result<PathBuf> {
    if let Some(output) = output {
        return Ok(output);
    }

    let file_name = if let Some(track) = track {
        let stem = if track.artist.trim().is_empty() {
            track.title.clone()
        } else {
            format!("{} - {}", track.artist, track.title)
        };
        let extension = preferred_extension
            .map(str::to_owned)
            .unwrap_or_else(|| extension_from_url(url));
        format!("{}.{}", sanitize_filename_component(&stem), extension)
    } else {
        file_name_from_url(url)
    };
    Ok(output_dir
        .unwrap_or_else(|| PathBuf::from("."))
        .join(file_name))
}

fn file_name_from_url(value: &str) -> String {
    let parsed = url::Url::parse(value).ok();
    let segment = parsed
        .as_ref()
        .and_then(|url| url.path_segments())
        .and_then(|mut segments| segments.next_back())
        .filter(|segment| !segment.is_empty());
    match segment {
        Some(segment) => {
            let sanitized = sanitize_filename_component(segment);
            if sanitized.contains('.') {
                sanitized
            } else {
                format!("{sanitized}.{}", extension_from_url(value))
            }
        }
        None => format!("download.{}", extension_from_url(value)),
    }
}

fn extension_from_url(value: &str) -> String {
    let extension = url::Url::parse(value)
        .ok()
        .and_then(|url| {
            url.path_segments()
                .and_then(|mut segments| segments.next_back())
                .and_then(|segment| {
                    segment
                        .rsplit_once('.')
                        .map(|(_, extension)| extension.to_owned())
                })
        })
        .filter(|extension| {
            (1..=10).contains(&extension.len())
                && extension
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        });
    extension
        .map(|extension| extension.to_ascii_lowercase())
        .unwrap_or_else(|| "audio".to_owned())
}

fn sanitize_filename_component(value: &str) -> String {
    let mut sanitized: String = value
        .chars()
        .map(|character| {
            if character.is_control() || r#"<>:"/\|?*"#.contains(character) {
                '_'
            } else {
                character
            }
        })
        .take(120)
        .collect();
    sanitized = sanitized.trim().trim_end_matches(['.', ' ']).to_owned();
    if sanitized.is_empty() {
        return "download".to_owned();
    }

    let base = sanitized
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || base
            .strip_prefix("COM")
            .or_else(|| base.strip_prefix("LPT"))
            .is_some_and(|number| {
                number.len() == 1 && number.as_bytes()[0].is_ascii_digit() && number != "0"
            });
    if reserved {
        sanitized.insert(0, '_');
    }
    sanitized
}

fn stream_config(url: String, args: StreamArgs) -> StreamConfig {
    let profile = args.profile;
    let (default_format, default_rate, default_channels, default_bitrate, default_frame_ms) =
        match profile {
            Some(AudioProfile::Xiaozhi) => (AudioFormat::OpusPackets, 24_000, 1, 64_000, 60.0),
            Some(AudioProfile::WebVoice) => (AudioFormat::OpusOgg, 48_000, 2, 96_000, 20.0),
            Some(AudioProfile::Pcm16k) => (AudioFormat::PcmS16le, 16_000, 1, 64_000, 20.0),
            Some(AudioProfile::Pcm24k) => (AudioFormat::PcmS16le, 24_000, 1, 64_000, 20.0),
            None => (AudioFormat::PcmS16le, 24_000, 1, 64_000, 20.0),
        };
    StreamConfig {
        url,
        format: args.format.unwrap_or(default_format),
        sample_rate: args.sample_rate.unwrap_or(default_rate),
        channels: args.channels.unwrap_or(default_channels),
        bitrate: args.bitrate.unwrap_or(default_bitrate),
        frame_ms: args.frame_ms.unwrap_or(default_frame_ms),
        framing: args.framing,
        output: (args.output != "-").then(|| PathBuf::from(args.output)),
        ffmpeg: args.ffmpeg,
        allow_private_network: args.allow_private_network,
        start_seconds: args.start_seconds,
        duration_seconds: args.duration_seconds,
        events_json: args.events_json,
    }
}

async fn check_ffmpeg(command: &Path) -> DependencyStatus {
    check_dependency(command, &["-version"]).await
}

async fn check_dependency(command: &Path, args: &[&str]) -> DependencyStatus {
    match Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
    {
        Ok(output) if output.status.success() => {
            let first_line = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .map(str::to_owned);
            DependencyStatus {
                ok: true,
                command: command.display().to_string(),
                version: first_line,
                error: None,
            }
        }
        Ok(output) => DependencyStatus {
            ok: false,
            command: command.display().to_string(),
            version: None,
            error: Some(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
        },
        Err(error) => DependencyStatus {
            ok: false,
            command: command.display().to_string(),
            version: None,
            error: Some(error.to_string()),
        },
    }
}

fn print_json<T: Serialize>(value: &T, pretty: bool) -> Result<()> {
    let text = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .map_err(|error| EasyMusicError::new(ErrorCode::Io, error.to_string()))?;
    println!("{text}");
    Ok(())
}

fn print_error(error: EasyMusicError) -> ExitCode {
    eprintln!(
        "{}",
        json!({
            "ok": false,
            "error": {
                "code": error.code,
                "message": error.message,
            }
        })
    );
    error.code.as_exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_accepts_explicit_yt_dlp_and_js_runtime_paths() {
        let cli = Cli::try_parse_from([
            "easymusic",
            "--yt-dlp",
            "/opt/easymusic/yt-dlp",
            "--js-runtime",
            "quickjs:/opt/easymusic/qjs",
            "--cookies",
            "/run/secrets/youtube-cookies.txt",
            "search",
            "--keyword",
            "天地龙鳞",
        ])
        .unwrap();

        assert_eq!(cli.yt_dlp, Some(PathBuf::from("/opt/easymusic/yt-dlp")));
        assert_eq!(
            cli.js_runtime.as_deref(),
            Some("quickjs:/opt/easymusic/qjs")
        );
        assert_eq!(
            cli.cookies,
            Some(PathBuf::from("/run/secrets/youtube-cookies.txt"))
        );
    }

    #[test]
    fn title_and_artist_are_combined_for_yt_dlp_search() {
        assert_eq!(
            music_query(Some(" 晴天 "), Some(" 周杰伦 ")).unwrap(),
            "晴天 周杰伦"
        );
        assert_eq!(music_query(None, Some("周杰伦")).unwrap(), "周杰伦");
        assert!(music_query(Some(" "), None).is_err());
    }

    #[test]
    fn creates_safe_track_file_names() {
        let track = Track {
            id: "id".to_owned(),
            title: "晴天: Live?".to_owned(),
            artist: "周杰伦".to_owned(),
            artwork_url: None,
        };
        let path = download_output_path(
            None,
            Some(PathBuf::from("music")),
            Some(&track),
            "https://example.com/song.MP3?token=temporary",
            None,
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("music/周杰伦 - 晴天_ Live_.mp3"));

        let yt_dlp_path = download_output_path(
            None,
            Some(PathBuf::from("music")),
            Some(&track),
            "https://example.googlevideo.com/videoplayback?token=temporary",
            Some("webm"),
        )
        .unwrap();
        assert_eq!(
            yt_dlp_path,
            PathBuf::from("music/周杰伦 - 晴天_ Live_.webm")
        );
    }

    #[test]
    fn protects_windows_reserved_file_names() {
        assert_eq!(sanitize_filename_component("CON"), "_CON");
        assert_eq!(sanitize_filename_component("LPT1.txt"), "_LPT1.txt");
        assert_eq!(sanitize_filename_component("..."), "download");
    }
}
