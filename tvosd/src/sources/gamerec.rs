//! Game recommender — decides WHICH games appear in "Games for you".
//!
//! Deliberately separate from GameHub, which only answers "where do I buy
//! it and for how much". The candidate pool is the store charts minus what
//! you own (gamehub::charts_unowned); the ORDER comes from your taste when
//! the on-box embedding model is ready: your recent plays and watches are
//! embedded into a profile vector (recency-weighted mean, same space as the
//! movie recommender) and candidates are ranked by cosine similarity.
//! Without a model or history it falls back to the charts' popularity order.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::model::{ContentItem, Kind, Row};
use crate::sources::gamehub;
use crate::{embed, recommend};

const RECS_LIMIT: usize = 24;
/// How many recent plays/watches shape the taste profile.
const PROFILE_ITEMS: usize = 12;

/// Steam genre names → store search tag ids (used by the paginated top-seller
/// search). Genres without a useful tag (Free To Play, Early Access, …) don't
/// make a row.
const GENRE_TAGS: &[(&str, u32)] = &[
    ("action", 19),
    ("adventure", 21),
    ("rpg", 122),
    ("strategy", 9),
    ("simulation", 599),
    ("casual", 597),
    ("indie", 492),
    ("racing", 699),
    ("sports", 701),
    ("massively multiplayer", 128),
];

static GENRES_CACHE: Mutex<Option<(Instant, Vec<(String, u32)>)>> = Mutex::new(None);

/// The genres the user actually plays — tallied from their recent games'
/// store pages, best first, as (display name, store search tag id). Cached: the
/// tally needs a storefront lookup per recent game.
pub fn top_genres(limit: usize) -> Vec<(String, u32)> {
    const TTL: Duration = Duration::from_secs(900);
    {
        let cache = GENRES_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, genres)) = &*cache {
            if at.elapsed() < TTL {
                return genres.iter().take(limit).cloned().collect();
            }
        }
    }
    let mut tally: HashMap<String, usize> = HashMap::new();
    for item in recommend::LOG.recent_items(24) {
        if item.kind != Kind::Game {
            continue;
        }
        let Some(appid) = item
            .id
            .strip_prefix("steam:")
            .or_else(|| item.id.strip_prefix("gshop:"))
        else {
            continue;
        };
        let Some(meta) = crate::sources::steam::store_meta(appid) else {
            continue;
        };
        for genre in meta.genres {
            *tally.entry(genre).or_default() += 1;
        }
    }
    let mut counts: Vec<(String, usize)> = tally.into_iter().collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let genres: Vec<(String, u32)> = counts
        .into_iter()
        .filter_map(|(name, _)| {
            GENRE_TAGS
                .iter()
                .find(|(g, _)| *g == name.to_lowercase())
                .map(|(_, tag)| (name, *tag))
        })
        .collect();
    *GENRES_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), genres.clone()));
    genres.into_iter().take(limit).collect()
}

pub fn recommended(library: &[Row]) -> Vec<ContentItem> {
    let candidates = gamehub::recommend_pool(library);
    if candidates.is_empty() {
        return Vec::new();
    }
    let ranked = taste_ranked(&candidates).unwrap_or(candidates);
    ranked.into_iter().take(RECS_LIMIT).collect()
}

/// Candidates reordered by similarity to the user's recent media, or None
/// when there's nothing to personalize with (no model / no history yet).
fn taste_ranked(candidates: &[ContentItem]) -> Option<Vec<ContentItem>> {
    if !embed::ready() {
        return None;
    }
    let recent = recommend::LOG.recent_items(PROFILE_ITEMS);
    if recent.is_empty() {
        return None;
    }

    // One batch: profile texts first, then the candidates.
    let texts: Vec<String> = recent
        .iter()
        .map(|i| i.title.clone())
        .chain(candidates.iter().map(|c| c.title.clone()))
        .collect();
    let vectors = embed::embed(texts)?;
    let (profile_vecs, candidate_vecs) = vectors.split_at(recent.len());

    // Recency-weighted mean: the last thing you played counts the most.
    let dim = profile_vecs.first()?.len();
    let mut profile = vec![0.0f32; dim];
    for (i, v) in profile_vecs.iter().enumerate() {
        let weight = 1.0 / (i as f32 + 1.0);
        for (p, x) in profile.iter_mut().zip(v) {
            *p += x * weight;
        }
    }

    let mut scored: Vec<(f32, &ContentItem)> = candidate_vecs
        .iter()
        .zip(candidates)
        .map(|(v, c)| (embed::cosine(&profile, v), c))
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    Some(scored.into_iter().map(|(_, c)| c.clone()).collect())
}
