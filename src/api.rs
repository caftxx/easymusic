use std::path::{Path, PathBuf};

use crate::error::{EasyMusicError, Result};
use crate::model::{ResolvedTrack, SearchResult};
use crate::ytdlp::YtDlp;

pub use crate::network::validate_http_url;

const DEFAULT_SEARCH_LIMIT: usize = 20;
const MAX_SEARCH_LIMIT: usize = 50;

/// Configuration for the external `yt-dlp` process.
#[derive(Debug, Clone)]
pub struct YtDlpConfig {
    /// Path to the standalone `yt-dlp` executable.
    pub executable: PathBuf,
    /// Optional value passed to `yt-dlp --js-runtimes`, such as
    /// `quickjs:/opt/easymusic/qjs` or `deno:/opt/easymusic/deno`.
    pub js_runtime: Option<String>,
    /// Optional Netscape-format cookies file used for restricted YouTube
    /// requests. This does not require a browser on the server.
    pub cookies: Option<PathBuf>,
}

impl Default for YtDlpConfig {
    fn default() -> Self {
        Self {
            executable: default_yt_dlp_executable(),
            js_runtime: None,
            cookies: None,
        }
    }
}

/// Music search and audio URL resolution backed exclusively by `yt-dlp`.
#[derive(Clone)]
pub struct MusicClient {
    backend: YtDlp,
}

impl MusicClient {
    /// Discover a bundled `yt-dlp` executable next to `easymusic`, falling
    /// back to `PATH` when no bundled executable exists.
    pub fn new() -> Self {
        Self::with_config(YtDlpConfig::default())
    }

    pub fn with_config(config: YtDlpConfig) -> Self {
        Self {
            backend: YtDlp::new(config),
        }
    }

    pub fn yt_dlp_executable(&self) -> &Path {
        self.backend.executable()
    }

    pub fn js_runtime(&self) -> Option<&str> {
        self.backend.js_runtime()
    }

    pub async fn search(&self, keyword: &str, limit: usize) -> Result<SearchResult> {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return Err(EasyMusicError::invalid("keyword must not be empty"));
        }
        if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
            return Err(EasyMusicError::invalid(format!(
                "search limit must be between 1 and {MAX_SEARCH_LIMIT}"
            )));
        }
        self.backend.search(keyword, limit).await
    }

    pub async fn resolve(&self, id: &str) -> Result<ResolvedTrack> {
        let id = id.trim();
        if id.is_empty() {
            return Err(EasyMusicError::invalid("track id must not be empty"));
        }
        if id.len() > 64
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(EasyMusicError::invalid("track id is not a valid video ID"));
        }
        self.backend.resolve(id).await
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

fn default_yt_dlp_executable() -> PathBuf {
    let file_name = if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    };
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(|parent| parent.join(file_name)));
    bundled
        .filter(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_invalid_inputs_before_starting_yt_dlp() {
        let client = MusicClient::with_config(YtDlpConfig {
            executable: PathBuf::from("definitely-not-an-executable"),
            js_runtime: None,
            cookies: None,
        });

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
        assert_eq!(
            client
                .resolve("../video")
                .await
                .unwrap_err()
                .code
                .exit_code(),
            2
        );
    }

    #[test]
    fn explicit_configuration_is_exposed_for_diagnostics() {
        let client = MusicClient::with_config(YtDlpConfig {
            executable: PathBuf::from("tools/yt-dlp"),
            js_runtime: Some("quickjs:tools/qjs".to_owned()),
            cookies: Some(PathBuf::from("tools/cookies.txt")),
        });

        assert_eq!(client.yt_dlp_executable(), Path::new("tools/yt-dlp"));
        assert_eq!(client.js_runtime(), Some("quickjs:tools/qjs"));
    }
}
