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

mod addons;
mod install;
mod launcher;
mod logging;
mod media;
mod model;
mod recommend;
mod settings;
mod sources;
mod upscale;
mod util;

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tower_http::services::{ServeDir, ServeFile};

const LISTEN_ADDR: &str = "127.0.0.1:8484";

struct App {
    sources: sources::Registry,
    installs: install::InstallManager,
}

type Shared = Arc<App>;

#[tokio::main]
async fn main() {
    logging::init();
    let app = Arc::new(App {
        sources: sources::Registry::detect(),
        installs: install::InstallManager::default(),
    });

    let ui_dir = ui_dir();
    let serve_ui = ServeDir::new(&ui_dir).fallback(ServeFile::new(ui_dir.join("index.html")));

    let router = Router::new()
        .route("/api/library", get(get_library))
        .route("/api/sources", get(get_sources))
        .route("/api/launch", post(post_launch))
        .route("/api/install", post(post_install))
        .route("/api/installs", get(get_installs))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/steam/status", get(get_steam_status))
        .route("/api/addons", get(get_addons).post(post_addon))
        .route("/api/addons/remove", post(post_addon_remove))
        .route("/api/meta", get(get_meta))
        .route("/api/streams", get(get_streams))
        .route("/api/play", post(post_play))
        .route("/api/open", post(post_open))
        .route("/api/version", get(get_version))
        .fallback_service(serve_ui)
        .with_state(app);

    let listener = tokio::net::TcpListener::bind(LISTEN_ADDR)
        .await
        .unwrap_or_else(|e| panic!("cannot bind {LISTEN_ADDR}: {e}"));
    log_info!(
        "tvosd listening on http://{LISTEN_ADDR} (ui: {})",
        ui_dir.display()
    );
    axum::serve(listener, router).await.expect("server error");
}

/// Where the built shell lives. `TVOS_UI_DIR` wins (used in dev and by the
/// portable demo); otherwise we search the user install (`~/.local/share`)
/// and the system install (`/usr/share`, `/usr/local/share`) so the same
/// binary works whether installed as a user app or a pacman package.
fn ui_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TVOS_UI_DIR") {
        return PathBuf::from(dir);
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

/// Library building shells out to store CLIs and may hit catalog APIs, so it
/// runs on a blocking thread instead of stalling the async executor.
/// Personalized rows (Continue / Recommended) come first.
async fn get_library(State(app): State<Shared>) -> Json<Vec<model::Row>> {
    let mut rows = recommend::LOG.rows();
    rows.extend(
        tokio::task::spawn_blocking(move || app.sources.library())
            .await
            .unwrap_or_default(),
    );
    rows.retain(|row| !row.items.is_empty());
    Json(rows)
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

async fn get_settings() -> Json<settings::Settings> {
    Json(settings::STORE.get())
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
        log_error!("launch '{}' failed: {e}", req.id);
    }
    if result.is_ok() {
        if let (Some(title), Some(kind)) = (req.title, req.kind) {
            recommend::LOG.record(model::ContentItem {
                id: req.id,
                kind,
                title,
                art: req.art,
                action: model::Action::Play,
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
async fn get_meta(Query(q): Query<IdQuery>) -> Json<media::Meta> {
    let meta = tokio::task::spawn_blocking(move || meta_for(&q.id))
        .await
        .unwrap_or_default();
    Json(meta)
}

fn meta_for(id: &str) -> media::Meta {
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
        other => media::Meta {
            id: id.to_string(),
            kind: if other == "video" { "movie" } else { "game" }.to_string(),
            ..Default::default()
        },
    }
}

/// All streams for a video item / episode, ranked best-first.
async fn get_streams(Query(q): Query<IdQuery>) -> Json<Vec<media::Stream>> {
    let streams = tokio::task::spawn_blocking(move || match sources::resolve_video(&q.id) {
        Ok((kind, id)) => sources::stremio::streams(&kind, &id),
        Err(_) => Vec::new(),
    })
    .await
    .unwrap_or_default();
    Json(streams)
}

#[derive(Deserialize)]
struct PlayRequest {
    stream: media::Stream,
    /// Item details, recorded for the recommender on a successful play.
    item: Option<ItemMeta>,
}

#[derive(Deserialize)]
struct ItemMeta {
    id: String,
    title: String,
    kind: model::Kind,
    art: Option<String>,
}

/// Plays a stream the user picked on the details page.
async fn post_play(Json(req): Json<PlayRequest>) -> Result<StatusCode, (StatusCode, String)> {
    let stream = req.stream;
    let result = tokio::task::spawn_blocking(move || sources::stremio::play_stream(&stream))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let Err(e) = &result {
        log_error!("play failed: {e}");
    }
    if result.is_ok() {
        if let Some(item) = req.item {
            recommend::LOG.record(model::ContentItem {
                id: item.id,
                kind: item.kind,
                title: item.title,
                art: item.art,
                action: model::Action::Play,
            });
        }
    }
    result
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))
}

/// Opens a URL with the system handler — an addon's /configure page, etc.
async fn post_open(Json(req): Json<AddonRequest>) -> Result<StatusCode, (StatusCode, String)> {
    launcher::open_external(&req.url)
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))
}
