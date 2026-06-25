//! Streams via installed Stremio-compatible addons (see addons.rs).
//!
//! Catalogs become home rows; pressing play asks every stream-capable addon
//! for streams, ranks them, and plays the best in mpv through the Enhance
//! pipeline. The ranker prefers direct HTTPS streams and higher resolutions;
//! torrent-only entries (infoHash without a url) are skipped — there is no
//! torrent engine, debrid-backed addons hand out direct URLs instead.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::addons::{self, Addon};
use crate::model::{Action, ContentItem, Kind, Row};
use crate::sources::Source;
use crate::util::percent_encode;
use crate::{launcher, settings, upscale};

const CATALOG_TTL: Duration = Duration::from_secs(600);
const ROW_LIMIT: usize = 25;
const CATALOGS_PER_ADDON: usize = 2;

#[derive(Default)]
pub struct Stremio {
    catalog_cache: Mutex<HashMap<String, (Instant, Vec<ContentItem>)>>,
}

impl Source for Stremio {
    fn id(&self) -> &'static str {
        "strm"
    }

    fn available(&self) -> bool {
        !addons::STORE.list().is_empty()
    }

    fn rows(&self) -> Vec<Row> {
        addons::STORE
            .list()
            .iter()
            .flat_map(|addon| {
                addon
                    .catalogs
                    .iter()
                    .take(CATALOGS_PER_ADDON)
                    .map(|catalog| Row {
                        title: format!("{} · {}", addon.name, catalog.name),
                        items: self.catalog_items(addon, &catalog.kind, &catalog.id),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn launch(&self, item_id: &str) -> Result<(), String> {
        let (kind, meta_id) = parse_id(item_id)?;
        let streams = collect_streams(&addons::STORE.list(), kind, meta_id);
        let best = streams
            .first()
            .ok_or("No playable stream found — install a stream addon that carries this title")?;
        let profile = upscale::resolve(settings::STORE.get().enhance, &best.describe());
        println!(
            "stream pick: {} ({} candidates)",
            best.describe(),
            streams.len()
        );
        launcher::play_video(&best.url, &profile)
    }
}

impl Stremio {
    fn catalog_items(&self, addon: &Addon, kind: &str, catalog_id: &str) -> Vec<ContentItem> {
        let url = format!(
            "{}/catalog/{}/{}.json",
            addon.base,
            percent_encode(kind),
            percent_encode(catalog_id)
        );
        let mut cache = self.catalog_cache.lock().unwrap();
        if let Some((at, items)) = cache.get(&url) {
            if at.elapsed() < CATALOG_TTL {
                return items.clone();
            }
        }
        let items = addons::http_get(&url)
            .map(|json| parse_catalog(&json))
            .unwrap_or_default();
        if !items.is_empty() {
            cache.insert(url, (Instant::now(), items.clone()));
        }
        items
    }
}

/// "strm:movie:tt0133093" → ("movie", "tt0133093"); meta ids may themselves
/// contain ':' (series episodes), so split only twice.
fn parse_id(item_id: &str) -> Result<(&str, &str), String> {
    let mut parts = item_id.splitn(3, ':');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("strm"), Some(kind), Some(meta_id)) if !meta_id.is_empty() => Ok((kind, meta_id)),
        _ => Err(format!("bad stream id '{item_id}'")),
    }
}

/// A playable candidate after filtering and ranking.
#[derive(Debug, PartialEq)]
struct Candidate {
    url: String,
    label: String,
}

impl Candidate {
    /// Text the upscale resolver can read hints (1080p, anime tags…) from.
    fn describe(&self) -> String {
        format!("{} {}", self.label, self.url)
    }
}

/// Asks every stream-capable addon, merges and ranks. For a bare series id,
/// falls back to its first episode — proper episode browsing is a later phase.
fn collect_streams(installed: &[Addon], kind: &str, meta_id: &str) -> Vec<Candidate> {
    let mut all = Vec::new();
    for addon in installed.iter().filter(|a| a.streams) {
        let mut ids = vec![meta_id.to_string()];
        if kind == "series" && !meta_id.contains(':') {
            ids.push(format!("{meta_id}:1:1"));
        }
        for id in ids {
            let url = format!(
                "{}/stream/{}/{}.json",
                addon.base,
                percent_encode(kind),
                percent_encode(&id)
            );
            if let Ok(json) = addons::http_get(&url) {
                all.extend(parse_streams(&json));
                if !all.is_empty() {
                    break; // got streams for the bare id; skip the fallback
                }
            }
        }
    }
    rank(&mut all);
    all
}

fn parse_streams(json: &str) -> Vec<Candidate> {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(streams) = value.get("streams").and_then(|s| s.as_array()) else {
        return Vec::new();
    };
    streams.iter().filter_map(candidate).collect()
}

/// Direct URLs play as-is; YouTube ids go through mpv's yt-dlp hook.
/// infoHash-only (torrent) entries are not playable here and are dropped.
fn candidate(stream: &Value) -> Option<Candidate> {
    let label = ["name", "title", "description"]
        .iter()
        .filter_map(|k| stream.get(k).and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    let url = if let Some(url) = stream.get("url").and_then(|u| u.as_str()) {
        url.to_string()
    } else if let Some(yt) = stream.get("ytId").and_then(|y| y.as_str()) {
        format!("https://www.youtube.com/watch?v={yt}")
    } else {
        return None;
    };
    Some(Candidate { url, label })
}

/// Best first: resolution read from the label, then https over http.
fn rank(streams: &mut [Candidate]) {
    streams.sort_by_key(|s| {
        let text = s.describe().to_lowercase();
        let height = [2160, 1440, 1080, 720, 480]
            .into_iter()
            .find(|h| text.contains(&format!("{h}p")) || text.contains(&format!("{h}i")))
            .unwrap_or(0);
        let https = i32::from(s.url.starts_with("https://"));
        std::cmp::Reverse(height * 10 + https)
    });
}

fn parse_catalog(json: &str) -> Vec<ContentItem> {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(metas) = value.get("metas").and_then(|m| m.as_array()) else {
        return Vec::new();
    };
    metas
        .iter()
        .take(ROW_LIMIT)
        .filter_map(|m| {
            let kind = m.get("type")?.as_str()?;
            Some(ContentItem {
                id: format!("strm:{}:{}", kind, m.get("id")?.as_str()?),
                kind: match kind {
                    "series" => Kind::Series,
                    "movie" => Kind::Movie,
                    _ => Kind::Video,
                },
                title: m.get("name")?.as_str()?.to_string(),
                art: m.get("poster").and_then(|p| p.as_str()).map(String::from),
                action: Action::Play,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_parsing_keeps_episode_colons() {
        assert_eq!(parse_id("strm:movie:tt0133093"), Ok(("movie", "tt0133093")));
        assert_eq!(
            parse_id("strm:series:tt0903747:2:8"),
            Ok(("series", "tt0903747:2:8"))
        );
        assert!(parse_id("strm:movie:").is_err());
        assert!(parse_id("rom:gb/x.gb").is_err());
    }

    #[test]
    fn parses_catalog_metas() {
        let json = r#"{"metas": [
            {"type": "movie", "id": "bbb", "name": "Big Buck Bunny", "poster": "https://x/p.jpg"},
            {"type": "series", "id": "tt1", "name": "Some Show"}
        ]}"#;
        let items = parse_catalog(json);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "strm:movie:bbb");
        assert_eq!(items[1].id, "strm:series:tt1");
    }

    #[test]
    fn streams_drop_torrents_and_map_youtube() {
        let json = r#"{"streams": [
            {"infoHash": "abcd", "title": "Torrent 2160p"},
            {"ytId": "xyz", "title": "Trailer"},
            {"url": "https://cdn/movie-1080p.mp4", "name": "CDN 1080p"}
        ]}"#;
        let mut streams = parse_streams(json);
        assert_eq!(streams.len(), 2); // torrent dropped
        rank(&mut streams);
        assert_eq!(streams[0].url, "https://cdn/movie-1080p.mp4"); // 1080p beats unlabeled
    }

    #[test]
    fn ranking_prefers_resolution_then_https() {
        let mut streams = vec![
            Candidate {
                url: "http://a/720.mkv".into(),
                label: "720p".into(),
            },
            Candidate {
                url: "https://b/2160.mkv".into(),
                label: "4K 2160p".into(),
            },
            Candidate {
                url: "https://c/1080.mkv".into(),
                label: "FHD 1080p".into(),
            },
        ];
        rank(&mut streams);
        let order: Vec<&str> = streams.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(order, vec!["4K 2160p", "FHD 1080p", "720p"]);
    }
}
