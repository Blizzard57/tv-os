//! Stremio-compatible addons: catalogs, rich metadata (with episode lists),
//! and every kind of stream they return.
//!
//! Streams come in four shapes and we support them all:
//!   - `url`         → direct/debrid stream, played in our mpv + upscaler
//!   - `ytId`        → YouTube, played via mpv's yt-dlp hook
//!   - `externalUrl` → a link to a service/app (WatchHub) → opened with the system
//!   - `infoHash`    → a BitTorrent magnet (Torrentio) → streamed via a torrent helper
//!
//! The details page lists them all and lets the user pick; `play_meta` is the
//! auto-pick fallback used when something is launched without opening details.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::addons::{self, Addon};
use crate::media::{Episode, Meta, Stream, StreamKind};
use crate::model::{Action, ContentItem, Kind, Row};
use crate::sources::Source;
use crate::util::percent_encode;
use crate::{launcher, settings, upscale};

const CATALOG_TTL: Duration = Duration::from_secs(600);
const ROW_LIMIT: usize = 25;
const CATALOGS_PER_ADDON: usize = 2;

/// A few well-known public trackers added to every magnet so a torrent can
/// find peers even when the addon supplies none.
const DEFAULT_TRACKERS: [&str; 4] = [
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://tracker.openbittorrent.com:6969/announce",
    "udp://exodus.desync.com:6969/announce",
];

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
        play_meta(kind, meta_id)
    }
}

// ---- Public resolver API used by the details endpoints (main.rs) ----

/// Full metadata (incl. episode list) from the first meta-capable addon that
/// answers. `kind` is "movie" / "series"; `id` is an IMDb-style id.
pub fn meta(kind: &str, id: &str) -> Option<Meta> {
    for addon in addons::STORE.list().iter().filter(|a| a.meta) {
        let url = format!(
            "{}/meta/{}/{}.json",
            addon.base,
            percent_encode(kind),
            percent_encode(id)
        );
        if let Ok(json) = addons::http_get(&url) {
            if let Some(meta) = parse_meta(&json) {
                return Some(meta);
            }
        }
    }
    None
}

/// Every stream from every stream-capable addon, ranked best-first. `id` is a
/// movie id (`tt…`) or an episode id (`tt…:S:E`).
pub fn streams(kind: &str, id: &str) -> Vec<Stream> {
    let mut all: Vec<Stream> = addons::STORE
        .list()
        .iter()
        .filter(|a| a.streams)
        .filter_map(|addon| {
            let url = format!(
                "{}/stream/{}/{}.json",
                addon.base,
                percent_encode(kind),
                percent_encode(id)
            );
            addons::http_get(&url).ok().map(|json| parse_streams(&json))
        })
        .flatten()
        .collect();
    rank(&mut all);
    all
}

/// Launches a stream the user (or the auto-picker) chose.
pub fn play_stream(stream: &Stream) -> Result<(), String> {
    let mode = settings::STORE.get().enhance;
    let hint = describe(stream);
    match stream.kind {
        StreamKind::Direct | StreamKind::Youtube => {
            let profile = upscale::resolve(mode, &hint);
            launcher::play_video(&stream.url, &profile, mode, &hint)
        }
        StreamKind::External => launcher::open_external(&stream.url),
        StreamKind::Torrent => {
            let profile = upscale::resolve(mode, &hint);
            launcher::play_torrent(&stream.url, stream.file_idx, &profile, mode, &hint)
        }
    }
}

/// Auto-pick: best playable stream for `(kind, id)`. Used by Source::launch
/// (the quick path). Prefers a directly-playable stream; for a bare series id
/// it falls back to the first episode.
pub fn play_meta(kind: &str, id: &str) -> Result<(), String> {
    let mut found = streams(kind, id);
    if found.is_empty() && kind == "series" && !id.contains(':') {
        found = streams(kind, &format!("{id}:1:1"));
    }
    let pick = found
        .iter()
        .find(|s| matches!(s.kind, StreamKind::Direct | StreamKind::Youtube))
        .or_else(|| found.iter().find(|s| s.kind == StreamKind::Torrent))
        .or_else(|| found.first())
        .ok_or("No playable stream found — install a stream addon that carries this title")?;
    crate::log_info!(
        "auto-pick: [{:?}] {}",
        pick.kind,
        pick.name.replace('\n', " ")
    );
    play_stream(pick)
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

/// Hint text the upscale resolver reads (resolution, anime tags…).
fn describe(stream: &Stream) -> String {
    format!("{} {} {}", stream.name, stream.title, stream.url)
}

// ---- Parsing ----

fn parse_streams(json: &str) -> Vec<Stream> {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(streams) = value.get("streams").and_then(|s| s.as_array()) else {
        return Vec::new();
    };
    streams.iter().filter_map(parse_stream).collect()
}

fn parse_stream(v: &Value) -> Option<Stream> {
    let str_of = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    let name = str_of("name").unwrap_or_default();
    let title = str_of("title")
        .or_else(|| str_of("description"))
        .unwrap_or_default();

    let (kind, url, file_idx) = if let Some(url) = str_of("url") {
        (StreamKind::Direct, url, None)
    } else if let Some(yt) = str_of("ytId") {
        (
            StreamKind::Youtube,
            format!("https://www.youtube.com/watch?v={yt}"),
            None,
        )
    } else if let Some(ext) = str_of("externalUrl") {
        (StreamKind::External, ext, None)
    } else if let Some(hash) = str_of("infoHash") {
        let file_idx = v.get("fileIdx").and_then(|x| x.as_i64());
        let dn = if name.is_empty() { &title } else { &name };
        (
            StreamKind::Torrent,
            build_magnet(&hash, dn, v.get("sources")),
            file_idx,
        )
    } else {
        return None;
    };

    Some(Stream {
        kind,
        url,
        name: if name.is_empty() {
            "Source".to_string()
        } else {
            name
        },
        title,
        file_idx,
    })
}

/// Builds a magnet URI from an infoHash, the display name, the addon's tracker
/// `sources`, and a handful of default public trackers.
fn build_magnet(info_hash: &str, name: &str, sources: Option<&Value>) -> String {
    let mut magnet = format!("magnet:?xt=urn:btih:{info_hash}");
    let clean_name = name.replace('\n', " ");
    if !clean_name.is_empty() {
        magnet.push_str(&format!("&dn={}", percent_encode(&clean_name)));
    }
    if let Some(list) = sources.and_then(|s| s.as_array()) {
        for tr in list
            .iter()
            .filter_map(|s| s.as_str())
            .filter_map(|s| s.strip_prefix("tracker:"))
        {
            magnet.push_str(&format!("&tr={}", percent_encode(tr)));
        }
    }
    for tr in DEFAULT_TRACKERS {
        magnet.push_str(&format!("&tr={}", percent_encode(tr)));
    }
    magnet
}

/// Orders the details-page list: resolution desc, then directly-playable
/// sources ahead of torrents/externals.
fn rank(streams: &mut [Stream]) {
    streams.sort_by_key(|s| {
        let text = describe(s).to_lowercase();
        let height = [2160, 1440, 1080, 720, 480]
            .into_iter()
            .find(|h| text.contains(&format!("{h}p")) || text.contains(&format!("{h}i")))
            .unwrap_or(0);
        let kind_rank = match s.kind {
            StreamKind::Direct => 3,
            StreamKind::Torrent => 2,
            StreamKind::Youtube => 1,
            StreamKind::External => 0,
        };
        std::cmp::Reverse((height, kind_rank))
    });
}

fn parse_meta(json: &str) -> Option<Meta> {
    let value = serde_json::from_str::<Value>(json).ok()?;
    let m = value.get("meta")?;
    let str_of = |k: &str| m.get(k).and_then(|x| x.as_str()).map(String::from);
    let kind = str_of("type").unwrap_or_else(|| "movie".to_string());

    let genres = m
        .get("genres")
        .and_then(|g| g.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut episodes: Vec<Episode> = m
        .get("videos")
        .and_then(|v| v.as_array())
        .map(|vids| vids.iter().filter_map(parse_episode).collect())
        .unwrap_or_default();
    episodes.sort_by_key(|e| (e.season, e.episode));

    Some(Meta {
        id: str_of("id").unwrap_or_default(),
        kind,
        title: str_of("name").unwrap_or_default(),
        poster: str_of("poster"),
        background: str_of("background"),
        logo: str_of("logo"),
        description: str_of("description").or_else(|| str_of("overview")),
        release_info: str_of("releaseInfo").or_else(|| str_of("year")),
        rating: str_of("imdbRating"),
        runtime: str_of("runtime"),
        genres,
        episodes,
        ..Default::default()
    })
}

fn parse_episode(v: &Value) -> Option<Episode> {
    let id = v.get("id")?.as_str()?.to_string();
    let season = v.get("season").and_then(|x| x.as_i64()).unwrap_or(0);
    let episode = v
        .get("episode")
        .or_else(|| v.get("number"))
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let title = v
        .get("name")
        .or_else(|| v.get("title"))
        .and_then(|x| x.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("Episode {episode}"));
    Some(Episode {
        id,
        title,
        season,
        episode,
        overview: v
            .get("overview")
            .or_else(|| v.get("description"))
            .and_then(|x| x.as_str())
            .map(String::from),
        thumbnail: v
            .get("thumbnail")
            .and_then(|x| x.as_str())
            .map(String::from),
        released: v
            .get("released")
            .or_else(|| v.get("firstAired"))
            .and_then(|x| x.as_str())
            .map(String::from),
    })
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
    fn parses_every_stream_kind() {
        // Shapes taken from real Torrentio / WatchHub / direct responses.
        let json = r#"{"streams": [
            {"name": "CDN 1080p", "url": "https://cdn/m-1080p.mp4"},
            {"name": "Torrentio\n4k", "title": "Movie.2160p.mkv\n👤 89", "infoHash": "ABC123",
             "fileIdx": 2, "sources": ["tracker:udp://t.example:1337/announce", "dht:xyz"]},
            {"name": "Netflix", "externalUrl": "https://www.netflix.com/title/1"},
            {"name": "Trailer", "ytId": "dQw4"}
        ]}"#;
        let s = parse_streams(json);
        assert_eq!(s.len(), 4);
        let by = |k: StreamKind| s.iter().find(|x| x.kind == k).unwrap();
        assert_eq!(by(StreamKind::Direct).url, "https://cdn/m-1080p.mp4");
        assert_eq!(
            by(StreamKind::External).url,
            "https://www.netflix.com/title/1"
        );
        assert_eq!(
            by(StreamKind::Youtube).url,
            "https://www.youtube.com/watch?v=dQw4"
        );
        let torrent = by(StreamKind::Torrent);
        assert_eq!(torrent.file_idx, Some(2));
        assert!(torrent.url.starts_with("magnet:?xt=urn:btih:ABC123"));
        assert!(torrent.url.contains("tracker.opentrackr.org")); // default tracker added
        assert!(torrent.url.contains("t.example")); // addon-supplied tracker kept
    }

    #[test]
    fn ranking_prefers_resolution_then_direct() {
        let mk = |kind, name: &str| Stream {
            kind,
            url: "x".into(),
            name: name.into(),
            title: String::new(),
            file_idx: None,
        };
        let mut streams = vec![
            mk(StreamKind::Torrent, "720p"),
            mk(StreamKind::Torrent, "2160p"),
            mk(StreamKind::Direct, "1080p"),
            mk(StreamKind::Direct, "2160p"),
        ];
        rank(&mut streams);
        let order: Vec<&str> = streams.iter().map(|s| s.name.as_str()).collect();
        // 2160p direct, then 2160p torrent, then 1080p direct, then 720p torrent.
        assert_eq!(order, vec!["2160p", "2160p", "1080p", "720p"]);
        assert_eq!(streams[0].kind, StreamKind::Direct);
    }

    #[test]
    fn parses_series_meta_with_sorted_episodes() {
        let json = r#"{"meta": {
            "id": "tt1", "type": "series", "name": "Show",
            "poster": "p.jpg", "description": "A show.", "genres": ["Drama", "Sci-Fi"],
            "videos": [
                {"id": "tt1:1:2", "name": "Second", "season": 1, "episode": 2},
                {"id": "tt1:1:1", "name": "Pilot", "season": 1, "number": 1, "overview": "start"}
            ]
        }}"#;
        let meta = parse_meta(json).unwrap();
        assert_eq!(meta.title, "Show");
        assert_eq!(meta.genres, vec!["Drama", "Sci-Fi"]);
        assert_eq!(meta.episodes.len(), 2);
        assert_eq!(meta.episodes[0].id, "tt1:1:1"); // sorted
        assert_eq!(meta.episodes[0].title, "Pilot");
        assert_eq!(meta.episodes[1].episode, 2);
    }

    #[test]
    fn garbage_inputs_are_safe() {
        assert!(parse_streams("oops").is_empty());
        assert!(parse_meta("oops").is_none());
        assert!(parse_catalog("oops").is_empty());
    }
}
