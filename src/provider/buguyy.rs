use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use url::Url;

use super::MusicProvider;
use crate::error::{EasyMusicError, Result};
use crate::model::{ResolvedTrack, SearchResult, Track};
use crate::network::validate_http_url;

pub(super) struct BuguyyProvider {
    client: Client,
    base_url: Url,
}

impl BuguyyProvider {
    pub(super) fn new(client: Client, base_url: Url) -> Self {
        Self { client, base_url }
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        self.base_url
            .join(path)
            .map_err(|error| EasyMusicError::upstream(error.to_string()))
    }
}

#[async_trait]
impl MusicProvider for BuguyyProvider {
    fn name(&self) -> &str {
        "buguyy"
    }

    async fn search(&self, keyword: &str) -> Result<SearchResult> {
        let response = self
            .client
            .get(self.endpoint("api/search")?)
            .query(&[("keyword", keyword)])
            .send()
            .await?
            .error_for_status()
            .map_err(|error| EasyMusicError::upstream(error.to_string()))?
            .json::<SearchResponse>()
            .await
            .map_err(|error| {
                EasyMusicError::upstream(format!("invalid Buguyy search response: {error}"))
            })?;

        if !response.success {
            return Err(EasyMusicError::upstream(
                "Buguyy search API returned success=false",
            ));
        }

        let tracks = response
            .data
            .into_iter()
            .map(Track::from)
            .collect::<Vec<_>>();
        Ok(SearchResult {
            ok: true,
            keyword: if response.keyword.is_empty() {
                keyword.to_owned()
            } else {
                response.keyword
            },
            count: response.count.max(tracks.len()),
            tracks,
        })
    }

    async fn resolve(&self, id: &str) -> Result<ResolvedTrack> {
        let response = self
            .client
            .get(self.endpoint("api/geturl")?)
            .query(&[("id", id)])
            .send()
            .await?
            .error_for_status()
            .map_err(|error| EasyMusicError::upstream(error.to_string()))?
            .json::<ResolveResponse>()
            .await
            .map_err(|error| {
                EasyMusicError::upstream(format!("invalid Buguyy resolve response: {error}"))
            })?;

        if !response.success || response.url.trim().is_empty() {
            return Err(EasyMusicError::source(
                "Buguyy detail API did not return a playable URL",
            ));
        }
        validate_http_url(&response.url)?;

        Ok(ResolvedTrack {
            id: id.to_owned(),
            title: response.name,
            url: response.url,
        })
    }
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    success: bool,
    #[serde(default)]
    data: Vec<SearchTrack>,
    #[serde(default)]
    count: usize,
    #[serde(default)]
    keyword: String,
}

#[derive(Debug, Deserialize)]
struct SearchTrack {
    id: String,
    title: String,
    #[serde(default)]
    singer: String,
    #[serde(default)]
    picurl: Option<String>,
}

impl From<SearchTrack> for Track {
    fn from(value: SearchTrack) -> Self {
        Self {
            id: value.id,
            title: value.title,
            artist: value.singer,
            artwork_url: value.picurl,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ResolveResponse {
    success: bool,
    #[serde(default)]
    url: String,
    #[serde(default)]
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_endpoints_under_the_configured_base_path() {
        let provider = BuguyyProvider::new(
            Client::new(),
            Url::parse("https://example.com/root/").unwrap(),
        );
        assert_eq!(
            provider.endpoint("api/search").unwrap().as_str(),
            "https://example.com/root/api/search"
        );
    }
}
