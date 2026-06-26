//! TMDB discovery source: "Trending Movies" and "Trending Shows" rows.
//!
//! Enabled by a TMDB API key set in the Settings panel (or the TVOS_TMDB_KEY
//! env var). TMDB only provides metadata/art, not streams — so to *play* an
//! item we map its TMDB id to an IMDb id (TMDB external_ids) and hand that to
//! the Stremio stream resolver, which is exactly what Stremio addons key on.
//! That means TMDB browsing is playable as long as a stream addon carries the
//! title. Results are cached so the home screen stays fast.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::model::{Action, ContentItem, Kind, Row};
use crate::settings;
use crate::sources::{stremio, Source};

const CACHE_TTL: Duration = Duration::from_secs(900);
const ROW_LIMIT: usize = 25;

#[derive(Default)]
pub struct Tmdb {
    /// Cached rows keyed by request URL (the URL embeds the key, so a key
    /// change naturally misses the cache).
    cache: Mutex<HashMap<String, (Instant, Vec<ContentItem>)>>,
}

impl Tmdb {
    /// Key from settings, falling back to the env var for headless setups.
    fn api_key(&self) -> Option<String> {
        let from_settings = settings::STORE.get().tmdb_key;
        if !from_settings.is_empty() {
            return Some(from_settings);
        }
        std::env::var("TVOS_TMDB_KEY")
            .ok()
            .filter(|k| !k.is_empty())
    }

    /// `media` is TMDB's path segment: "movie" or "tv".
    fn trending(&self, key: &str, media: &str) -> Vec<ContentItem> {
        let url = format!("https://api.themoviedb.org/3/trending/{media}/week?api_key={key}");
        let mut cache = self.cache.lock().unwrap();
        if let Some((at, items)) = cache.get(&url) {
            if at.elapsed() < CACHE_TTL {
                return items.clone();
            }
        }
        let items = fetch(&url)
            .map(|json| parse_trending(&json, media))
            .unwrap_or_default();
        if !items.is_empty() {
            cache.insert(url, (Instant::now(), items.clone()));
        }
        items
    }
}

impl Source for Tmdb {
    fn id(&self) -> &'static str {
        "tmdb"
    }

    fn available(&self) -> bool {
        self.api_key().is_some()
    }

    fn rows(&self) -> Vec<Row> {
        let Some(key) = self.api_key() else {
            return Vec::new();
        };
        vec![
            Row {
                title: "Trending Movies".to_string(),
                items: self.trending(&key, "movie"),
            },
            Row {
                title: "Trending Shows".to_string(),
                items: self.trending(&key, "tv"),
            },
        ]
    }

    fn launch(&self, item_id: &str) -> Result<(), String> {
        let key = self.api_key().ok_or("Set a TMDB API key in Settings")?;
        let (media, tmdb_id) = parse_id(item_id)?;
        let imdb =
            imdb_id(&key, media, tmdb_id).ok_or("Couldn't find this title's IMDb id on TMDB")?;
        // TMDB "tv" is Stremio "series".
        let stremio_kind = if media == "tv" { "series" } else { "movie" };
        stremio::play_meta(stremio_kind, &imdb)
    }
}

/// Maps a `tmdb:…` item id to `(stremio_kind, imdb_id)` for the meta/stream
/// resolvers — the shared bridge from TMDB browsing to Stremio addons.
pub fn resolve_imdb(item_id: &str) -> Result<(String, String), String> {
    let key = settings::STORE.get().tmdb_key;
    let key = if key.is_empty() {
        std::env::var("TVOS_TMDB_KEY").map_err(|_| "Set a TMDB API key in Settings".to_string())?
    } else {
        key
    };
    let (media, tmdb_id) = parse_id(item_id)?;
    let imdb = imdb_id(&key, media, tmdb_id).ok_or("Couldn't find this title's IMDb id on TMDB")?;
    let kind = if media == "tv" { "series" } else { "movie" };
    Ok((kind.to_string(), imdb))
}

/// "tmdb:movie:603" → ("movie", "603"); "tmdb:tv:1399" → ("tv", "1399").
fn parse_id(item_id: &str) -> Result<(&str, &str), String> {
    let mut parts = item_id.splitn(3, ':');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("tmdb"), Some(media @ ("movie" | "tv")), Some(id)) if !id.is_empty() => {
            Ok((media, id))
        }
        _ => Err(format!("bad tmdb id '{item_id}'")),
    }
}

fn imdb_id(key: &str, media: &str, tmdb_id: &str) -> Option<String> {
    let url = format!("https://api.themoviedb.org/3/{media}/{tmdb_id}/external_ids?api_key={key}");
    let json: Value = serde_json::from_str(&fetch(&url)?).ok()?;
    json.get("imdb_id")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn fetch(url: &str) -> Option<String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .ok()?
        .get(url)
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .text()
        .ok()
}

fn parse_trending(json: &str, media: &str) -> Vec<ContentItem> {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(results) = value.get("results").and_then(|r| r.as_array()) else {
        return Vec::new();
    };
    let kind = if media == "tv" {
        Kind::Series
    } else {
        Kind::Movie
    };
    results
        .iter()
        .take(ROW_LIMIT)
        .filter_map(|m| {
            // Movies use "title", shows use "name".
            let title = m
                .get("title")
                .or_else(|| m.get("name"))?
                .as_str()?
                .to_string();
            Some(ContentItem {
                id: format!("tmdb:{media}:{}", m.get("id")?.as_i64()?),
                kind,
                title,
                art: m
                    .get("poster_path")
                    .and_then(|p| p.as_str())
                    .map(|p| format!("https://image.tmdb.org/t/p/w500{p}")),
                action: Action::Play,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_trending_movies() {
        let json = r#"{"results": [
            {"id": 603, "title": "The Matrix", "poster_path": "/abc.jpg"},
            {"id": 604, "title": "No Poster Movie", "poster_path": null}
        ]}"#;
        let items = parse_trending(json, "movie");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "tmdb:movie:603");
        assert_eq!(items[0].kind, Kind::Movie);
        assert_eq!(
            items[0].art.as_deref(),
            Some("https://image.tmdb.org/t/p/w500/abc.jpg")
        );
        assert_eq!(items[1].art, None);
    }

    #[test]
    fn parses_trending_shows_using_name() {
        let json =
            r#"{"results": [{"id": 1399, "name": "Game of Thrones", "poster_path": "/got.jpg"}]}"#;
        let items = parse_trending(json, "tv");
        assert_eq!(items[0].id, "tmdb:tv:1399");
        assert_eq!(items[0].kind, Kind::Series);
        assert_eq!(items[0].title, "Game of Thrones");
    }

    #[test]
    fn id_parsing() {
        assert_eq!(parse_id("tmdb:movie:603"), Ok(("movie", "603")));
        assert_eq!(parse_id("tmdb:tv:1399"), Ok(("tv", "1399")));
        assert!(parse_id("tmdb:music:1").is_err());
        assert!(parse_id("strm:movie:tt1").is_err());
    }

    #[test]
    fn garbage_json_yields_no_items() {
        assert!(parse_trending("oops", "movie").is_empty());
    }
}
