pub use crate::config::{ClientConfig, YtDlpConfig, default_plugin_dirs};
use crate::error::{EasyMusicError, Result};
use crate::model::{ResolvedTrack, SearchResult};
use crate::provider::{SourceDiagnostics, SourceRegistry};

pub use crate::network::validate_http_url;

const DEFAULT_SEARCH_LIMIT: usize = 20;
const MAX_SEARCH_LIMIT: usize = 50;

/// Multi-source music search and audio URL resolution.
///
/// Backends are pluggable [`crate::provider::MusicSource`] implementations
/// managed by a [`SourceRegistry`]: the built-in `youtube` (yt-dlp),
/// `netease`, and `kuwo` sources plus any external `easymusic-source-*`
/// plugins found on disk.
#[derive(Clone)]
pub struct MusicClient {
    registry: SourceRegistry,
}

impl MusicClient {
    /// Discover `yt-dlp` and load every built-in and plugin source.
    pub fn new() -> Self {
        Self::with_config(ClientConfig::default())
    }

    pub fn with_config(config: ClientConfig) -> Self {
        Self::try_with_config(config).expect("valid client configuration")
    }

    pub fn try_with_config(config: ClientConfig) -> Result<Self> {
        Ok(Self::with_registry(
            crate::provider::builder::build_registry(&config)?,
        ))
    }

    pub fn registry(&self) -> &SourceRegistry {
        &self.registry
    }

    /// Build a client on top of a caller-supplied registry, for example one
    /// created with [`SourceRegistry::from_sources`] and a custom in-process
    /// [`crate::provider::MusicSource`] implementation.
    pub fn with_registry(registry: SourceRegistry) -> Self {
        Self { registry }
    }

    pub fn sources(&self) -> Vec<&str> {
        self.registry.names()
    }

    /// Diagnostics come from the same source instance used for requests.
    pub fn source_diagnostics(&self, name: &str) -> Option<SourceDiagnostics<'_>> {
        self.registry.diagnostics(name)
    }

    /// Search the first source that succeeds. Pass `source` (for example
    /// `"netease"`) to pin the search to one music source.
    pub async fn search(&self, keyword: &str, limit: usize) -> Result<SearchResult> {
        self.search_from(None, keyword, limit).await
    }

    pub async fn search_from(
        &self,
        source: Option<&str>,
        keyword: &str,
        limit: usize,
    ) -> Result<SearchResult> {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return Err(EasyMusicError::invalid("keyword must not be empty"));
        }
        validate_search_limit(limit)?;
        if let Some(name) = source {
            return self.registry.search(name, keyword, limit).await;
        }
        let mut failures = Vec::new();
        for name in self.sources() {
            match self.registry.search(name, keyword, limit).await {
                Ok(result) => return Ok(result),
                Err(error) => failures.push(format!("{name}: {error}")),
            }
        }
        Err(EasyMusicError::upstream(format!(
            "all music sources failed: {}",
            failures.join(" | ")
        )))
    }

    /// Resolve a track ID with a required `<source>:` namespace (for
    /// example `youtube:DYptgVvkVLQ` or `netease:186016`).
    /// The resolved track's `id` keeps that namespace, so it stays reusable.
    pub async fn resolve(&self, id: &str) -> Result<ResolvedTrack> {
        let id = id.trim();
        if id.is_empty() {
            return Err(EasyMusicError::invalid("track id must not be empty"));
        }
        self.registry.resolve(id).await
    }
}

impl Default for MusicClient {
    fn default() -> Self {
        Self::new()
    }
}

pub fn default_search_limit() -> usize {
    DEFAULT_SEARCH_LIMIT
}

pub fn max_search_limit() -> usize {
    MAX_SEARCH_LIMIT
}

/// Reject search limits outside `1..=max_search_limit()`. Shared by the
/// single-source and merged search paths so both behave identically.
pub fn validate_search_limit(limit: usize) -> Result<()> {
    if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
        return Err(EasyMusicError::invalid(format!(
            "search limit must be between 1 and {MAX_SEARCH_LIMIT}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn test_client() -> MusicClient {
        MusicClient::with_config(ClientConfig {
            yt_dlp: YtDlpConfig {
                executable: PathBuf::from("definitely-not-an-executable"),
                js_runtime: None,
                cookies: None,
            },
            sources: None,
            // A directory that cannot exist disables plugin discovery.
            plugin_dirs: vec![PathBuf::from("definitely-not-a-plugin-dir")],
        })
    }

    #[tokio::test]
    async fn rejects_invalid_inputs_before_starting_backends() {
        let client = test_client();

        assert_eq!(
            client.search("   ", 10).await.unwrap_err().code.exit_code(),
            2
        );
        assert_eq!(
            client.search("song", 0).await.unwrap_err().code.exit_code(),
            2
        );
        assert_eq!(
            client
                .search("song", 51)
                .await
                .unwrap_err()
                .code
                .exit_code(),
            2
        );
        assert_eq!(client.resolve("").await.unwrap_err().code.exit_code(), 2);
    }

    #[tokio::test]
    async fn routes_namespaced_ids_and_rejects_unknown_sources() {
        let client = test_client();
        assert_eq!(client.sources(), ["youtube", "netease", "kuwo"]);

        // Unknown namespaces fail with a helpful argument error.
        let error = client.resolve("tidal:123").await.unwrap_err();
        assert_eq!(error.code.exit_code(), 2);
        assert!(error.message.contains("unknown music source"));

        // A namespaced ID reaches the right source (which then fails because
        // netease ids must be numeric, not because routing went wrong).
        let error = client.resolve("netease:abc").await.unwrap_err();
        assert_eq!(error.code.exit_code(), 2);
        assert!(error.message.contains("netease"));

        // YouTube IDs require the same namespace as every other source.
        let error = client.resolve("DYptgVvkVLQ").await.unwrap_err();
        assert_eq!(error.code.exit_code(), 2);
        assert!(error.message.contains("<source>:<native id>"));

        let error = client.resolve("youtube:../video").await.unwrap_err();
        assert_eq!(error.code.exit_code(), 2);
        assert!(error.message.contains("YouTube"));
    }

    #[tokio::test]
    async fn pinned_search_validates_source_existence() {
        let client = test_client();
        let error = client
            .search_from(Some("tidal"), "song", 5)
            .await
            .unwrap_err();
        assert_eq!(error.code.exit_code(), 2);
        assert!(error.message.contains("unknown music source"));
    }

    #[test]
    fn explicit_configuration_is_exposed_for_diagnostics() {
        let client = MusicClient::with_config(ClientConfig {
            yt_dlp: YtDlpConfig {
                executable: PathBuf::from("tools/yt-dlp"),
                js_runtime: Some("quickjs:tools/qjs".to_owned()),
                cookies: Some(PathBuf::from("tools/cookies.txt")),
            },
            sources: Some(vec!["youtube".to_owned()]),
            plugin_dirs: vec![PathBuf::from("definitely-not-a-plugin-dir")],
        });

        assert_eq!(
            client.source_diagnostics("youtube").unwrap().executable,
            Some(Path::new("tools/yt-dlp"))
        );
        assert_eq!(
            client.source_diagnostics("youtube").unwrap().js_runtime,
            Some("quickjs:tools/qjs")
        );
        assert_eq!(client.sources(), ["youtube"]);
    }
}
