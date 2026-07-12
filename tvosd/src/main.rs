//! tvosd — the TV OS daemon.
//!
//! Serves the shell UI (static files) and a small JSON API on 127.0.0.1:8484:
//!
//!   GET  /api/library   → home-screen rows of ContentItems
//!   GET  /api/sources   → which sources were detected on this system
//!   POST /api/launch    → {"id": "steam:620"} plays/runs an item
//!   POST /api/install   → {"id": "epic:Sugar"} starts a download job
//!   GET  /api/installs  → status of all download jobs
//!   GET  /api/settings  → user settings (enhance mode)
//!   PUT  /api/settings  → update + persist settings
//!   GET  /api/addons    → installed Stremio-compatible addons
//!   POST /api/addons    → {"url": "…/manifest.json"} install an addon
//!   POST /api/addons/remove → {"url": …} uninstall
//!   GET  /api/source-manifests → installed CloudStream-style source manifests
//!   POST /api/source-manifests → {"text": url-or-json} add (URL or pasted JSON)
//!   POST /api/source-manifests/remove → {"id": …} uninstall
//!   POST /api/source-manifests/toggle → {"id","name","enabled"} enable a source
//!   POST /api/source-manifests/test → {"id"?} probe + auto-disable unreachable

mod addons;
mod embed;
mod fuzzy;
mod install;
mod launcher;
mod logging;
mod media;
mod model;
mod recommend;
mod resume;
mod search;
mod settings;
mod shaders;
mod sources;
mod tracking;
mod upscale;
mod util;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tower_http::services::{ServeDir, ServeFile};

/// Default listen address; `TVOS_LISTEN` overrides it (dev/test instances).
const LISTEN_ADDR: &str = "127.0.0.1:8484";

/// Upper bound on any request body — the API only carries small JSON.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Upper bound on user-supplied URLs (addon manifests, play/open URLs).
const MAX_URL_LEN: usize = 2048;

fn listen_addr() -> String {
    std::env::var("TVOS_LISTEN").unwrap_or_else(|_| LISTEN_ADDR.to_string())
}

/// Optional shared secret. When set, mutating endpoints require it (Bearer
/// header or `?token=`). Required before we'll bind a non-loopback address.
fn auth_token() -> Option<String> {
    std::env::var("TVOS_AUTH_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
}

/// Whether every socket address `addr` resolves to is loopback. A non-loopback
/// bind exposes the API to the network, so it demands an auth token.
fn addr_is_loopback(addr: &str) -> bool {
    use std::net::ToSocketAddrs;
    match addr.to_socket_addrs() {
        Ok(it) => {
            let addrs: Vec<_> = it.collect();
            !addrs.is_empty() && addrs.iter().all(|s| s.ip().is_loopback())
        }
        // Unresolvable here — let the actual bind below fail with a clear error.
        Err(_) => false,
    }
}

/// Middleware: reject mutating requests (POST/PUT) that don't carry the token,
/// as `Authorization: Bearer <token>` or `?token=<token>`. Reads are unguarded.
async fn require_auth(
    axum::extract::State(token): axum::extract::State<Arc<String>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::Method;
    use axum::response::IntoResponse;
    let mutating = matches!(req.method(), &Method::POST | &Method::PUT | &Method::DELETE);
    if !mutating {
        return next.run(req).await;
    }
    let header_ok = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| t == token.as_str());
    let query_ok = req.uri().query().is_some_and(|q| {
        q.split('&')
            .filter_map(|kv| kv.strip_prefix("token="))
            .any(|t| t == token.as_str())
    });
    if header_ok || query_ok {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "missing or invalid auth token").into_response()
    }
}

struct App {
    sources: sources::Registry,
    installs: install::InstallManager,
    /// Recent snapshot of the source rows, so search-as-you-type doesn't
    /// re-shell out to store CLIs on every keystroke.
    library_cache: Mutex<Option<(Instant, Vec<model::Row>)>>,
}

const LIBRARY_CACHE_TTL: Duration = Duration::from_secs(120);

impl App {
    /// Fresh source rows; also refreshes the search cache.
    fn refresh_library(&self) -> Vec<model::Row> {
        let rows = self.sources.library();
        *self.library_cache.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((Instant::now(), rows.clone()));
        rows
    }

    /// Source rows for search: the cached snapshot if it's recent, else fresh.
    fn cached_library(&self) -> Vec<model::Row> {
        {
            let cache = self.library_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((at, rows)) = &*cache {
                if at.elapsed() < LIBRARY_CACHE_TTL {
                    return rows.clone();
                }
            }
        }
        self.refresh_library()
    }
}

type Shared = Arc<App>;

#[tokio::main]
async fn main() {
    logging::init();
    let app = Arc::new(App {
        sources: sources::Registry::detect(),
        installs: install::InstallManager::default(),
        library_cache: Mutex::new(None),
    });
    // Warm the library cache so the first search/home-load doesn't wait on
    // store CLIs and catalog fetches. The native Mac UI requests /api/library
    // immediately after startup, so warming there would duplicate the same
    // store/network scan and waste CPU and battery.
    if !matches!(std::env::var("TVOS_MAC_APP").as_deref(), Ok("1")) {
        let warm = app.clone();
        std::thread::spawn(move || {
            warm.refresh_library();
        });
    }
    // Sweep player completion markers → Trakt/AniList/MAL scrobbles.
    tracking::start_worker();

    let ui_dir = ui_dir();
    let serve_ui = ServeDir::new(&ui_dir).fallback(ServeFile::new(ui_dir.join("index.html")));

    let router = Router::new()
        .route("/api/library", get(get_library))
        .route("/api/sources", get(get_sources))
        .route("/api/launch", post(post_launch))
        .route("/api/install", post(post_install))
        .route("/api/installs", get(get_installs))
        .route("/api/installs/cancel", post(post_install_cancel))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/steam/status", get(get_steam_status))
        .route("/api/addons", get(get_addons).post(post_addon))
        .route("/api/addons/remove", post(post_addon_remove))
        .route(
            "/api/source-manifests",
            get(get_source_manifests).post(post_source_manifest),
        )
        .route(
            "/api/source-manifests/remove",
            post(post_source_manifest_remove),
        )
        .route(
            "/api/source-manifests/toggle",
            post(post_source_manifest_toggle),
        )
        .route(
            "/api/source-manifests/test",
            post(post_source_manifest_test),
        )
        .route("/api/meta", get(get_meta))
        .route("/api/preference", get(get_preference).post(post_preference))
        .route("/api/streams", get(get_streams))
        .route("/api/search", get(get_search))
        .route("/api/search/deep", get(get_search_deep))
        .route("/api/similar", get(get_similar))
        .route("/api/youtube/status", get(get_youtube_status))
        .route("/api/live/status", get(get_live_status))
        .route("/api/game", get(get_game))
        .route("/api/tracking/status", get(get_tracking_status))
        .route("/api/trakt/connect", post(post_trakt_connect))
        .route("/api/mal/login", get(get_mal_login))
        .route("/api/mal/callback", get(get_mal_callback))
        .route("/api/resume", get(get_resume))
        .route("/api/play", post(post_play))
        .route("/api/open", post(post_open))
        .route("/api/version", get(get_version))
        .fallback_service(serve_ui)
        .layer(axum::middleware::map_response(no_html_cache))
        // Cap request bodies — the API only ever carries small JSON payloads.
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(app);

    let addr = listen_addr();
    let token = auth_token();
    let loopback = addr_is_loopback(&addr);

    // Refuse to expose the API to the network without an auth token: the
    // endpoints launch programs and open URLs, so an open non-loopback bind is
    // remote-code-execution-by-design.
    if !loopback && token.is_none() {
        panic!(
            "refusing to bind non-loopback address {addr} without TVOS_AUTH_TOKEN set \
             (the API can launch programs); set a token or bind 127.0.0.1"
        );
    }

    // When a token is configured, guard all mutating endpoints with it. On a
    // loopback bind with no token we keep the original open behavior.
    let router = match &token {
        Some(t) => {
            if !loopback {
                log_warn!(
                    "tvosd binding non-loopback address {addr}: mutating endpoints require \
                     the TVOS_AUTH_TOKEN (Bearer header or ?token=)"
                );
            }
            router.layer(axum::middleware::from_fn_with_state(
                Arc::new(t.clone()),
                require_auth,
            ))
        }
        None => router,
    };

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("cannot bind {addr}: {e}"));
    log_info!(
        "tvosd listening on http://{addr} (ui: {})",
        ui_dir.display()
    );
    start_background_warmups();
    axum::serve(listener, router).await.expect("server error");
}

fn start_background_warmups() {
    if !background_warmups_enabled() {
        log_info!("background model/shader warmups disabled");
        return;
    }
    // Warm the embedding model in the background (it downloads once and is slow
    // to load); until it's ready the recommender uses the content-based fallback.
    std::thread::spawn(|| {
        if embed::init() {
            sources::tmdb::prewarm_recommender();
            log_info!("embedding recommender ready");
        }
    });
    // Zero-config upscaling (PLAN §5): fetch the Enhance shader files once in
    // the background so "Enhance" works out of the box, no manual script.
    std::thread::spawn(|| {
        if shaders::ensure() {
            log_info!("upscaler shaders ready");
        }
    });
}

fn background_warmups_enabled() -> bool {
    match std::env::var("TVOS_BACKGROUND_WARMUPS").as_deref() {
        Ok("0") | Ok("false") | Ok("no") => false,
        Ok("1") | Ok("true") | Ok("yes") => true,
        _ => !matches!(std::env::var("TVOS_MAC_APP").as_deref(), Ok("1")),
    }
}

/// Unwraps a `spawn_blocking` join result, logging the `JoinError` (a panicked
/// or cancelled blocking task) before falling back to the default so silent
/// failures on read endpoints become observable in the log.
fn join_or_default<T: Default>(what: &str, result: Result<T, tokio::task::JoinError>) -> T {
    result.unwrap_or_else(|e| {
        log_error!("{what} task failed: {e}");
        T::default()
    })
}

/// The shell is served from disk and swapped in place by install.sh — never
/// let the browser cache HTML, or a relaunched window can keep showing an old
/// build. Hashed asset filenames keep everything else safely cacheable.
async fn no_html_cache(mut res: axum::response::Response) -> axum::response::Response {
    use axum::http::header;
    let is_html = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.contains("text/html"));
    if is_html {
        res.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-cache"),
        );
    }
    res
}

/// Where the built shell lives. `TVOS_UI_DIR` wins (used in dev and by the
/// portable demo); otherwise we search the user install (`~/.local/share`)
/// and the system install (`/usr/share`, `/usr/local/share`) so the same
/// binary works whether installed as a user app or a pacman package.
fn ui_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TVOS_UI_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(cwd) = std::env::current_dir() {
        let dev = cwd.join("shell/dist");
        if dev.is_dir() {
            return dev;
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let user_default = PathBuf::from(format!("{home}/.local/share/tvos/ui"));
    [
        user_default.clone(),
        PathBuf::from("/usr/share/tvos/ui"),
        PathBuf::from("/usr/local/share/tvos/ui"),
    ]
    .into_iter()
    .find(|p| p.is_dir())
    .unwrap_or(user_default)
}

/// Personal library rows kept on the recommendation-only home — your watchlist
/// and owned/started rows — as opposed to generic browse catalogues (Cinemeta
/// "Popular"/"Trending", TMDB discovery, etc.), which are dropped. Mirrors the
/// shell's LIBRARY_ROW grouping (see shell/src/tabs.ts).
fn is_personal_row(title: &str) -> bool {
    let t = title.to_ascii_lowercase();
    t.contains("because you")
        || ["continue", "watchlist", "ready to", "my ", "your "]
            .iter()
            .any(|p| t.starts_with(p))
}

/// Library building shells out to store CLIs and may hit catalog APIs, so it
/// runs on a blocking thread instead of stalling the async executor.
/// Personalized rows (Continue / Recommended) come first.
async fn get_library(State(app): State<Shared>) -> Json<Vec<model::Row>> {
    tracking::sync_local_play_markers();
    // "Continue" comes from the in-memory event log (instant).
    let mut rows = recommend::LOG.rows();
    rows.extend(recommend::LOG.preference_rows());
    let local_recent = recommend::LOG.recommendation_seeds(8);
    let disliked = recommend::LOG.disliked_ids();
    // Recommendations ("Because you watched …") and the source catalogs both hit
    // the network, so build them on a blocking thread. Recs come right after
    // Continue, then everything else.
    rows.extend(join_or_default(
        "library",
        tokio::task::spawn_blocking(move || {
            // Seed recommendations from what you've watched *anywhere*: recent
            // local plays first, then your Trakt history (a no-op/blocking call
            // only when Trakt is connected — hence inside this blocking task).
            let mut recent = local_recent;
            for watched in tracking::watched_history(24) {
                if !recent.iter().any(|i| i.id == watched.id) {
                    recent.push(watched);
                }
            }
            recent.truncate(16);

            // The home is strictly a recommendation feed (Google-TV "For you"):
            // personalized rows only — no browse catalogues. "Top picks for you"
            // leads; "Because you watched X" follows.
            let mut r = sources::tmdb::for_you(&recent);
            let library = app.refresh_library();

            // Recommendation rows built in parallel while the library is whole:
            // "Because you watched X" (video), taste-ranked game picks for the
            // Games hub, and "Because you play X" genre rows.
            let (bcw, recs, genre_rows) = std::thread::scope(|s| {
                let bcw = s.spawn(|| sources::tmdb::because_you_watched(&recent, 3));
                let recs = s.spawn(|| sources::gamerec::recommended(&library));
                let genres = s.spawn(|| {
                    sources::gamerec::top_genres(3)
                        .into_iter()
                        .map(|(name, tag)| model::Row {
                            title: format!("Because you play {name}"),
                            items: sources::gamehub::genre_row(tag, &library, 20),
                        })
                        .filter(|row| !row.items.is_empty())
                        .collect::<Vec<_>>()
                });
                (
                    bcw.join().unwrap_or_default(),
                    recs.join().unwrap_or_default(),
                    genres.join().unwrap_or_default(),
                )
            });

            r.extend(bcw);

            // Your installed/owned games → the "Games" row. Non-game library
            // rows are kept ONLY when personal (your watchlist / owned rows);
            // generic browse catalogues (Cinemeta "Popular"/"Trending", etc.)
            // are dropped — the home is recommendation-only.
            let mut owned_games: Vec<model::ContentItem> = Vec::new();
            let mut personal: Vec<model::Row> = Vec::new();
            // Live rows (the Live tab) are kept whole, like games — the tab is a
            // client-side filter over this feed, so the data must reach it.
            let mut live_rows: Vec<model::Row> = Vec::new();
            for mut row in library {
                let all_games =
                    !row.items.is_empty() && row.items.iter().all(|i| i.kind == model::Kind::Game);
                let all_live =
                    !row.items.is_empty() && row.items.iter().all(|i| i.kind == model::Kind::Live);
                if all_games {
                    owned_games.append(&mut row.items);
                } else if all_live {
                    live_rows.push(row);
                } else if is_personal_row(&row.title) {
                    personal.push(row);
                }
            }

            // De-duplicate game recommendations across every rec row so a title
            // never repeats: the "Recommended games" row claims its picks first,
            // then each "Because you play X" row keeps only what's still unseen.
            let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
            let recs: Vec<model::ContentItem> = recs
                .into_iter()
                .filter(|i| used.insert(i.id.clone()))
                .collect();
            let genre_rows: Vec<model::Row> = genre_rows
                .into_iter()
                .filter_map(|mut row| {
                    row.items.retain(|i| used.insert(i.id.clone()));
                    (!row.items.is_empty()).then_some(row)
                })
                .collect();

            if !owned_games.is_empty() {
                r.push(model::Row {
                    title: "Games".to_string(),
                    items: owned_games,
                });
            }
            if !recs.is_empty() {
                r.push(model::Row {
                    title: "Recommended games".to_string(),
                    items: recs,
                });
            }
            r.extend(genre_rows);
            r.extend(personal);
            // Taste-biased, lightly random item order per row — fresh finds
            // mixed with the familiar on every visit.
            sources::tmdb::personalize(&mut r, &recent);
            // Live rows last, in their curated order (Live now → sports → what's
            // coming up), untouched by the taste shuffle above.
            r.extend(live_rows);
            if !disliked.is_empty() {
                for row in &mut r {
                    row.items.retain(|i| !disliked.contains(&i.id));
                }
                r.retain(|row| !row.items.is_empty());
            }
            r
        })
        .await,
    ));
    // No blank movie/show cards: drop catalog items without artwork. Games and
    // live channels are kept regardless — a logo-less IPTV channel still plays,
    // and you want your whole library and every live stream visible, art or not.
    for row in &mut rows {
        row.items.retain(|i| {
            matches!(i.kind, model::Kind::Game | model::Kind::Live)
                || i.art.as_deref().is_some_and(|a| !a.is_empty())
        });
    }
    rows.retain(|row| !row.items.is_empty());
    Json(rows)
}

#[derive(Deserialize)]
struct PreferenceRequest {
    action: String,
    item: ItemMeta,
}

async fn get_preference(Query(q): Query<IdQuery>) -> Json<recommend::PreferenceStatus> {
    Json(recommend::LOG.preference(&q.id))
}

async fn post_preference(
    Json(req): Json<PreferenceRequest>,
) -> Result<Json<recommend::PreferenceStatus>, (StatusCode, String)> {
    let action = match req.action.as_str() {
        "watchlist" => recommend::PreferenceAction::Watchlist,
        "watched" => recommend::PreferenceAction::Watched,
        "like" => recommend::PreferenceAction::Like,
        "dislike" => recommend::PreferenceAction::Dislike,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "unknown preference action".to_string(),
            ))
        }
    };
    let status = recommend::LOG.set_preference(
        model::ContentItem {
            id: req.item.id,
            kind: req.item.kind,
            title: req.item.title,
            art: req.item.art,
            action: req.item.action.unwrap_or(model::Action::Play),
            note: None,
                },
        action,
    );
    Ok(Json(status))
}

async fn get_version() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "version": env!("CARGO_PKG_VERSION") }))
}

async fn get_sources(State(app): State<Shared>) -> Json<Vec<sources::SourceInfo>> {
    Json(app.sources.sources())
}

async fn get_installs(State(app): State<Shared>) -> Json<Vec<install::Job>> {
    Json(app.installs.jobs())
}

/// Cancel a running download job by id; the worker aborts cooperatively and
/// removes its partial file.
async fn post_install_cancel(
    State(app): State<Shared>,
    Json(req): Json<ItemRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    app.installs
        .cancel(&req.id)
        .map(|()| StatusCode::ACCEPTED)
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))
}

/// Secrets (steam_api_key, trakt_client_secret, trakt_token, anilist_token,
/// mal_token) are write-only: this returns them blanked, with sibling
/// `<field>_set` booleans so the UI can still show "configured". PUT keeps the
/// full values; an empty secret on PUT is treated as "unchanged".
async fn get_settings() -> Json<serde_json::Value> {
    Json(settings::STORE.get().redacted())
}

async fn put_settings(
    Json(new): Json<settings::Settings>,
) -> Result<StatusCode, (StatusCode, String)> {
    settings::STORE
        .set(new)
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Tests the saved Steam credentials; runs on a blocking thread (network).
async fn get_steam_status() -> Json<serde_json::Value> {
    let result = tokio::task::spawn_blocking(sources::steam::connection_test)
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    Json(match result {
        Ok(count) => serde_json::json!({ "connected": true, "count": count }),
        Err(error) => serde_json::json!({ "connected": false, "error": error }),
    })
}

async fn get_addons() -> Json<Vec<addons::Addon>> {
    Json(addons::STORE.list())
}

#[derive(Deserialize)]
struct AddonRequest {
    url: String,
}

/// Installing fetches the manifest over the network — blocking thread.
async fn post_addon(
    Json(req): Json<AddonRequest>,
) -> Result<Json<addons::Addon>, (StatusCode, String)> {
    tokio::task::spawn_blocking(move || addons::STORE.install(&req.url))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))
}

async fn post_addon_remove(
    Json(req): Json<AddonRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    addons::STORE
        .remove(&req.url)
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))
}

async fn get_source_manifests() -> Json<Vec<sources::cloudstream::Manifest>> {
    Json(sources::cloudstream::STORE.list())
}

/// Add a source manifest from either a URL (fetched) or its JSON pasted
/// directly — the daemon auto-detects which. A URL install fetches over the
/// network, so it runs on a blocking thread.
#[derive(Deserialize)]
struct SourceManifestInput {
    /// URL or raw JSON. `url` is accepted as an alias for older clients.
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

async fn post_source_manifest(
    Json(req): Json<SourceManifestInput>,
) -> Result<Json<sources::cloudstream::Manifest>, (StatusCode, String)> {
    let input = req.text.or(req.url).unwrap_or_default();
    tokio::task::spawn_blocking(move || sources::cloudstream::STORE.install(&input))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))
}

#[derive(Deserialize)]
struct SourceManifestId {
    id: String,
}

async fn post_source_manifest_remove(
    Json(req): Json<SourceManifestId>,
) -> Result<StatusCode, (StatusCode, String)> {
    sources::cloudstream::STORE
        .remove(&req.id)
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))
}

#[derive(Deserialize)]
struct SourceToggle {
    id: String,
    name: String,
    enabled: bool,
}

async fn post_source_manifest_toggle(
    Json(req): Json<SourceToggle>,
) -> Result<Json<sources::cloudstream::Manifest>, (StatusCode, String)> {
    sources::cloudstream::STORE
        .set_enabled(&req.id, &req.name, req.enabled)
        .map(Json)
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))
}

#[derive(Deserialize)]
struct SourceTest {
    /// Manifest to test; when absent, every installed manifest is tested.
    #[serde(default)]
    id: Option<String>,
}

/// Probes each source for reachability and auto-disables the unreachable ones —
/// network work, so it runs on a blocking thread.
async fn post_source_manifest_test(
    Json(req): Json<SourceTest>,
) -> Json<Vec<sources::cloudstream::Manifest>> {
    let manifests =
        tokio::task::spawn_blocking(move || sources::cloudstream::STORE.test(req.id.as_deref()))
            .await
            .unwrap_or_default();
    Json(manifests)
}

#[derive(Deserialize)]
struct ItemRequest {
    id: String,
    /// Optional item details; when present, a successful launch is recorded
    /// in the recommender's event log. The shell always sends them.
    title: Option<String>,
    kind: Option<model::Kind>,
    art: Option<String>,
}

async fn post_launch(
    State(app): State<Shared>,
    Json(req): Json<ItemRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let id = req.id.clone();
    let result = tokio::task::spawn_blocking(move || app.sources.launch(&id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let Err(e) = &result {
        log_error!(
            "launch '{}' failed: {}",
            req.id,
            util::scrub_secrets(&e.to_string())
        );
    }
    if result.is_ok() {
        if let (Some(title), Some(kind)) = (req.title, req.kind) {
            recommend::LOG.record(model::ContentItem {
                id: req.id,
                kind,
                title,
                art: req.art,
                action: model::Action::Play,
                note: None,
                        });
        }
    }
    result
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))
}

async fn post_install(
    State(app): State<Shared>,
    Json(req): Json<ItemRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let result = tokio::task::spawn_blocking(move || app.sources.install(&req.id, &app.installs))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    result
        .map(|()| StatusCode::ACCEPTED)
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))
}

#[derive(Deserialize)]
struct IdQuery {
    id: String,
}

/// Details-page metadata: description, background, and (for series) the full
/// episode list. Video items resolve through stream addons (Cinemeta etc.);
/// Steam games use the public storefront API; others return a minimal stub.
async fn get_meta(State(app): State<Shared>, Query(q): Query<IdQuery>) -> Json<media::Meta> {
    // The item's title (for games with no storefront of their own — Epic/GOG —
    // used to borrow metadata from Steam by name).
    let title = title_in_library(&app.cached_library(), &q.id);
    let meta = join_or_default(
        "meta",
        tokio::task::spawn_blocking(move || meta_for(&q.id, title.as_deref())).await,
    );
    Json(meta)
}

/// The title of a library item by id, if it's currently in the cached library.
fn title_in_library(library: &[model::Row], id: &str) -> Option<String> {
    library
        .iter()
        .flat_map(|r| &r.items)
        .find(|i| i.id == id)
        .map(|i| i.title.clone())
}

fn meta_for(id: &str, title: Option<&str>) -> media::Meta {
    let prefix = id.split(':').next().unwrap_or_default();
    match prefix {
        "strm" | "tmdb" => sources::resolve_video(id)
            .ok()
            .and_then(|(kind, sid)| sources::stremio::meta(&kind, &sid))
            .unwrap_or_else(|| media::Meta {
                id: id.to_string(),
                kind: if id.contains(":tv:") || id.contains(":series:") {
                    "series".to_string()
                } else {
                    "movie".to_string()
                },
                ..Default::default()
            }),
        "steam" => {
            sources::steam::store_meta(id.trim_start_matches("steam:")).unwrap_or(media::Meta {
                id: id.to_string(),
                kind: "game".to_string(),
                ..Default::default()
            })
        }
        "yt" => sources::youtube::video_meta(id).unwrap_or(media::Meta {
            id: id.to_string(),
            kind: "video".to_string(),
            ..Default::default()
        }),
        // Live streams/fixtures: a light stub (title from the library) — never a
        // Steam game lookup like the fallback below would do.
        "live" => media::Meta {
            id: id.to_string(),
            kind: "video".to_string(),
            title: title.unwrap_or_default().to_string(),
            ..Default::default()
        },
        // Unowned games (GameHub): the Steam storefront has rich metadata.
        "gshop" => {
            sources::steam::store_meta(id.trim_start_matches("gshop:")).unwrap_or(media::Meta {
                id: id.to_string(),
                kind: "game".to_string(),
                ..Default::default()
            })
        }
        // Owned games from stores without a storefront API of their own
        // (Epic, GOG, retro/homebrew). Borrow rich metadata — description,
        // screenshots, genres and the stylized logo — from Steam by matching
        // the title, so their details page isn't a bare stub.
        other => {
            let kind = if other == "video" { "movie" } else { "game" };
            if kind == "game" {
                if let Some(t) = title.filter(|t| !t.is_empty()) {
                    if let Some(appid) = sources::steam::store_search(t) {
                        if let Some(mut m) = sources::steam::store_meta(&appid.to_string()) {
                            m.id = id.to_string(); // keep the owned id, not the Steam one
                            m.title = t.to_string(); // and the title as the library shows it
                            return m;
                        }
                    }
                }
            }
            media::Meta {
                id: id.to_string(),
                kind: kind.to_string(),
                title: title.unwrap_or_default().to_string(),
                ..Default::default()
            }
        }
    }
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

/// Fast as-you-type search: library + TMDB titles + addon catalogs, one
/// fuzzy-ranked list (see search.rs).
async fn get_search(
    State(app): State<Shared>,
    Query(query): Query<SearchQuery>,
) -> Json<Vec<model::ContentItem>> {
    let items = join_or_default(
        "search",
        tokio::task::spawn_blocking(move || {
            let library = app.cached_library();
            search::flat(&query.q, library)
        })
        .await,
    );
    Json(items)
}

/// Deep search over the entire space — titles, actors' filmographies, plot
/// keywords, genre/idiom discovery ("k drama"), library, addons — returned as
/// titled sections (see search.rs).
async fn get_search_deep(
    State(app): State<Shared>,
    Query(query): Query<SearchQuery>,
) -> Json<Vec<model::Row>> {
    let rows = join_or_default(
        "deep search",
        tokio::task::spawn_blocking(move || {
            let library = app.cached_library();
            search::deep(&query.q, library)
        })
        .await,
    );
    Json(rows)
}

/// Game-page extras: playtime, HowLongToBeat, and the achievement lists.
/// Everything degrades to null/absent when a source isn't available.
async fn get_game(Query(q): Query<IdQuery>) -> Json<serde_json::Value> {
    let value = tokio::task::spawn_blocking(move || {
        let appid =
            q.id.strip_prefix("steam:")
                .or_else(|| q.id.strip_prefix("gshop:"))
                .unwrap_or_default()
                .to_string();
        if appid.is_empty() {
            return serde_json::json!({});
        }
        let owned = q.id.starts_with("steam:");
        // Title for HLTB comes from the storefront (cached by store_meta).
        let title = sources::steam::store_meta(&appid)
            .map(|m| m.title)
            .unwrap_or_default();
        let (playtime, achievements, hltb) = std::thread::scope(|s| {
            let playtime = s.spawn(|| {
                owned
                    .then(|| sources::steam::playtime_minutes(&appid))
                    .flatten()
            });
            let ach = s.spawn(|| {
                owned
                    .then(|| sources::steam::achievements(&appid))
                    .flatten()
            });
            let hltb = s.spawn(|| {
                (!title.is_empty())
                    .then(|| sources::hltb::lookup(&title))
                    .flatten()
            });
            (
                playtime.join().unwrap_or(None),
                ach.join().unwrap_or(None),
                hltb.join().unwrap_or(None),
            )
        });
        serde_json::json!({
            "playtime_minutes": playtime,
            "hltb": hltb,
            "achievements": achievements.map(|(unlocked, locked)| serde_json::json!({
                "unlocked": unlocked,
                "locked": locked,
            })),
        })
    })
    .await
    .unwrap_or_else(|e| {
        log_error!("game extras task failed: {e}");
        serde_json::json!({})
    });
    Json(value)
}

async fn get_tracking_status() -> Json<serde_json::Value> {
    Json(tracking::status())
}

/// Kicks off the Trakt device-code flow; the panel shows the code, a
/// background thread polls until the user approves.
async fn post_trakt_connect() -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    tokio::task::spawn_blocking(tracking::trakt_connect)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))
}

/// Redirects to MyAnimeList's OAuth page (PKCE, plain method).
async fn get_mal_login() -> Result<axum::response::Redirect, (StatusCode, String)> {
    tracking::mal_login_url()
        .map(|url| axum::response::Redirect::temporary(&url))
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))
}

#[derive(Deserialize)]
struct MalCallback {
    code: String,
}

/// MAL redirects here after approval; we exchange the code and show a
/// human-readable result (this opens in the TV OS browser window).
async fn get_mal_callback(Query(q): Query<MalCallback>) -> axum::response::Html<String> {
    let result = tokio::task::spawn_blocking(move || tracking::mal_callback(&q.code))
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    let message = match result {
        Ok(()) => "MyAnimeList connected — you can close this window.".to_string(),
        Err(e) => format!("MyAnimeList connection failed: {e}"),
    };
    axum::response::Html(format!(
        "<body style=\"background:#0b0a12;color:#f4f3f8;font-family:sans-serif;\
         display:flex;align-items:center;justify-content:center;height:100vh\">\
         <h2>{message}</h2></body>"
    ))
}

/// Whether the signed-in YouTube feeds are reachable (cookie check + a real
/// feed fetch) — the Settings panel's "check connection" button.
async fn get_youtube_status() -> Json<serde_json::Value> {
    let (connected, detail) = tokio::task::spawn_blocking(sources::youtube::account_status)
        .await
        .unwrap_or_else(|e| {
            log_error!("youtube status task failed: {e}");
            (false, "status check failed".to_string())
        });
    Json(serde_json::json!({ "connected": connected, "detail": detail }))
}

async fn get_live_status() -> Json<serde_json::Value> {
    Json(
        tokio::task::spawn_blocking(sources::live::status)
            .await
            .unwrap_or_else(|e| {
                log_error!("live status task failed: {e}");
                serde_json::json!({ "detail": "status check failed" })
            }),
    )
}

/// "More like this" for a details page item. Video items resolve through
/// TMDB recommendations (addon items map IMDb → TMDB first); other kinds
/// (games) have no similar-content source yet and return empty.
async fn get_similar(Query(q): Query<IdQuery>) -> Json<Vec<model::ContentItem>> {
    let items = join_or_default(
        "similar",
        tokio::task::spawn_blocking(move || similar_for(&q.id)).await,
    );
    Json(items)
}

fn similar_for(id: &str) -> Vec<model::ContentItem> {
    match id.split(':').next().unwrap_or_default() {
        "tmdb" => {
            let mut parts = id.splitn(3, ':');
            parts.next();
            match (parts.next(), parts.next().and_then(|s| s.parse().ok())) {
                (Some(media @ ("movie" | "tv")), Some(tmdb_id)) => {
                    sources::tmdb::similar(media, tmdb_id)
                }
                _ => Vec::new(),
            }
        }
        "strm" => {
            let Ok((_, sid)) = sources::resolve_video(id) else {
                return Vec::new();
            };
            // Episode ids carry ":season:episode" — similar is per-title.
            let imdb = sid.split(':').next().unwrap_or(&sid);
            sources::tmdb::find_by_imdb(imdb)
                .map(|(media, tmdb_id)| sources::tmdb::similar(&media, tmdb_id))
                .unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// Resume info for an item: the source last used and the saved position.
async fn get_resume(Query(q): Query<IdQuery>) -> Json<serde_json::Value> {
    match resume::STORE.stream(&q.id) {
        Some(stream) => serde_json::json!({
            "stream": stream,
            "position": resume::position(&q.id).unwrap_or(0.0),
        })
        .into(),
        None => serde_json::Value::Null.into(),
    }
}

/// Sources for an item: streams for videos/episodes (ranked best-first), or
/// "where to buy" offers for unowned games (GameHub, cheapest first).
async fn get_streams(Query(q): Query<IdQuery>) -> Json<Vec<media::Stream>> {
    let streams = join_or_default(
        "streams",
        tokio::task::spawn_blocking(move || {
            if let Some(appid) = q.id.strip_prefix("gshop:") {
                return sources::gamehub::offers(appid);
            }
            match sources::resolve_video(&q.id) {
                Ok((kind, id)) => sources::stremio::streams(&kind, &id),
                Err(_) => Vec::new(),
            }
        })
        .await,
    );
    Json(streams)
}

#[derive(Deserialize)]
struct PlayRequest {
    stream: media::Stream,
    /// Item details, recorded for the recommender on a successful play.
    item: Option<ItemMeta>,
    /// Precise watched id for the scrobbler (an episode carries season:episode),
    /// distinct from `item.id` which is the show — so "Continue" surfaces the
    /// show while Trakt/AniList get the exact episode. Defaults to `item.id`.
    track_id: Option<String>,
}

#[derive(Deserialize)]
struct ItemMeta {
    id: String,
    title: String,
    kind: model::Kind,
    art: Option<String>,
    #[serde(default)]
    action: Option<model::Action>,
}

/// Plays a stream the user picked on the details page.
async fn post_play(Json(req): Json<PlayRequest>) -> Result<StatusCode, (StatusCode, String)> {
    if req.stream.url.len() > MAX_URL_LEN {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "stream URL is too long".to_string(),
        ));
    }
    let stream = req.stream;
    let item_id = req.item.as_ref().map(|i| i.id.clone());
    let display_title = req.item.as_ref().map(|i| i.title.clone());
    let track_id = req.track_id.clone();
    if let (Some(item), Some(content_id)) = (req.item.as_ref(), item_id.as_deref()) {
        tracking::remember_local_play(
            content_id,
            &model::ContentItem {
                id: item.id.clone(),
                kind: item.kind,
                title: item.title.clone(),
                art: item.art.clone(),
                action: model::Action::Play,
                note: None,
                        },
        );
    }
    let result = tokio::task::spawn_blocking(move || {
        let first = sources::stremio::play_stream(
            &stream,
            item_id.as_deref(),
            track_id.as_deref(),
            display_title.as_deref(),
        );
        if first.is_ok() || stream.kind == media::StreamKind::External {
            return first;
        }
        let Some(item_id) = item_id.as_deref() else {
            return first;
        };
        let Ok((kind, sid)) = sources::resolve_video(item_id) else {
            return first;
        };
        log_warn!(
            "selected/resume source failed, refreshing sources for {}: {}",
            item_id,
            util::scrub_secrets(first.as_ref().err().unwrap())
        );
        sources::stremio::play_meta(&kind, &sid, Some(item_id), display_title.as_deref()).or(first)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let Err(e) = &result {
        log_error!("play failed: {}", util::scrub_secrets(&e.to_string()));
    }
    if result.is_ok() {
        if let Some(item) = req.item {
            recommend::LOG.record(model::ContentItem {
                id: item.id,
                kind: item.kind,
                title: item.title,
                art: item.art,
                action: model::Action::Play,
                note: None,
                        });
        }
    }
    result
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))
}

/// Opens a URL with the system handler — an addon's /configure page, etc.
/// Only http(s) URLs are handed to xdg-open; anything else (file:, custom
/// schemes, shell tricks) is refused so a client can't open arbitrary handlers.
async fn post_open(Json(req): Json<AddonRequest>) -> Result<StatusCode, (StatusCode, String)> {
    validate_web_url(&req.url)?;
    launcher::open_external(&req.url)
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))
}

/// Accepts only `http`/`https` URLs of a sane length, for endpoints that hand a
/// client-supplied URL to an external program (xdg-open) or player.
fn validate_web_url(url: &str) -> Result<(), (StatusCode, String)> {
    if url.len() > MAX_URL_LEN {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "URL is too long".to_string(),
        ));
    }
    let scheme = url
        .split_once("://")
        .map(|(s, _)| s.to_ascii_lowercase())
        .unwrap_or_default();
    if scheme != "http" && scheme != "https" {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "only http and https URLs are allowed".to_string(),
        ));
    }
    Ok(())
}
