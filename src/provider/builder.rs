//! Composition root: create configured sources and discover external plugins.
use super::external::{ExternalSource, discover_external_sources};
use super::kuwo::KuwoSource;
use super::netease::NeteaseSource;
use super::{MusicSource, SourceRegistry, YouTubeSource};
use crate::config::{ClientConfig, YtDlpConfig, plugin_dirs};
use crate::error::{EasyMusicError, Result};
use std::sync::Arc;

pub fn build_registry(config: &ClientConfig) -> Result<SourceRegistry> {
    SourceRegistry::with_plugins(
        config.yt_dlp.clone(),
        config.sources.as_deref(),
        &plugin_dirs(config),
    )
}

// Keep the existing construction helpers as a compatibility facade. All
// knowledge of concrete providers lives in this module rather than routing.
impl SourceRegistry {
    /// Built-in sources only, honoring the enabled-name filter. An empty or
    /// absent list enables every built-in source.
    pub fn built_in(yt_dlp: YtDlpConfig, enabled: Option<&[String]>) -> Result<Self> {
        let all = built_in_sources(yt_dlp)?;
        Self::from_sources(select_sources(&all, enabled)?)
    }

    /// Built-in sources plus every `easymusic-source-*` executable found in
    /// the plugin directory. Names in `enabled` that match no discovered
    /// source are reported as an error.
    pub fn with_plugins(
        yt_dlp: YtDlpConfig,
        enabled: Option<&[String]>,
        plugin_dirs: &[std::path::PathBuf],
    ) -> Result<Self> {
        let mut sources = built_in_sources(yt_dlp)?;
        let built_in_names = sources
            .iter()
            .map(|source| source.name().to_owned())
            .collect::<Vec<_>>();
        for source in discover_external_sources(plugin_dirs) {
            // Built-in names are reserved so a stray plugin cannot shadow them.
            if built_in_names.contains(&source.name) {
                continue;
            }
            sources.push(Arc::new(ExternalSource::new(source)));
        }
        Self::from_sources(select_sources(&sources, enabled)?)
    }
}

fn built_in_sources(yt_dlp: YtDlpConfig) -> Result<Vec<Arc<dyn MusicSource>>> {
    Ok(vec![
        Arc::new(YouTubeSource::new(yt_dlp)),
        Arc::new(NeteaseSource::new()?),
        Arc::new(KuwoSource::new()?),
    ])
}

/// Pick the enabled sources in the requested priority order, dropping
/// duplicates. An absent or empty request keeps every discovered source.
fn select_sources(
    all: &[Arc<dyn MusicSource>],
    enabled: Option<&[String]>,
) -> Result<Vec<Arc<dyn MusicSource>>> {
    let Some(names) = enabled.filter(|names| !names.is_empty()) else {
        return Ok(all.to_vec());
    };
    let available = all
        .iter()
        .map(|source| source.name().to_owned())
        .collect::<Vec<_>>();
    let mut picked: Vec<Arc<dyn MusicSource>> = Vec::with_capacity(names.len());
    for name in names {
        if picked.iter().any(|source| source.name() == name.as_str()) {
            continue;
        }
        let Some(source) = all.iter().find(|source| source.name() == name.as_str()) else {
            return Err(EasyMusicError::invalid(format!(
                "unknown music source {name:?}; available: {}",
                available.join(", ")
            )));
        };
        picked.push(source.clone());
    }
    if picked.is_empty() {
        return Err(EasyMusicError::invalid(
            "no music sources enabled; check --sources",
        ));
    }
    Ok(picked)
}
