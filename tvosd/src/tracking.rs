//! Watch tracking — pushes finished movies/episodes to Trakt, AniList and
//! MyAnimeList, whichever are connected.
//!
//! The player writes a `<position-file>.done` marker (containing the item id)
//! when a title ends naturally or is quit in the last 10%. A background
//! worker sweeps the positions directory, resolves each marker to the
//! services' ids (Trakt by IMDb id; AniList/MAL by title search, series
//! only) and records the watch. Everything is best-effort: a service that
//! isn't configured or doesn't match is silently skipped.
//!
//! The sync also runs the other way: [`watched_history`] pulls your recent
//! Trakt history so the "For You" row is seeded by what you've watched
//! anywhere, not just what you've played on this box.

use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::model::{Action, ContentItem, Kind};
use crate::settings::{self, config_dir};
use crate::{log_error, log_info, recommend};

const SWEEP_SECS: u64 = 15;
const MAC_SWEEP_SECS: u64 = 60;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a pull of Trakt history is reused before refetching.
const HISTORY_TTL: Duration = Duration::from_secs(600);

/// Trakt device-code flow state (set by /api/trakt/connect, polled off-thread).
static TRAKT_PENDING: Mutex<Option<String>> = Mutex::new(None); // user_code shown in UI
/// MAL PKCE verifier for the in-flight login (single user, single flow).
static MAL_VERIFIER: Mutex<Option<String>> = Mutex::new(None);
/// Cached Trakt + AniList watch history (seeds for recommendations).
static HISTORY_CACHE: LazyLock<Mutex<Option<(Instant, Vec<ContentItem>)>>> =
    LazyLock::new(|| Mutex::new(None));

pub fn start_worker() {
    let sweep_secs = if matches!(std::env::var("TVOS_MAC_APP").as_deref(), Ok("1")) {
        MAC_SWEEP_SECS
    } else {
        SWEEP_SECS
    };
    std::thread::spawn(move || loop {
        sweep_markers();
        std::thread::sleep(Duration::from_secs(sweep_secs));
    });
}

/// Which services are connected (for the Settings panel).
pub fn status() -> Value {
    let s = settings::STORE.get();
    json!({
        "trakt": !s.trakt_token.is_empty(),
        "trakt_pending": TRAKT_PENDING.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        "anilist": !s.anilist_token.is_empty(),
        "mal": !s.mal_token.is_empty(),
    })
}

fn client() -> Option<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(concat!("tvos/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()
}

// ---- Completion markers → scrobbles ----------------------------------------

fn sweep_markers() {
    sync_local_play_markers();
    let dir = config_dir().join("positions");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("played") {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("done") {
            continue;
        }
        let item_id = std::fs::read_to_string(&path).unwrap_or_default();
        let item_id = item_id.trim();
        if item_id.is_empty() {
            // Nothing to sync — drop the empty marker so it doesn't pile up.
            let _ = std::fs::remove_file(&path);
            continue;
        }
        log_info!("watched: {item_id} — syncing trackers");
        // Only delete the marker once the sync actually succeeds; a transient
        // failure (offline, service 5xx) would otherwise lose the watch
        // forever. On failure, leave it for the next sweep to retry — unless
        // it's unscrobblable (a game/YouTube id), which we clear immediately.
        let finished = finished_item(item_id);
        match scrobble(item_id) {
            Scrobble::Synced | Scrobble::NotApplicable => {
                if let Some(item) = finished {
                    recommend::LOG.record(item);
                }
                let _ = std::fs::remove_file(&path);
            }
            Scrobble::Failed => {
                log_error!("watch sync failed for {item_id} — keeping marker to retry");
            }
        }
    }
}

/// Fold mpv's local "played" markers into Continue without doing any network
/// scrobbling. Called by `/api/library` too, so returning from the player can
/// refresh the home row immediately instead of waiting for the tracker worker.
pub fn sync_local_play_markers() {
    let dir = config_dir().join("positions");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("played") {
            sweep_played_marker(&path);
        }
    }
}

/// Remember the shell's rich item metadata next to the resume position. mpv
/// later writes a `.played` marker using this same content id, so a normal quit
/// can still update Continue without guessing titles/art from the tracker id.
pub fn remember_local_play(content_id: &str, item: &ContentItem) {
    let path = local_item_file(content_id);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(json) = serde_json::to_string(item) else {
        return;
    };
    if let Err(e) = std::fs::write(&path, json) {
        log_error!("local play metadata write failed: {e}");
    }
}

fn sweep_played_marker(path: &std::path::Path) {
    let content_id = std::fs::read_to_string(path).unwrap_or_default();
    let content_id = content_id.trim();
    if content_id.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Some(item) = local_item(content_id) {
        recommend::LOG.record(item);
    } else {
        log_error!("local play marker without metadata: {content_id}");
    }
    let _ = std::fs::remove_file(path);
}

fn local_item(content_id: &str) -> Option<ContentItem> {
    std::fs::read_to_string(local_item_file(content_id))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

fn local_item_file(content_id: &str) -> std::path::PathBuf {
    crate::resume::position_file(content_id).with_extension("item.json")
}

/// Outcome of a scrobble attempt, deciding whether the marker can be removed.
enum Scrobble {
    /// At least one connected service recorded the watch.
    Synced,
    /// Nothing to do (unscrobblable id, or no service connected/matched).
    NotApplicable,
    /// A connected service was tried and failed transiently — retry later.
    Failed,
}

/// What a finished item means to the trackers.
struct Watched {
    imdb: String,
    /// (season, episode) for series; None = movie.
    episode: Option<(i64, i64)>,
    title: Option<String>,
}

/// "strm:movie:tt123" / "strm:series:tt123:2:5" / "tmdb:movie:603" → the pieces
/// trackers need. The title comes from the recommender log (every play records
/// it). Games and YouTube (`steam:`, `yt:`, …) don't scrobble anywhere.
fn parse_watched(item_id: &str) -> Option<Watched> {
    let title = title_for(item_id);
    match item_id.split(':').next().unwrap_or_default() {
        // IMDb-keyed ids carry the episode directly (…:season:episode).
        "strm" => {
            let mut parts = item_id.split(':');
            parts.next(); // "strm"
            let kind = parts.next()?;
            let imdb = parts.next()?.to_string();
            if !imdb.starts_with("tt") {
                return None;
            }
            let episode = if kind == "series" {
                Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
            } else {
                None
            };
            Some(Watched {
                imdb,
                episode,
                title,
            })
        }
        // TMDB catalog ids resolve to an IMDb id via TMDB. They name a whole
        // title (no episode), so only movies scrobble this way; TMDB series are
        // watched per-episode through their resolved `strm:…:s:e` id above.
        "tmdb" => {
            let (kind, imdb) = crate::sources::resolve_video(item_id).ok()?;
            if kind == "series" || !imdb.starts_with("tt") {
                return None;
            }
            Some(Watched {
                imdb,
                episode: None,
                title,
            })
        }
        _ => None,
    }
}

/// The title recorded for an id in the recommender log (used for AniList/MAL
/// anime matching). Matches the id itself or any episode id under it.
fn title_for(item_id: &str) -> Option<String> {
    recommend::LOG
        .recent_items(64)
        .into_iter()
        .find(|i| i.id == item_id || item_id.starts_with(&format!("{}:", i.id)))
        .map(|i| i.title)
}

/// A completed player marker is also a local watch event. That keeps Continue
/// and the recommender ordered by what actually finished, not only by what was
/// clicked. Episodes are recorded on their show id so the home row opens the
/// show detail page and resumes the last episode source.
fn finished_item(item_id: &str) -> Option<ContentItem> {
    let watched = parse_watched(item_id)?;
    let title = watched.title?;
    let (id, kind) = if watched.episode.is_some() {
        (format!("strm:series:{}", watched.imdb), Kind::Series)
    } else if item_id.starts_with("tmdb:movie:") {
        (item_id.to_string(), Kind::Movie)
    } else {
        (format!("strm:movie:{}", watched.imdb), Kind::Movie)
    };
    Some(ContentItem {
        id,
        kind,
        title,
        art: None,
        action: Action::Play,
        note: None,
    })
}

fn scrobble(item_id: &str) -> Scrobble {
    let Some(watched) = parse_watched(item_id) else {
        return Scrobble::NotApplicable; // game/YouTube/unresolvable — nothing to sync
    };
    let s = settings::STORE.get();
    let mut tried = false; // a connected service was attempted
    let mut failed = false; // …and at least one attempt failed transiently
    let mut synced = false; // …and at least one recorded the watch

    if !s.trakt_token.is_empty() && !s.trakt_client_id.is_empty() {
        tried = true;
        match trakt_history(&s.trakt_client_id, &s.trakt_token, &watched) {
            Ok(()) => synced = true,
            Err(e) => {
                log_error!("trakt sync failed: {e}");
                failed = true;
            }
        }
    }
    // AniList/MAL track anime series by episode number.
    if let (Some((_, ep)), Some(title)) = (watched.episode, watched.title.as_deref()) {
        if !s.anilist_token.is_empty() {
            tried = true;
            match anilist_progress(&s.anilist_token, title, ep) {
                Ok(()) => synced = true,
                Err(e) => {
                    log_error!("anilist sync failed: {e}");
                    failed = true;
                }
            }
        }
        if !s.mal_token.is_empty() {
            tried = true;
            match mal_progress(&s.mal_token, title, ep) {
                Ok(()) => synced = true,
                Err(e) => {
                    log_error!("mal sync failed: {e}");
                    failed = true;
                }
            }
        }
    }

    match (tried, synced, failed) {
        // No connected service to try, or everything reported "no match" (Ok
        // without a real sync): nothing more we can do — clear the marker.
        (false, _, _) => Scrobble::NotApplicable,
        // Something failed and nothing succeeded → keep the marker to retry.
        (true, false, true) => Scrobble::Failed,
        // At least one service recorded it (or all matched cleanly).
        _ => Scrobble::Synced,
    }
}

// ---- Trakt ------------------------------------------------------------------

fn trakt_history(client_id: &str, token: &str, w: &Watched) -> Result<(), String> {
    let body = match w.episode {
        None => json!({ "movies": [{ "ids": { "imdb": w.imdb } }] }),
        Some((season, episode)) => json!({
            "shows": [{
                "ids": { "imdb": w.imdb },
                "seasons": [{ "number": season, "episodes": [{ "number": episode }] }]
            }]
        }),
    };
    let res = client()
        .ok_or("no http client")?
        .post("https://api.trakt.tv/sync/history")
        .header("trakt-api-version", "2")
        .header("trakt-api-key", client_id)
        .bearer_auth(token)
        .json(&body)
        .send()
        .map_err(|e| e.to_string())?;
    res.error_for_status()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Recent Trakt and AniList history as recommendation seeds — so the "For You"
/// row is personal to what you've *actually watched anywhere*, not just what
/// you played on this box. Episodes seed on their show (recommendations are
/// per-title). Trakt hands us TMDB ids, so the recommender resolves these with
/// no extra lookups. Empty (and free) when Trakt isn't connected. Cached.
pub fn watched_history(limit: usize) -> Vec<ContentItem> {
    let s = settings::STORE.get();
    if let Some((at, items)) = HISTORY_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        if at.elapsed() < HISTORY_TTL {
            return items.clone();
        }
    }
    let mut items = Vec::new();
    if !s.trakt_token.is_empty() && !s.trakt_client_id.is_empty() {
        match fetch_trakt_history(&s.trakt_client_id, &s.trakt_token, limit) {
            Ok(found) => items.extend(found),
            Err(e) => log_error!("trakt history fetch failed: {e}"),
        }
    }
    if !s.anilist_token.is_empty() {
        match fetch_anilist_history(&s.anilist_token, limit) {
            Ok(found) => items.extend(found),
            Err(e) => log_error!("anilist history fetch failed: {e}"),
        }
    }
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.id.clone()));
    items.truncate(limit);
    if !items.is_empty() {
        *HISTORY_CACHE.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((Instant::now(), items.clone()));
    }
    items
}

fn fetch_anilist_history(token: &str, limit: usize) -> Result<Vec<ContentItem>, String> {
    let http = client().ok_or("no http client")?;
    let viewer: Value = http
        .post("https://graphql.anilist.co")
        .bearer_auth(token)
        .json(&json!({"query": "query { Viewer { id } }"}))
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    let user_id = viewer
        .pointer("/data/Viewer/id")
        .and_then(Value::as_i64)
        .ok_or("AniList viewer id missing")?;
    let query = r#"
      query ($userId: Int) {
        MediaListCollection(userId: $userId, type: ANIME,
          status_in: [CURRENT, COMPLETED, REPEATING]) {
          lists { entries { progress score repeat updatedAt status
            media { id title { english romaji } coverImage { extraLarge } }
          } }
        }
      }"#;
    let value: Value = http
        .post("https://graphql.anilist.co")
        .bearer_auth(token)
        .json(&json!({"query": query, "variables": {"userId": user_id}}))
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    let mut entries: Vec<&Value> = value
        .pointer("/data/MediaListCollection/lists")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|l| {
            l.get("entries")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .collect();
    entries.sort_by_key(|e| {
        std::cmp::Reverse(e.get("updatedAt").and_then(Value::as_i64).unwrap_or(0))
    });
    Ok(entries
        .into_iter()
        .take(limit)
        .filter_map(|entry| {
            let media = entry.get("media")?;
            let id = media.get("id")?.as_i64()?;
            let title = media
                .pointer("/title/english")
                .or_else(|| media.pointer("/title/romaji"))?
                .as_str()?;
            let item = ContentItem {
                id: format!("anilist:{id}"),
                kind: Kind::Series,
                title: title.to_string(),
                art: media
                    .pointer("/coverImage/extraLarge")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                action: Action::None,
                note: Some(format!(
                    "AniList · {} episodes",
                    entry.get("progress").and_then(Value::as_i64).unwrap_or(0)
                )),
            };
            let status = entry
                .get("status")
                .and_then(Value::as_str)
                .map(|s| s.to_ascii_lowercase());
            crate::profile::STORE.import_history(
                "anilist",
                &id.to_string(),
                &item,
                entry.get("progress").and_then(Value::as_f64),
                entry.get("score").and_then(Value::as_f64),
                status.as_deref(),
                entry.get("updatedAt").and_then(Value::as_i64),
                entry,
            );
            Some(item)
        })
        .collect())
}

fn fetch_trakt_history(
    client_id: &str,
    token: &str,
    limit: usize,
) -> Result<Vec<ContentItem>, String> {
    let v: Value = client()
        .ok_or("no http client")?
        .get(format!("https://api.trakt.tv/sync/history?limit={limit}"))
        .header("trakt-api-version", "2")
        .header("trakt-api-key", client_id)
        .bearer_auth(token)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    let entries = v.as_array().ok_or("history is not an array")?;
    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for entry in entries {
        if let Some(item) = history_seed(entry) {
            if seen.insert(item.id.clone()) {
                crate::profile::STORE.import_history(
                    "trakt",
                    &item.id,
                    &item,
                    None,
                    entry.get("rating").and_then(Value::as_f64),
                    Some("completed"),
                    None,
                    entry,
                );
                items.push(item);
            }
        }
    }
    Ok(items)
}

/// One Trakt history entry → a recommendation seed, or None for anything
/// without a resolvable id.
fn history_seed(entry: &Value) -> Option<ContentItem> {
    let (obj, media, kind) = match entry.get("type")?.as_str()? {
        "movie" => (entry.get("movie")?, "movie", Kind::Movie),
        "episode" => (entry.get("show")?, "tv", Kind::Series),
        _ => return None,
    };
    let ids = obj.get("ids")?;
    let title = obj
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    // Prefer the TMDB id (no resolution needed); fall back to IMDb.
    let id = if let Some(tmdb) = ids.get("tmdb").and_then(|x| x.as_i64()) {
        format!("tmdb:{media}:{tmdb}")
    } else if let Some(imdb) = ids.get("imdb").and_then(|x| x.as_str()) {
        let k = if media == "tv" { "series" } else { "movie" };
        format!("strm:{k}:{imdb}")
    } else {
        return None;
    };
    Some(ContentItem {
        id,
        kind,
        title,
        art: None,
        action: Action::Play,
        note: None,
    })
}

/// Starts the Trakt device flow: returns (user_code, verification_url) for
/// the Settings panel and polls for the token in the background.
pub fn trakt_connect() -> Result<Value, String> {
    let s = settings::STORE.get();
    if s.trakt_client_id.is_empty() || s.trakt_client_secret.is_empty() {
        return Err("save your Trakt client id & secret first".to_string());
    }
    let http = client().ok_or("no http client")?;
    let v: Value = http
        .post("https://api.trakt.tv/oauth/device/code")
        .json(&json!({ "client_id": s.trakt_client_id }))
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("device code request failed: {e}"))?
        .json()
        .map_err(|e| e.to_string())?;
    let device_code = v["device_code"].as_str().unwrap_or_default().to_string();
    let user_code = v["user_code"].as_str().unwrap_or_default().to_string();
    let url = v["verification_url"]
        .as_str()
        .unwrap_or("https://trakt.tv/activate")
        .to_string();
    let interval = v["interval"].as_u64().unwrap_or(5).max(3);
    let expires = v["expires_in"].as_u64().unwrap_or(600);
    *TRAKT_PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(user_code.clone());

    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(expires);
        while std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_secs(interval));
            let s = settings::STORE.get();
            let Some(http) = client() else { break };
            let Ok(res) = http
                .post("https://api.trakt.tv/oauth/device/token")
                .json(&json!({
                    "code": device_code,
                    "client_id": s.trakt_client_id,
                    "client_secret": s.trakt_client_secret,
                }))
                .send()
            else {
                continue;
            };
            if res.status().is_success() {
                if let Ok(v) = res.json::<Value>() {
                    if let Some(token) = v["access_token"].as_str() {
                        let mut updated = settings::STORE.get();
                        updated.trakt_token = token.to_string();
                        let _ = settings::STORE.set(updated);
                        log_info!("trakt connected");
                    }
                }
                break;
            }
            if res.status().as_u16() != 400 {
                break; // 400 = still pending; anything else is fatal
            }
        }
        *TRAKT_PENDING.lock().unwrap_or_else(|e| e.into_inner()) = None;
    });

    Ok(json!({ "user_code": user_code, "url": url }))
}

// ---- AniList -----------------------------------------------------------------

fn anilist_progress(token: &str, title: &str, episode: i64) -> Result<(), String> {
    let http = client().ok_or("no http client")?;
    // Find the anime by name; not everything is anime — no match is fine.
    let search = json!({
        "query": "query($q:String){Media(search:$q,type:ANIME){id episodes}}",
        "variables": { "q": title },
    });
    let v: Value = http
        .post("https://graphql.anilist.co")
        .bearer_auth(token)
        .json(&search)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    let Some(media_id) = v["data"]["Media"]["id"].as_i64() else {
        return Ok(()); // not on AniList — not anime, skip quietly
    };
    let total = v["data"]["Media"]["episodes"].as_i64().unwrap_or(0);
    let status = if total > 0 && episode >= total {
        "COMPLETED"
    } else {
        "CURRENT"
    };
    let save = json!({
        "query": "mutation($id:Int,$p:Int,$st:MediaListStatus){SaveMediaListEntry(mediaId:$id,progress:$p,status:$st){id}}",
        "variables": { "id": media_id, "p": episode, "st": status },
    });
    http.post("https://graphql.anilist.co")
        .bearer_auth(token)
        .json(&save)
        .send()
        .and_then(|r| r.error_for_status())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ---- MyAnimeList --------------------------------------------------------------

fn mal_progress(token: &str, title: &str, episode: i64) -> Result<(), String> {
    let http = client().ok_or("no http client")?;
    let v: Value = http
        .get(format!(
            "https://api.myanimelist.net/v2/anime?q={}&limit=1",
            crate::util::percent_encode(title)
        ))
        .bearer_auth(token)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    let Some(anime_id) = v["data"][0]["node"]["id"].as_i64() else {
        return Ok(()); // not anime as far as MAL is concerned
    };
    http.put(format!(
        "https://api.myanimelist.net/v2/anime/{anime_id}/my_list_status"
    ))
    .bearer_auth(token)
    .form(&[
        ("num_watched_episodes", episode.to_string()),
        ("status", "watching".to_string()),
    ])
    .send()
    .and_then(|r| r.error_for_status())
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// The MAL PKCE login URL (plain method: challenge == verifier). The user's
/// MAL API client must list http://localhost:8484/api/mal/callback as its
/// redirect URI.
pub fn mal_login_url() -> Result<String, String> {
    let s = settings::STORE.get();
    if s.mal_client_id.is_empty() {
        return Err("save your MAL client id first".to_string());
    }
    let verifier: String = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("tvos{seed:x}tvos{seed:x}tvos{seed:x}") // 43+ chars, unreserved
    };
    *MAL_VERIFIER.lock().unwrap_or_else(|e| e.into_inner()) = Some(verifier.clone());
    Ok(format!(
        "https://myanimelist.net/v1/oauth2/authorize?response_type=code&client_id={}&code_challenge={verifier}&code_challenge_method=plain",
        s.mal_client_id
    ))
}

/// Exchanges the callback code for a token and saves it.
pub fn mal_callback(code: &str) -> Result<(), String> {
    let verifier = MAL_VERIFIER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .ok_or("no login in progress — start again from Settings")?;
    let s = settings::STORE.get();
    let http = client().ok_or("no http client")?;
    let v: Value = http
        .post("https://myanimelist.net/v1/oauth2/token")
        .form(&[
            ("client_id", s.mal_client_id.as_str()),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("token exchange failed: {e}"))?
        .json()
        .map_err(|e| e.to_string())?;
    let token = v["access_token"]
        .as_str()
        .ok_or("no access token in response")?;
    let mut updated = settings::STORE.get();
    updated.mal_token = token.to_string();
    settings::STORE.set(updated)?;
    log_info!("myanimelist connected");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_watched_ids() {
        let m = parse_watched("strm:movie:tt0133093").unwrap();
        assert_eq!(m.imdb, "tt0133093");
        assert!(m.episode.is_none());

        let e = parse_watched("strm:series:tt0903747:2:5").unwrap();
        assert_eq!(e.imdb, "tt0903747");
        assert_eq!(e.episode, Some((2, 5)));

        assert!(parse_watched("steam:620").is_none());
        assert!(parse_watched("yt:abc").is_none());
        assert!(parse_watched("strm:movie:notimdb").is_none());
    }

    #[test]
    fn history_seeds_prefer_tmdb_then_imdb() {
        let movie = json!({
            "type": "movie",
            "movie": { "title": "The Matrix", "ids": { "tmdb": 603, "imdb": "tt0133093" } }
        });
        let seed = history_seed(&movie).unwrap();
        assert_eq!(seed.id, "tmdb:movie:603"); // TMDB id wins
        assert_eq!(seed.kind, Kind::Movie);

        // Episodes seed on their *show*, TMDB missing → IMDb fallback.
        let episode = json!({
            "type": "episode",
            "show": { "title": "Breaking Bad", "ids": { "imdb": "tt0903747" } },
            "episode": { "season": 2, "number": 5 }
        });
        let seed = history_seed(&episode).unwrap();
        assert_eq!(seed.id, "strm:series:tt0903747");
        assert_eq!(seed.kind, Kind::Series);

        assert!(history_seed(&json!({ "type": "movie", "movie": { "ids": {} } })).is_none());
        assert!(history_seed(&json!({ "type": "person" })).is_none());
    }
}
