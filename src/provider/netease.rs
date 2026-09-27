//! NetEase Cloud Music (网易云音乐) source.
//!
//! Search uses the open web search API; playback resolution uses the
//! 128k mp3 outer-link endpoint, which answers with a redirect to a
//! playable CDN URL. No eapi encryption is involved. The endpoint choices
//! follow widely documented community implementations (see
//! THIRD-PARTY-NOTICES.md); this code is written for easymusic.

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::{EasyMusicError, Result};
use crate::network::validate_http_url;
use crate::provider::{
    DEFAULT_USER_AGENT, MusicSource, build_http_client, clean_text, is_http_url, non_empty,
    upstream_error,
};
use crate::provider::{SourceResolvedTrack, SourceTrack};
use crate::track_id::NativeTrackId;

const SEARCH_URL: &str = "https://music.163.com/api/search/get/web";
const DETAIL_URL: &str = "https://music.163.com/api/song/detail/";
const OUTER_URL: &str = "https://music.163.com/song/media/outer/url";

pub(crate) struct NeteaseSource {
    http: reqwest::Client,
}

impl NeteaseSource {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            http: build_http_client(DEFAULT_USER_AGENT, "https://music.163.com/", false)?,
        })
    }

    fn outer_url(id: &str) -> String {
        format!("{OUTER_URL}?id={id}&type=mp3")
    }

    /// Best-effort song name lookup so resolved tracks carry a readable
    /// title; the outer-link endpoint itself returns no metadata.
    async fn song_title(&self, native_id: &str) -> Option<String> {
        let ids = format!(r#"["{native_id}"]"#);
        let response = self
            .http
            .get(DETAIL_URL)
            .query(&[("id", native_id), ("ids", ids.as_str())])
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body = response.text().await.ok()?;
        parse_song_title(native_id, &body)
    }
}

#[derive(Debug, Deserialize)]
struct DetailResponse {
    songs: Option<Vec<RawSong>>,
}

fn parse_song_title(native_id: &str, body: &str) -> Option<String> {
    let detail: DetailResponse = serde_json::from_str(body).ok()?;
    let song = detail.songs?.into_iter().next()?;
    // Guard against an endpoint echoing a different song for the requested id.
    if song.id.is_some_and(|id| id.to_string() != native_id) {
        return None;
    }
    song.name.as_deref().and_then(clean_non_empty)
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    code: Option<i64>,
    result: Option<SearchResults>,
}

#[derive(Debug, Deserialize)]
struct SearchResults {
    #[serde(default)]
    songs: Vec<RawSong>,
}

#[derive(Debug, Deserialize)]
struct RawSong {
    id: Option<i64>,
    name: Option<String>,
    #[serde(default)]
    artists: Vec<RawArtist>,
    #[serde(default)]
    ar: Vec<RawArtist>,
    album: Option<RawAlbum>,
    #[serde(rename = "picUrl")]
    pic_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawArtist {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawAlbum {
    #[serde(rename = "picUrl")]
    pic_url: Option<String>,
}

fn parse_search_response(body: &str) -> Result<Vec<SourceTrack>> {
    let response: SearchResponse = serde_json::from_str(body).map_err(|error| {
        EasyMusicError::upstream(format!("netease returned invalid search JSON: {error}"))
    })?;
    if !matches!(response.code, Some(200) | None) {
        return Err(EasyMusicError::upstream(format!(
            "netease search returned abnormal code={}",
            response.code.unwrap_or(-1)
        )));
    }
    let songs = response
        .result
        .map(|result| result.songs)
        .unwrap_or_default();
    let mut tracks = Vec::with_capacity(songs.len());
    for song in songs {
        let Some(id) = song.id.filter(|id| *id > 0) else {
            continue;
        };
        let Some(title) = song.name.as_deref().and_then(clean_non_empty) else {
            continue;
        };
        let artist = song
            .artists
            .iter()
            .chain(song.ar.iter())
            .filter_map(|artist| artist.name.as_deref().and_then(clean_non_empty))
            .collect::<Vec<_>>()
            .join(" / ");
        let artist = if artist.is_empty() {
            "未知歌手".to_owned()
        } else {
            artist
        };
        let cover = song
            .album
            .as_ref()
            .and_then(|album| album.pic_url.as_deref())
            .or(song.pic_url.as_deref())
            .and_then(clean_non_empty)
            .filter(|cover| is_http_url(cover));
        tracks.push(SourceTrack {
            id: NativeTrackId::new(id.to_string()),
            title,
            artist,
            artwork_url: cover,
        });
    }
    Ok(tracks)
}

fn clean_non_empty(value: &str) -> Option<String> {
    non_empty(&clean_text(value))
}

fn parse_resolve_response(
    native_id: &str,
    outer_url: &str,
    status: reqwest::StatusCode,
    location: Option<&str>,
    body_len: usize,
) -> Result<SourceResolvedTrack> {
    let url = if status.is_redirection() {
        let location = location.ok_or_else(|| {
            EasyMusicError::source("netease track is unavailable (possibly VIP or removed)")
        })?;
        let parsed = validate_http_url(location).map_err(|_| {
            EasyMusicError::source("netease track is unavailable (possibly VIP or removed)")
        })?;
        // Unplayable songs (VIP, removed, region-locked) redirect back to a
        // site page such as https://music.163.com/404 instead of a CDN host.
        if parsed
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("music.163.com"))
        {
            return Err(EasyMusicError::source(
                "netease track is unavailable (possibly VIP or removed)",
            ));
        }
        location.to_owned()
    } else if status == reqwest::StatusCode::OK && body_len > 0 {
        // Some deployments answer 200 with the audio bytes directly; the
        // outer URL itself remains the canonical playable address.
        outer_url.to_owned()
    } else {
        return Err(EasyMusicError::source(
            "netease track is unavailable (possibly VIP or removed)",
        ));
    };
    Ok(SourceResolvedTrack {
        id: NativeTrackId::new(native_id),
        title: native_id.to_owned(),
        url,
        extension: Some("mp3".to_owned()),
    })
}

#[async_trait]
impl MusicSource for NeteaseSource {
    fn name(&self) -> &'static str {
        "netease"
    }

    fn display_name(&self) -> &'static str {
        "网易云音乐"
    }

    async fn search(&self, keyword: &str, limit: usize) -> Result<Vec<SourceTrack>> {
        let response = self
            .http
            .get(SEARCH_URL)
            .query(&[
                ("csrf_token", ""),
                ("s", keyword),
                ("type", "1"),
                ("offset", "0"),
                ("limit", &limit.to_string()),
            ])
            .send()
            .await
            .map_err(|error| upstream_error("netease", "search", error))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| upstream_error("netease", "search", error))?;
        if !status.is_success() {
            return Err(EasyMusicError::upstream(format!(
                "netease search failed with HTTP {status}"
            )));
        }
        parse_search_response(&body)
    }

    async fn resolve(&self, native: &NativeTrackId) -> Result<SourceResolvedTrack> {
        let id = native.as_str();
        let native_id = id.trim();
        if native_id.is_empty() || !native_id.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(EasyMusicError::invalid(
                "netease track id must be a numeric song id",
            ));
        }
        let outer_url = NeteaseSource::outer_url(native_id);
        let mut response = self
            .http
            .get(&outer_url)
            .send()
            .await
            .map_err(|error| upstream_error("netease", "audio URL resolution", error))?;
        let status = response.status();
        // The redirect case must not buffer the body; only a 200 response is
        // read (and capped) because it carries the audio bytes themselves.
        let body_len = if status == reqwest::StatusCode::OK {
            response
                .chunk()
                .await
                .map_err(|error| upstream_error("netease", "audio URL resolution", error))?
                .map(|chunk| chunk.len())
                .unwrap_or(0)
        } else {
            0
        };
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut resolved =
            parse_resolve_response(native_id, &outer_url, status, location.as_deref(), body_len)?;
        if let Some(title) = self.song_title(native_id).await {
            resolved.title = title;
        }
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_netease_search_response() {
        let body = r#"{
            "code": 200,
            "result": {
                "songLength": 0,
                "songs": [
                    {
                        "id": 186016,
                        "name": "晴天",
                        "artists": [{"id": 7, "name": "周杰伦"}],
                        "album": {"name": "叶惠美", "picUrl": "http://p3.music.126.net/cover.jpg"},
                        "duration": 269000
                    },
                    {"id": 0, "name": "missing id"},
                    {"id": 2, "name": "  "}
                ]
            }
        }"#;

        let result = parse_search_response(body).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id.as_str(), "186016");
        assert_eq!(result[0].title, "晴天");
        assert_eq!(result[0].artist, "周杰伦");
        assert_eq!(
            result[0].artwork_url.as_deref(),
            Some("http://p3.music.126.net/cover.jpg")
        );
    }

    #[test]
    fn rejects_abnormal_codes_and_invalid_json() {
        let error = parse_search_response(r#"{"code": 500}"#).unwrap_err();
        assert!(matches!(error.code, crate::error::ErrorCode::UpstreamApi));
        assert!(parse_search_response("not json").is_err());
    }

    #[test]
    fn parses_song_detail_title() {
        let body =
            r#"{"songs":[{"name":"晴天","id":3440441479,"artists":[{"name":"Jay"}]}],"code":200}"#;
        assert_eq!(
            parse_song_title("3440441479", body).as_deref(),
            Some("晴天")
        );
        // A mismatched echo is ignored and leaves the id as the title.
        assert_eq!(parse_song_title("999", body), None);
        assert_eq!(parse_song_title("1", "not json"), None);
    }

    #[test]
    fn resolve_prefers_redirect_location() {
        let resolved = parse_resolve_response(
            "186016",
            "https://music.163.com/song/media/outer/url?id=186016&type=mp3",
            reqwest::StatusCode::FOUND,
            Some("http://m10.music.126.net/2026/song.mp3"),
            0,
        )
        .unwrap();
        assert_eq!(resolved.id.as_str(), "186016");
        assert!(resolved.url.starts_with("http://m10.music.126.net/"));
        assert_eq!(resolved.extension.as_deref(), Some("mp3"));
    }

    #[test]
    fn resolve_rejects_site_error_page_redirects() {
        let error = parse_resolve_response(
            "186016",
            "https://music.163.com/song/media/outer/url?id=186016&type=mp3",
            reqwest::StatusCode::FOUND,
            Some("http://music.163.com/404"),
            0,
        )
        .unwrap_err();
        assert!(error.message.contains("unavailable"));
    }

    #[test]
    fn resolve_rejects_vip_or_broken_links() {
        let no_location = parse_resolve_response(
            "1",
            "https://music.163.com/song/media/outer/url?id=1&type=mp3",
            reqwest::StatusCode::FOUND,
            None,
            0,
        )
        .unwrap_err();
        assert!(no_location.message.contains("unavailable"));

        let direct_body = parse_resolve_response(
            "1",
            "https://music.163.com/song/media/outer/url?id=1&type=mp3",
            reqwest::StatusCode::OK,
            None,
            4096,
        )
        .unwrap();
        assert!(direct_body.url.contains("outer/url"));

        let not_found = parse_resolve_response(
            "1",
            "https://music.163.com/song/media/outer/url?id=1&type=mp3",
            reqwest::StatusCode::NOT_FOUND,
            None,
            0,
        )
        .unwrap_err();
        assert!(not_found.message.contains("unavailable"));
    }
}
