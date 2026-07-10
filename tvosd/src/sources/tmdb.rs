//! TMDB discovery source: Google-TV-style recommendation rows (Trending,
//! Popular, New, Top Rated) — see `CATALOGS`.
//!
//! Enabled by a TMDB API key set in the Settings panel (or the TVOS_TMDB_KEY
//! env var). TMDB only provides metadata/art, not streams — so to *play* an
//! item we map its TMDB id to an IMDb id (TMDB external_ids) and hand that to
//! the Stremio stream resolver, which is exactly what Stremio addons key on.
//! That means TMDB browsing is playable as long as a stream addon carries the
//! title. Results are cached so the home screen stays fast.

use std::collections::HashMap;
use std::sync::{Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::model::{Action, ContentItem, Kind, Row};
use crate::settings;
use crate::sources::{stremio, Source};
use crate::util::percent_encode;

const CACHE_TTL: Duration = Duration::from_secs(900);
const ROW_LIMIT: usize = 25;
/// Cap on simultaneous TMDB requests across the parallel catalog fan-out, so we
/// stay well under TMDB's rate limit instead of firing a dozen at once.
const MAX_CONCURRENT_FETCHES: usize = 4;

/// A tiny counting semaphore for bounding fan-out concurrency. `acquire`
/// returns a guard that releases the permit on drop.
struct Semaphore {
    count: Mutex<usize>,
    cvar: Condvar,
}

struct Permit<'a>(&'a Semaphore);

impl Semaphore {
    fn new(permits: usize) -> Self {
        Semaphore {
            count: Mutex::new(permits),
            cvar: Condvar::new(),
        }
    }

    fn acquire(&self) -> Permit<'_> {
        let mut count = self.count.lock().unwrap_or_else(|e| e.into_inner());
        while *count == 0 {
            count = self.cvar.wait(count).unwrap();
        }
        *count -= 1;
        Permit(self)
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *self.0.count.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.0.cvar.notify_one();
    }
}

#[derive(Default)]
pub struct Tmdb {
    /// Cached rows keyed by request URL (the URL embeds the key, so a key
    /// change naturally misses the cache).
    cache: Mutex<HashMap<String, (Instant, Vec<ContentItem>)>>,
}

/// The home is strictly a recommendation feed (Google-TV "For you"), so TMDB
/// contributes NO generic browse rows — no Trending / Popular / Top Rated /
/// genre catalogues. The personalized rows come from `for_you` ("Top picks for
/// you") and `because_you_watched` ("Because you watched X") instead; the
/// candidate pool for those is `candidate_corpus`, not this list. Kept as an
/// (empty) hook so a future setting could opt back into browse rows.
const CATALOGS: &[(&str, &str, &str)] = &[];

impl Tmdb {
    /// Key from settings, falling back to the env var for headless setups.
    fn api_key(&self) -> Option<String> {
        api_key()
    }

    /// Fetches one catalog (`path` is a TMDB path + optional query), cached by
    /// URL. The cache lock is released across the HTTP call so the many home-row
    /// fetches can run in parallel.
    fn catalog(&self, key: &str, media: &str, path: &str) -> Vec<ContentItem> {
        let sep = if path.contains('?') { '&' } else { '?' };
        let url = format!("https://api.themoviedb.org/3/{path}{sep}api_key={key}");
        if let Some((at, items)) = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&url)
        {
            if at.elapsed() < CACHE_TTL {
                return items.clone();
            }
        }
        let items = fetch(&url)
            .map(|json| parse_trending(&json, media))
            .unwrap_or_default();
        if !items.is_empty() {
            self.cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(url, (Instant::now(), items.clone()));
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
        // Fetch catalogs in parallel, but cap concurrency so we don't open a
        // dozen simultaneous connections to TMDB and trip its rate limiter.
        let gate = Semaphore::new(MAX_CONCURRENT_FETCHES);
        std::thread::scope(|scope| {
            let handles: Vec<_> = CATALOGS
                .iter()
                .map(|&(title, media, path)| {
                    let key = &key;
                    let gate = &gate;
                    scope.spawn(move || {
                        let _permit = gate.acquire();
                        Row {
                            title: title.to_string(),
                            items: self.catalog(key, media, path),
                        }
                    })
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|h| match h.join() {
                    Ok(row) => Some(row),
                    Err(_) => {
                        crate::log_warn!("tmdb: a catalog fetch thread panicked — skipping");
                        None
                    }
                })
                .collect()
        })
    }

    fn launch(&self, item_id: &str) -> Result<(), String> {
        let key = self.api_key().ok_or("Set a TMDB API key in Settings")?;
        let (media, tmdb_id) = parse_id(item_id)?;
        let imdb =
            imdb_id(&key, media, tmdb_id).ok_or("Couldn't find this title's IMDb id on TMDB")?;
        // TMDB "tv" is Stremio "series".
        let stremio_kind = if media == "tv" { "series" } else { "movie" };
        stremio::play_meta(stremio_kind, &imdb, Some(item_id))
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
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .ok()?;
    // A couple of retries with backoff on HTTP 429 (TMDB's rate limiter),
    // honoring Retry-After when present. Other statuses fail fast as before.
    let mut backoff = Duration::from_millis(500);
    for attempt in 0..3 {
        let resp = client.get(url).send().ok()?;
        if resp.status().as_u16() == 429 {
            let wait = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs)
                .unwrap_or(backoff);
            if attempt < 2 {
                std::thread::sleep(wait.min(Duration::from_secs(5)));
                backoff *= 2;
                continue;
            }
        }
        return resp.error_for_status().ok()?.text().ok();
    }
    None
}

/// TMDB key from settings, falling back to the env var (headless setups).
fn api_key() -> Option<String> {
    let from_settings = settings::STORE.get().tmdb_key;
    if !from_settings.is_empty() {
        return Some(from_settings);
    }
    std::env::var("TVOS_TMDB_KEY")
        .ok()
        .filter(|k| !k.is_empty())
}

/// The embedding recommender's candidate corpus (rebuilt hourly).
static CORPUS: LazyLock<Mutex<Option<(Instant, Vec<(ContentItem, String)>)>>> =
    LazyLock::new(|| Mutex::new(None));
/// Item id → embedding vector (covers both corpus and watched items).
static EMB_CACHE: LazyLock<Mutex<HashMap<String, Vec<f32>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// TMDB genre id → name (movie + tv merged), fetched once.
static GENRES: LazyLock<Mutex<Option<HashMap<i64, String>>>> = LazyLock::new(|| Mutex::new(None));

const CORPUS_TTL: Duration = Duration::from_secs(3600);

/// Lists that make up the recommendation candidate pool (default media, path).
/// "all" means the items carry their own media_type (trending/all).
const CORPUS_SOURCES: &[(&str, &str)] = &[
    ("all", "trending/all/week"),
    (
        "movie",
        "discover/movie?sort_by=popularity.desc&vote_count.gte=300",
    ),
    ("tv", "discover/tv?sort_by=popularity.desc"),
    ("movie", "movie/top_rated"),
    ("tv", "tv/top_rated"),
    ("tv", "discover/tv?with_genres=16&sort_by=popularity.desc"),
];

/// Pre-builds and embeds the candidate corpus so the first "For You" is instant.
/// Called from the background warmup once the embedding model is ready.
pub fn prewarm_recommender() {
    if let Some(key) = api_key() {
        let corpus = candidate_corpus(&key);
        embed_corpus(&corpus);
    }
}

/// On-box embedding recommender (PLAN.md §6): builds a taste vector from your
/// recent watches (recency-weighted mean of their embeddings — games and video
/// in the same space) and ranks a catalog of candidates by cosine similarity.
fn for_you_embeddings(key: &str, recent: &[ContentItem]) -> Vec<Row> {
    let Some(profile) = profile_vector(key, recent) else {
        return Vec::new();
    };
    let corpus = candidate_corpus(key);
    if corpus.is_empty() {
        return Vec::new();
    }
    embed_corpus(&corpus);

    let watched: std::collections::HashSet<&str> = recent.iter().map(|i| i.id.as_str()).collect();
    let cache = EMB_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let mut scored: Vec<(f32, ContentItem)> = corpus
        .iter()
        .filter(|(item, _)| !watched.contains(item.id.as_str()))
        .filter_map(|(item, _)| {
            let vec = cache.get(&item.id)?;
            Some((crate::embed::cosine(&profile, vec), item.clone()))
        })
        .collect();
    drop(cache);
    if scored.is_empty() {
        return Vec::new();
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    vec![Row {
        title: "Top picks for you".to_string(),
        items: scored.into_iter().take(25).map(|(_, item)| item).collect(),
    }]
}

/// Recency-weighted mean of the recent watches' embeddings = the taste vector.
fn profile_vector(key: &str, recent: &[ContentItem]) -> Option<Vec<f32>> {
    let mut acc: Vec<f32> = Vec::new();
    let mut weight_sum = 0.0f32;
    for (idx, item) in recent.iter().take(8).enumerate() {
        let Some(vec) = ensure_embedding(key, item) else {
            continue;
        };
        let weight = 1.0 / (idx as f32 + 1.0); // newest watch weighs most
        if acc.is_empty() {
            acc = vec![0.0; vec.len()];
        }
        for (a, v) in acc.iter_mut().zip(&vec) {
            *a += v * weight;
        }
        weight_sum += weight;
    }
    if weight_sum == 0.0 {
        return None;
    }
    for a in &mut acc {
        *a /= weight_sum;
    }
    Some(acc)
}

/// Embedding for a watched item, cached. Resolves rich text (title + genres +
/// overview) for video; games fall back to their title (so taste still crosses
/// domains, e.g. an anime habit nudging anime games).
fn ensure_embedding(key: &str, item: &ContentItem) -> Option<Vec<f32>> {
    if let Some(v) = EMB_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&item.id)
    {
        return Some(v.clone());
    }
    let text = item_text(key, item)?;
    let vec = crate::embed::embed(vec![text])?.into_iter().next()?;
    EMB_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(item.id.clone(), vec.clone());
    Some(vec)
}

fn item_text(key: &str, item: &ContentItem) -> Option<String> {
    if let Some((media, tmdb_id)) = seed(key, item) {
        if let Some(text) = detail_text(key, &media, &tmdb_id) {
            return Some(text);
        }
    }
    (!item.title.is_empty()).then(|| item.title.clone())
}

/// "Title. Genre, Genre. Overview." from a TMDB detail lookup.
fn detail_text(key: &str, media: &str, tmdb_id: &str) -> Option<String> {
    let url = format!("https://api.themoviedb.org/3/{media}/{tmdb_id}?api_key={key}");
    let json: Value = serde_json::from_str(&fetch(&url)?).ok()?;
    let title = json
        .get("title")
        .or_else(|| json.get("name"))?
        .as_str()?
        .to_string();
    let genres = json
        .get("genres")
        .and_then(|g| g.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|g| g.get("name").and_then(|n| n.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let overview = json.get("overview").and_then(|o| o.as_str()).unwrap_or("");
    Some(format!("{title}. {genres}. {overview}"))
}

/// Embeds any corpus items not yet cached, in one batch.
fn embed_corpus(corpus: &[(ContentItem, String)]) {
    let missing: Vec<(String, String)> = {
        let cache = EMB_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        corpus
            .iter()
            .filter(|(item, _)| !cache.contains_key(&item.id))
            .map(|(item, text)| (item.id.clone(), text.clone()))
            .collect()
    };
    if missing.is_empty() {
        return;
    }
    let texts: Vec<String> = missing.iter().map(|(_, t)| t.clone()).collect();
    if let Some(vecs) = crate::embed::embed(texts) {
        let mut cache = EMB_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        for ((id, _), vec) in missing.into_iter().zip(vecs) {
            cache.insert(id, vec);
        }
    }
}

/// The candidate pool: a broad, varied set of TMDB titles with rich text for
/// embedding, deduped and cached.
fn candidate_corpus(key: &str) -> Vec<(ContentItem, String)> {
    if let Some((at, corpus)) = CORPUS.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if at.elapsed() < CORPUS_TTL {
            return corpus.clone();
        }
    }
    let genres = genre_map(key);
    let mut out: Vec<(ContentItem, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (media, path) in CORPUS_SOURCES {
        let sep = if path.contains('?') { '&' } else { '?' };
        let url = format!("https://api.themoviedb.org/3/{path}{sep}api_key={key}");
        if let Some(json) = fetch(&url) {
            for (item, text) in parse_rich(&json, media, &genres) {
                if seen.insert(item.id.clone()) {
                    out.push((item, text));
                }
            }
        }
    }
    if !out.is_empty() {
        *CORPUS.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), out.clone()));
    }
    out
}

fn parse_rich(
    json: &str,
    default_media: &str,
    genres: &HashMap<i64, String>,
) -> Vec<(ContentItem, String)> {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(results) = value.get("results").and_then(|r| r.as_array()) else {
        return Vec::new();
    };
    results
        .iter()
        .filter_map(|m| {
            let media = m
                .get("media_type")
                .and_then(|t| t.as_str())
                .filter(|t| *t == "movie" || *t == "tv")
                .unwrap_or(default_media);
            if media != "movie" && media != "tv" {
                return None;
            }
            let title = m
                .get("title")
                .or_else(|| m.get("name"))?
                .as_str()?
                .to_string();
            let art = art_of(m)?;
            let overview = m.get("overview").and_then(|o| o.as_str()).unwrap_or("");
            let genre_names = m
                .get("genre_ids")
                .and_then(|g| g.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|g| g.as_i64())
                        .filter_map(|id| genres.get(&id).cloned())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let item = ContentItem {
                id: format!("tmdb:{media}:{}", m.get("id")?.as_i64()?),
                kind: if media == "tv" {
                    Kind::Series
                } else {
                    Kind::Movie
                },
                title: title.clone(),
                art: Some(art),
                action: Action::Play,
            };
            Some((item, format!("{title}. {genre_names}. {overview}")))
        })
        .collect()
}

/// TMDB genre id → name (movie + tv), fetched once and cached.
fn genre_map(key: &str) -> HashMap<i64, String> {
    if let Some(map) = GENRES.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return map.clone();
    }
    let mut map = HashMap::new();
    for kind in ["movie", "tv"] {
        let url = format!("https://api.themoviedb.org/3/genre/{kind}/list?api_key={key}");
        if let Some(json) = fetch(&url) {
            if let Ok(v) = serde_json::from_str::<Value>(&json) {
                if let Some(arr) = v.get("genres").and_then(|g| g.as_array()) {
                    for g in arr {
                        if let (Some(id), Some(name)) = (
                            g.get("id").and_then(|i| i.as_i64()),
                            g.get("name").and_then(|n| n.as_str()),
                        ) {
                            map.insert(id, name.to_string());
                        }
                    }
                }
            }
        }
    }
    if !map.is_empty() {
        *GENRES.lock().unwrap_or_else(|e| e.into_inner()) = Some(map.clone());
    }
    map
}

/// The "For You" row. Uses the on-box embedding recommender (PLAN.md §6) once
/// the model is warm to surface *new* titles by taste similarity; otherwise
/// falls back to the documented local scorer (frequency × 14-day recency decay
/// + time-of-day boost) over your own event log — see [`recommend::EventLog::recommended`].
pub fn for_you(recent: &[ContentItem]) -> Vec<Row> {
    if crate::embed::ready() {
        if let Some(key) = api_key() {
            let rows = for_you_embeddings(&key, recent);
            if !rows.is_empty() {
                return rows;
            }
        }
    }
    for_you_fallback()
}

/// Fallback recommender (no embeddings, no network): the documented local
/// scorer over the launch log. Frequency × recency decay (half-life 14 days)
/// with a time-of-day boost, excluding the item leading Continue.
fn for_you_fallback() -> Vec<Row> {
    let items = crate::recommend::LOG.recommended(25);
    if items.is_empty() {
        return Vec::new();
    }
    vec![Row {
        title: "Top picks for you".to_string(),
        items,
    }]
}

/// Google-TV "Because you watched X" rows: for your most recent distinct
/// watched titles, a row of TMDB recommendations for each. Both our own
/// `tmdb:` items and IMDb/Stremio (`strm:`) items resolve via [`seed`]; anything
/// that doesn't map to a TMDB title is skipped. At most `max` rows, each needing
/// a few recommendations to be worth showing.
pub fn because_you_watched(recent: &[ContentItem], max: usize) -> Vec<Row> {
    let Some(key) = api_key() else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in recent {
        if rows.len() >= max {
            break;
        }
        if item.title.trim().is_empty() || !seen.insert(item.title.clone()) {
            continue;
        }
        let Some((media, id)) = seed(&key, item) else {
            continue;
        };
        let Ok(tmdb_id) = id.parse::<i64>() else {
            continue;
        };
        let items: Vec<ContentItem> = similar(&media, tmdb_id)
            .into_iter()
            .filter(|i| i.id != item.id)
            .take(ROW_LIMIT)
            .collect();
        if items.len() >= 4 {
            rows.push(Row {
                title: format!("Because you watched {}", item.title),
                items,
            });
        }
    }
    rows
}

/// Maps a watched item to a TMDB `(media, id)` seed, if it's a resolvable video.
fn seed(key: &str, item: &ContentItem) -> Option<(String, String)> {
    let mut parts = item.id.splitn(3, ':');
    match (parts.next(), parts.next(), parts.next()) {
        // Already a TMDB id (from one of our catalog rows).
        (Some("tmdb"), Some(media @ ("movie" | "tv")), Some(id)) => {
            Some((media.to_string(), id.to_string()))
        }
        // A Stremio/IMDb id (tt…) — resolve it to TMDB via /find.
        (Some("strm"), Some(kind), Some(rest)) => {
            let imdb = rest.split(':').next().unwrap_or(rest);
            if !imdb.starts_with("tt") {
                return None;
            }
            let media = if kind == "series" { "tv" } else { "movie" };
            find_tmdb(key, imdb, media).map(|id| (media.to_string(), id))
        }
        _ => None,
    }
}

/// IMDb id → TMDB id via /find.
fn find_tmdb(key: &str, imdb: &str, media: &str) -> Option<String> {
    let url =
        format!("https://api.themoviedb.org/3/find/{imdb}?external_source=imdb_id&api_key={key}");
    let json: Value = serde_json::from_str(&fetch(&url)?).ok()?;
    let field = if media == "tv" {
        "tv_results"
    } else {
        "movie_results"
    };
    json.get(field)?
        .as_array()?
        .first()?
        .get("id")?
        .as_i64()
        .map(|id| id.to_string())
}

/// Landscape backdrop (preferred) or poster image URL, if the item has any
/// artwork. Google TV shows content as wide 16:9 cards, so the home/search/
/// similar rows want the backdrop; the poster is only a fallback for the rare
/// title that has no backdrop. (The details page fetches its own poster
/// separately via `media.rs`, so it still gets a real 2:3 poster.)
fn art_of(m: &Value) -> Option<String> {
    m.get("backdrop_path")
        .and_then(|p| p.as_str())
        .map(|p| format!("https://image.tmdb.org/t/p/w780{p}"))
        .or_else(|| {
            m.get("poster_path")
                .and_then(|p| p.as_str())
                .map(|p| format!("https://image.tmdb.org/t/p/w500{p}"))
        })
}

/// Catalog search (TMDB multi-search) → movies and shows with artwork.
pub fn search(query: &str) -> Vec<ContentItem> {
    multi_search(query).0
}

/// A person (actor/director) from multi-search, for deep search.
pub struct Person {
    pub id: i64,
    pub name: String,
    pub popularity: f64,
}

/// TMDB multi-search: title matches *and* the people the query named.
pub fn multi_search(query: &str) -> (Vec<ContentItem>, Vec<Person>) {
    let query = query.trim();
    if query.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let Some(key) = api_key() else {
        return (Vec::new(), Vec::new());
    };
    let url = format!(
        "https://api.themoviedb.org/3/search/multi?api_key={key}&include_adult=false&query={}",
        percent_encode(query)
    );
    fetch(&url)
        .map(|json| parse_search(&json))
        .unwrap_or_default()
}

fn parse_search(json: &str) -> (Vec<ContentItem>, Vec<Person>) {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return (Vec::new(), Vec::new());
    };
    let Some(results) = value.get("results").and_then(|r| r.as_array()) else {
        return (Vec::new(), Vec::new());
    };
    let items = results
        .iter()
        .filter_map(|m| {
            let media = m.get("media_type")?.as_str()?;
            if media != "movie" && media != "tv" {
                return None;
            }
            let title = m
                .get("title")
                .or_else(|| m.get("name"))?
                .as_str()?
                .to_string();
            let art = art_of(m)?; // skip results with no artwork
            Some(ContentItem {
                id: format!("tmdb:{media}:{}", m.get("id")?.as_i64()?),
                kind: if media == "tv" {
                    Kind::Series
                } else {
                    Kind::Movie
                },
                title,
                art: Some(art),
                action: Action::Play,
            })
        })
        .take(ROW_LIMIT * 2)
        .collect();
    let mut persons: Vec<Person> = results
        .iter()
        .filter_map(|m| {
            if m.get("media_type")?.as_str()? != "person" {
                return None;
            }
            Some(Person {
                id: m.get("id")?.as_i64()?,
                name: m.get("name")?.as_str()?.to_string(),
                popularity: m.get("popularity").and_then(|p| p.as_f64()).unwrap_or(0.0),
            })
        })
        .collect();
    persons.sort_by(|a, b| b.popularity.total_cmp(&a.popularity));
    (items, persons)
}

/// A person's best-known movies and shows, most popular first.
pub fn person_credits(person_id: i64) -> Vec<ContentItem> {
    let Some(key) = api_key() else {
        return Vec::new();
    };
    let url =
        format!("https://api.themoviedb.org/3/person/{person_id}/combined_credits?api_key={key}");
    let Some(json) = fetch(&url) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&json) else {
        return Vec::new();
    };
    let Some(cast) = value.get("cast").and_then(|c| c.as_array()) else {
        return Vec::new();
    };
    let mut credits: Vec<(f64, ContentItem)> = cast
        .iter()
        .filter_map(|m| {
            let media = m.get("media_type")?.as_str()?;
            if media != "movie" && media != "tv" {
                return None;
            }
            // Skip obscure titles and one-off appearances: low-vote entries,
            // talk/news/reality formats, and 1–2 episode TV guest spots
            // (everyone famous has been on The Late Show — that's not
            // their filmography).
            if m.get("vote_count").and_then(|v| v.as_i64()).unwrap_or(0) < 20 {
                return None;
            }
            let genre_ids: Vec<i64> = m
                .get("genre_ids")
                .and_then(|g| g.as_array())
                .map(|g| g.iter().filter_map(|v| v.as_i64()).collect())
                .unwrap_or_default();
            if genre_ids.iter().any(|g| [10767, 10763, 10764].contains(g)) {
                return None;
            }
            if media == "tv"
                && m.get("episode_count")
                    .and_then(|e| e.as_i64())
                    .unwrap_or(i64::MAX)
                    <= 2
            {
                return None;
            }
            let title = m
                .get("title")
                .or_else(|| m.get("name"))?
                .as_str()?
                .to_string();
            let art = art_of(m)?;
            let popularity = m.get("popularity").and_then(|p| p.as_f64()).unwrap_or(0.0);
            Some((
                popularity,
                ContentItem {
                    id: format!("tmdb:{media}:{}", m.get("id")?.as_i64()?),
                    kind: if media == "tv" {
                        Kind::Series
                    } else {
                        Kind::Movie
                    },
                    title,
                    art: Some(art),
                    action: Action::Play,
                },
            ))
        })
        .collect();
    credits.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut seen = std::collections::HashSet::new();
    credits
        .into_iter()
        .map(|(_, item)| item)
        .filter(|item| seen.insert(item.id.clone())) // same show, several roles
        .take(ROW_LIMIT)
        .collect()
}

/// TMDB keyword ids matching free text ("time travel" → 4379 Time Travel).
pub fn keyword_ids(text: &str) -> Vec<(i64, String)> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let Some(key) = api_key() else {
        return Vec::new();
    };
    let url = format!(
        "https://api.themoviedb.org/3/search/keyword?api_key={key}&query={}",
        percent_encode(text)
    );
    let Some(json) = fetch(&url) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&json) else {
        return Vec::new();
    };
    value
        .get("results")
        .and_then(|r| r.as_array())
        .map(|results| {
            results
                .iter()
                .filter_map(|k| {
                    Some((k.get("id")?.as_i64()?, k.get("name")?.as_str()?.to_string()))
                })
                .take(5)
                .collect()
        })
        .unwrap_or_default()
}

/// Title-only embeddings for row personalization (no per-item TMDB lookups —
/// titles alone are enough to bias ordering). Keyed by lowercase title.
static TITLE_EMB: LazyLock<Mutex<HashMap<String, Vec<f32>>>> = LazyLock::new(Mutex::default);

fn xorshift01(state: &mut u64) -> f32 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    ((*state >> 40) as f32) / ((1u64 << 24) as f32)
}

/// Fresh-feeling home: every section is reordered by similarity to what the
/// user actually consumes (when the on-box embedder is warm) plus a dash of
/// randomness, so each visit surfaces new finds mixed with familiar ones.
/// Continue rows (recency) and YouTube rows (upload order) keep their order.
pub fn personalize(rows: &mut [Row], recent: &[ContentItem]) {
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e3779b9)
        | 1;

    let profile = if crate::embed::ready() && !recent.is_empty() {
        api_key().and_then(|key| profile_vector(&key, recent))
    } else {
        None
    };

    let skip = |title: &str| title.starts_with("Continue") || title.starts_with("YouTube");

    // One batched model call for every title we haven't embedded yet.
    if profile.is_some() {
        let missing: Vec<String> = {
            let cache = TITLE_EMB.lock().unwrap_or_else(|e| e.into_inner());
            rows.iter()
                .filter(|r| !skip(&r.title))
                .flat_map(|r| r.items.iter())
                .map(|i| i.title.to_lowercase())
                .filter(|t| !t.is_empty() && !cache.contains_key(t))
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect()
        };
        if !missing.is_empty() {
            if let Some(vecs) = crate::embed::embed(missing.clone()) {
                let mut cache = TITLE_EMB.lock().unwrap_or_else(|e| e.into_inner());
                for (title, vec) in missing.into_iter().zip(vecs) {
                    cache.insert(title, vec);
                }
            }
        }
    }

    for row in rows.iter_mut() {
        if skip(&row.title) || row.items.len() < 3 {
            continue;
        }
        let cache = TITLE_EMB.lock().unwrap_or_else(|e| e.into_inner());
        let mut scored: Vec<(f32, ContentItem)> = row
            .items
            .drain(..)
            .map(|item| {
                let taste = match (&profile, cache.get(&item.title.to_lowercase())) {
                    (Some(p), Some(v)) => crate::embed::cosine(p, v),
                    _ => 0.0,
                };
                // Taste leads, the jitter keeps it from fossilizing.
                (taste + xorshift01(&mut seed) * 0.25, item)
            })
            .collect();
        drop(cache);
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        row.items = scored.into_iter().map(|(_, item)| item).collect();
    }
}

/// Cache for "More like this" lookups, keyed by request URL.
static SIMILAR_CACHE: LazyLock<Mutex<HashMap<String, (Instant, Vec<ContentItem>)>>> =
    LazyLock::new(Mutex::default);

/// "More like this" for a title: TMDB recommendations (better curated),
/// falling back to /similar when a title has none. `media` is "movie"/"tv".
pub fn similar(media: &str, tmdb_id: i64) -> Vec<ContentItem> {
    let Some(key) = api_key() else {
        return Vec::new();
    };
    for path in ["recommendations", "similar"] {
        let url = format!("https://api.themoviedb.org/3/{media}/{tmdb_id}/{path}?api_key={key}");
        if let Some((at, items)) = SIMILAR_CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&url)
        {
            if at.elapsed() < CACHE_TTL {
                if items.is_empty() {
                    continue;
                }
                return items.clone();
            }
        }
        let items = fetch(&url)
            .map(|json| parse_trending(&json, media))
            .unwrap_or_default();
        SIMILAR_CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(url, (Instant::now(), items.clone()));
        if !items.is_empty() {
            return items;
        }
    }
    Vec::new()
}

/// TMDB (media, id) for an IMDb id — addon items (Cinemeta) are IMDb-keyed.
pub fn find_by_imdb(imdb_id: &str) -> Option<(String, i64)> {
    let key = api_key()?;
    let url = format!(
        "https://api.themoviedb.org/3/find/{}?api_key={key}&external_source=imdb_id",
        percent_encode(imdb_id)
    );
    let value: Value = serde_json::from_str(&fetch(&url)?).ok()?;
    for (list, media) in [("movie_results", "movie"), ("tv_results", "tv")] {
        if let Some(id) = value
            .get(list)
            .and_then(|r| r.as_array())
            .and_then(|r| r.first())
            .and_then(|m| m.get("id"))
            .and_then(|i| i.as_i64())
        {
            return Some((media.to_string(), id));
        }
    }
    None
}

/// Discover: popular titles matching genres / original language / keywords.
/// `media` is "movie" or "tv"; empty filters are omitted. Keywords are OR'd.
/// `min_votes` is the junk floor — callers pick it: high for broad genre
/// browses (canon), low for keyword digs (niche is the point).
pub fn discover(
    media: &str,
    genres: &[i64],
    without_genres: &[i64],
    language: Option<&str>,
    keywords: &[i64],
    min_votes: i64,
) -> Vec<ContentItem> {
    let Some(key) = api_key() else {
        return Vec::new();
    };
    let join = |ids: &[i64], sep: &str| {
        ids.iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(sep)
    };
    let mut url = format!(
        "https://api.themoviedb.org/3/discover/{media}?api_key={key}&include_adult=false&sort_by=popularity.desc&vote_count.gte={min_votes}"
    );
    if !genres.is_empty() {
        url += &format!("&with_genres={}", join(genres, ","));
    }
    if !without_genres.is_empty() {
        url += &format!("&without_genres={}", join(without_genres, ","));
    }
    if let Some(lang) = language {
        url += &format!("&with_original_language={lang}");
    }
    if !keywords.is_empty() {
        url += &format!("&with_keywords={}", join(keywords, "|"));
    }
    fetch(&url)
        .map(|json| parse_trending(&json, media))
        .unwrap_or_default()
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
            // Require artwork (poster, else backdrop) so the home screen never
            // shows blank cards.
            let art = art_of(m)?;
            Some(ContentItem {
                id: format!("tmdb:{media}:{}", m.get("id")?.as_i64()?),
                kind,
                title,
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
    fn parses_trending_movies_and_skips_artless() {
        let json = r#"{"results": [
            {"id": 603, "title": "The Matrix", "poster_path": "/abc.jpg", "backdrop_path": "/wide.jpg"},
            {"id": 605, "title": "Poster Only", "poster_path": "/ps.jpg", "backdrop_path": null},
            {"id": 604, "title": "No Art Movie", "poster_path": null}
        ]}"#;
        let items = parse_trending(json, "movie");
        // The art-less movie is dropped. Cards prefer the landscape backdrop
        // (Google-TV style); the poster is only a fallback.
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "tmdb:movie:603");
        assert_eq!(items[0].kind, Kind::Movie);
        assert_eq!(
            items[0].art.as_deref(),
            Some("https://image.tmdb.org/t/p/w780/wide.jpg")
        );
        assert_eq!(
            items[1].art.as_deref(),
            Some("https://image.tmdb.org/t/p/w500/ps.jpg")
        );
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
