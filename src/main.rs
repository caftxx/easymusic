use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use easymusic::mcp_server::{McpServerConfig, serve_mcp};
use easymusic::{
    ClientConfig, DownloadConfig, DownloadedFile, EasyMusicError, ErrorCode, MusicClient,
    MusicQuery, Result, SearchStrategy, Track, YtDlpConfig, download_audio,
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

    /// Music source used for search (youtube, netease, kuwo, or a plugin
    /// name). Without it, sources are tried in priority order until one works.
    #[arg(long, global = true, env = "EASYMUSIC_SOURCE", value_name = "NAME")]
    source: Option<String>,

    /// Comma-separated list of enabled music sources in priority order.
    /// Defaults to every built-in and discovered plugin source.
    #[arg(
        long,
        global = true,
        env = "EASYMUSIC_SOURCES",
        value_name = "NAMES",
        value_delimiter = ','
    )]
    sources: Vec<String>,

    /// Directory scanned for external `easymusic-source-*` plugin executables.
    #[arg(
        long,
        global = true,
        env = "EASYMUSIC_PLUGINS_DIR",
        value_name = "PATH"
    )]
    plugins_dir: Option<PathBuf>,

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

    /// Pretty-print JSON output.
    #[arg(long, global = true)]
    pretty: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Search music sources by song title or artist keyword.
    Search(SearchArgs),
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

    /// Optional artist hint appended to the search query and used for local ranking.
    #[arg(long)]
    artist: Option<String>,

    /// Search every enabled source and merge the ranked results.
    #[arg(long)]
    all_sources: bool,

    #[arg(long, default_value_t = 10)]
    limit: usize,
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
    /// Also issue a small search through every enabled music source.
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
    sources: Vec<String>,
    search: Option<Vec<OnlineSearchStatus>>,
}

#[derive(Debug, Serialize)]
struct DependencyStatus {
    ok: bool,
    /// Whether the current configuration actually needs this tool. yt-dlp is
    /// only required while the `youtube` source is enabled.
    required: bool,
    command: String,
    version: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct OnlineSearchStatus {
    source: String,
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
    let client = build_client(&cli)?;
    let configured_yt_dlp = yt_dlp_config(&cli);
    match cli.command {
        Commands::Search(args) => {
            let query = MusicQuery::new(Some(&args.keyword), args.artist.as_deref())?;
            let strategy = if args.all_sources {
                SearchStrategy::Merge(cli.source.iter().cloned().collect())
            } else {
                SearchStrategy::from_source(cli.source.as_deref())
            };
            let result = client.search_query(&query, &strategy, args.limit).await?;
            Ok(Some(serde_json::to_value(result).expect("serializable")))
        }
        Commands::Download(args) => {
            let result = run_download(&client, cli.source.as_deref(), args).await?;
            Ok(Some(serde_json::to_value(result).expect("serializable")))
        }
        Commands::Mcp(args) => {
            serve_mcp(client, mcp_config(args, cli.source.clone())).await?;
            Ok(None)
        }
        Commands::Doctor(args) => {
            let result = run_doctor(&client, &args, &configured_yt_dlp).await?;
            Ok(Some(serde_json::to_value(result).expect("serializable")))
        }
    }
}

fn mcp_config(args: McpArgs, default_source: Option<String>) -> McpServerConfig {
    McpServerConfig {
        stream_bind: args.stream_bind,
        stream_ttl: Duration::from_secs(args.stream_ttl_seconds),
        ffmpeg: args.ffmpeg,
        default_source,
    }
}

fn yt_dlp_config(cli: &Cli) -> YtDlpConfig {
    let mut yt_dlp = YtDlpConfig::default();
    if let Some(executable) = &cli.yt_dlp {
        yt_dlp.executable = executable.clone();
    }
    if let Some(js_runtime) = &cli.js_runtime {
        yt_dlp.js_runtime = Some(js_runtime.clone());
    }
    yt_dlp.cookies = cli.cookies.clone();
    yt_dlp
}

fn build_client(cli: &Cli) -> Result<MusicClient> {
    let yt_dlp = yt_dlp_config(cli);
    let sources = parse_sources_list(&cli.sources)?;
    let plugin_dirs = cli
        .plugins_dir
        .clone()
        .map(|dir| vec![dir])
        .unwrap_or_default();
    let client = MusicClient::try_with_config(ClientConfig {
        yt_dlp,
        sources,
        plugin_dirs,
    })?;
    if let Some(source) = cli.source.as_deref()
        && !client.registry().contains(source)
    {
        return Err(EasyMusicError::invalid(format!(
            "unknown music source {source:?}; available: {}",
            client.sources().join(", ")
        )));
    }
    Ok(client)
}

fn parse_sources_list(raw: &[String]) -> Result<Option<Vec<String>>> {
    if raw.is_empty() {
        return Ok(None);
    }
    let names = raw
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if names.is_empty() {
        return Err(EasyMusicError::invalid(
            "--sources must not be a list of empty names",
        ));
    }
    Ok(Some(names))
}

async fn run_doctor(
    client: &MusicClient,
    args: &DoctorArgs,
    configured: &YtDlpConfig,
) -> Result<DoctorResult> {
    let diagnostics = client.source_diagnostics("youtube");
    let executable = diagnostics
        .and_then(|d| d.executable)
        .unwrap_or(&configured.executable);
    let mut yt_dlp = check_dependency(executable, &["--version"]).await;
    // Only the YouTube source shells out to yt-dlp, so a netease/kuwo/plugin
    // only configuration must not fail on its absence.
    yt_dlp.required = client.sources().contains(&"youtube");
    let ffmpeg = check_ffmpeg(&args.ffmpeg).await;
    let search = if args.online {
        let mut statuses = Vec::new();
        for name in client.sources() {
            statuses.push(
                match client.search_from(Some(name), "晴天 周杰伦", 1).await {
                    Ok(result) if !result.tracks.is_empty() => OnlineSearchStatus {
                        source: name.to_owned(),
                        ok: true,
                        error: None,
                    },
                    Ok(_) => OnlineSearchStatus {
                        source: name.to_owned(),
                        ok: false,
                        error: Some("search returned no tracks".to_owned()),
                    },
                    Err(error) => OnlineSearchStatus {
                        source: name.to_owned(),
                        ok: false,
                        error: Some(error.message),
                    },
                },
            );
        }
        Some(statuses)
    } else {
        None
    };
    let ok = doctor_is_ok(&yt_dlp, &ffmpeg, search.as_ref());
    Ok(DoctorResult {
        ok,
        yt_dlp,
        ffmpeg,
        js_runtime: diagnostics
            .and_then(|d| d.js_runtime)
            .or(configured.js_runtime.as_deref())
            .map(str::to_owned),
        sources: client.sources().into_iter().map(str::to_owned).collect(),
        search,
    })
}

/// A missing dependency only fails the doctor run when the active
/// configuration actually needs it, and `--online` passes as soon as any one
/// enabled source answered.
fn doctor_is_ok(
    yt_dlp: &DependencyStatus,
    ffmpeg: &DependencyStatus,
    search: Option<&Vec<OnlineSearchStatus>>,
) -> bool {
    [
        yt_dlp.required.then_some(yt_dlp.ok),
        ffmpeg.required.then_some(ffmpeg.ok),
    ]
    .into_iter()
    .flatten()
    .all(|ok| ok)
        && search.is_none_or(|statuses| statuses.iter().any(|status| status.ok))
}

async fn run_download(
    client: &MusicClient,
    source: Option<&str>,
    args: DownloadArgs,
) -> Result<DownloadCommandResult> {
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
        let query = MusicQuery::new(args.title.as_deref(), args.artist.as_deref())?;
        let selection = client
            .select_query(&query, &SearchStrategy::from_source(source))
            .await?;
        let mut track = selection.selected.track;
        let resolved = client.resolve(&track.id).await?;
        if track.title.trim().is_empty() {
            track.title = resolved.title;
        }
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
                required: true,
                command: command.display().to_string(),
                version: first_line,
                error: None,
            }
        }
        Ok(output) => DependencyStatus {
            ok: false,
            required: true,
            command: command.display().to_string(),
            version: None,
            error: Some(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
        },
        Err(error) => DependencyStatus {
            ok: false,
            required: true,
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
    use clap::CommandFactory;

    #[test]
    fn cli_exposes_only_search_download_mcp_and_doctor() {
        let subcommands = Cli::command()
            .get_subcommands()
            .map(|command| command.get_name().to_owned())
            .collect::<Vec<_>>();

        assert_eq!(subcommands, ["search", "download", "mcp", "doctor"]);
    }

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
            "--source",
            "netease",
            "--sources",
            "netease,kuwo",
            "--plugins-dir",
            "/opt/easymusic/plugins",
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
        assert_eq!(cli.source.as_deref(), Some("netease"));
        assert_eq!(cli.sources, ["netease".to_owned(), "kuwo".to_owned()]);
        assert_eq!(
            cli.plugins_dir,
            Some(PathBuf::from("/opt/easymusic/plugins"))
        );
    }

    #[test]
    fn search_can_merge_all_sources() {
        let cli =
            Cli::try_parse_from(["easymusic", "search", "--keyword", "晴天", "--all-sources"])
                .unwrap();
        match cli.command {
            Commands::Search(args) => assert!(args.all_sources),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn mcp_config_carries_the_startup_source() {
        let cli = Cli::try_parse_from(["easymusic", "--source", "kuwo", "mcp"]).unwrap();
        let Commands::Mcp(args) = cli.command else {
            panic!("expected the mcp subcommand");
        };
        let config = mcp_config(args, cli.source.clone());
        assert_eq!(config.default_source.as_deref(), Some("kuwo"));

        // Without --source the server keeps falling back through all sources.
        let cli = Cli::try_parse_from(["easymusic", "mcp"]).unwrap();
        let Commands::Mcp(args) = cli.command else {
            panic!("expected the mcp subcommand");
        };
        assert_eq!(mcp_config(args, cli.source.clone()).default_source, None);
    }

    fn dependency(ok: bool, required: bool) -> DependencyStatus {
        DependencyStatus {
            ok,
            required,
            command: "tool".to_owned(),
            version: None,
            error: None,
        }
    }

    fn online_search(outcomes: &[(&str, bool)]) -> Vec<OnlineSearchStatus> {
        outcomes
            .iter()
            .map(|(source, ok)| OnlineSearchStatus {
                source: (*source).to_owned(),
                ok: *ok,
                error: None,
            })
            .collect()
    }

    #[test]
    fn doctor_only_requires_yt_dlp_when_the_youtube_source_is_enabled() {
        let ffmpeg = dependency(true, true);

        // Regression: plugin/netease/kuwo-only setups passed every check but
        // still reported ok:false because yt-dlp was absent.
        assert!(doctor_is_ok(
            &dependency(false, false),
            &ffmpeg,
            Some(&online_search(&[("netease", true)]))
        ));
        // While YouTube is enabled its executable stays a hard requirement.
        assert!(!doctor_is_ok(
            &dependency(false, true),
            &ffmpeg,
            Some(&online_search(&[("youtube", false), ("netease", true)]))
        ));
        // ffmpeg is unconditional, and one answering source is enough.
        assert!(!doctor_is_ok(
            &dependency(true, true),
            &dependency(false, true),
            None
        ));
        assert!(doctor_is_ok(
            &dependency(true, true),
            &ffmpeg,
            Some(&online_search(&[("a", false), ("b", true)]))
        ));
        assert!(!doctor_is_ok(
            &dependency(true, true),
            &ffmpeg,
            Some(&online_search(&[("a", false)]))
        ));
    }

    #[test]
    fn sources_list_parsing_rejects_blanks() {
        assert_eq!(parse_sources_list(&[]).unwrap(), None);
        assert_eq!(
            parse_sources_list(&[" netease ".to_owned(), "kuwo".to_owned()]).unwrap(),
            Some(vec!["netease".to_owned(), "kuwo".to_owned()])
        );
        assert!(parse_sources_list(&["  ".to_owned(), "".to_owned()]).is_err());
    }

    #[test]
    fn build_client_requires_source_inside_enabled_sources() {
        let mut cli = Cli::try_parse_from([
            "easymusic",
            "--source",
            "kuwo",
            "--sources",
            "netease",
            "search",
            "--keyword",
            "晴天",
        ])
        .unwrap();
        assert!(build_client(&cli).is_err());

        cli.source = Some("netease".to_owned());
        // Point plugin discovery at a directory that cannot exist so the
        // test never picks up real plugins from the build directory.
        let client = build_client_for_test(cli);
        assert_eq!(client.sources(), ["netease"]);
    }

    fn build_client_for_test(mut cli: Cli) -> MusicClient {
        cli.plugins_dir = Some(PathBuf::from("definitely-not-a-plugin-dir"));
        build_client(&cli).expect("test client")
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
