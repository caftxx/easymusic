//! Pluggable music source ("音源") layer.
//!
//! Every backend that can search tracks and turn a track ID into a playable
//! audio URL implements [`MusicSource`]. Sources are either built in
//! (`youtube`, `netease`, `kuwo`) or discovered as external executables that
//! speak the JSON subprocess protocol in [`external`].

pub mod builder;
pub mod external;
pub(crate) mod kuwo;
pub(crate) mod netease;
mod registry;
mod types;
use crate::track_id::NativeTrackId;
pub use types::{SourceResolvedTrack, SourceTrack};
pub mod youtube;

use std::time::Duration;

use async_trait::async_trait;

use crate::error::{EasyMusicError, ErrorCode, Result};

pub use registry::SourceRegistry;
pub use youtube::YouTubeSource;

/// Optional runtime details borrowed from the active provider instance.
#[derive(Debug, Default, Clone, Copy)]
pub struct SourceDiagnostics<'a> {
    pub executable: Option<&'a std::path::Path>,
    pub js_runtime: Option<&'a str>,
}

/// Default browser-like user agent shared by HTTP-based sources.
pub(crate) const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
pub(crate) const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// A music source plugin: search plus playable URL resolution.
///
/// [`MusicSource::search`] returns the source's own native IDs. The
/// [`SourceRegistry`] namespaces them as `"<name>:<native id>"` so any source
/// can resolve them unambiguously.
#[async_trait]
pub trait MusicSource: Send + Sync {
    /// Stable short identifier used in CLI flags and ID namespaces.
    fn name(&self) -> &str;

    /// Human-readable display name.
    fn display_name(&self) -> &str {
        self.name()
    }

    fn diagnostics(&self) -> SourceDiagnostics<'_> {
        SourceDiagnostics::default()
    }

    async fn search(&self, keyword: &str, limit: usize) -> Result<Vec<SourceTrack>>;

    async fn resolve(&self, id: &NativeTrackId) -> Result<SourceResolvedTrack>;
}

pub(crate) fn is_http_url(value: &str) -> bool {
    crate::network::validate_http_url(value).is_ok()
}

pub(crate) fn build_http_client(
    user_agent: &str,
    referer: &str,
    follow_redirects: bool,
) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(DEFAULT_HTTP_TIMEOUT)
        .user_agent(user_agent)
        .default_headers({
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::REFERER,
                reqwest::header::HeaderValue::from_str(referer)
                    .map_err(|error| EasyMusicError::new(ErrorCode::Io, error.to_string()))?,
            );
            headers.insert(
                reqwest::header::ACCEPT,
                reqwest::header::HeaderValue::from_static("application/json, text/plain, */*"),
            );
            headers
        })
        .redirect(if follow_redirects {
            reqwest::redirect::Policy::limited(10)
        } else {
            reqwest::redirect::Policy::none()
        })
        .build()
        .map_err(|error| EasyMusicError::new(ErrorCode::Io, error.to_string()))
}

pub(crate) fn upstream_error(
    source: &str,
    operation: &str,
    error: reqwest::Error,
) -> EasyMusicError {
    EasyMusicError::upstream(format!("{source} {operation} failed: {error}"))
}

/// Strip HTML tags and decode the handful of entities these APIs emit.
pub(crate) fn clean_text(value: &str) -> String {
    let without_br = value.replace("<br>", " ").replace("<br/>", " ");
    let mut cleaned = String::with_capacity(without_br.len());
    let mut inside_tag = false;
    for ch in without_br.chars() {
        match ch {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            other if !inside_tag => cleaned.push(other),
            _ => {}
        }
    }
    cleaned
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("\\u0026", "&")
        .replace("\\&", "&")
        .replace("&amp;", "&")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}
