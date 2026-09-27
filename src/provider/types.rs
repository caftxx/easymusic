//! Provider output uses native IDs; public response envelopes belong to the client.
use crate::track_id::NativeTrackId;

#[derive(Debug, Clone)]
pub struct SourceTrack {
    pub id: NativeTrackId,
    pub title: String,
    pub artist: String,
    pub artwork_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SourceResolvedTrack {
    pub id: NativeTrackId,
    pub title: String,
    pub url: String,
    pub extension: Option<String>,
}
