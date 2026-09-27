//! Shared search and selection workflows for the CLI, MCP, and library.

use crate::api::{MusicClient, default_search_limit, max_search_limit, validate_search_limit};
use crate::error::{EasyMusicError, Result};
use crate::model::{SearchResult, SelectResult};
use crate::ranking::{rank_tracks, select_track};

#[cfg(test)]
mod tests;

/// Keep ranking hints separate from the combined upstream search keyword.
#[derive(Debug, Clone)]
pub struct MusicQuery {
    title: Option<String>,
    artist: Option<String>,
    keyword: String,
}

impl MusicQuery {
    pub fn new(title: Option<&str>, artist: Option<&str>) -> Result<Self> {
        let clean = |value: Option<&str>| {
            value
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let title = clean(title);
        let artist = clean(artist);
        let keyword = [title.as_deref(), artist.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        if keyword.is_empty() {
            return Err(EasyMusicError::invalid("title or artist is required"));
        }
        Ok(Self {
            title,
            artist,
            keyword,
        })
    }

    pub fn keyword(&self) -> &str {
        &self.keyword
    }
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }
    pub fn artist(&self) -> Option<&str> {
        self.artist.as_deref()
    }
}

#[derive(Debug, Clone, Default)]
pub enum SearchStrategy {
    #[default]
    FirstAvailable,
    Pinned(String),
    /// Merge these sources; an empty list means all enabled sources.
    Merge(Vec<String>),
}

impl SearchStrategy {
    pub fn from_source(source: Option<&str>) -> Self {
        match source.map(str::trim).filter(|s| !s.is_empty()) {
            Some(name) => Self::Pinned(name.to_owned()),
            None => Self::FirstAvailable,
        }
    }
}

impl MusicClient {
    /// Search, rank by the separate title/artist hints, and apply the final limit.
    pub async fn search_query(
        &self,
        query: &MusicQuery,
        strategy: &SearchStrategy,
        limit: usize,
    ) -> Result<SearchResult> {
        let mut result = self.search_candidates(query, strategy, limit).await?;
        result.tracks = rank_tracks(&result.tracks, query.title(), query.artist())
            .into_iter()
            .take(limit)
            .map(|item| item.track)
            .collect();
        result.count = result.tracks.len();
        Ok(result)
    }

    /// Search and select once, preserving upstream ordering for confidence scoring.
    pub async fn select_query(
        &self,
        query: &MusicQuery,
        strategy: &SearchStrategy,
    ) -> Result<SelectResult> {
        let result = self
            .search_candidates(query, strategy, default_search_limit())
            .await?;
        select_track(&result.tracks, query.title(), query.artist())
    }

    async fn search_candidates(
        &self,
        query: &MusicQuery,
        strategy: &SearchStrategy,
        limit: usize,
    ) -> Result<SearchResult> {
        validate_search_limit(limit)?;
        let requested = match strategy {
            SearchStrategy::FirstAvailable => return self.search(query.keyword(), limit).await,
            SearchStrategy::Pinned(name) => {
                return self.search_from(Some(name), query.keyword(), limit).await;
            }
            SearchStrategy::Merge(names) => names,
        };
        let targets = if requested.is_empty() {
            self.sources()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        } else {
            requested.clone()
        };
        // Validate the full request before contacting any upstream.
        for name in &targets {
            self.registry().require_source(name)?;
        }
        let per_source = (limit * 2).min(max_search_limit());
        let mut tracks = Vec::new();
        let mut failures = Vec::new();
        let mut visited = std::collections::HashSet::new();
        for name in targets {
            if !visited.insert(name.clone()) {
                continue;
            }
            match self
                .search_from(Some(&name), query.keyword(), per_source)
                .await
            {
                Ok(result) => tracks.extend(result.tracks),
                Err(error) => failures.push(format!("{name}: {error}")),
            }
        }
        if tracks.is_empty() {
            return Err(EasyMusicError::upstream(if failures.is_empty() {
                "no music sources returned results".to_owned()
            } else {
                format!("all music sources failed: {}", failures.join(" | "))
            }));
        }
        Ok(SearchResult {
            ok: true,
            keyword: query.keyword().to_owned(),
            count: tracks.len(),
            source: Some("all".to_owned()),
            tracks,
        })
    }
}
