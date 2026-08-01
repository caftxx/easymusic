use crate::error::{EasyMusicError, ErrorCode, Result};
use crate::model::{RankedTrack, SelectResult, Track};

const VARIANT_MARKERS: &[&str] = &[
    "live",
    "dj",
    "伴奏",
    "纯音乐",
    "翻唱",
    "cover",
    "remix",
    "手鼓版",
    "歌词",
    "lyric",
    "无损",
    "flac",
    "纯享",
];

const ORIGINAL_MARKERS: &[&str] = &["official", "官方", "topic"];

pub fn rank_tracks(
    tracks: &[Track],
    title: Option<&str>,
    artist: Option<&str>,
) -> Vec<RankedTrack> {
    let wanted_title = title.map(normalize).unwrap_or_default();
    let wanted_artist = artist.map(normalize).unwrap_or_default();
    let query_has_variant = VARIANT_MARKERS
        .iter()
        .any(|marker| wanted_title.contains(&normalize(marker)));

    let mut ranked: Vec<(usize, RankedTrack)> = tracks
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, track)| {
            let candidate_title = normalize(&track.title);
            let candidate_artist = normalize(&track.artist);
            let mut score = 0;

            if !wanted_title.is_empty() {
                if candidate_title == wanted_title {
                    score += 120;
                } else if candidate_title.starts_with(&wanted_title) {
                    score += 80;
                } else if candidate_title.contains(&wanted_title)
                    || wanted_title.contains(&candidate_title)
                {
                    score += 50;
                }
            }

            if !wanted_artist.is_empty() {
                if candidate_artist == wanted_artist {
                    score += 100;
                } else if candidate_artist.contains(&wanted_artist) {
                    score += 70;
                } else {
                    score -= 60;
                }
            }

            if !query_has_variant
                && VARIANT_MARKERS
                    .iter()
                    .any(|marker| candidate_title.contains(&normalize(marker)))
            {
                score -= 25;
            }

            if ORIGINAL_MARKERS.iter().any(|marker| {
                candidate_title.contains(&normalize(marker))
                    || candidate_artist.contains(&normalize(marker))
            }) {
                score += 20;
            }

            // Preserve useful upstream popularity/relevance ordering as a small tiebreak.
            score += (20_i32 - index.min(20) as i32).max(0);
            (index, RankedTrack { track, score })
        })
        .collect();

    ranked.sort_by(|(left_index, left), (right_index, right)| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left_index.cmp(right_index))
    });
    ranked.into_iter().map(|(_, track)| track).collect()
}

pub fn select_track(
    tracks: &[Track],
    title: Option<&str>,
    artist: Option<&str>,
) -> Result<SelectResult> {
    if tracks.is_empty() {
        return Err(EasyMusicError::new(
            ErrorCode::NoResults,
            "no matching tracks found",
        ));
    }

    let ranked = rank_tracks(tracks, title, artist);
    let selected = ranked[0].clone();
    let runner_up_score = ranked
        .get(1)
        .map(|item| item.score)
        .unwrap_or(selected.score - 40);
    let margin = selected.score - runner_up_score;
    let title_exact = title
        .map(|value| normalize(value) == normalize(&selected.track.title))
        .unwrap_or(false);
    let artist_matches = artist
        .map(|value| normalize(&selected.track.artist).contains(&normalize(value)))
        .unwrap_or(false);

    let needs_confirmation = title.is_none()
        || (title.is_some() && !title_exact)
        || (artist.is_some() && !artist_matches)
        || margin < 15;

    let base: f64 = if title_exact { 0.72 } else { 0.42 };
    let artist_bonus: f64 = if artist.is_none() {
        0.0
    } else if artist_matches {
        0.16
    } else {
        -0.2
    };
    let margin_bonus = (margin.max(0) as f64 / 100.0).min(0.12);
    let confidence =
        ((base + artist_bonus + margin_bonus).clamp(0.0_f64, 0.99_f64) * 100.0).round() / 100.0;

    Ok(SelectResult {
        ok: true,
        selected,
        confidence,
        needs_confirmation,
        alternatives: ranked.into_iter().skip(1).take(4).collect(),
    })
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, title: &str, artist: &str) -> Track {
        Track {
            id: id.to_owned(),
            title: title.to_owned(),
            artist: artist.to_owned(),
            artwork_url: None,
        }
    }

    #[test]
    fn exact_title_and_artist_beat_live_and_cover_versions() {
        let tracks = vec![
            track("live", "晴天 (Live)", "周杰伦"),
            track("cover", "晴天", "其他歌手"),
            track("studio", "晴天", "周杰伦"),
        ];
        let selected = select_track(&tracks, Some("晴天"), Some("周杰伦")).unwrap();
        assert_eq!(selected.selected.track.id, "studio");
        assert!(!selected.needs_confirmation);
        assert!(selected.confidence > 0.8);
    }

    #[test]
    fn artist_only_requests_need_confirmation() {
        let tracks = vec![
            track("one", "晴天", "周杰伦"),
            track("two", "夜曲", "周杰伦"),
        ];
        let selected = select_track(&tracks, None, Some("周杰伦")).unwrap();
        assert!(selected.needs_confirmation);
    }

    #[test]
    fn ambiguous_versions_need_confirmation() {
        let tracks = vec![
            track("one", "晴天", "周杰伦"),
            track("two", "晴天", "周杰伦"),
        ];
        let selected = select_track(&tracks, Some("晴天"), Some("周杰伦")).unwrap();
        assert!(selected.needs_confirmation);
    }

    #[test]
    fn official_upload_beats_lyrics_and_lossless_reuploads() {
        let tracks = vec![
            track("lyrics", "晴天 周杰伦 (歌词版)", "GM Lyric"),
            track(
                "official",
                "周杰倫 Jay Chou【晴天 Sunny Day】-Official Music Video",
                "周杰倫 Jay Chou",
            ),
            track(
                "flac",
                "周杰倫 晴天 無損音樂FLAC 歌詞LYRICS 純享",
                "MusicDelta",
            ),
        ];

        let selected = select_track(&tracks, Some("晴天"), Some("周杰伦")).unwrap();
        assert_eq!(selected.selected.track.id, "official");
    }
}
