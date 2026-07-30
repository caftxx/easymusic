use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use clap::{ArgGroup, Args, Parser, Subcommand};
use easy_music::mcp_server::{McpServerConfig, serve_mcp};
use easy_music::model::PlaybackHint;
use easy_music::{
    AudioFormat, AudioProfile, DEFAULT_PROVIDER_NAME, DownloadConfig, DownloadedFile,
    EasyMusicError, ErrorCode, Framing, MusicClient, PreparedTrack, ProviderInfo, Result,
    StreamConfig, Track, download_audio, rank_tracks, select_track, stream_audio,
    supported_providers,
};
use serde::Serialize;
use serde_json::json;
use tokio::process::Command;

#[derive(Debug, Parser)]
#[command(name = "easy-music", version, about)]
struct Cli {
    /// Music provider name. Run `easy-music provider --list` to list values.
    #[arg(
        long,
        global = true,
        env = "EASY_MUSIC_PROVIDER",
        default_value = DEFAULT_PROVIDER_NAME,
        value_name = "NAME"
    )]
    provider: String,

    /// Pretty-print metadata JSON. Never applies to binary stream output.
    #[arg(long, global = true)]
    pretty: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Inspect the built-in music providers.
    Provider(ProviderArgs),
    /// Search by song title or artist keyword.
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
    /// Check ffmpeg and optionally the upstream API.
    Doctor(DoctorArgs),
}

#[derive(Debug, Args)]
struct ProviderArgs {
    /// List all supported provider names.
    #[arg(long, required = true)]
    list: bool,
}

#[derive(Debug, Args)]
struct SearchArgs {
    /// Song title or artist keyword passed to the upstream search API.
    #[arg(long)]
    keyword: String,

    /// Optional artist hint used for local ranking; it is not appended to keyword.
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

    #[arg(long, env = "EASY_MUSIC_FFMPEG", default_value = "ffmpeg")]
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
    #[arg(long, env = "EASY_MUSIC_STREAM_BIND", default_value = "127.0.0.1:0")]
    stream_bind: SocketAddr,

    /// Seconds before an unused one-time stream URL expires.
    #[arg(long, default_value_t = 60)]
    stream_ttl_seconds: u64,

    #[arg(long, env = "EASY_MUSIC_FFMPEG", default_value = "ffmpeg")]
    ffmpeg: PathBuf,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    /// Also issue a small search request against the configured API.
    #[arg(long)]
    online: bool,

    #[arg(long, env = "EASY_MUSIC_FFMPEG", default_value = "ffmpeg")]
    ffmpeg: PathBuf,
}

#[derive(Debug, Serialize)]
struct DoctorResult {
    ok: bool,
    ffmpeg: DependencyStatus,
    api: Option<ApiStatus>,
}

#[derive(Debug, Serialize)]
struct DependencyStatus {
    ok: bool,
    command: String,
    version: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ApiStatus {
    ok: bool,
    provider: String,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ProviderListResult {
    ok: bool,
    default: &'static str,
    providers: &'static [ProviderInfo],
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
    if let Commands::Provider(args) = &cli.command {
        if args.list {
            let result = ProviderListResult {
                ok: true,
                default: DEFAULT_PROVIDER_NAME,
                providers: supported_providers(),
            };
            return Ok(Some(
                serde_json::to_value(result).expect("provider list is serializable"),
            ));
        }
        return Err(EasyMusicError::invalid("provider command requires --list"));
    }

    let client = MusicClient::new(&cli.provider)?;
    match cli.command {
        Commands::Provider(_) => unreachable!("provider command handled before client creation"),
        Commands::Search(args) => {
            let mut result = client.search(&args.keyword).await?;
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
            let ffmpeg = check_ffmpeg(&args.ffmpeg).await;
            let api = if args.online {
                Some(match client.search("晴天").await {
                    Ok(_) => ApiStatus {
                        ok: true,
                        provider: cli.provider,
                        error: None,
                    },
                    Err(error) => ApiStatus {
                        ok: false,
                        provider: cli.provider,
                        error: Some(error.message),
                    },
                })
            } else {
                None
            };
            let ok = ffmpeg.ok && api.as_ref().is_none_or(|status| status.ok);
            Ok(Some(
                serde_json::to_value(DoctorResult { ok, ffmpeg, api }).expect("serializable"),
            ))
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

    let (url, track, confidence, needs_confirmation) = if let Some(id) = &args.id {
        let resolved = client.resolve(id).await?;
        (
            resolved.url,
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
        (url.clone(), None, None, None)
    } else {
        let selection =
            select_from_query(client, args.title.as_deref(), args.artist.as_deref()).await?;
        let track = selection.selected.track;
        let resolved = client.resolve(&track.id).await?;
        (
            resolved.url,
            Some(track),
            Some(selection.confidence),
            Some(selection.needs_confirmation),
        )
    };

    let output = download_output_path(args.output, args.output_dir, track.as_ref(), &url)?;
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
) -> Result<easy_music::SelectResult> {
    let keyword = title.or(artist).ok_or_else(|| {
        EasyMusicError::new(ErrorCode::InvalidArguments, "title or artist is required")
    })?;
    let search = client.search(keyword).await?;
    select_track(&search.tracks, title, artist)
}

fn download_output_path(
    output: Option<PathBuf>,
    output_dir: Option<PathBuf>,
    track: Option<&Track>,
    url: &str,
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
        format!(
            "{}.{}",
            sanitize_filename_component(&stem),
            extension_from_url(url)
        )
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

async fn check_ffmpeg(command: &PathBuf) -> DependencyStatus {
    match Command::new(command)
        .arg("-version")
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
    fn cli_selects_providers_by_name_and_defaults_to_qianqian() {
        let default_cli =
            Cli::try_parse_from(["easy-music", "search", "--keyword", "天地龙鳞"]).unwrap();
        assert_eq!(default_cli.provider, "qianqian");

        let selected_cli = Cli::try_parse_from([
            "easy-music",
            "--provider",
            "yymp3",
            "search",
            "--keyword",
            "刚好遇见你",
        ])
        .unwrap();
        assert_eq!(selected_cli.provider, "yymp3");
    }

    #[tokio::test]
    async fn provider_list_command_returns_the_registry() {
        let cli = Cli::try_parse_from(["easy-music", "provider", "--list"]).unwrap();
        let output = run(cli).await.unwrap().unwrap();

        assert_eq!(output["default"], "qianqian");
        assert_eq!(output["providers"][0]["name"], "qianqian");
        assert_eq!(output["providers"][1]["name"], "yymp3");
        assert_eq!(output["providers"][2]["name"], "buguyy");
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
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("music/周杰伦 - 晴天_ Live_.mp3"));
    }

    #[test]
    fn protects_windows_reserved_file_names() {
        assert_eq!(sanitize_filename_component("CON"), "_CON");
        assert_eq!(sanitize_filename_component("LPT1.txt"), "_LPT1.txt");
        assert_eq!(sanitize_filename_component("..."), "download");
    }
}
