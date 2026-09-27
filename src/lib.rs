//! Core library for the `easymusic` CLI.
//!
//! The public API is intentionally small: resolve music metadata with
//! [`MusicClient`] (backed by pluggable [`provider::MusicSource`] sources),
//! rank candidates with [`select_track`], and stream a resolved URL through
//! ffmpeg with [`stream_audio`] or download its original bytes with
//! [`download_audio`].

pub mod api;
pub mod config;
pub mod download;
pub mod error;
pub mod mcp_server;
pub mod model;
mod network;
mod process;
pub mod provider;
pub mod ranking;
pub mod search;
pub mod streaming;
pub mod track_id;

pub use api::{
    ClientConfig, MusicClient, YtDlpConfig, default_search_limit, max_search_limit,
    validate_search_limit,
};
pub use download::download_audio;
pub use error::{EasyMusicError, ErrorCode, Result};
pub use model::{
    AudioChunk, AudioChunkKind, AudioFormat, AudioProfile, DownloadConfig, DownloadedFile, Framing,
    PreparedTrack, ResolvedTrack, SearchResult, SelectResult, StreamConfig, StreamStats, Track,
};
pub use provider::{
    MusicSource, SourceDiagnostics, SourceRegistry, SourceResolvedTrack, SourceTrack, YouTubeSource,
};
pub use ranking::{rank_tracks, select_track};
pub use streaming::{AudioStream, spawn_audio_stream, stream_audio};

pub use search::{MusicQuery, SearchStrategy};

pub use track_id::{NativeTrackId, TrackId};
