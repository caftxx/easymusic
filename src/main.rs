use std::path::PathBuf;
use std::process::{ExitCode, Stdio};

use clap::{ArgGroup, Args, Parser, Subcommand};
use easy_music::api::DEFAULT_API_BASE_URL;
use easy_music::model::PlaybackHint;
use easy_music::{
    AudioFormat, AudioProfile, EasyMusicError, ErrorCode, Framing, MusicClient, PreparedTrack,
    Result, StreamConfig, rank_tracks, select_track, stream_audio,
};
use serde::Serialize;
use serde_json::json;
use tokio::process::Command;

#[derive(Debug, Parser)]
#[command(name = "easy-music", version, about)]
struct Cli {
    /// Music API origin. Useful for compatible mirrors and deterministic tests.
    #[arg(
        long,
        global = true,
        env = "EASY_MUSIC_API_BASE_URL",
        default_value = DEFAULT_API_BASE_URL
    )]
    api_base_url: String,

    /// Pretty-print metadata JSON. Never applies to binary stream output.
    #[arg(long, global = true)]
    pretty: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
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
    /// Check ffmpeg and optionally the upstream API.
    Doctor(DoctorArgs),
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
    base_url: String,
    error: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let pretty = cli.pretty;
    match run(cli).await {
        Ok(output) => {
            if let Some(value) = output {
                if let Err(error) = print_json(&value, pretty) {
                    return print_error(error);
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => print_error(error),
    }
}

async fn run(cli: Cli) -> Result<Option<serde_json::Value>> {
    let client = MusicClient::new(&cli.api_base_url)?;
    match cli.command {
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
        Commands::Doctor(args) => {
            let ffmpeg = check_ffmpeg(&args.ffmpeg).await;
            let api = if args.online {
                Some(match client.search("晴天").await {
                    Ok(_) => ApiStatus {
                        ok: true,
                        base_url: cli.api_base_url,
                        error: None,
                    },
                    Err(error) => ApiStatus {
                        ok: false,
                        base_url: cli.api_base_url,
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
