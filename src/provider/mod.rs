mod buguyy;
mod qianqian;
mod yymp3;

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::Serialize;
use url::Url;

use crate::error::{EasyMusicError, Result};
use crate::model::{ResolvedTrack, SearchResult};
use crate::network::validate_http_url;

use buguyy::BuguyyProvider;
use qianqian::QianqianProvider;
use yymp3::Yymp3Provider;

pub const DEFAULT_PROVIDER_NAME: &str = "qianqian";

const BUGUYY_BASE_URL: &str = "https://buguyy.top/";
const QIANQIAN_HOMEPAGE: &str = "https://music.91q.com/";
const YYMP3_BASE_URL: &str = "https://www.yymp3.com/";

const PROVIDERS: [ProviderInfo; 3] = [
    ProviderInfo {
        name: "qianqian",
        display_name: "Qianqian / 91Q",
        homepage: QIANQIAN_HOMEPAGE,
    },
    ProviderInfo {
        name: "yymp3",
        display_name: "YYMP3",
        homepage: YYMP3_BASE_URL,
    },
    ProviderInfo {
        name: "buguyy",
        display_name: "Buguyy",
        homepage: BUGUYY_BASE_URL,
    },
];

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct ProviderInfo {
    pub name: &'static str,
    pub display_name: &'static str,
    pub homepage: &'static str,
}

pub fn supported_providers() -> &'static [ProviderInfo] {
    &PROVIDERS
}

/// Search and resolve contract implemented by every music source.
///
/// Providers own their protocol details. Callers should depend on this
/// interface through [`crate::MusicClient`] instead of branching on a source.
#[async_trait]
pub trait MusicProvider: Send + Sync {
    /// Stable provider name for diagnostics.
    fn name(&self) -> &str;

    /// Search the provider catalog with an already validated keyword.
    async fn search(&self, keyword: &str) -> Result<SearchResult>;

    /// Resolve a provider-specific track ID to a temporary playable URL.
    async fn resolve(&self, id: &str) -> Result<ResolvedTrack>;
}

pub(crate) fn built_in_provider(
    client: Client,
    name: impl AsRef<str>,
) -> Result<Arc<dyn MusicProvider>> {
    let name = name.as_ref().trim().to_ascii_lowercase();
    let provider: Arc<dyn MusicProvider> = match name.as_str() {
        "qianqian" => Arc::new(QianqianProvider::new(client)),
        "yymp3" => Arc::new(Yymp3Provider::new(client, provider_url(YYMP3_BASE_URL)?)),
        "buguyy" => Arc::new(BuguyyProvider::new(client, provider_url(BUGUYY_BASE_URL)?)),
        _ => {
            let supported = supported_providers()
                .iter()
                .map(|provider| provider.name)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(EasyMusicError::invalid(format!(
                "unknown music provider {name:?}; supported providers: {supported}"
            )));
        }
    };
    Ok(provider)
}

fn provider_url(value: &str) -> Result<Url> {
    let url = Url::parse(value)
        .map_err(|error| EasyMusicError::upstream(format!("invalid provider URL: {error}")))?;
    validate_http_url(url.as_str())?;
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> Client {
        Client::new()
    }

    #[test]
    fn selects_builtin_providers_by_name() {
        assert_eq!(
            built_in_provider(client(), "qianqian").unwrap().name(),
            "qianqian"
        );
        assert_eq!(
            built_in_provider(client(), "YYMP3").unwrap().name(),
            "yymp3"
        );
        assert_eq!(
            built_in_provider(client(), " buguyy ").unwrap().name(),
            "buguyy"
        );
    }

    #[test]
    fn lists_the_default_and_rejects_unknown_providers() {
        assert_eq!(DEFAULT_PROVIDER_NAME, "qianqian");
        assert_eq!(
            supported_providers()
                .iter()
                .map(|provider| provider.name)
                .collect::<Vec<_>>(),
            vec!["qianqian", "yymp3", "buguyy"]
        );

        let error = built_in_provider(client(), "missing").err().unwrap();
        assert!(error.message.contains("qianqian, yymp3, buguyy"));
    }
}
