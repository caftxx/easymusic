use async_trait::async_trait;
use reqwest::Client;
use url::Url;

use super::MusicProvider;
use crate::error::{EasyMusicError, Result};
use crate::model::{ResolvedTrack, SearchResult, Track};
use crate::network::validate_http_url;

const MEDIA_BASE_URL: &str = "https://ting789.yymp3.com/";

pub(super) struct Yymp3Provider {
    client: Client,
    base_url: Url,
}

impl Yymp3Provider {
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
impl MusicProvider for Yymp3Provider {
    fn name(&self) -> &str {
        "yymp3"
    }

    async fn search(&self, keyword: &str) -> Result<SearchResult> {
        let html = self
            .client
            .get(self.endpoint("search/")?)
            .query(&[("tp", "1"), ("key", keyword)])
            .send()
            .await?
            .error_for_status()
            .map_err(|error| EasyMusicError::upstream(error.to_string()))?
            .text()
            .await
            .map_err(|error| {
                EasyMusicError::upstream(format!("invalid YYMP3 search response: {error}"))
            })?;

        if !html.contains("searchResult") {
            return Err(EasyMusicError::upstream(
                "YYMP3 search returned an unexpected HTML response",
            ));
        }
        let tracks = parse_search_results(&html);
        let count = parse_result_count(&html)
            .unwrap_or(tracks.len())
            .max(tracks.len());
        Ok(SearchResult {
            ok: true,
            keyword: keyword.to_owned(),
            count,
            tracks,
        })
    }

    async fn resolve(&self, id: &str) -> Result<ResolvedTrack> {
        let script = self
            .client
            .get(self.endpoint("p/top.aspx")?)
            .query(&[("n", "1"), ("musicid", id)])
            .send()
            .await?
            .error_for_status()
            .map_err(|error| EasyMusicError::upstream(error.to_string()))?
            .text()
            .await
            .map_err(|error| {
                EasyMusicError::upstream(format!("invalid YYMP3 detail response: {error}"))
            })?;
        let record = parse_song_record(&script, id).ok_or_else(|| {
            EasyMusicError::source("YYMP3 did not return metadata for this track ID")
        })?;
        let url = media_url(&record.path)?;
        validate_http_url(&url)?;

        Ok(ResolvedTrack {
            id: id.to_owned(),
            title: record.title,
            url,
        })
    }
}

fn parse_search_results(html: &str) -> Vec<Track> {
    let lowercase = html.to_ascii_lowercase();
    let mut tracks = Vec::new();
    let mut offset = 0;

    while let Some(relative_start) = lowercase[offset..].find("<li") {
        let start = offset + relative_start;
        let Some(relative_end) = lowercase[start..].find("</li>") else {
            break;
        };
        let end = start + relative_end + "</li>".len();
        let block = &html[start..end];
        let Some((play_path, title)) = find_anchor(block, "/play/") else {
            offset = end;
            continue;
        };
        let Some(id) = track_id(&play_path) else {
            offset = end;
            continue;
        };
        if tracks.iter().any(|track: &Track| track.id == id) {
            offset = end;
            continue;
        }
        let artist = find_anchor(block, "/singer/")
            .map(|(_, artist)| artist)
            .unwrap_or_default();
        tracks.push(Track {
            id,
            title,
            artist,
            artwork_url: None,
        });
        offset = end;
    }
    tracks
}

fn find_anchor(block: &str, href_fragment: &str) -> Option<(String, String)> {
    let lowercase = block.to_ascii_lowercase();
    let path_start = lowercase.find(href_fragment)?;
    let quote = block[..path_start]
        .chars()
        .next_back()
        .filter(|quote| matches!(quote, '"' | '\''))?;
    let path_end = block[path_start..].find(quote)? + path_start;
    let href = block[path_start..path_end].to_owned();
    let text_start = block[path_end..].find('>')? + path_end + 1;
    let text_end = lowercase[text_start..].find("</a>")? + text_start;
    let text = normalize_html_text(&block[text_start..text_end]);
    Some((href, text))
}

fn track_id(path: &str) -> Option<String> {
    let file_name = path.split(['?', '#']).next()?.rsplit('/').next()?;
    let (id, extension) = file_name.rsplit_once('.')?;
    (extension.eq_ignore_ascii_case("htm")
        && !id.is_empty()
        && id.chars().all(|character| character.is_ascii_digit()))
    .then(|| id.to_owned())
}

fn normalize_html_text(value: &str) -> String {
    let mut text = String::new();
    let mut inside_tag = false;
    for character in value.chars() {
        match character {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => text.push(character),
            _ => {}
        }
    }
    let decoded = decode_html_entities(&text);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_html_entities(value: &str) -> String {
    let mut decoded = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(start) = remaining.find('&') {
        decoded.push_str(&remaining[..start]);
        remaining = &remaining[start..];
        let Some(end) = remaining.find(';').filter(|end| *end <= 10) else {
            decoded.push('&');
            remaining = &remaining[1..];
            continue;
        };
        let entity = &remaining[1..end];
        let replacement = match entity {
            "amp" => Some('&'),
            "apos" | "#39" => Some('\''),
            "gt" => Some('>'),
            "lt" => Some('<'),
            "nbsp" => Some(' '),
            "quot" => Some('"'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|value| u32::from_str_radix(value, 16).ok())
                .and_then(char::from_u32)
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|value| value.parse::<u32>().ok())
                        .and_then(char::from_u32)
                }),
        };
        if let Some(replacement) = replacement {
            decoded.push(replacement);
        } else {
            decoded.push_str(&remaining[..=end]);
        }
        remaining = &remaining[end + 1..];
    }
    decoded.push_str(remaining);
    decoded
}

fn parse_result_count(html: &str) -> Option<usize> {
    let marker = "</b>条记录";
    let marker_start = html.find(marker)?;
    let number_start = html[..marker_start].rfind("<b>")? + "<b>".len();
    html[number_start..marker_start].trim().parse().ok()
}

struct SongRecord {
    title: String,
    path: String,
}

fn parse_song_record(script: &str, expected_id: &str) -> Option<SongRecord> {
    let marker = format!("\"{expected_id}|");
    let start = script.find(&marker)? + 1;
    let encoded = extract_js_string(&script[start..])?;
    let decoded = decode_js_string(encoded);
    let fields = decoded.split('|').collect::<Vec<_>>();
    if fields.len() < 6 || fields[0] != expected_id {
        return None;
    }
    let title = fields[1].trim().to_owned();
    let path = fields[4].trim().to_owned();
    (!title.is_empty() && !path.is_empty()).then_some(SongRecord { title, path })
}

fn extract_js_string(value: &str) -> Option<&str> {
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if index == 0 {
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '"' => return Some(&value[..index]),
            _ => {}
        }
    }
    None
}

fn decode_js_string(value: &str) -> String {
    let mut decoded = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        match characters.next() {
            Some('n') => decoded.push('\n'),
            Some('r') => decoded.push('\r'),
            Some('t') => decoded.push('\t'),
            Some('\\') => decoded.push('\\'),
            Some('"') => decoded.push('"'),
            Some('\'') => decoded.push('\''),
            Some(other) => {
                decoded.push('\\');
                decoded.push(other);
            }
            None => decoded.push('\\'),
        }
    }
    decoded
}

fn media_url(path: &str) -> Result<String> {
    let path = path.trim();
    if path.contains("://") {
        return Err(EasyMusicError::source(
            "YYMP3 returned a media path outside its audio host",
        ));
    }
    let normalized = path.replace("//", "/");
    let normalized = normalized.trim_start_matches('/').to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(EasyMusicError::source(
            "YYMP3 returned an invalid media path",
        ));
    }
    let normalized = normalized
        .strip_suffix(".wma")
        .map(|path| format!("{path}.mp3"))
        .unwrap_or(normalized);
    let base =
        Url::parse(MEDIA_BASE_URL).map_err(|error| EasyMusicError::source(error.to_string()))?;
    let url = base
        .join(&normalized)
        .map_err(|error| EasyMusicError::source(error.to_string()))?;
    if url.scheme() != "https" || url.host_str() != base.host_str() {
        return Err(EasyMusicError::source(
            "YYMP3 returned a media path outside its audio host",
        ));
    }
    Ok(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_search_html() {
        let html = r#"
            <ul class="searchResult c">
              <li class="SR_header c"><div>歌曲名称</div></li>
              <li>
                <div class="p3"><a href="/Play/23350/268453.htm"><font color=red>刚好&amp;遇见你</font></a></div>
                <div class="p2"><a href="/Singer/7788.htm">李玉刚</a></div>
                <div class="p"><a href="/Play/23350/268453.htm">试听</a></div>
              </li>
            </ul>
            共找到：<b>刚好遇见你</b> <b>1</b>条记录
        "#;
        let tracks = parse_search_results(html);
        assert_eq!(
            tracks,
            vec![Track {
                id: "268453".to_owned(),
                title: "刚好&遇见你".to_owned(),
                artist: "李玉刚".to_owned(),
                artwork_url: None,
            }]
        );
        assert_eq!(parse_result_count(html), Some(1));
    }

    #[test]
    fn parses_song_script_and_builds_media_url() {
        let script = r#"var dc=$song_data[1];if($song_data[1].indexOf("268453|刚好遇见你|7788|李玉刚|new27/liyugang6/6.WMA|23350||")==-1){}"#;
        let record = parse_song_record(script, "268453").unwrap();
        assert_eq!(record.title, "刚好遇见你");
        assert_eq!(
            media_url(&record.path).unwrap(),
            "https://ting789.yymp3.com/new27/liyugang6/6.mp3"
        );
    }

    #[test]
    fn rejects_media_paths_that_change_hosts() {
        let error = media_url("https://example.com/song.wma").unwrap_err();
        assert!(error.message.contains("outside its audio host"));
    }

    #[test]
    fn accepts_empty_search_results() {
        let html = r#"<ul class="searchResult c"></ul>共找到：<b>x</b> <b>0</b>条记录"#;
        assert!(parse_search_results(html).is_empty());
        assert_eq!(parse_result_count(html), Some(0));
    }
}
