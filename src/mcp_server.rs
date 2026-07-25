//! MCP control plane and loopback HTTP data plane for Agent integrations.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rand::Rng;
use rand::distr::Alphanumeric;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ServiceExt, schemars, tool, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use crate::error::{EasyMusicError, ErrorCode};
use crate::model::{AudioChunkKind, AudioFormat, Framing, StreamConfig};
use crate::{MusicClient, select_track, spawn_audio_stream};

const STREAM_CONTENT_TYPE: &str = "application/x-opus-packets";
const STREAM_PATH_PREFIX: &str = "/streams/";

#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub stream_bind: SocketAddr,
    pub stream_ttl: Duration,
    pub ffmpeg: PathBuf,
}

#[derive(Clone)]
struct StreamBroker {
    base_url: String,
    ttl: Duration,
    entries: Arc<Mutex<HashMap<String, PendingStream>>>,
}

struct PendingStream {
    config: StreamConfig,
    content_type: &'static str,
    expires_at: Instant,
}

impl StreamBroker {
    fn new(address: SocketAddr, ttl: Duration) -> Self {
        let host = if address.is_ipv6() {
            format!("[{}]", address.ip())
        } else {
            address.ip().to_string()
        };
        Self {
            base_url: format!("http://{host}:{}", address.port()),
            ttl,
            entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn insert(&self, config: StreamConfig, content_type: &'static str) -> StreamLease {
        let now = Instant::now();
        let mut entries = self.entries.lock().await;
        entries.retain(|_, pending| pending.expires_at > now);

        let token: String = rand::rng()
            .sample_iter(&Alphanumeric)
            .take(48)
            .map(char::from)
            .collect();
        entries.insert(
            token.clone(),
            PendingStream {
                config,
                content_type,
                expires_at: now + self.ttl,
            },
        );
        StreamLease {
            url: format!("{}{STREAM_PATH_PREFIX}{token}", self.base_url),
            expires_in_seconds: self.ttl.as_secs(),
        }
    }

    async fn take(&self, token: &str) -> Option<PendingStream> {
        let pending = self.entries.lock().await.remove(token)?;
        (pending.expires_at > Instant::now()).then_some(pending)
    }
}

#[derive(Debug, Serialize)]
struct StreamLease {
    url: String,
    expires_in_seconds: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchMusicParams {
    #[schemars(description = "Song title. Keep separate from artist.")]
    title: Option<String>,
    #[schemars(description = "Optional artist name used for ranking.")]
    artist: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PrepareStreamParams {
    #[schemars(description = "Track ID returned by the search tool.")]
    id: String,
    #[schemars(
        description = "Terminal output profile. Defaults to xiaozhi-v1. Supported values: xiaozhi-v1, web-opus, pcm-s16le-16k, pcm-s16le-24k."
    )]
    profile: Option<McpOutputProfile>,
    #[schemars(description = "Optional starting offset in seconds.")]
    start_seconds: Option<f64>,
    #[schemars(description = "Optional playback duration in seconds.")]
    duration_seconds: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum McpOutputProfile {
    #[default]
    #[serde(rename = "xiaozhi-v1")]
    XiaozhiV1,
    #[serde(rename = "web-opus")]
    WebOpus,
    #[serde(rename = "pcm-s16le-16k")]
    PcmS16le16k,
    #[serde(rename = "pcm-s16le-24k")]
    PcmS16le24k,
}

#[derive(Debug, Clone, Copy)]
struct OutputProfile {
    name: &'static str,
    format: AudioFormat,
    sample_rate: u32,
    channels: u8,
    bitrate: u32,
    frame_ms: f32,
    content_type: &'static str,
    codec: &'static str,
    framing: &'static str,
}

impl McpOutputProfile {
    fn config(self) -> OutputProfile {
        match self {
            Self::XiaozhiV1 => OutputProfile {
                name: "xiaozhi-v1",
                format: AudioFormat::OpusPackets,
                sample_rate: 24_000,
                channels: 1,
                bitrate: 64_000,
                frame_ms: 60.0,
                content_type: STREAM_CONTENT_TYPE,
                codec: "opus",
                framing: "len32be",
            },
            Self::WebOpus => OutputProfile {
                name: "web-opus",
                format: AudioFormat::OpusOgg,
                sample_rate: 48_000,
                channels: 2,
                bitrate: 96_000,
                frame_ms: 20.0,
                content_type: "audio/ogg",
                codec: "opus",
                framing: "ogg",
            },
            Self::PcmS16le16k => OutputProfile {
                name: "pcm-s16le-16k",
                format: AudioFormat::PcmS16le,
                sample_rate: 16_000,
                channels: 1,
                bitrate: 256_000,
                frame_ms: 20.0,
                content_type: "audio/pcm",
                codec: "pcm-s16le",
                framing: "raw",
            },
            Self::PcmS16le24k => OutputProfile {
                name: "pcm-s16le-24k",
                format: AudioFormat::PcmS16le,
                sample_rate: 24_000,
                channels: 1,
                bitrate: 384_000,
                frame_ms: 20.0,
                content_type: "audio/pcm",
                codec: "pcm-s16le",
                framing: "raw",
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct PreparedStream {
    ok: bool,
    stream_url: String,
    track_id: String,
    title: String,
    profile: &'static str,
    content_type: &'static str,
    codec: &'static str,
    framing: &'static str,
    sample_rate: u32,
    channels: u8,
    frame_duration_ms: u32,
    expires_in_seconds: u64,
}

#[derive(Clone)]
struct EasyMusicMcp {
    client: MusicClient,
    broker: StreamBroker,
    ffmpeg: PathBuf,
}

#[tool_router(server_handler)]
impl EasyMusicMcp {
    #[tool(
        description = "Always call this first for a music request. Search by title and/or artist and rank the best match. Inspect needs_confirmation in the JSON result: when true, ask the user to choose from selected and alternatives and do not call prepare_stream until confirmed."
    )]
    async fn search_music(&self, Parameters(params): Parameters<SearchMusicParams>) -> String {
        tool_json(self.search_music_inner(params).await)
    }

    #[tool(
        description = "Call only after the selected track is confirmed, or search_music returned needs_confirmation=false. Prepare a terminal-neutral, one-time audio stream using the requested output profile, mapping offsets to start_seconds and playback limits to duration_seconds. Pass stream_url and its returned format metadata immediately to a compatible playback tool; the URL is single-use and expires quickly."
    )]
    async fn prepare_stream(&self, Parameters(params): Parameters<PrepareStreamParams>) -> String {
        tool_json(self.prepare_stream_inner(params).await)
    }
}

impl EasyMusicMcp {
    async fn search_music_inner(
        &self,
        params: SearchMusicParams,
    ) -> crate::Result<crate::SelectResult> {
        let title = trimmed(params.title);
        let artist = trimmed(params.artist);
        let keyword = title
            .as_deref()
            .or(artist.as_deref())
            .ok_or_else(|| EasyMusicError::invalid("title or artist is required"))?;
        let search = self.client.search(keyword).await?;
        select_track(&search.tracks, title.as_deref(), artist.as_deref())
    }

    async fn prepare_stream_inner(
        &self,
        params: PrepareStreamParams,
    ) -> crate::Result<PreparedStream> {
        let id = params.id.trim();
        if id.is_empty() {
            return Err(EasyMusicError::invalid("track id is required"));
        }
        if params.start_seconds.is_some_and(|value| value < 0.0) {
            return Err(EasyMusicError::invalid(
                "start_seconds must be non-negative",
            ));
        }
        if params.duration_seconds.is_some_and(|value| value <= 0.0) {
            return Err(EasyMusicError::invalid("duration_seconds must be positive"));
        }

        let resolved = self.client.resolve(id).await?;
        let profile = params.profile.unwrap_or_default().config();
        let config = StreamConfig {
            url: resolved.url,
            format: profile.format,
            sample_rate: profile.sample_rate,
            channels: profile.channels,
            bitrate: profile.bitrate,
            frame_ms: profile.frame_ms,
            framing: Framing::Len32be,
            output: None,
            ffmpeg: self.ffmpeg.clone(),
            allow_private_network: false,
            start_seconds: params.start_seconds,
            duration_seconds: params.duration_seconds,
            events_json: false,
        };
        let lease = self.broker.insert(config, profile.content_type).await;
        Ok(PreparedStream {
            ok: true,
            stream_url: lease.url,
            track_id: id.to_owned(),
            title: resolved.title,
            profile: profile.name,
            content_type: profile.content_type,
            codec: profile.codec,
            framing: profile.framing,
            sample_rate: profile.sample_rate,
            channels: profile.channels,
            frame_duration_ms: profile.frame_ms as u32,
            expires_in_seconds: lease.expires_in_seconds,
        })
    }
}

pub async fn serve_mcp(client: MusicClient, config: McpServerConfig) -> crate::Result<()> {
    if config.stream_ttl.is_zero() {
        return Err(EasyMusicError::invalid(
            "stream_ttl_seconds must be greater than zero",
        ));
    }
    let listener = TcpListener::bind(config.stream_bind)
        .await
        .map_err(|error| {
            EasyMusicError::new(
                ErrorCode::Io,
                format!("failed to bind music stream server: {error}"),
            )
        })?;
    let address = listener.local_addr()?;
    if !address.ip().is_loopback() {
        return Err(EasyMusicError::invalid(
            "MCP stream server must bind to a loopback address",
        ));
    }

    let broker = StreamBroker::new(address, config.stream_ttl);
    let app = Router::new()
        .route("/streams/{token}", get(stream_handler))
        .with_state(broker.clone());
    let http_task = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            eprintln!("easy-music stream server failed: {error}");
        }
    });

    let service = EasyMusicMcp {
        client,
        broker,
        ffmpeg: config.ffmpeg,
    }
    .serve(rmcp::transport::stdio())
    .await
    .map_err(|error| {
        EasyMusicError::new(
            ErrorCode::Io,
            format!("failed to start MCP server: {error}"),
        )
    })?;
    let result = service.waiting().await.map_err(|error| {
        EasyMusicError::new(
            ErrorCode::Io,
            format!("MCP server stopped with an error: {error}"),
        )
    });
    http_task.abort();
    result.map(|_| ())
}

async fn stream_handler(State(broker): State<StreamBroker>, Path(token): Path<String>) -> Response {
    let Some(pending) = broker.take(&token).await else {
        return (
            StatusCode::GONE,
            "music stream is unknown, expired, or already consumed",
        )
            .into_response();
    };
    let content_type = pending.content_type;
    let mut audio = match spawn_audio_stream(&pending.config).await {
        Ok(audio) => audio,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("failed to start audio stream: {}", error.message),
            )
                .into_response();
        }
    };

    let stream = async_stream::stream! {
        while let Some(chunk) = audio.next_chunk().await {
            let bytes = if chunk.kind == AudioChunkKind::OpusPacket {
                let Ok(length) = u32::try_from(chunk.data.len()) else {
                    eprintln!("easy-music stream packet exceeded u32 length");
                    break;
                };
                let mut framed = Vec::with_capacity(4 + chunk.data.len());
                framed.extend_from_slice(&length.to_be_bytes());
                framed.extend_from_slice(&chunk.data);
                framed
            } else {
                chunk.data
            };
            yield Ok::<Bytes, io::Error>(Bytes::from(bytes));
        }
        if let Err(error) = audio.finish().await {
            eprintln!("easy-music stream failed: {}", error.message);
            yield Err(io::Error::other(error.message));
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn tool_json<T: Serialize>(result: crate::Result<T>) -> String {
    match result {
        Ok(value) => serde_json::to_string(&value).expect("MCP tool result must serialize"),
        Err(error) => json!({
            "ok": false,
            "error": {
                "code": error.code,
                "message": error.message,
            }
        })
        .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> StreamConfig {
        StreamConfig {
            url: "https://example.com/song.mp3".to_owned(),
            format: AudioFormat::OpusPackets,
            sample_rate: 24_000,
            channels: 1,
            bitrate: 64_000,
            frame_ms: 60.0,
            framing: Framing::Len32be,
            output: None,
            ffmpeg: PathBuf::from("ffmpeg"),
            allow_private_network: false,
            start_seconds: None,
            duration_seconds: None,
            events_json: false,
        }
    }

    #[tokio::test]
    async fn stream_leases_are_one_time() {
        let broker = StreamBroker::new("127.0.0.1:1234".parse().unwrap(), Duration::from_secs(30));
        let lease = broker.insert(config(), STREAM_CONTENT_TYPE).await;
        let token = lease.url.rsplit('/').next().unwrap();
        assert!(broker.take(token).await.is_some());
        assert!(broker.take(token).await.is_none());
    }

    #[test]
    fn output_profiles_expose_terminal_specific_metadata() {
        let xiaozhi = McpOutputProfile::XiaozhiV1.config();
        assert_eq!(xiaozhi.name, "xiaozhi-v1");
        assert_eq!(xiaozhi.format, AudioFormat::OpusPackets);
        assert_eq!(xiaozhi.framing, "len32be");
        assert_eq!(xiaozhi.sample_rate, 24_000);

        let web = McpOutputProfile::WebOpus.config();
        assert_eq!(web.format, AudioFormat::OpusOgg);
        assert_eq!(web.content_type, "audio/ogg");
        assert_eq!(web.sample_rate, 48_000);

        let pcm = McpOutputProfile::PcmS16le16k.config();
        assert_eq!(pcm.format, AudioFormat::PcmS16le);
        assert_eq!(pcm.framing, "raw");
        assert_eq!(pcm.channels, 1);

        let parsed: McpOutputProfile = serde_json::from_str(r#""pcm-s16le-24k""#).unwrap();
        assert_eq!(parsed.config().name, "pcm-s16le-24k");
    }

    #[test]
    fn tool_errors_are_structured_json() {
        let output = tool_json::<serde_json::Value>(Err(EasyMusicError::invalid("bad input")));
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "invalid_arguments");
        assert_eq!(value["error"]["message"], "bad input");
    }
}
