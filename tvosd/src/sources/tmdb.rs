//! TMDB catalog source: a "Trending Movies" row for discovery.
//!
//! Enabled by setting TVOS_TMDB_KEY (a free TMDB API key). Catalog items are
//! browsable but not yet playable — pressing A explains that stream sources
//! arrive in a later phase. Results are cached so the home screen stays fast.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::model::{Action, ContentItem, Kind, Row};
use crate::sources::Source;

const CACHE_TTL: Duration = Duration::from_secs(900);

pub struct Tmdb {
    api_key: Option<String>,
    cache: Mutex<Option<(Instant, Vec<ContentItem>)>>,
}

impl Tmdb {
    pub fn from_env() -> Self {
        Self {
            api_key: std::env::var("TVOS_TMDB_KEY")
                .ok()
                .filter(|k| !k.is_empty()),
            cache: Mutex::new(None),
        }
    }

    fn trending(&self) -> Vec<ContentItem> {
        let Some(key) = &self.api_key else {
            return Vec::new();
        };
        let mut cache = self.cache.lock().unwrap();
        if let Some((at, items)) = cache.as_ref() {
            if at.elapsed() < CACHE_TTL {
                return items.clone();
            }
        }
        let url = format!("https://api.themoviedb.org/3/trending/movie/week?api_key={key}");
        let items = fetch(&url)
            .map(|json| parse_trending(&json))
            .unwrap_or_default();
        if !items.is_empty() {
            *cache = Some((Instant::now(), items.clone()));
        }
        items
    }
}

impl Source for Tmdb {
    fn id(&self) -> &'static str {
        "tmdb"
    }

    fn available(&self) -> bool {
        self.api_key.is_some()
    }

    fn rows(&self) -> Vec<Row> {
        vec![Row {
            title: "Trending Movies".to_string(),
            items: self.trending(),
        }]
    }

    fn launch(&self, _item_id: &str) -> Result<(), String> {
        Err("No way to play this yet — stream sources arrive in the streams phase".to_string())
    }
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

fn parse_trending(json: &str) -> Vec<ContentItem> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(results) = value.get("results").and_then(|r| r.as_array()) else {
        return Vec::new();
    };
    results
        .iter()
        .filter_map(|m| {
            Some(ContentItem {
                id: format!("tmdb:movie:{}", m.get("id")?.as_i64()?),
                kind: Kind::Movie,
                title: m.get("title")?.as_str()?.to_string(),
                art: m
                    .get("poster_path")
                    .and_then(|p| p.as_str())
                    .map(|p| format!("https://image.tmdb.org/t/p/w500{p}")),
                action: Action::None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRENDING: &str = r#"{
        "results": [
            {"id": 603, "title": "The Matrix", "poster_path": "/abc.jpg"},
            {"id": 604, "title": "No Poster Movie", "poster_path": null}
        ]
    }"#;

    #[test]
    fn parses_trending_movies() {
        let items = parse_trending(TRENDING);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "tmdb:movie:603");
        assert_eq!(
            items[0].art.as_deref(),
            Some("https://image.tmdb.org/t/p/w500/abc.jpg")
        );
        assert_eq!(items[1].art, None);
    }

    #[test]
    fn garbage_json_yields_no_items() {
        assert!(parse_trending("oops").is_empty());
    }
}
