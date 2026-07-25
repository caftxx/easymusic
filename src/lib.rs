//! Core library for the `easy-music` CLI.
//!
//! The public API is intentionally small: resolve music metadata with
//! [`MusicClient`], rank candidates with [`select_track`], and stream a
//! resolved URL through ffmpeg with [`stream_audio`].

pub mod api;
pub mod error;
pub mod model;
pub mod ranking;
pub mod streaming;

pub use api::MusicClient;
pub use error::{EasyMusicError, ErrorCode, Result};
pub use model::{
    AudioChunk, AudioChunkKind, AudioFormat, AudioProfile, Framing, PreparedTrack, ResolvedTrack,
    SearchResult, SelectResult, StreamConfig, StreamStats, Track,
};
pub use ranking::{rank_tracks, select_track};
pub use streaming::{AudioStream, spawn_audio_stream, stream_audio};
