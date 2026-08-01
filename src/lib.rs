//! Core library for the `easymusic` CLI.
//!
//! The public API is intentionally small: resolve music metadata with
//! [`MusicClient`], rank candidates with [`select_track`], and stream a
//! resolved URL through ffmpeg with [`stream_audio`] or download its original
//! bytes with [`download_audio`].

pub mod api;
pub mod download;
pub mod error;
pub mod mcp_server;
pub mod model;
mod network;
pub mod ranking;
pub mod streaming;
mod ytdlp;

pub use api::{MusicClient, YtDlpConfig, default_search_limit};
pub use download::download_audio;
pub use error::{EasyMusicError, ErrorCode, Result};
pub use model::{
    AudioChunk, AudioChunkKind, AudioFormat, AudioProfile, DownloadConfig, DownloadedFile, Framing,
    PreparedTrack, ResolvedTrack, SearchResult, SelectResult, StreamConfig, StreamStats, Track,
};
pub use ranking::{rank_tracks, select_track};
pub use streaming::{AudioStream, spawn_audio_stream, stream_audio};
