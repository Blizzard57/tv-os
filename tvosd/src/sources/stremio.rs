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

/// Short caches for the resolver results so re-opening a details page (or the
/// auto-picker firing right after the UI) doesn't refetch every addon.
const META_TTL: Duration = Duration::from_secs(300);
const STREAM_TTL: Duration = Duration::from_secs(60);

static META_CACHE: std::sync::LazyLock<Mutex<HashMap<String, (Instant, Option<Meta>)>>> =
    std::sync::LazyLock::new(Mutex::default);
static STREAM_CACHE: std::sync::LazyLock<Mutex<HashMap<String, (Instant, Vec<Stream>)>>> =
    std::sync::LazyLock::new(Mutex::default);

/// Well-known, high-population public trackers added to every magnet so peers
/// are found fast (quicker start) even when the addon supplies few or none.
const DEFAULT_TRACKERS: [&str; 8] = [
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://tracker.openbittorrent.com:6969/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://tracker.dler.org:6969/announce",
    "udp://explodie.org:6969/announce",
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
                    .filter(|c| c.browse)
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
        play_meta(kind, meta_id, Some(item_id))
    }
}

// ---- Public resolver API used by the details endpoints (main.rs) ----

/// Full metadata (incl. episode list) from the first meta-capable addon that
/// answers. `kind` is "movie" / "series"; `id` is an IMDb-style id. Addons are
/// queried in parallel so one slow addon can't hold up the details page, and
/// the result is cached briefly.
pub fn meta(kind: &str, id: &str) -> Option<Meta> {
    let cache_key = format!("{kind}:{id}");
    if let Some((at, meta)) = META_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&cache_key)
    {
        if at.elapsed() < META_TTL {
            return meta.clone();
        }
    }

    let addons = addons::STORE.list();
    let meta = std::thread::scope(|scope| {
        let handles: Vec<_> = addons
            .iter()
            .filter(|a| a.meta)
            .map(|addon| {
                scope.spawn(move || {
                    let url = format!(
                        "{}/meta/{}/{}.json",
                        addon.base,
                        percent_encode(kind),
                        percent_encode(id)
                    );
                    addons::http_get(&url).ok().and_then(|json| parse_meta(&json))
                })
            })
            .collect();
        // First addon (in list order) that returned metadata wins.
        handles
            .into_iter()
            .find_map(|h| h.join().ok().flatten())
    });

    let mut cache = META_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 64 {
        cache.clear();
    }
    cache.insert(cache_key, (Instant::now(), meta.clone()));
    meta
}

/// Every stream from every stream-capable addon, ranked best-first. `id` is a
/// movie id (`tt…`) or an episode id (`tt…:S:E`). Addons are queried in
/// parallel so playback isn't held up by the slowest one, and results are
/// cached for a short window.
pub fn streams(kind: &str, id: &str) -> Vec<Stream> {
    let cache_key = format!("{kind}:{id}");
    if let Some((at, streams)) = STREAM_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&cache_key)
    {
        if at.elapsed() < STREAM_TTL {
            return streams.clone();
        }
    }

    let addons = addons::STORE.list();
    let mut all: Vec<Stream> = std::thread::scope(|scope| {
        let handles: Vec<_> = addons
            .iter()
            .filter(|a| a.streams)
            .map(|addon| {
                scope.spawn(move || {
                    let url = format!(
                        "{}/stream/{}/{}.json",
                        addon.base,
                        percent_encode(kind),
                        percent_encode(id)
                    );
                    addons::http_get(&url)
                        .ok()
                        .map(|json| parse_streams(&json))
                        .unwrap_or_default()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    });
    rank(&mut all);

    let mut cache = STREAM_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 64 {
        cache.clear();
    }
    cache.insert(cache_key, (Instant::now(), all.clone()));
    all
}

const SEARCH_TTL: Duration = Duration::from_secs(300);
const SEARCH_CATALOGS_PER_ADDON: usize = 3;

static SEARCH_CACHE: std::sync::LazyLock<Mutex<HashMap<String, (Instant, Vec<ContentItem>)>>> =
    std::sync::LazyLock::new(Mutex::default);

/// Searches every installed addon's search-capable catalogs (the `search`
/// extra of the addon protocol). Addons are queried in parallel and results
/// cached briefly, so search-as-you-type stays responsive.
pub fn search(query: &str) -> Vec<ContentItem> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    if let Some((at, items)) = SEARCH_CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(&query.to_lowercase()) {
        if at.elapsed() < SEARCH_TTL {
            return items.clone();
        }
    }

    let addons = addons::STORE.list();
    let mut items: Vec<ContentItem> = Vec::new();
    std::thread::scope(|scope| {
        let handles: Vec<_> = addons
            .iter()
            .map(|addon| {
                scope.spawn(move || {
                    addon
                        .catalogs
                        .iter()
                        .filter(|c| c.search)
                        .take(SEARCH_CATALOGS_PER_ADDON)
                        .flat_map(|catalog| {
                            let url = format!(
                                "{}/catalog/{}/{}/search={}.json",
                                addon.base,
                                percent_encode(&catalog.kind),
                                percent_encode(&catalog.id),
                                percent_encode(query)
                            );
                            addons::http_get_quick(&url)
                                .map(|json| parse_catalog(&json))
                                .unwrap_or_default()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut seen = std::collections::HashSet::new();
        for handle in handles {
            for item in handle.join().unwrap_or_default() {
                if seen.insert(item.id.clone()) {
                    items.push(item);
                }
            }
        }
    });

    let mut cache = SEARCH_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 64 {
        cache.clear(); // crude but sufficient: queries are tiny and short-lived
    }
    cache.insert(query.to_lowercase(), (Instant::now(), items.clone()));
    items
}

/// Launches a stream the user (or the auto-picker) chose. `item_id` is the
/// content id used for resume + remembering the source for the next episode
/// (a series id, so "Continue" surfaces the *show*). `track_id` is the precise
/// watched id for the scrobbler — an episode id (`…:season:episode`) so Trakt
/// and AniList record the exact episode; it defaults to `item_id`.
pub fn play_stream(
    stream: &Stream,
    item_id: Option<&str>,
    track_id: Option<&str>,
) -> Result<(), String> {
    let mode = settings::STORE.get().enhance;
    let hint = describe(stream);
    // Remember the source so Continue / the next episode can reuse it.
    if let Some(id) = item_id {
        if stream.kind != StreamKind::External {
            crate::resume::STORE.remember(id, stream);
        }
    }
    let track_id = track_id.or(item_id);
    match stream.kind {
        StreamKind::Direct | StreamKind::Youtube => {
            let profile = upscale::resolve(mode, &hint);
            launcher::play_video(&stream.url, &profile, mode, &hint, item_id, track_id)
        }
        StreamKind::External => launcher::open_external(&stream.url),
        StreamKind::Torrent => {
            let profile = upscale::resolve(mode, &hint);
            launcher::play_torrent(
                &stream.url,
                stream.file_idx,
                &profile,
                mode,
                &hint,
                item_id,
                track_id,
            )
        }
    }
}

/// How many sources the auto-picker will try before giving up. Each attempt now
/// waits for playback to actually start, so a dead/seederless source fails fast-
/// ish and we move on — "sometimes the stream doesn't start" becomes "we tried
/// the next one".
const AUTO_PICK_ATTEMPTS: usize = 3;

/// Auto-pick: play the best source for `(kind, id)`, falling back to the next
/// one if it doesn't start. Used by Source::launch (the quick path). Prefers the
/// source the show last used (so the next episode stays on the same addon /
/// quality), then directly-playable streams, then torrents. `item_id` is the
/// content id, used for resume and same-source memory.
pub fn play_meta(kind: &str, id: &str, item_id: Option<&str>) -> Result<(), String> {
    let mut found = streams(kind, id);
    if found.is_empty() && kind == "series" && !id.contains(':') {
        found = streams(kind, &format!("{id}:1:1"));
    }
    if found.is_empty() {
        return Err(
            "No playable stream found — install a stream addon that carries this title".to_string(),
        );
    }

    // For the next episode of a show, float sources matching the one the show
    // last played (same addon + quality) to the very front.
    let preferred = item_id.and_then(|id| crate::resume::STORE.series_stream(id));
    let matches_preferred = |s: &Stream| {
        preferred
            .as_ref()
            .is_some_and(|p| same_source(p, s))
    };

    // Order: remembered source → directly-playable → torrents → externals.
    let mut order: Vec<&Stream> = found.iter().filter(|s| matches_preferred(s)).collect();
    order.extend(
        found
            .iter()
            .filter(|s| !matches_preferred(s) && matches!(s.kind, StreamKind::Direct | StreamKind::Youtube)),
    );
    order.extend(
        found
            .iter()
            .filter(|s| !matches_preferred(s) && s.kind == StreamKind::Torrent),
    );
    order.extend(found.iter().filter(|s| s.kind == StreamKind::External));

    let mut last_error = "No source could be started".to_string();
    for stream in order.into_iter().take(AUTO_PICK_ATTEMPTS) {
        crate::log_info!(
            "auto-pick trying [{:?}] {}",
            stream.kind,
            stream.name.replace('\n', " ")
        );
        match play_stream(stream, item_id, None) {
            Ok(()) => return Ok(()),
            Err(e) => {
                crate::log_warn!("source failed, trying next: {e}");
                last_error = e;
            }
        }
    }
    Err(last_error)
}

/// Whether two streams are "the same source" — same addon/release/quality,
/// ignoring the per-episode bits. Used to keep a show on one source. The whole
/// `name` (provider + quality, e.g. "Torrentio\n1080p") is compared: per-episode
/// details live in `title`, so `name` is stable across a season.
fn same_source(a: &Stream, b: &Stream) -> bool {
    a.kind == b.kind && a.name == b.name
}

impl Stremio {
    fn catalog_items(&self, addon: &Addon, kind: &str, catalog_id: &str) -> Vec<ContentItem> {
        let url = format!(
            "{}/catalog/{}/{}.json",
            addon.base,
            percent_encode(kind),
            percent_encode(catalog_id)
        );
        let mut cache = self.catalog_cache.lock().unwrap_or_else(|e| e.into_inner());
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

/// Orders the list best-first: the **highest resolution that will actually
/// stream**. Directly-playable (debrid/HTTP) sources win outright; among
/// torrents we group by a seeder "tier" (enough seeders to stream → preferred,
/// a few → weaker, none → dead/last) and, within a tier, prefer higher
/// resolution. So a well-seeded 4K ranks first (best for a capable system),
/// while a seederless 4K that would never start drops below a solid 1080p.
fn rank(streams: &mut [Stream]) {
    streams.sort_by(|a, b| {
        stream_score(b)
            .partial_cmp(&stream_score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Seeders considered "enough" to stream the higher resolutions comfortably.
const GOOD_SEEDERS: i64 = 5;

/// Higher is better. See [`rank`].
fn stream_score(s: &Stream) -> f64 {
    let text = describe(s);
    let height = resolution_height(&text.to_lowercase());
    match s.kind {
        // Instant, reliable sources first; still prefer higher resolution, and
        // (per the docs) nudge https above plain http on an otherwise tie.
        StreamKind::Direct => {
            let https_bonus = if s.url.starts_with("https") { 1.0 } else { 0.0 };
            1_000_000_000.0 + height + https_bonus
        }
        StreamKind::Youtube => 500_000_000.0,
        // Official "watch on …" services (WatchHub): visible above the
        // torrent pile so they read as recommendations, not buried under 50
        // entries. The auto-picker still skips them (it filters by kind).
        StreamKind::External => 400_000_000.0,
        StreamKind::Torrent => {
            let seeders = parse_seeders(&text).unwrap_or(0);
            // Tier first (viability), then resolution (4K on top), then seeders.
            let tier = if seeders >= GOOD_SEEDERS {
                2.0
            } else if seeders >= 1 {
                1.0
            } else {
                0.0
            };
            // Tiny tiebreak: among otherwise-equal sources, the smaller file
            // starts a touch faster (always < 1, so it never beats a seeder).
            let smaller = parse_size_gb(&text)
                .map(|gb| gb.min(100.0) / 1000.0)
                .unwrap_or(0.0);
            tier * 100_000_000.0 + height * 1_000.0 + (seeders.min(9_999) as f64) - smaller
        }
    }
}

/// The numeric resolution height advertised in the source name (2160, 1080, …),
/// used so the best quality sorts to the top of its tier. Tokens must sit on a
/// word boundary so short ones ("4k"/"uhd") don't match inside a title (e.g.
/// "H4KER" or a movie literally named "UHD").
fn resolution_height(lower: &str) -> f64 {
    if has_token(lower, "2160p") || has_token(lower, "4k") || has_token(lower, "uhd") {
        2160.0
    } else if has_token(lower, "1440p") {
        1440.0
    } else if has_token(lower, "1080p") || has_token(lower, "1080i") {
        1080.0
    } else if has_token(lower, "720p") {
        720.0
    } else if has_token(lower, "480p") {
        480.0
    } else {
        600.0 // unknown — between SD and HD
    }
}

/// Whether `token` appears in `haystack` bounded by non-alphanumeric
/// characters (or the string edges) — a lightweight word-boundary match so a
/// short quality tag can't hit letters inside an unrelated word.
fn has_token(haystack: &str, token: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut from = 0;
    while let Some(rel) = haystack[from..].find(token) {
        let start = from + rel;
        let end = start + token.len();
        let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let after_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// Seeder count from a Torrentio-style "👤 89" label, if present.
fn parse_seeders(text: &str) -> Option<i64> {
    let after = text.split('👤').nth(1)?.trim_start();
    let digits: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .collect();
    digits.replace(',', "").parse().ok()
}

/// File size in GB from a "💾 35.09 GB" / "💾 700 MB" label, if present.
fn parse_size_gb(text: &str) -> Option<f64> {
    let after = text.split('💾').nth(1)?.trim_start();
    let num: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let value: f64 = num.parse().ok()?;
    let unit = after[num.len()..].trim_start().to_uppercase();
    if unit.starts_with("TB") {
        Some(value * 1024.0)
    } else if unit.starts_with("GB") {
        Some(value)
    } else if unit.starts_with("MB") {
        Some(value / 1024.0)
    } else {
        None
    }
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
            // Require a poster — skip catalog entries with no artwork.
            let art = m
                .get("poster")
                .and_then(|p| p.as_str())
                .filter(|p| !p.is_empty())
                .map(String::from)
                .or_else(|| {
                    m.get("background")
                        .and_then(|p| p.as_str())
                        .filter(|p| !p.is_empty())
                        .map(String::from)
                })?;
            Some(ContentItem {
                id: format!("strm:{}:{}", kind, m.get("id")?.as_str()?),
                kind: match kind {
                    "series" => Kind::Series,
                    "movie" => Kind::Movie,
                    _ => Kind::Video,
                },
                title: m.get("name")?.as_str()?.to_string(),
                art: Some(art),
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
            {"type": "series", "id": "tt1", "name": "Some Show", "poster": "https://x/s.jpg"},
            {"type": "movie", "id": "nope", "name": "No Art"}
        ]}"#;
        let items = parse_catalog(json);
        assert_eq!(items.len(), 2); // the art-less "No Art" entry is dropped
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
    fn ranking_prefers_streamable_sources() {
        let mk = |kind, name: &str, title: &str| Stream {
            kind,
            url: "x".into(),
            name: name.into(),
            title: title.into(),
            file_idx: None,
        };
        let mut streams = vec![
            // a barely-seeded 4K — would never start, so it must drop down
            mk(StreamKind::Torrent, "4k weak", "Movie.2160p.REMUX 👤 1 💾 60 GB"),
            // a well-seeded 1080p
            mk(StreamKind::Torrent, "1080p", "Movie.1080p 👤 200 💾 2.4 GB"),
            // a debrid/direct link — always wins
            mk(StreamKind::Direct, "direct", "Movie.1080p"),
            // "watch on …" service (WatchHub) — visible above the torrents
            mk(StreamKind::External, "netflix", "Subscription"),
            // a low-but-ok-seeded 720p
            mk(StreamKind::Torrent, "720p", "Movie.720p 👤 6 💾 1.1 GB"),
            // a well-seeded 4K — best for a capable system, should top the torrents
            mk(StreamKind::Torrent, "4k", "Movie.2160p 👤 80 💾 40 GB"),
        ];
        rank(&mut streams);
        let order: Vec<&str> = streams.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(order, vec!["direct", "netflix", "4k", "1080p", "720p", "4k weak"]);
        assert_eq!(streams[0].kind, StreamKind::Direct);
    }

    #[test]
    fn resolution_tokens_need_word_boundaries() {
        // Real quality tags match…
        assert_eq!(resolution_height("movie.2160p.remux"), 2160.0);
        assert_eq!(resolution_height("show 4k hdr"), 2160.0);
        assert_eq!(resolution_height("great.uhd.rip"), 2160.0);
        assert_eq!(resolution_height("thing 1080p"), 1080.0);
        // …but the same letters buried inside a title do not.
        assert_eq!(resolution_height("h4ker the movie"), 600.0);
        assert_eq!(resolution_height("uhded up"), 600.0);
    }

    #[test]
    fn direct_https_outranks_http_on_a_tie() {
        let mk = |url: &str| Stream {
            kind: StreamKind::Direct,
            url: url.into(),
            name: "src".into(),
            title: "Movie 1080p".into(),
            file_idx: None,
        };
        assert!(stream_score(&mk("https://cdn/x.mp4")) > stream_score(&mk("http://cdn/x.mp4")));
    }

    #[test]
    fn same_source_matches_provider_and_quality() {
        let mk = |name: &str| Stream {
            kind: StreamKind::Torrent,
            url: "x".into(),
            name: name.into(),
            title: String::new(),
            file_idx: None,
        };
        assert!(same_source(&mk("Torrentio\n1080p"), &mk("Torrentio\n1080p")));
        assert!(!same_source(&mk("Torrentio\n1080p"), &mk("Torrentio\n4k")));
    }

    #[test]
    fn parses_seeders_and_size() {
        assert_eq!(parse_seeders("Torrentio 👤 89 💾 35.09 GB"), Some(89));
        assert_eq!(parse_seeders("👤 1,234 peers"), Some(1234));
        assert_eq!(parse_seeders("no marker"), None);
        assert_eq!(parse_size_gb("💾 35.09 GB"), Some(35.09));
        assert_eq!(parse_size_gb("💾 700 MB"), Some(700.0 / 1024.0));
        assert_eq!(parse_size_gb("nothing"), None);
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
