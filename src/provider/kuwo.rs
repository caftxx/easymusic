//! Kuwo Music (酷我音乐) source.
//!
//! Search hits `search.kuwo.cn/r.s`, which answers with a Python literal
//! dict (single quotes, `True`/`False`/`None`) rather than JSON; the mini
//! translator below turns it into JSON before parsing. Playable URLs come
//! from `antiserver.kuwo.cn/anti.s`, which returns a bare mp3 URL or a small
//! JSON object with a `url` field. The endpoint choices follow widely
//! documented community implementations (see THIRD-PARTY-NOTICES.md); this
//! code is written for easymusic.

use async_trait::async_trait;
use serde_json::{Map, Value};

use crate::error::{EasyMusicError, Result};
use crate::network::validate_http_url;
use crate::provider::{
    DEFAULT_USER_AGENT, MusicSource, build_http_client, clean_text, is_http_url, non_empty,
    upstream_error,
};
use crate::provider::{SourceResolvedTrack, SourceTrack};
use crate::track_id::NativeTrackId;

const SEARCH_URL: &str = "https://search.kuwo.cn/r.s";
const PLAY_URL: &str = "https://antiserver.kuwo.cn/anti.s";

pub(crate) struct KuwoSource {
    http: reqwest::Client,
}

impl KuwoSource {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            http: build_http_client(DEFAULT_USER_AGENT, "https://www.kuwo.cn/", true)?,
        })
    }
}

#[async_trait]
impl MusicSource for KuwoSource {
    fn name(&self) -> &'static str {
        "kuwo"
    }

    fn display_name(&self) -> &'static str {
        "酷我音乐"
    }

    async fn search(&self, keyword: &str, limit: usize) -> Result<Vec<SourceTrack>> {
        let response = self
            .http
            .get(SEARCH_URL)
            .query(&[
                ("all", keyword),
                ("ft", "music"),
                ("itemset", "web_2013"),
                ("client", "kt"),
                ("pn", "0"),
                ("rn", &limit.to_string()),
                ("rformat", "json"),
                ("encoding", "utf8"),
            ])
            .send()
            .await
            .map_err(|error| upstream_error("kuwo", "search", error))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| upstream_error("kuwo", "search", error))?;
        if !status.is_success() {
            return Err(EasyMusicError::upstream(format!(
                "kuwo search failed with HTTP {status}"
            )));
        }
        parse_search_response(&body)
    }

    async fn resolve(&self, native: &NativeTrackId) -> Result<SourceResolvedTrack> {
        let id = native.as_str();
        let native_id = id.trim();
        let rid = extract_rid(native_id).ok_or_else(|| {
            EasyMusicError::invalid("kuwo track id must reference an alphanumeric song rid")
        })?;
        // Paid-only tracks sometimes expose an aac variant when mp3 fails.
        let mut last_error = String::from("kuwo returned no playable URL");
        for format in ["mp3", "aac"] {
            match self.convert_url(rid, format).await {
                Ok(url) => {
                    validate_http_url(&url)?;
                    return Ok(SourceResolvedTrack {
                        id: NativeTrackId::new(native_id),
                        title: native_id.to_owned(),
                        url,
                        extension: Some(format.to_owned()),
                    });
                }
                Err(error) => last_error = error.message,
            }
        }
        Err(EasyMusicError::source(format!(
            "kuwo track is unavailable (possibly paid): {last_error}"
        )))
    }
}

impl KuwoSource {
    async fn convert_url(&self, rid: &str, format: &str) -> Result<String> {
        let response = self
            .http
            .get(PLAY_URL)
            .query(&[
                ("type", "convert_url3"),
                ("rid", rid),
                ("format", format),
                ("response", "url"),
            ])
            .send()
            .await
            .map_err(|error| upstream_error("kuwo", "audio URL resolution", error))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| upstream_error("kuwo", "audio URL resolution", error))?;
        if !status.is_success() {
            return Err(EasyMusicError::source(format!(
                "kuwo play API failed with HTTP {status}"
            )));
        }
        extract_play_url(&text)
    }
}

fn extract_rid(native_id: &str) -> Option<&str> {
    let rid = native_id
        .strip_prefix("Kuwo_song_")
        .or_else(|| native_id.strip_prefix("MUSIC_"))
        .unwrap_or(native_id)
        .trim();
    (!rid.is_empty() && rid.bytes().all(|byte| byte.is_ascii_alphanumeric())).then_some(rid)
}

fn extract_play_url(text: &str) -> Result<String> {
    let text = text.trim();
    let candidate = if text.starts_with('{') {
        let value: Value = serde_json::from_str(text).map_err(|error| {
            EasyMusicError::source(format!("kuwo returned invalid JSON: {error}"))
        })?;
        value
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    } else {
        text.to_owned()
    };
    non_empty(&candidate)
        .filter(|url| !matches!(url.to_ascii_lowercase().as_str(), "null" | "none" | "error"))
        .ok_or_else(|| EasyMusicError::source("kuwo returned no playable URL"))
}

fn parse_search_response(body: &str) -> Result<Vec<SourceTrack>> {
    let value = python_literal_to_json(body).map_err(|error| {
        EasyMusicError::upstream(format!(
            "kuwo returned an unparsable search payload: {error}"
        ))
    })?;
    let items = value
        .get("abslist")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut tracks = Vec::with_capacity(items.len());
    for item in items {
        let rid = field_str(&item, ["MUSICRID", "musicrid", "SONGID", "songid"]);
        let Some(rid) = rid.filter(|rid| !rid.is_empty()) else {
            continue;
        };
        let Some(title) = field_str(&item, ["NAME", "SONGTITLE", "SONGNAME"])
            .map(|title| clean_text(&title))
            .and_then(|title| non_empty(&title))
        else {
            continue;
        };
        let artist = field_str(&item, ["ARTIST", "artist"])
            .map(|artist| clean_text(&artist))
            .and_then(|artist| non_empty(&artist))
            .unwrap_or_else(|| "未知歌手".to_owned());
        let picture = field_str(
            &item,
            [
                "PICPATH",
                "web_albumpic_short",
                "WEB_ALBUM_PIC",
                "PICPATH_NEW",
            ],
        )
        .map(|picture| clean_text(&picture))
        .filter(|picture| is_http_url(picture));
        tracks.push(SourceTrack {
            id: NativeTrackId::new(rid),
            title,
            artist,
            artwork_url: picture,
        });
    }
    Ok(tracks)
}

fn field_str<const N: usize>(item: &Value, keys: [&str; N]) -> Option<String> {
    keys.into_iter().find_map(|key| {
        item.get(key).and_then(|value| match value {
            Value::String(text) => non_empty(text),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
    })
}

/// Translate a Python literal (`{'a': [1, True, None, '文本']}`) into a
/// `serde_json::Value`. Handles the subset the kuwo API emits: dicts, lists,
/// single/double-quoted strings with escapes, ints, and True/False/None.
fn python_literal_to_json(text: &str) -> std::result::Result<Value, String> {
    let mut parser = LiteralParser {
        chars: &text.chars().collect::<Vec<_>>(),
        pos: 0,
    };
    parser.skip_ws();
    let value = parser.parse_value()?;
    parser.skip_ws();
    if parser.pos != parser.chars.len() {
        return Err(format!("trailing characters at offset {}", parser.pos));
    }
    Ok(value)
}

struct LiteralParser<'a> {
    chars: &'a [char],
    pos: usize,
}

impl<'a> LiteralParser<'a> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next_char(&mut self) -> Option<char> {
        let ch = self.peek();
        if ch.is_some() {
            self.pos += 1;
        }
        ch
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(|ch| ch.is_whitespace()) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, ch: char) -> std::result::Result<(), String> {
        if self.next_char() == Some(ch) {
            Ok(())
        } else {
            Err(format!("expected {ch:?} at offset {}", self.pos))
        }
    }

    fn parse_value(&mut self) -> std::result::Result<Value, String> {
        match self.peek() {
            Some('{') => self.parse_map(),
            Some('[') => self.parse_list(),
            Some('\'') | Some('"') => self.parse_string().map(Value::String),
            Some('-') | Some('0'..='9') => self.parse_number(),
            Some(_) if self.matches_keyword("True") => {
                self.pos += 4;
                Ok(Value::Bool(true))
            }
            Some(_) if self.matches_keyword("False") => {
                self.pos += 5;
                Ok(Value::Bool(false))
            }
            Some(_) if self.matches_keyword("None") => {
                self.pos += 4;
                Ok(Value::Null)
            }
            Some(ch) => Err(format!(
                "unexpected character {ch:?} at offset {}",
                self.pos
            )),
            None => Err("unexpected end of input".to_owned()),
        }
    }

    fn matches_keyword(&self, keyword: &str) -> bool {
        keyword
            .chars()
            .enumerate()
            .all(|(index, ch)| self.chars.get(self.pos + index) == Some(&ch))
    }

    fn parse_map(&mut self) -> std::result::Result<Value, String> {
        self.expect('{')?;
        let mut map = Map::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(Value::Object(map));
        }
        loop {
            self.skip_ws();
            let key = match self.peek() {
                Some('\'') | Some('"') => self.parse_string()?,
                Some(ch) => return Err(format!("expected string key, found {ch:?}")),
                None => return Err("unterminated dict".to_owned()),
            };
            self.skip_ws();
            self.expect(':')?;
            self.skip_ws();
            let value = self.parse_value()?;
            map.insert(key, value);
            self.skip_ws();
            match self.next_char() {
                Some(',') => continue,
                Some('}') => return Ok(Value::Object(map)),
                other => return Err(format!("expected ',' or '}}', found {other:?}")),
            }
        }
    }

    fn parse_list(&mut self) -> std::result::Result<Value, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.next_char() {
                Some(',') => continue,
                Some(']') => return Ok(Value::Array(items)),
                other => return Err(format!("expected ',' or ']', found {other:?}")),
            }
        }
    }

    fn parse_string(&mut self) -> std::result::Result<String, String> {
        let quote = self.next_char().ok_or("unterminated string")?;
        let mut out = String::new();
        loop {
            match self.next_char() {
                None => return Err("unterminated string".to_owned()),
                Some(ch) if ch == quote => return Ok(out),
                Some('\\') => {
                    let escaped = self.next_char().ok_or("unterminated escape")?;
                    match escaped {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'b' => out.push('\u{08}'),
                        'f' => out.push('\u{0c}'),
                        'u' => out.push(self.parse_unicode_escape()?),
                        'x' => {
                            let code = self.parse_hex(2)?;
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        other => out.push(other),
                    }
                }
                Some(ch) => out.push(ch),
            }
        }
    }

    fn parse_unicode_escape(&mut self) -> std::result::Result<char, String> {
        let code = self.parse_hex(4)?;
        // Combine surrogate pairs the way Python literal data may contain them.
        if (0xD800..0xDC00).contains(&code) {
            if self.peek() == Some('\\') {
                let save = self.pos;
                self.pos += 1;
                if self.next_char() == Some('u') {
                    let low = self.parse_hex(4)?;
                    if (0xDC00..0xE000).contains(&low) {
                        let combined = 0x1_0000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                        return char::from_u32(combined).ok_or("invalid surrogate pair".to_owned());
                    }
                }
                self.pos = save;
            }
            return Ok('\u{fffd}');
        }
        char::from_u32(code).ok_or("invalid unicode escape".to_owned())
    }

    fn parse_hex(&mut self, length: usize) -> std::result::Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..length {
            let digit = self
                .next_char()
                .and_then(|ch| ch.to_digit(16))
                .ok_or("invalid hex escape")?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn parse_number(&mut self) -> std::result::Result<Value, String> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
            self.pos += 1;
        }
        let digits: String = self.chars[start..self.pos].iter().collect();
        digits
            .parse::<i64>()
            .map(|number| Value::Number(number.into()))
            .map_err(|_| format!("invalid number at offset {start}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_python_literal_search_payload() {
        let body = r"{'PID': [2323, 2], 'ERRTYPE': 0, 'abslist': [{'SONGID': '4518370', 'MUSICRID': 'Kuwo_song_4518370', 'NAME': '<mark>晴天</mark>', 'ARTIST': '周杰伦\\u0026乐队', 'DURATION': '269', 'ALBUM': '叶惠美', 'PICPATH': 'http://img4.kuwo.cn/star/albumcover/120/s3.53.jpg'}, {'SONGID': 0, 'NAME': '', 'MUSICRID': ''}, {'MUSICRID': 'Kuwo_song_2', 'NAME': '没有歌手'}], 'total': '12'}";

        let result = parse_search_response(body).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id.as_str(), "Kuwo_song_4518370");
        assert_eq!(result[0].title, "晴天");
        assert_eq!(result[0].artist, "周杰伦&乐队");
        assert_eq!(
            result[0].artwork_url.as_deref(),
            Some("http://img4.kuwo.cn/star/albumcover/120/s3.53.jpg")
        );
        assert_eq!(result[1].artist, "未知歌手");
    }

    #[test]
    fn literal_parser_handles_escapes_and_keywords() {
        let value =
            python_literal_to_json(r"{'a': True, 'b': None, 'c': False, 'd': 'x\'y\\z', 'e': -12}")
                .unwrap();
        assert_eq!(value["a"], Value::Bool(true));
        assert_eq!(value["b"], Value::Null);
        assert_eq!(value["c"], Value::Bool(false));
        assert_eq!(value["d"], Value::String(r"x'y\z".to_owned()));
        assert_eq!(value["e"], Value::from(-12));
    }

    #[test]
    fn literal_parser_rejects_garbage() {
        assert!(python_literal_to_json("not a dict").is_err());
        assert!(python_literal_to_json("{'unterminated").is_err());
    }

    #[test]
    fn extracts_plain_and_json_play_urls() {
        assert_eq!(
            extract_play_url("http://ns.sfkuvod.mp3.cn/song.mp3?sign=1").unwrap(),
            "http://ns.sfkuvod.mp3.cn/song.mp3?sign=1"
        );
        assert_eq!(
            extract_play_url(r#"{"url":"https://www.kuwo.cn/song.m4a"}"#).unwrap(),
            "https://www.kuwo.cn/song.m4a"
        );
        assert!(extract_play_url("").is_err());
        assert!(extract_play_url("null").is_err());
        assert!(extract_play_url(r#"{"msg":"no url"}"#).is_err());
    }

    #[test]
    fn rejects_non_alphanumeric_rids_before_request() {
        // Rid validation happens before any network access.
        assert_eq!(extract_rid("4518370"), Some("4518370"));
        assert_eq!(extract_rid("MUSIC_51685512"), Some("51685512"));
        assert_eq!(extract_rid("Kuwo_song_4518370"), Some("4518370"));
        assert_eq!(extract_rid("../../etc/passwd"), None);
        assert_eq!(extract_rid(""), None);
    }
}
