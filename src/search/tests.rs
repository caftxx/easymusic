use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::*;
use crate::provider::{
    MusicSource, SourceDiagnostics, SourceRegistry, SourceResolvedTrack, SourceTrack,
};
use crate::track_id::NativeTrackId;

struct FakeSource {
    name: String,
    fail: bool,
    tracks: Vec<SourceTrack>,
    requests: Mutex<Vec<(String, usize)>>,
    executable: PathBuf,
}

impl FakeSource {
    fn new(name: &str, artist: &str, fail: bool) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
            fail,
            tracks: vec![SourceTrack {
                id: NativeTrackId::new(format!("{name}:native")),
                title: "晴天".to_owned(),
                artist: artist.to_owned(),
                artwork_url: None,
            }],
            requests: Mutex::new(Vec::new()),
            executable: PathBuf::from("custom/active-provider"),
        })
    }
}

#[async_trait]
impl MusicSource for FakeSource {
    fn name(&self) -> &str {
        &self.name
    }
    fn diagnostics(&self) -> SourceDiagnostics<'_> {
        SourceDiagnostics {
            executable: Some(&self.executable),
            js_runtime: Some("runtime:active"),
        }
    }
    async fn search(&self, keyword: &str, limit: usize) -> Result<Vec<SourceTrack>> {
        self.requests
            .lock()
            .unwrap()
            .push((keyword.to_owned(), limit));
        if self.fail {
            return Err(EasyMusicError::upstream("offline"));
        }
        Ok(self.tracks.clone())
    }
    async fn resolve(&self, id: &NativeTrackId) -> Result<SourceResolvedTrack> {
        Ok(SourceResolvedTrack {
            id: id.clone(),
            title: "晴天".into(),
            url: "https://cdn.example.com/song.mp3".into(),
            extension: Some("mp3".into()),
        })
    }
}

fn client(sources: &[Arc<FakeSource>]) -> MusicClient {
    MusicClient::with_registry(
        SourceRegistry::from_sources(
            sources
                .iter()
                .map(|s| s.clone() as Arc<dyn MusicSource>)
                .collect(),
        )
        .unwrap(),
    )
}

#[test]
fn query_normalizes_hints_without_losing_the_artist() {
    let query = MusicQuery::new(Some(" 晴天 "), Some(" 周杰伦 ")).unwrap();
    assert_eq!(query.keyword(), "晴天 周杰伦");
    assert_eq!(query.title(), Some("晴天"));
    assert_eq!(query.artist(), Some("周杰伦"));
    assert_eq!(
        MusicQuery::new(None, Some("周杰伦")).unwrap().keyword(),
        "周杰伦"
    );
    assert!(MusicQuery::new(Some(" "), None).is_err());
}

#[tokio::test]
async fn merge_ranks_with_the_artist_and_preserves_ids_through_resolution() {
    let cover = FakeSource::new("a", "其他歌手", false);
    let original = FakeSource::new("b", "周杰伦", false);
    let client = client(&[cover.clone(), original.clone()]);
    let query = MusicQuery::new(Some("晴天"), Some("周杰伦")).unwrap();
    let result = client
        .search_query(&query, &SearchStrategy::Merge(vec![]), 1)
        .await
        .unwrap();
    assert_eq!(result.tracks[0].id, "b:b:native");
    assert_eq!(result.count, 1);
    assert_eq!(result.source.as_deref(), Some("all"));
    assert_eq!(
        original.requests.lock().unwrap().as_slice(),
        &[("晴天 周杰伦".to_owned(), 2)]
    );
    let resolved = client.resolve(&result.tracks[0].id).await.unwrap();
    assert_eq!(resolved.id, result.tracks[0].id);
    assert_eq!(client.resolve(&resolved.id).await.unwrap().id, resolved.id);
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "ok": true, "keyword": "晴天 周杰伦", "count": 1, "source": "all",
            "tracks": [{"id":"b:b:native", "title":"晴天", "artist":"周杰伦"}]
        })
    );
    let selected = client
        .select_query(&query, &SearchStrategy::Pinned("b".into()))
        .await
        .unwrap();
    assert_eq!(selected.selected.track.id, result.tracks[0].id);
    assert!(!selected.needs_confirmation);
}

#[tokio::test]
async fn fallback_pinned_and_partial_merge_obey_the_same_policy_for_all_callers() {
    let broken = FakeSource::new("broken", "其他歌手", true);
    let healthy = FakeSource::new("healthy", "周杰伦", false);
    let client = client(&[broken.clone(), healthy.clone()]);
    let query = MusicQuery::new(Some("晴天"), None).unwrap();
    assert_eq!(
        client
            .search_query(&query, &SearchStrategy::FirstAvailable, 1)
            .await
            .unwrap()
            .source
            .as_deref(),
        Some("healthy")
    );
    let count = healthy.requests.lock().unwrap().len();
    assert!(
        client
            .select_query(&query, &SearchStrategy::Pinned("broken".into()))
            .await
            .is_err()
    );
    assert_eq!(healthy.requests.lock().unwrap().len(), count);
    assert_eq!(
        client
            .search_query(&query, &SearchStrategy::Merge(vec![]), 1)
            .await
            .unwrap()
            .count,
        1
    );
    let failures = client
        .search_query(&query, &SearchStrategy::Merge(vec!["broken".into()]), 1)
        .await
        .unwrap_err();
    assert!(failures.message.contains("broken:"));
}

#[tokio::test]
async fn invalid_limits_and_unknown_merge_sources_never_contact_backends() {
    let source = FakeSource::new("demo", "周杰伦", false);
    let client = client(std::slice::from_ref(&source));
    let query = MusicQuery::new(Some("晴天"), None).unwrap();
    for strategy in [
        SearchStrategy::FirstAvailable,
        SearchStrategy::Pinned("demo".into()),
        SearchStrategy::Merge(vec![]),
    ] {
        for limit in [0, 51] {
            assert!(client.search_query(&query, &strategy, limit).await.is_err());
        }
    }
    assert!(
        client
            .search_query(
                &query,
                &SearchStrategy::Merge(vec!["demo".into(), "unknown".into()]),
                1
            )
            .await
            .is_err()
    );
    assert!(source.requests.lock().unwrap().is_empty());
}

#[test]
fn diagnostics_come_from_a_supplied_registry_not_a_second_youtube_instance() {
    let source = FakeSource::new("youtube", "周杰伦", false);
    let client = client(&[source]);
    let details = client.source_diagnostics("youtube").unwrap();
    assert_eq!(
        details.executable,
        Some(Path::new("custom/active-provider"))
    );
    assert_eq!(details.js_runtime, Some("runtime:active"));
    assert!(client.source_diagnostics("missing").is_none());
}

#[test]
fn registries_reject_ambiguous_source_names() {
    for names in [["demo", "demo"], ["demo", "bad:name"], ["demo", " "]] {
        let sources: Vec<Arc<dyn MusicSource>> = names
            .into_iter()
            .map(|name| FakeSource::new(name, "artist", false) as Arc<dyn MusicSource>)
            .collect();
        assert!(SourceRegistry::from_sources(sources).is_err());
    }
}
