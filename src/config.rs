//! Configuration and default paths, independent of source implementations.
use std::path::PathBuf;

pub const SOURCE_PATH_ENV: &str = "EASYMUSIC_SOURCE_PATH";

/// Configuration for the external `yt-dlp` process (YouTube source).
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

/// Client-wide configuration: which music sources exist and which are used.
#[derive(Debug, Clone, Default)]
pub struct ClientConfig {
    /// yt-dlp executable settings for the built-in YouTube source.
    pub yt_dlp: YtDlpConfig,
    /// Enabled source names in priority order. `None` enables all discovered
    /// sources. Bare track IDs always route to the first enabled source.
    pub sources: Option<Vec<String>>,
    /// Directories scanned for `easymusic-source-*` plugin executables.
    /// Empty means [`default_plugin_dirs`].
    pub plugin_dirs: Vec<PathBuf>,
}

pub(crate) fn plugin_dirs(config: &ClientConfig) -> Vec<PathBuf> {
    if !config.plugin_dirs.is_empty() {
        return config.plugin_dirs.clone();
    }
    default_plugin_dirs()
}

/// Standard plugin locations: a `plugins` directory next to the easymusic
/// executable plus any directories listed in `EASYMUSIC_SOURCE_PATH`.
pub fn default_plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(executable_dir) = std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(|parent| parent.join("plugins")))
    {
        dirs.push(executable_dir);
    }
    if let Some(value) = std::env::var_os(SOURCE_PATH_ENV) {
        dirs.extend(std::env::split_paths(&value).filter(|dir| !dir.as_os_str().is_empty()));
    }
    dirs
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
