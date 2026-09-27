//! MCP control plane and loopback HTTP data plane for Agent integrations.

use std::collections::{HashMap, VecDeque};
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
use crate::model::{AudioChunk, AudioChunkKind, AudioFormat, Framing, StreamConfig};
use crate::{AudioStream, MusicClient, MusicQuery, SearchStrategy, spawn_audio_stream};

const STREAM_CONTENT_TYPE: &str = "application/x-opus-packets";
const STREAM_PATH_PREFIX: &str = "/streams/";
const PREBUFFER_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_PENDING_STREAMS: usize = 8;

#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub stream_bind: SocketAddr,
    pub stream_ttl: Duration,
    pub ffmpeg: PathBuf,
    /// Default music source for `search_music` calls that do not name one,
    /// e.g. the CLI's global `--source` / `EASYMUSIC_SOURCE`.
    pub default_source: Option<String>,
}

#[derive(Clone)]
struct StreamBroker {
    base_url: String,
    ttl: Duration,
    entries: Arc<Mutex<HashMap<String, PendingStream>>>,
}

struct PendingStream {
    audio: AudioStream,
    buffered: VecDeque<AudioChunk>,
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

    async fn insert(
        &self,
        audio: AudioStream,
        buffered: VecDeque<AudioChunk>,
        content_type: &'static str,
    ) -> StreamLease {
        let now = Instant::now();
        let mut entries = self.entries.lock().await;
        entries.retain(|_, pending| pending.expires_at > now);
        if entries.len() >= MAX_PENDING_STREAMS
            && let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, pending)| pending.expires_at)
                .map(|(token, _)| token.clone())
        {
            entries.remove(&oldest);
        }

        let token: String = rand::rng()
            .sample_iter(&Alphanumeric)
            .take(48)
            .map(char::from)
            .collect();
        entries.insert(
            token.clone(),
            PendingStream {
                audio,
                buffered,
                content_type,
                expires_at: now + self.ttl,
            },
        );
        let lease = StreamLease {
            url: format!("{}{STREAM_PATH_PREFIX}{token}", self.base_url),
            expires_in_seconds: self.ttl.as_secs(),
        };
        drop(entries);

        let cleanup_entries = self.entries.clone();
        let cleanup_token = token;
        let ttl = self.ttl;
        tokio::spawn(async move {
            tokio::time::sleep(ttl).await;
            let mut entries = cleanup_entries.lock().await;
            if entries
                .get(&cleanup_token)
                .is_some_and(|pending| pending.expires_at <= Instant::now())
            {
                entries.remove(&cleanup_token);
            }
        });
        lease
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
    #[schemars(
        with = "String",
        description = "Song title. It is combined with artist for the music source search query."
    )]
    title: Option<String>,
    #[schemars(
        with = "String",
        description = "Optional artist name used for both search and ranking."
    )]
    artist: Option<String>,
    #[schemars(
        with = "String",
        description = "Optional music source to pin the search to, for example youtube, netease, or kuwo. Overrides the server default; without it the default source or the configured priority order is used."
    )]
    source: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PrepareStreamParams {
    #[schemars(
        description = "Track ID returned by the search tool, including its \"<source>:\" namespace."
    )]
    id: String,
    #[schemars(
        schema_with = "mcp_output_profile_schema",
        description = "Terminal output profile. Defaults to xiaozhi-v1. Supported values: xiaozhi-v1, web-opus, pcm-s16le-16k, pcm-s16le-24k."
    )]
    profile: Option<McpOutputProfile>,
    #[schemars(with = "f64", description = "Optional starting offset in seconds.")]
    start_seconds: Option<f64>,
    #[schemars(with = "f64", description = "Optional playback duration in seconds.")]
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

fn mcp_output_profile_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "enum": [
            "xiaozhi-v1",
            "web-opus",
            "pcm-s16le-16k",
            "pcm-s16le-24k"
        ]
    })
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
    ready: bool,
    prebuffered_bytes: usize,
    prepare_latency_ms: u64,
    expires_in_seconds: u64,
}

#[derive(Clone)]
struct EasyMusicMcp {
    client: MusicClient,
    broker: StreamBroker,
    ffmpeg: PathBuf,
    default_source: Option<String>,
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
        description = "Call only after the selected track is confirmed, or search_music returned needs_confirmation=false. Start ffmpeg and wait until the first chunk is buffered, then return a terminal-neutral, one-time audio stream using the requested output profile, mapping offsets to start_seconds and playback limits to duration_seconds. Pass stream_url and its returned format metadata immediately to a compatible playback tool; the URL is single-use and expires quickly."
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
        let query = MusicQuery::new(params.title.as_deref(), params.artist.as_deref())?;
        let source = effective_source(params.source, self.default_source.as_deref());
        self.client
            .select_query(&query, &SearchStrategy::from_source(source.as_deref()))
            .await
    }

    async fn prepare_stream_inner(
        &self,
        params: PrepareStreamParams,
    ) -> crate::Result<PreparedStream> {
        let prepare_started = Instant::now();
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
        let audio = spawn_audio_stream(&config).await?;
        let warmed = prebuffer_first_chunk(audio, PREBUFFER_TIMEOUT).await?;
        let prebuffered_bytes = warmed.buffered.iter().map(|chunk| chunk.data.len()).sum();
        let lease = self
            .broker
            .insert(warmed.audio, warmed.buffered, profile.content_type)
            .await;
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
            ready: true,
            prebuffered_bytes,
            prepare_latency_ms: prepare_started.elapsed().as_millis().min(u64::MAX as u128) as u64,
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
            eprintln!("easymusic stream server failed: {error}");
        }
    });

    let service = EasyMusicMcp {
        client,
        broker,
        ffmpeg: config.ffmpeg,
        default_source: config
            .default_source
            .map(|source| source.trim().to_owned())
            .filter(|source| !source.is_empty()),
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
    let Some(mut pending) = broker.take(&token).await else {
        return (
            StatusCode::GONE,
            "music stream is unknown, expired, or already consumed",
        )
            .into_response();
    };
    let content_type = pending.content_type;

    let stream = async_stream::stream! {
        while let Some(chunk) = pending.buffered.pop_front() {
            yield frame_stream_chunk(chunk);
        }
        while let Some(chunk) = pending.audio.next_chunk().await {
            yield frame_stream_chunk(chunk);
        }
        if let Err(error) = pending.audio.finish().await {
            eprintln!("easymusic stream failed: {}", error.message);
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

struct WarmedAudio {
    audio: AudioStream,
    buffered: VecDeque<AudioChunk>,
}

async fn prebuffer_first_chunk(
    mut audio: AudioStream,
    timeout: Duration,
) -> crate::Result<WarmedAudio> {
    let chunk = tokio::time::timeout(timeout, audio.next_chunk())
        .await
        .map_err(|_| EasyMusicError::transcode("timed out waiting for the first audio chunk"))?;
    let Some(chunk) = chunk else {
        audio.finish().await?;
        return Err(EasyMusicError::transcode(
            "ffmpeg completed without producing audio",
        ));
    };
    Ok(WarmedAudio {
        audio,
        buffered: VecDeque::from([chunk]),
    })
}

fn frame_stream_chunk(chunk: AudioChunk) -> Result<Bytes, io::Error> {
    if chunk.kind != AudioChunkKind::OpusPacket {
        return Ok(Bytes::from(chunk.data));
    }
    let length = u32::try_from(chunk.data.len())
        .map_err(|_| io::Error::other("easymusic Opus packet exceeded u32 length"))?;
    let mut framed = Vec::with_capacity(4 + chunk.data.len());
    framed.extend_from_slice(&length.to_be_bytes());
    framed.extend_from_slice(&chunk.data);
    Ok(Bytes::from(framed))
}

fn trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// A per-call `source` argument wins over the server's configured default.
fn effective_source(request: Option<String>, configured: Option<&str>) -> Option<String> {
    trimmed(request).or_else(|| trimmed(configured.map(str::to_owned)))
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

    #[tokio::test]
    async fn stream_leases_are_one_time() {
        let broker = StreamBroker::new("127.0.0.1:1234".parse().unwrap(), Duration::from_secs(30));
        let audio = AudioStream::from_test_chunks(vec![AudioChunk {
            kind: AudioChunkKind::OpusPacket,
            data: b"ready".to_vec(),
        }]);
        let warmed = prebuffer_first_chunk(audio, Duration::from_secs(1))
            .await
            .unwrap();
        let lease = broker
            .insert(warmed.audio, warmed.buffered, STREAM_CONTENT_TYPE)
            .await;
        let token = lease.url.rsplit('/').next().unwrap();
        assert!(broker.take(token).await.is_some());
        assert!(broker.take(token).await.is_none());
    }

    #[tokio::test]
    async fn expired_warmed_streams_are_removed_without_another_insert() {
        let broker =
            StreamBroker::new("127.0.0.1:1234".parse().unwrap(), Duration::from_millis(10));
        let audio = AudioStream::from_test_chunks(vec![AudioChunk {
            kind: AudioChunkKind::Bytes,
            data: b"ready".to_vec(),
        }]);
        let warmed = prebuffer_first_chunk(audio, Duration::from_secs(1))
            .await
            .unwrap();
        broker
            .insert(warmed.audio, warmed.buffered, "audio/ogg")
            .await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(broker.entries.lock().await.is_empty());
    }

    #[tokio::test]
    async fn pending_streams_are_bounded_and_evict_the_oldest() {
        let broker = StreamBroker::new("127.0.0.1:1234".parse().unwrap(), Duration::from_secs(30));
        let mut first_token = String::new();

        for index in 0..=MAX_PENDING_STREAMS {
            let audio = AudioStream::from_test_chunks(vec![AudioChunk {
                kind: AudioChunkKind::Bytes,
                data: vec![index as u8],
            }]);
            let warmed = prebuffer_first_chunk(audio, Duration::from_secs(1))
                .await
                .unwrap();
            let lease = broker
                .insert(warmed.audio, warmed.buffered, "audio/pcm")
                .await;
            if index == 0 {
                first_token = lease.url.rsplit('/').next().unwrap().to_owned();
            }
        }

        assert_eq!(broker.entries.lock().await.len(), MAX_PENDING_STREAMS);
        assert!(broker.take(&first_token).await.is_none());
    }

    #[tokio::test]
    async fn prebuffer_rejects_streams_without_audio() {
        let error = prebuffer_first_chunk(
            AudioStream::from_test_chunks(Vec::new()),
            Duration::from_secs(1),
        )
        .await
        .err()
        .unwrap();

        assert!(matches!(error.code, ErrorCode::Transcode));
        assert!(error.message.contains("without producing audio"));
    }

    #[test]
    fn opus_chunks_are_length_prefixed_after_prebuffering() {
        let framed = frame_stream_chunk(AudioChunk {
            kind: AudioChunkKind::OpusPacket,
            data: b"opus".to_vec(),
        })
        .unwrap();
        assert_eq!(framed.as_ref(), b"\0\0\0\x04opus");
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
    fn mcp_parameter_schemas_inline_optional_value_types() {
        let search = serde_json::to_value(schemars::schema_for!(SearchMusicParams)).unwrap();
        assert_eq!(search["properties"]["title"]["type"], "string");
        assert_eq!(search["properties"]["artist"]["type"], "string");
        assert_eq!(search["properties"]["source"]["type"], "string");

        let prepare = serde_json::to_value(schemars::schema_for!(PrepareStreamParams)).unwrap();
        let profile = &prepare["properties"]["profile"];
        assert_eq!(profile["type"], "string");
        assert_eq!(
            profile["enum"],
            json!(["xiaozhi-v1", "web-opus", "pcm-s16le-16k", "pcm-s16le-24k"])
        );
        assert_eq!(prepare["properties"]["start_seconds"]["type"], "number");
        assert_eq!(prepare["properties"]["duration_seconds"]["type"], "number");
        assert!(prepare.get("$defs").is_none());
        assert!(profile.get("anyOf").is_none());
        assert!(profile.get("$ref").is_none());
    }

    #[test]
    fn tool_level_source_overrides_the_server_default() {
        // No tool argument: the startup `--source` decides.
        assert_eq!(
            effective_source(None, Some("kuwo")).as_deref(),
            Some("kuwo")
        );
        assert_eq!(
            effective_source(Some("  ".to_owned()), Some("kuwo")).as_deref(),
            Some("kuwo")
        );
        // An explicit tool argument wins.
        assert_eq!(
            effective_source(Some("netease".to_owned()), Some("kuwo")).as_deref(),
            Some("netease")
        );
        // Nothing configured anywhere: fall through to registry priority.
        assert_eq!(effective_source(None, None), None);
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
