//! Source-native identities and their public routing representation.
use crate::error::{EasyMusicError, Result};

/// An opaque ID owned by a source. Never interpret or strip its contents while routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTrackId(String);

impl NativeTrackId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NativeTrackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A routed ID, always formatted as `<source>:<native id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackId {
    source: String,
    native: NativeTrackId,
}

impl TrackId {
    pub fn new(source: impl Into<String>, native: NativeTrackId) -> Self {
        Self {
            source: source.into(),
            native,
        }
    }

    /// Parse a track ID with a required source namespace.
    pub fn parse(value: &str) -> Result<Self> {
        let value = value.trim();
        if value.is_empty() {
            return Err(EasyMusicError::invalid("track id must not be empty"));
        }
        let (source, native) = value.split_once(':').ok_or_else(|| {
            EasyMusicError::invalid("track id must use <source>:<native id> format")
        })?;
        if source.is_empty() || native.trim().is_empty() {
            return Err(EasyMusicError::invalid(
                "track id must contain a source and a non-empty native id",
            ));
        }
        Ok(Self {
            source: source.to_owned(),
            native: NativeTrackId::new(native),
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn native(&self) -> &NativeTrackId {
        &self.native
    }
}

impl std::fmt::Display for TrackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.source, self.native)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_colons_survive_a_wire_round_trip() {
        let id = TrackId::new("demo", NativeTrackId::new("demo:track:42"));
        assert_eq!(id.to_string(), "demo:demo:track:42");
        assert_eq!(TrackId::parse(&id.to_string()).unwrap(), id);
    }
    #[test]
    fn namespaced_ids_keep_their_wire_spelling() {
        for text in ["youtube:abc123", "youtube:abc:def"] {
            let id = TrackId::parse(text).unwrap();
            assert_eq!(id.source(), "youtube");
            assert_eq!(id.native().as_str(), text.strip_prefix("youtube:").unwrap());
            assert_eq!(id.to_string(), text);
        }
    }
    #[test]
    fn bare_and_incomplete_ids_are_rejected() {
        for text in ["", "abc123", "demo:", "demo:  ", ":42"] {
            assert!(TrackId::parse(text).is_err());
        }
    }
}
