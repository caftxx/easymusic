use std::path::PathBuf;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artwork_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub ok: bool,
    pub keyword: String,
    pub count: usize,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RankedTrack {
    #[serde(flatten)]
    pub track: Track,
    pub score: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SelectResult {
    pub ok: bool,
    pub selected: RankedTrack,
    pub confidence: f64,
    pub needs_confirmation: bool,
    pub alternatives: Vec<RankedTrack>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedTrack {
    pub id: String,
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreparedTrack {
    pub ok: bool,
    pub track: Track,
    pub url: String,
    pub confidence: f64,
    pub needs_confirmation: bool,
    pub playback: PlaybackHint,
    pub alternatives: Vec<RankedTrack>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlaybackHint {
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<AudioProfile>,
}

#[derive(Debug, Clone, Copy, Serialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AudioProfile {
    WebVoice,
    Xiaozhi,
    Pcm16k,
    Pcm24k,
}

#[derive(Debug, Clone, Copy, Serialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AudioFormat {
    PcmS16le,
    OpusOgg,
    OpusPackets,
}

#[derive(Debug, Clone, Copy, Serialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Framing {
    Len32be,
}

#[derive(Debug, Clone)]
pub struct StreamConfig {
    pub url: String,
    pub format: AudioFormat,
    pub sample_rate: u32,
    pub channels: u8,
    pub bitrate: u32,
    pub frame_ms: f32,
    pub framing: Framing,
    pub output: Option<PathBuf>,
    pub ffmpeg: PathBuf,
    pub allow_private_network: bool,
    pub start_seconds: Option<f64>,
    pub duration_seconds: Option<f64>,
    pub events_json: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudioChunkKind {
    Bytes,
    OpusPacket,
}

#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub kind: AudioChunkKind,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct StreamStats {
    pub bytes_written: u64,
    pub packets_written: u64,
}

#[derive(Debug, Clone)]
pub struct DownloadConfig {
    pub url: String,
    pub output: PathBuf,
    pub overwrite: bool,
    pub allow_private_network: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadedFile {
    pub path: PathBuf,
    pub source_url: String,
    pub bytes_written: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}
