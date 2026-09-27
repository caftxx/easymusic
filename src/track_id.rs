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

/// A routed ID. The source is always known; `qualified` controls only its wire format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackId {
    source: String,
    native: NativeTrackId,
    qualified: bool,
}

impl TrackId {
    pub fn new(source: impl Into<String>, native: NativeTrackId) -> Self {
        Self {
            source: source.into(),
            native,
            qualified: true,
        }
    }

    /// Bare input belongs to the configured default source, preserving legacy IDs.
    pub fn parse(value: &str, default_source: &str) -> Result<Self> {
        let value = value.trim();
        if value.is_empty() {
            return Err(EasyMusicError::invalid("track id must not be empty"));
        }
        let (source, native, qualified) = match value.split_once(':') {
            Some((source, native)) => (source, native, true),
            None => (default_source, value, false),
        };
        if source.is_empty() || native.trim().is_empty() {
            return Err(EasyMusicError::invalid(
                "track id must contain a source and a non-empty native id",
            ));
        }
        Ok(Self {
            source: source.to_owned(),
            native: NativeTrackId::new(native),
            qualified,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn native(&self) -> &NativeTrackId {
        &self.native
    }
    pub fn is_qualified(&self) -> bool {
        self.qualified
    }

    pub(crate) fn with_qualification(mut self, qualified: bool) -> Self {
        // IDs containing ':' must remain qualified even for a bare-ID owner.
        self.qualified = qualified || self.native.as_str().contains(':');
        self
    }
}

impl std::fmt::Display for TrackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.qualified {
            write!(f, "{}:{}", self.source, self.native)
        } else {
            self.native.fmt(f)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_colons_survive_a_wire_round_trip() {
        let id = TrackId::new("demo", NativeTrackId::new("demo:track:42"));
        assert_eq!(id.to_string(), "demo:demo:track:42");
        assert_eq!(TrackId::parse(&id.to_string(), "youtube").unwrap(), id);
    }
    #[test]
    fn bare_and_explicit_ids_keep_their_wire_spelling() {
        for text in ["abc123", "youtube:abc123"] {
            let id = TrackId::parse(text, "youtube").unwrap();
            assert_eq!(id.source(), "youtube");
            assert_eq!(id.native().as_str(), "abc123");
            assert_eq!(id.to_string(), text);
        }
        for text in ["", "demo:", ":42"] {
            assert!(TrackId::parse(text, "youtube").is_err());
        }
    }
}
