use std::time::Duration;

use reqwest::Client;
use url::Url;

use crate::error::{EasyMusicError, Result};
use crate::model::{
    ResolvedTrack, SearchResult, Track, UpstreamResolveResponse, UpstreamSearchResponse,
};

pub const DEFAULT_API_BASE_URL: &str = "https://buguyy.top";

#[derive(Clone)]
pub struct MusicClient {
    client: Client,
    base_url: Url,
}

impl MusicClient {
    pub fn new(base_url: impl AsRef<str>) -> Result<Self> {
        let base_url = normalize_base_url(base_url.as_ref())?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("easy-music/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| EasyMusicError::upstream(error.to_string()))?;
        Ok(Self { client, base_url })
    }

    pub async fn search(&self, keyword: &str) -> Result<SearchResult> {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return Err(EasyMusicError::invalid("keyword must not be empty"));
        }

        let url = self.endpoint("api/search")?;
        let response = self
            .client
            .get(url)
            .query(&[("keyword", keyword)])
            .send()
            .await?
            .error_for_status()
            .map_err(|error| EasyMusicError::upstream(error.to_string()))?
            .json::<UpstreamSearchResponse>()
            .await
            .map_err(|error| {
                EasyMusicError::upstream(format!("invalid search response: {error}"))
            })?;

        if !response.success {
            return Err(EasyMusicError::upstream(
                "music search API returned success=false",
            ));
        }

        let tracks: Vec<Track> = response.data.into_iter().map(Track::from).collect();
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

    pub async fn resolve(&self, id: &str) -> Result<ResolvedTrack> {
        let id = id.trim();
        if id.is_empty() {
            return Err(EasyMusicError::invalid("track id must not be empty"));
        }

        let url = self.endpoint("api/geturl")?;
        let response = self
            .client
            .get(url)
            .query(&[("id", id)])
            .send()
            .await?
            .error_for_status()
            .map_err(|error| EasyMusicError::upstream(error.to_string()))?
            .json::<UpstreamResolveResponse>()
            .await
            .map_err(|error| {
                EasyMusicError::upstream(format!("invalid resolve response: {error}"))
            })?;

        if !response.success || response.url.trim().is_empty() {
            return Err(EasyMusicError::source(
                "music detail API did not return a playable URL",
            ));
        }
        validate_http_url(&response.url)?;

        Ok(ResolvedTrack {
            id: id.to_owned(),
            title: response.name,
            url: response.url,
        })
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        self.base_url
            .join(path)
            .map_err(|error| EasyMusicError::upstream(error.to_string()))
    }
}

fn normalize_base_url(value: &str) -> Result<Url> {
    let mut value = value.trim().to_owned();
    if !value.ends_with('/') {
        value.push('/');
    }
    let url = Url::parse(&value)
        .map_err(|error| EasyMusicError::invalid(format!("invalid API base URL: {error}")))?;
    validate_http_url(url.as_str())?;
    Ok(url)
}

pub fn validate_http_url(value: &str) -> Result<Url> {
    let url = Url::parse(value)?;
    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(EasyMusicError::source(format!(
                "unsupported URL scheme {scheme:?}; only http and https are allowed"
            )));
        }
    }
    if url.host_str().is_none() {
        return Err(EasyMusicError::source("audio URL has no host"));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_base_url() {
        let client = MusicClient::new("https://example.com/root").unwrap();
        assert_eq!(
            client.endpoint("api/search").unwrap().as_str(),
            "https://example.com/root/api/search"
        );
    }

    #[test]
    fn rejects_non_http_urls() {
        let error = validate_http_url("file:///etc/passwd").unwrap_err();
        assert!(error.message.contains("only http and https"));
    }
}
