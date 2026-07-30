use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;

use crate::error::{EasyMusicError, Result};
use crate::model::{ResolvedTrack, SearchResult};
use crate::provider::{MusicProvider, built_in_provider};

pub use crate::network::validate_http_url;

/// Provider-independent facade used by the CLI, MCP server, and library API.
#[derive(Clone)]
pub struct MusicClient {
    provider: Arc<dyn MusicProvider>,
}

impl MusicClient {
    /// Select a built-in provider by its registered name.
    pub fn new(provider_name: impl AsRef<str>) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("easy-music/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| EasyMusicError::upstream(error.to_string()))?;
        Ok(Self {
            provider: built_in_provider(client, provider_name)?,
        })
    }

    /// Inject a custom provider implementation.
    pub fn with_provider(provider: impl MusicProvider + 'static) -> Self {
        Self {
            provider: Arc::new(provider),
        }
    }

    /// Inject an already shared provider implementation.
    pub fn with_shared_provider(provider: Arc<dyn MusicProvider>) -> Self {
        Self { provider }
    }

    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }

    pub async fn search(&self, keyword: &str) -> Result<SearchResult> {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return Err(EasyMusicError::invalid("keyword must not be empty"));
        }
        self.provider.search(keyword).await
    }

    pub async fn resolve(&self, id: &str) -> Result<ResolvedTrack> {
        let id = id.trim();
        if id.is_empty() {
            return Err(EasyMusicError::invalid("track id must not be empty"));
        }
        self.provider.resolve(id).await
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::model::Track;

    struct EchoProvider;

    #[async_trait]
    impl MusicProvider for EchoProvider {
        fn name(&self) -> &str {
            "echo"
        }

        async fn search(&self, keyword: &str) -> Result<SearchResult> {
            Ok(SearchResult {
                ok: true,
                keyword: keyword.to_owned(),
                count: 1,
                tracks: vec![Track {
                    id: "track-id".to_owned(),
                    title: keyword.to_owned(),
                    artist: String::new(),
                    artwork_url: None,
                }],
            })
        }

        async fn resolve(&self, id: &str) -> Result<ResolvedTrack> {
            Ok(ResolvedTrack {
                id: id.to_owned(),
                title: "resolved".to_owned(),
                url: "https://example.com/song.mp3".to_owned(),
            })
        }
    }

    #[tokio::test]
    async fn delegates_to_an_injected_provider_after_validating_input() {
        let client = MusicClient::with_provider(EchoProvider);
        assert_eq!(client.provider_name(), "echo");

        let search = client.search("  song  ").await.unwrap();
        assert_eq!(search.keyword, "song");
        assert_eq!(search.tracks[0].title, "song");

        let resolved = client.resolve("  id  ").await.unwrap();
        assert_eq!(resolved.id, "id");
        assert!(client.search("   ").await.is_err());
        assert!(client.resolve("").await.is_err());
    }

    #[test]
    fn selects_builtin_providers_without_exposing_protocol_branches() {
        assert_eq!(
            MusicClient::new("qianqian").unwrap().provider_name(),
            "qianqian"
        );
        assert_eq!(MusicClient::new("yymp3").unwrap().provider_name(), "yymp3");
        assert_eq!(
            MusicClient::new("buguyy").unwrap().provider_name(),
            "buguyy"
        );
    }
}
