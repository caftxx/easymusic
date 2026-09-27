//! Source registry and ID routing shared by the CLI, library, and MCP server.

use std::sync::Arc;

use crate::error::{EasyMusicError, Result};
use crate::model::{ResolvedTrack, SearchResult, Track};
use crate::provider::SourceTrack;
use crate::provider::{MusicSource, SourceDiagnostics};
use crate::track_id::TrackId;

/// Ordered collection of enabled music sources, with IDs routed by source namespace.
#[derive(Clone)]
pub struct SourceRegistry {
    sources: Arc<Vec<Arc<dyn MusicSource>>>,
}

impl std::fmt::Debug for SourceRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceRegistry")
            .field("sources", &self.names())
            .finish()
    }
}

impl SourceRegistry {
    /// Build a registry from an explicit source list. Library users can
    /// implement [`MusicSource`] in-process and register it here.
    pub fn from_sources(sources: Vec<Arc<dyn MusicSource>>) -> Result<Self> {
        if sources.is_empty() {
            return Err(EasyMusicError::invalid(
                "at least one music source is required",
            ));
        }
        let mut names = std::collections::HashSet::new();
        for source in &sources {
            let name = source.name();
            if name.is_empty() || name.trim() != name || name.contains(':') {
                return Err(EasyMusicError::invalid(format!(
                    "invalid music source name {name:?}"
                )));
            }
            if !names.insert(name) {
                return Err(EasyMusicError::invalid(format!(
                    "duplicate music source {name:?}"
                )));
            }
        }
        Ok(Self {
            sources: Arc::new(sources),
        })
    }

    /// Names of all registered sources, in resolution order.
    pub fn names(&self) -> Vec<&str> {
        self.sources.iter().map(|source| source.name()).collect()
    }

    pub fn diagnostics(&self, name: &str) -> Option<SourceDiagnostics<'_>> {
        self.lookup(name).map(|source| source.diagnostics())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.sources.iter().any(|source| source.name() == name)
    }

    pub fn require_source(&self, name: &str) -> Result<()> {
        if self.contains(name) {
            return Ok(());
        }
        Err(EasyMusicError::invalid(format!(
            "unknown music source {name:?}; available: {}",
            self.names().join(", ")
        )))
    }

    fn public_search(&self, source: &str, keyword: &str, tracks: Vec<SourceTrack>) -> SearchResult {
        let tracks = tracks
            .into_iter()
            .map(|track| Track {
                id: TrackId::new(source, track.id).to_string(),
                title: track.title,
                artist: track.artist,
                artwork_url: track.artwork_url,
            })
            .collect::<Vec<_>>();
        SearchResult {
            ok: true,
            keyword: keyword.to_owned(),
            count: tracks.len(),
            source: Some(source.to_owned()),
            tracks,
        }
    }

    fn lookup(&self, name: &str) -> Option<&Arc<dyn MusicSource>> {
        self.sources.iter().find(|source| source.name() == name)
    }

    /// Search exactly one registered source and expose its routed IDs.
    pub async fn search(&self, name: &str, keyword: &str, limit: usize) -> Result<SearchResult> {
        self.require_source(name)?;
        let source = self.lookup(name).expect("source was validated");
        let tracks = source.search(keyword, limit).await?;
        Ok(self.public_search(name, keyword, tracks))
    }

    /// Resolve a namespaced `<source>:<native id>` track ID.
    ///
    /// The returned ID carries the same namespace callers passed in (or that
    /// [`Self::search`] added), so a resolved ID stays reusable when the
    /// backend reports its own native ID back.
    pub async fn resolve(&self, id: &str) -> Result<ResolvedTrack> {
        let id = TrackId::parse(id)?;
        self.require_source(id.source())?;
        let source = self.lookup(id.source()).expect("source was validated");
        let resolved = source.resolve(id.native()).await?;
        Ok(ResolvedTrack {
            id: TrackId::new(source.name(), resolved.id).to_string(),
            title: resolved.title,
            url: resolved.url,
            extension: resolved.extension,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::config::YtDlpConfig;
    use crate::error::ErrorCode;
    use crate::provider::{MusicSource, SourceResolvedTrack, SourceTrack};
    use crate::track_id::NativeTrackId;
    use async_trait::async_trait;

    struct StubSource {
        name: &'static str,
        fail_search: bool,
        searches: AtomicUsize,
    }

    impl StubSource {
        fn new(name: &'static str, fail_search: bool) -> Arc<Self> {
            Arc::new(Self {
                name,
                fail_search,
                searches: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl MusicSource for StubSource {
        fn name(&self) -> &'static str {
            self.name
        }

        async fn search(&self, _keyword: &str, limit: usize) -> Result<Vec<SourceTrack>> {
            let _ = limit;
            self.searches.fetch_add(1, Ordering::SeqCst);
            if self.fail_search {
                return Err(EasyMusicError::upstream(format!("{} down", self.name)));
            }
            let tracks = vec![SourceTrack {
                // Sources hand out native IDs; namespacing is the registry's
                // job.
                id: NativeTrackId::new("track-1"),
                title: "晴天".to_owned(),
                artist: "周杰伦".to_owned(),
                artwork_url: None,
            }];
            Ok(tracks)
        }

        async fn resolve(&self, id: &NativeTrackId) -> Result<SourceResolvedTrack> {
            if self.fail_search {
                return Err(EasyMusicError::source("unavailable"));
            }
            let name = self.name;
            Ok(SourceResolvedTrack {
                id: id.clone(),
                title: "晴天".to_owned(),
                url: format!("https://cdn.example.com/{name}/{id}.mp3"),
                extension: Some("mp3".to_owned()),
            })
        }
    }

    fn registry_with(sources: Vec<Arc<StubSource>>) -> SourceRegistry {
        SourceRegistry::from_sources(
            sources
                .into_iter()
                .map(|source| source as Arc<dyn MusicSource>)
                .collect(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn search_falls_through_to_the_next_working_source() {
        let registry = registry_with(vec![
            StubSource::new("broken", true),
            StubSource::new("working", false),
        ]);

        let result = crate::MusicClient::with_registry(registry.clone())
            .search("晴天", 5)
            .await
            .unwrap();
        assert_eq!(result.source.as_deref(), Some("working"));
        assert_eq!(result.tracks[0].id, "working:track-1");
    }

    #[tokio::test]
    async fn search_reports_every_failure_when_no_source_works() {
        let registry = registry_with(vec![StubSource::new("a", true), StubSource::new("b", true)]);
        let error = crate::MusicClient::with_registry(registry.clone())
            .search("晴天", 5)
            .await
            .unwrap_err();
        assert!(matches!(error.code, ErrorCode::UpstreamApi));
        assert!(error.message.contains("a:") && error.message.contains("b:"));
    }

    #[tokio::test]
    async fn pinned_search_never_falls_back() {
        let broken = StubSource::new("broken", true);
        let registry = registry_with(vec![broken.clone(), StubSource::new("working", false)]);

        assert!(registry.search("broken", "晴天", 5).await.is_err());
        // The pinned source was queried exactly once and no fallback ran.
        assert_eq!(broken.searches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn resolve_uses_the_id_namespace_and_rejects_bare_ids() {
        let registry = registry_with(vec![
            StubSource::new("first", false),
            StubSource::new("second", false),
        ]);

        let namespaced = registry.resolve("second:42").await.unwrap();
        assert_eq!(namespaced.url, "https://cdn.example.com/second/42.mp3");
        let error = registry.resolve("42").await.unwrap_err();
        assert!(matches!(error.code, ErrorCode::InvalidArguments));
        assert!(error.message.contains("<source>:<native id>"));
    }

    #[tokio::test]
    async fn resolved_ids_keep_their_source_namespace() {
        // Regression: resolving "b:correct" returned the backend's bare
        // "correct", which then downloaded from source "a" when reused.
        let registry = registry_with(vec![
            StubSource::new("a", false),
            StubSource::new("b", false),
        ]);

        let resolved = registry.resolve("b:correct").await.unwrap();
        assert_eq!(resolved.id, "b:correct");
        // The repaired ID must route back to the same source.
        let again = registry.resolve(&resolved.id).await.unwrap();
        assert_eq!(again.url, "https://cdn.example.com/b/correct.mp3");

        let first = registry.resolve("a:correct").await.unwrap();
        assert_eq!(first.id, "a:correct");
        assert_eq!(first.url, "https://cdn.example.com/a/correct.mp3");
    }

    #[tokio::test]
    async fn search_ids_are_namespaced_and_reusable_across_source_orders() {
        let sources = vec![
            StubSource::new("youtube", false),
            StubSource::new("netease", false),
            StubSource::new("kuwo", false),
        ];
        let registry = registry_with(sources.clone());
        let reordered = registry_with(sources.into_iter().rev().collect());

        for name in ["youtube", "netease", "kuwo"] {
            for search_registry in [&registry, &reordered] {
                let searched = search_registry.search(name, "晴天", 5).await.unwrap();
                let id = &searched.tracks[0].id;
                assert_eq!(id, &format!("{name}:track-1"));
                for resolve_registry in [&registry, &reordered] {
                    let resolved = resolve_registry.resolve(id).await.unwrap();
                    assert_eq!(&resolved.id, id);
                    assert_eq!(
                        resolved.url,
                        format!("https://cdn.example.com/{name}/track-1.mp3")
                    );
                }
            }
        }

        let fallback = crate::MusicClient::with_registry(registry)
            .search("晴天", 5)
            .await
            .unwrap();
        assert_eq!(fallback.tracks[0].id, "youtube:track-1");
    }

    #[tokio::test]
    async fn unknown_namespace_is_rejected_with_the_source_list() {
        let registry = registry_with(vec![StubSource::new("first", false)]);
        let error = registry.resolve("tidal:1").await.unwrap_err();
        assert!(matches!(error.code, ErrorCode::InvalidArguments));
        assert!(error.message.contains("available: first"));
    }

    #[test]
    fn empty_registries_are_rejected() {
        let error = SourceRegistry::from_sources(Vec::new()).unwrap_err();
        assert!(matches!(error.code, ErrorCode::InvalidArguments));
    }

    #[test]
    fn built_in_sources_follow_the_requested_priority_order() {
        let registry = SourceRegistry::built_in(
            YtDlpConfig::default(),
            Some(&["kuwo".to_owned(), "netease".to_owned(), "kuwo".to_owned()]),
        )
        .unwrap();
        assert_eq!(registry.names(), ["kuwo", "netease"]);

        let all = SourceRegistry::built_in(YtDlpConfig::default(), None).unwrap();
        assert_eq!(all.names(), ["youtube", "netease", "kuwo"]);
    }

    #[test]
    fn unknown_builtin_source_names_are_rejected() {
        let error = SourceRegistry::built_in(YtDlpConfig::default(), Some(&["tidal".to_owned()]))
            .unwrap_err();
        assert!(matches!(error.code, ErrorCode::InvalidArguments));
        assert!(error.message.contains("available: youtube, netease, kuwo"));
    }
}
