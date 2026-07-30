use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use url::Url;

use super::MusicProvider;
use crate::error::{EasyMusicError, Result};
use crate::model::{ResolvedTrack, SearchResult, Track};
use crate::network::validate_http_url;

const API_BASE_URL: &str = "https://api-qianqian.91q.com/v1/";
const APP_ID: &str = "16073360";
const SIGNING_SECRET: &str = "0b50b02fd0d73a9c4c8c3a781c30845f";
const DEFAULT_RATE: &str = "320";

pub(super) struct QianqianProvider {
    client: Client,
}

impl QianqianProvider {
    pub(super) fn new(client: Client) -> Self {
        Self { client }
    }

    fn endpoint(path: &str) -> Result<Url> {
        Url::parse(API_BASE_URL)
            .map_err(|error| EasyMusicError::upstream(error.to_string()))?
            .join(path)
            .map_err(|error| EasyMusicError::upstream(error.to_string()))
    }

    async fn request<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        params: Vec<(String, String)>,
        operation: &str,
    ) -> Result<T> {
        let response = self
            .client
            .get(Self::endpoint(path)?)
            .header("from", "web")
            .header("referer", "https://music.91q.com/")
            .query(&params)
            .send()
            .await?
            .error_for_status()
            .map_err(|error| EasyMusicError::upstream(error.to_string()))?
            .json::<Envelope>()
            .await
            .map_err(|error| {
                EasyMusicError::upstream(format!("invalid 91Q {operation} response: {error}"))
            })?;
        response.data(operation)
    }
}

#[async_trait]
impl MusicProvider for QianqianProvider {
    fn name(&self) -> &str {
        "qianqian"
    }

    async fn search(&self, keyword: &str) -> Result<SearchResult> {
        let data: SearchData = self
            .request(
                "search",
                signed_params(
                    [
                        ("pageNo", "1"),
                        ("pageSize", "20"),
                        ("type", "1"),
                        ("word", keyword),
                    ],
                    unix_timestamp()?,
                ),
                "search",
            )
            .await?;
        let tracks = data
            .tracks
            .into_iter()
            .filter_map(SearchTrack::into_track)
            .collect::<Vec<_>>();
        Ok(SearchResult {
            ok: true,
            keyword: keyword.to_owned(),
            count: data.total.max(tracks.len()),
            tracks,
        })
    }

    async fn resolve(&self, id: &str) -> Result<ResolvedTrack> {
        let data: TracklinkData = self
            .request(
                "song/tracklink",
                signed_params([("TSID", id), ("rate", DEFAULT_RATE)], unix_timestamp()?),
                "tracklink",
            )
            .await?;
        let url = data.path.filter(|url| !url.trim().is_empty()).ok_or_else(|| {
            EasyMusicError::source(if data.is_vip {
                "91Q did not return a full track URL; this track requires an authorized VIP account"
            } else {
                "91Q did not return a playable URL for this track"
            })
        })?;
        validate_http_url(&url)?;

        Ok(ResolvedTrack {
            id: id.to_owned(),
            title: data.title,
            url,
        })
    }
}

fn unix_timestamp() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| {
            EasyMusicError::upstream(format!("system clock is before Unix epoch: {error}"))
        })
}

fn signed_params<'a>(
    params: impl IntoIterator<Item = (&'a str, &'a str)>,
    timestamp: u64,
) -> Vec<(String, String)> {
    let mut params = params
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect::<BTreeMap<_, _>>();
    params.insert("appid".to_owned(), APP_ID.to_owned());
    params.insert("timestamp".to_owned(), timestamp.to_string());

    let unsigned = params
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    let sign = format!("{:x}", md5::compute(format!("{unsigned}{SIGNING_SECRET}")));
    params.insert("sign".to_owned(), sign);
    params.into_iter().collect()
}

#[derive(Debug, Deserialize)]
struct Envelope {
    state: bool,
    #[serde(default)]
    errno: i64,
    #[serde(default)]
    errmsg: String,
    data: serde_json::Value,
}

impl Envelope {
    fn data<T: for<'de> Deserialize<'de>>(self, operation: &str) -> Result<T> {
        if !self.state {
            let message = if self.errmsg.trim().is_empty() {
                format!("91Q {operation} API returned errno={}", self.errno)
            } else {
                format!(
                    "91Q {operation} API returned errno={}: {}",
                    self.errno, self.errmsg
                )
            };
            return Err(EasyMusicError::upstream(message));
        }
        serde_json::from_value(self.data).map_err(|error| {
            EasyMusicError::upstream(format!("invalid 91Q {operation} data: {error}"))
        })
    }
}

#[derive(Debug, Deserialize)]
struct SearchData {
    #[serde(default)]
    total: usize,
    #[serde(default, rename = "typeTrack")]
    tracks: Vec<SearchTrack>,
}

#[derive(Debug, Deserialize)]
struct SearchTrack {
    #[serde(default, rename = "TSID")]
    tsid: Option<String>,
    #[serde(default, rename = "assetId")]
    asset_id: Option<String>,
    #[serde(default)]
    id: Option<String>,
    title: String,
    #[serde(default, rename = "artist")]
    artists: Vec<Artist>,
    #[serde(default)]
    pic: Option<String>,
}

impl SearchTrack {
    fn into_track(self) -> Option<Track> {
        let id = self
            .tsid
            .or(self.asset_id)
            .or(self.id)
            .filter(|id| !id.trim().is_empty())?;
        Some(Track {
            id,
            title: self.title,
            artist: self
                .artists
                .into_iter()
                .map(|artist| artist.name)
                .filter(|name| !name.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" / "),
            artwork_url: self.pic.filter(|url| !url.trim().is_empty()),
        })
    }
}

#[derive(Debug, Deserialize)]
struct Artist {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct TracklinkData {
    #[serde(default)]
    title: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default, rename = "isVip", deserialize_with = "deserialize_boolish")]
    is_vip: bool,
}

fn deserialize_boolish<'de, D>(deserializer: D) -> std::result::Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Boolish {
        Bool(bool),
        Integer(i64),
    }

    Ok(match Boolish::deserialize(deserializer)? {
        Boolish::Bool(value) => value,
        Boolish::Integer(value) => value != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_parameters_before_url_encoding() {
        let params = signed_params(
            [
                ("pageNo", "1"),
                ("pageSize", "20"),
                ("type", "1"),
                ("word", "天地龙鳞"),
            ],
            1_700_000_000,
        )
        .into_iter()
        .collect::<BTreeMap<_, _>>();

        assert_eq!(params["appid"], "16073360");
        assert_eq!(params["timestamp"], "1700000000");
        assert_eq!(params["sign"], "136e4c3b85dc4f46b1c5297a717b0048");
    }

    #[test]
    fn parses_search_and_tracklink_payloads() {
        let search: SearchData = serde_json::from_value(serde_json::json!({
            "total": 1,
            "typeTrack": [{
                "TSID": "T10062480746",
                "assetId": "T10062480746",
                "id": "T10062480746",
                "title": "天地龙鳞",
                "artist": [{"name": "王力宏"}],
                "pic": "https://example.com/cover.jpg"
            }]
        }))
        .unwrap();
        assert_eq!(search.total, 1);
        let track = search
            .tracks
            .into_iter()
            .next()
            .unwrap()
            .into_track()
            .unwrap();
        assert_eq!(track.id, "T10062480746");
        assert_eq!(track.artist, "王力宏");

        let tracklink: TracklinkData = serde_json::from_value(serde_json::json!({
            "title": "天地龙鳞",
            "path": "https://audio.example.com/song.mp3",
            "isVip": 0
        }))
        .unwrap();
        assert_eq!(
            tracklink.path.as_deref(),
            Some("https://audio.example.com/song.mp3")
        );
        assert!(!tracklink.is_vip);
    }
}
