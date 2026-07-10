//! CloudStream-style source manifests.
//!
//! A "source manifest" is a small JSON document — added either by URL or by
//! *pasting the JSON directly* — that declares one or more stream providers
//! with URL *templates*. When the details page (or the auto-picker) resolves a
//! title, every *enabled* provider's template is filled in for the requested
//! content and fetched, and the resulting streams are merged in alongside
//! Torrentio / WatchHub and ranked into one absolute best-first list.
//!
//! Manifest shape (CloudStream-flavoured, deliberately tiny):
//! ```json
//! {
//!   "name": "Community Sources",
//!   "sources": [
//!     {
//!       "name": "VidSrc",
//!       "movie":  "https://host/api/movie/{imdb}",
//!       "series": "https://host/api/series/{imdb}/{season}/{episode}",
//!       "format": "cloudstream"
//!     },
//!     {
//!       "name": "Mirror",
//!       "movie":  "https://host/stream/{type}/{id}.json",
//!       "format": "stremio"
//!     }
//!   ]
//! }
//! ```
//!
//! Template placeholders, filled from the requested content:
//!   `{type}`    → "movie" | "series"
//!   `{id}`      → the full stream id ("tt0133093" or "tt0944947:1:1")
//!   `{imdb}`    → the base IMDb id, no episode suffix ("tt0944947")
//!   `{season}`  → season number (series only)
//!   `{episode}` → episode number (series only)
//!
//! Two response `format`s are understood:
//!   - `"stremio"`     → a Stremio `{"streams":[…]}` body (Torrentio-compatible),
//!                       parsed by the shared stremio stream parser.
//!   - `"cloudstream"` → CloudStream's `ExtractorLink` shape: an array (or an
//!                       object with a `links`/`sources`/`streams` array) of
//!                       `{name|source, url|link, quality}` objects → direct
//!                       (or magnet) streams. This is the default.
//!
//! Each source can be individually enabled/disabled, and a reachability test
//! probes every source and auto-disables the ones that don't answer. All of
//! this — the manifest, the per-source enabled flag, and the last probe result
//! — is persisted to ~/.config/tvos/cloudstream.json so it survives a restart.
//!
//! Real CloudStream plugin repositories (`repo.json` / `plugins.json`) are also
//! accepted. Those `.cs3` entries are compiled Android/Dalvik plugins, so this
//! daemon can import their metadata and expand repository bundles like
//! MegaProvider, but it cannot execute their scraper bytecode as a stream
//! provider without an Android CloudStream runtime.

use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::addons::{self, MAX_URL_LEN};
use crate::media::{Stream, StreamKind};
use crate::settings::config_dir;

/// A well-known title used to probe a source for reachability.
const PROBE_MOVIE: &str = "tt0111161"; // The Shawshank Redemption
const PROBE_EPISODE: &str = "tt0944947:1:1"; // Game of Thrones S1E1

pub static STORE: LazyLock<ManifestStore> = LazyLock::new(ManifestStore::load);

pub struct ManifestStore {
    path: PathBuf,
    stored: Mutex<Vec<Stored>>,
}

/// One installed manifest, as persisted. `id` is the install URL, or
/// `paste:<name>` for a pasted manifest.
#[derive(Serialize, Deserialize, Clone)]
struct Stored {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
    manifest: Value,
    /// Per-source enable + last reachability, one entry per declared source.
    #[serde(default)]
    states: Vec<SourceState>,
}

#[derive(Serialize, Deserialize, Clone)]
struct SourceState {
    name: String,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reachable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latency_ms: Option<u64>,
}

fn yes() -> bool {
    true
}

// ---- UI-facing summaries (what /api/source-manifests returns) ----

#[derive(Serialize, Clone)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    pub sources: Vec<SourceSummary>,
}

#[derive(Serialize, Clone)]
pub struct SourceSummary {
    pub name: String,
    pub enabled: bool,
    /// True when this source has URL templates the daemon can query directly.
    pub playable: bool,
    /// True when Settings can probe this entry. `.cs3` descriptors are
    /// testable as package downloads even though they are not playable here.
    pub testable: bool,
    /// "template" for direct URL-template sources, "cs3" for CloudStream
    /// plugin metadata imported from a repository.
    pub kind: &'static str,
    /// Whether it declares a series template (movies-only sources can't play TV).
    pub series: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reachable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
}

/// One stream provider declared by a manifest.
struct SourceDef {
    name: String,
    movie: Option<String>,
    series: Option<String>,
    plugin: Option<String>,
    format: Format,
}

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Stremio,
    Cloudstream,
    Cs3,
}

impl SourceDef {
    fn playable(&self) -> bool {
        self.movie.is_some() || self.series.is_some()
    }

    fn testable(&self) -> bool {
        self.playable() || self.plugin.is_some()
    }

    fn kind(&self) -> &'static str {
        match self.format {
            Format::Cs3 => "cs3",
            _ => "template",
        }
    }
}

impl ManifestStore {
    fn load() -> Self {
        let path = config_dir().join("cloudstream.json");
        let stored = std::fs::read_to_string(&path)
            .ok()
            .map(|text| parse_store(&text))
            .unwrap_or_default();
        Self {
            path,
            stored: Mutex::new(stored),
        }
    }

    pub fn list(&self) -> Vec<Manifest> {
        self.stored
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(summarize)
            .collect()
    }

    /// Adds (or replaces) a manifest from raw input: either an `http(s)` URL we
    /// fetch, or the manifest JSON pasted directly (auto-detected by a leading
    /// `{` / `[`). Existing per-source enable/reachability is preserved on
    /// replace.
    pub fn install(&self, input: &str) -> Result<Manifest, String> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err("paste a manifest URL or its JSON".to_string());
        }

        // A leading '{' or '[' is inline JSON; anything else is a URL to fetch.
        let (source_url, value) = if trimmed.starts_with('{') || trimmed.starts_with('[') {
            let value: Value = serde_json::from_str(trimmed)
                .map_err(|e| format!("pasted text is not valid JSON: {e}"))?;
            (None, value)
        } else {
            if trimmed.len() > MAX_URL_LEN {
                return Err("manifest URL is too long".to_string());
            }
            // SSRF-guarded fetch (public hosts only; localhost for dev).
            let text = addons::http_get(trimmed)?;
            let value: Value =
                serde_json::from_str(&text).map_err(|e| format!("manifest is not JSON: {e}"))?;
            (Some(trimmed.to_string()), value)
        };

        // Turn whatever we got into a source manifest, or explain why we can't
        // (e.g. it's actually a CloudStream .cs3 plugin repo we can't execute).
        let manifest = interpret_manifest(&value)?;
        let name = manifest
            .get("name")
            .and_then(|n| n.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("Sources");
        let id = match &source_url {
            Some(url) => url.clone(),
            None => format!("paste:{name}"),
        };

        let defs = parse_sources(&manifest);
        if defs.is_empty() {
            return Err(
                "manifest declares no usable sources — each needs a \"movie\" or \"series\" URL template"
                    .to_string(),
            );
        }

        let mut stored = self.stored.lock().unwrap_or_else(|e| e.into_inner());
        // Preserve enable/reachability for sources that persist across a replace.
        let prior = stored.iter().find(|s| s.id == id).map(|s| s.states.clone());
        let states = reconcile_states(&defs, prior.as_deref());
        stored.retain(|s| s.id != id);
        stored.push(Stored {
            id: id.clone(),
            source_url,
            manifest,
            states,
        });
        self.persist(&stored)
            .map_err(|e| format!("could not save source manifests: {e}"))?;
        stored
            .iter()
            .find(|s| s.id == id)
            .and_then(summarize)
            .ok_or_else(|| "internal error saving manifest".to_string())
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let mut stored = self.stored.lock().unwrap_or_else(|e| e.into_inner());
        let before = stored.len();
        stored.retain(|s| s.id != id);
        if stored.len() == before {
            return Err("no such source manifest installed".to_string());
        }
        self.persist(&stored)
            .map_err(|e| format!("could not save source manifests: {e}"))
    }

    /// Enables or disables a single source within a manifest.
    pub fn set_enabled(&self, id: &str, name: &str, enabled: bool) -> Result<Manifest, String> {
        let mut stored = self.stored.lock().unwrap_or_else(|e| e.into_inner());
        let m = stored
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or("no such source manifest installed")?;
        let st = m
            .states
            .iter_mut()
            .find(|s| s.name == name)
            .ok_or("no such source in that manifest")?;
        st.enabled = enabled;
        let summary = summarize(m).ok_or("internal error")?;
        self.persist(&stored)
            .map_err(|e| format!("could not save source manifests: {e}"))?;
        Ok(summary)
    }

    /// Probes every source (of `id`, or all manifests when `None`) for
    /// reachability, records the result + latency, and **auto-disables** the
    /// ones that don't answer. Returns the refreshed summaries.
    pub fn test(&self, id: Option<&str>) -> Vec<Manifest> {
        // Collect probe targets (id, source name, probe url) under the lock,
        // then release it before doing any network work.
        struct Target {
            id: String,
            name: String,
            url: String,
        }
        let targets: Vec<Target> = {
            let stored = self.stored.lock().unwrap_or_else(|e| e.into_inner());
            stored
                .iter()
                .filter(|s| id.is_none_or(|want| s.id == want))
                .flat_map(|s| {
                    parse_sources(&s.manifest).into_iter().filter_map(|d| {
                        let url = probe_url(&d)?;
                        Some(Target {
                            id: s.id.clone(),
                            name: d.name,
                            url,
                        })
                    })
                })
                .collect()
        };

        // Probe every target in parallel — one dead host can't stall the rest.
        let results: Vec<(String, String, Option<u64>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = targets
                .iter()
                .map(|t| scope.spawn(move || (t.id.clone(), t.name.clone(), addons::probe(&t.url))))
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });

        // Apply: record reachability + latency, auto-disable the unreachable.
        let mut stored = self.stored.lock().unwrap_or_else(|e| e.into_inner());
        for (mid, name, latency) in results {
            if let Some(st) = stored
                .iter_mut()
                .find(|s| s.id == mid)
                .and_then(|s| s.states.iter_mut().find(|st| st.name == name))
            {
                let reachable = latency.is_some();
                st.reachable = Some(reachable);
                st.latency_ms = latency;
                if !reachable {
                    st.enabled = false;
                }
            }
        }
        let _ = self.persist(&stored);
        stored
            .iter()
            .filter(|s| id.is_none_or(|want| s.id == want))
            .filter_map(summarize)
            .collect()
    }

    /// Every *enabled* provider across every installed manifest.
    fn providers(&self) -> Vec<SourceDef> {
        self.stored
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .flat_map(|s| {
                let off: HashSet<&str> = s
                    .states
                    .iter()
                    .filter(|st| !st.enabled)
                    .map(|st| st.name.as_str())
                    .collect();
                parse_sources(&s.manifest)
                    .into_iter()
                    .filter(SourceDef::playable)
                    .filter(|d| !off.contains(d.name.as_str()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn persist(&self, stored: &[Stored]) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(stored).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, json).map_err(|e| e.to_string())
    }
}

/// Reads the persisted store, tolerating the older `Vec<(url, manifest)>`
/// format (migrated to the richer record with every source enabled).
fn parse_store(text: &str) -> Vec<Stored> {
    if let Ok(stored) = serde_json::from_str::<Vec<Stored>>(text) {
        return stored;
    }
    serde_json::from_str::<Vec<(String, Value)>>(text)
        .map(|old| {
            old.into_iter()
                .map(|(url, manifest)| {
                    let states = reconcile_states(&parse_sources(&manifest), None);
                    Stored {
                        id: url.clone(),
                        source_url: Some(url),
                        manifest,
                        states,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Rebuilds the per-source state list for a manifest's current sources,
/// carrying over enable/reachability for names that still exist.
fn reconcile_states(defs: &[SourceDef], prior: Option<&[SourceState]>) -> Vec<SourceState> {
    defs.iter()
        .map(|d| {
            let carry = prior.and_then(|p| p.iter().find(|s| s.name == d.name));
            SourceState {
                name: d.name.clone(),
                enabled: carry.map(|s| s.enabled).unwrap_or(true),
                reachable: carry.and_then(|s| s.reachable),
                latency_ms: carry.and_then(|s| s.latency_ms),
            }
        })
        .collect()
}

fn summarize(s: &Stored) -> Option<Manifest> {
    let name = s.manifest.get("name")?.as_str()?.to_string();
    let defs = parse_sources(&s.manifest);
    let sources = defs
        .iter()
        .map(|d| {
            let st = s.states.iter().find(|st| st.name == d.name);
            SourceSummary {
                name: d.name.clone(),
                enabled: st.map(|s| s.enabled).unwrap_or(true),
                playable: d.playable(),
                testable: d.testable(),
                kind: d.kind(),
                series: d.series.is_some(),
                reachable: st.and_then(|s| s.reachable),
                latency_ms: st.and_then(|s| s.latency_ms),
            }
        })
        .collect();
    Some(Manifest {
        id: s.id.clone(),
        name,
        source_url: s.source_url.clone(),
        sources,
    })
}

// ---- Streaming ----

/// Streams for `(kind, id)` from every enabled manifest provider, fetched in
/// parallel so one slow/dead source can't hold up the details page. The caller
/// (stremio::streams) merges these with the addon streams and ranks the whole
/// set together into one absolute best-first list.
pub fn streams(kind: &str, id: &str) -> Vec<Stream> {
    let providers = STORE.providers();
    if providers.is_empty() {
        return Vec::new();
    }
    let ctx = TemplateCtx::new(kind, id);

    std::thread::scope(|scope| {
        let handles: Vec<_> = providers
            .iter()
            .filter_map(|p| {
                // Series requests prefer the series template but fall back to the
                // movie one; movie requests need a movie template.
                let template = match ctx.kind {
                    "series" => p.series.as_deref().or(p.movie.as_deref()),
                    _ => p.movie.as_deref(),
                }?;
                let url = ctx.fill(template);
                let (name, format) = (p.name.clone(), p.format);
                Some(scope.spawn(move || fetch_provider(&name, &url, format)))
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    })
}

/// Fetches one provider URL and parses its body into streams (empty on any
/// failure — a bad provider is silently skipped, like a dead addon).
fn fetch_provider(source_name: &str, url: &str, format: Format) -> Vec<Stream> {
    let Ok(body) = addons::http_get_quick(url) else {
        return Vec::new();
    };
    match format {
        Format::Stremio => crate::sources::stremio::parse_streams(&body),
        Format::Cloudstream => parse_cloudstream(&body, source_name),
        Format::Cs3 => Vec::new(),
    }
}

/// The probe URL for a source: its movie template filled with a well-known
/// movie, else its series template filled with a well-known episode.
fn probe_url(d: &SourceDef) -> Option<String> {
    if let Some(t) = &d.movie {
        Some(TemplateCtx::new("movie", PROBE_MOVIE).fill(t))
    } else {
        d.series
            .as_ref()
            .map(|t| TemplateCtx::new("series", PROBE_EPISODE).fill(t))
            .or_else(|| d.plugin.clone())
    }
}

/// The values a URL template is filled with for one request.
struct TemplateCtx<'a> {
    kind: &'a str,
    id: &'a str,
    imdb: &'a str,
    season: String,
    episode: String,
}

impl<'a> TemplateCtx<'a> {
    fn new(kind: &'a str, id: &'a str) -> Self {
        let mut parts = id.split(':');
        let imdb = parts.next().unwrap_or(id);
        let season = parts.next().unwrap_or_default().to_string();
        let episode = parts.next().unwrap_or_default().to_string();
        Self {
            kind,
            id,
            imdb,
            season,
            episode,
        }
    }

    fn fill(&self, template: &str) -> String {
        template
            .replace("{type}", self.kind)
            .replace("{id}", self.id)
            .replace("{imdb}", self.imdb)
            .replace("{season}", &self.season)
            .replace("{episode}", &self.episode)
    }
}

// ---- Manifest parsing ----

/// Coerces arbitrary pasted/fetched JSON into a source-manifest object, or
/// returns a specific, actionable error.
fn interpret_manifest(value: &Value) -> Result<Value, String> {
    // A source manifest is an object with an array of sources. Accept the
    // native "sources" key plus a couple of common synonyms, normalising to
    // "sources".
    for key in ["sources", "providers", "list"] {
        if let Some(arr) = value.get(key).and_then(|s| s.as_array()) {
            let mut obj = value.clone();
            if key != "sources" {
                if let Some(map) = obj.as_object_mut() {
                    map.insert("sources".to_string(), Value::Array(arr.clone()));
                }
            }
            return Ok(obj);
        }
    }

    // A real CloudStream plugin repository: repo.json (has "pluginLists"), a
    // plugins.json array of compiled .cs3 plugins, or MegaProvider's central
    // repository database. Import the metadata and expand repository bundles.
    if value.get("pluginLists").is_some()
        || is_cs3_plugin_list(value)
        || is_cloudstream_repo_database(value)
    {
        return import_cloudstream_repository(value);
    }

    // A bare array of source objects (no wrapper) — accept it as the sources.
    if let Some(arr) = value.as_array() {
        if arr.iter().any(looks_like_source) {
            return Ok(serde_json::json!({ "name": "Sources", "sources": arr }));
        }
    }
    // A single source object (no wrapper) — wrap it.
    if looks_like_source(value) {
        return Ok(serde_json::json!({ "name": "Sources", "sources": [value] }));
    }

    Err(
        "not a source manifest — expected a JSON object with a \"sources\" array \
         (each source: a \"movie\"/\"series\" URL template), a bare array of such \
         sources, or a single source object. See the format in Settings."
            .to_string(),
    )
}

const MAX_CLOUDSTREAM_FETCHES: usize = 96;
const MAX_CLOUDSTREAM_PLUGINS: usize = 800;
const MAX_CS3_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Default)]
struct CloudstreamImport {
    visited: HashSet<String>,
    plugin_urls: HashSet<String>,
    sources: Vec<Value>,
    fetches: usize,
    repositories: usize,
    plugin_lists: usize,
    cs3_inspected: usize,
    errors: Vec<String>,
}

fn import_cloudstream_repository(value: &Value) -> Result<Value, String> {
    let name = value
        .get("name")
        .and_then(|n| n.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("CloudStream plugins")
        .to_string();
    let mut import = CloudstreamImport::default();
    import.collect_value(value);
    if import.sources.is_empty() {
        let detail = import
            .errors
            .first()
            .map(|e| format!(" Last error: {e}"))
            .unwrap_or_default();
        return Err(format!(
            "This is a CloudStream plugin repository, but no plugin metadata could be imported.{detail} \
             CloudStream .cs3 files are compiled Android/Dalvik extensions; direct playback in this \
             daemon still requires URL-template sources that return stream links."
        ));
    }
    Ok(serde_json::json!({
        "name": name,
        "sources": import.sources,
        "cloudstream": {
            "repositories": import.repositories,
            "pluginLists": import.plugin_lists,
            "cs3Inspected": import.cs3_inspected,
            "playback": "metadata-only; compiled .cs3 plugins require CloudStream's Android runtime"
        }
    }))
}

impl CloudstreamImport {
    fn collect_value(&mut self, value: &Value) {
        if self.sources.len() >= MAX_CLOUDSTREAM_PLUGINS {
            return;
        }

        if let Some(plugin_lists) = value.get("pluginLists").and_then(|v| v.as_array()) {
            self.repositories += 1;
            for url in plugin_lists.iter().filter_map(|v| v.as_str()) {
                self.collect_json_url(url);
            }
            return;
        }

        if is_cs3_plugin_list(value) {
            self.plugin_lists += 1;
            if let Some(arr) = value.as_array() {
                for plugin in arr {
                    self.collect_plugin(plugin);
                    if self.sources.len() >= MAX_CLOUDSTREAM_PLUGINS {
                        break;
                    }
                }
            }
            return;
        }

        if is_cloudstream_repo_database(value) {
            if let Some(arr) = value.as_array() {
                for url in arr.iter().filter_map(repo_database_url) {
                    self.collect_json_url(&url);
                }
            }
        }
    }

    fn collect_json_url(&mut self, url: &str) {
        if self.fetches >= MAX_CLOUDSTREAM_FETCHES || !self.visited.insert(url.to_string()) {
            return;
        }
        self.fetches += 1;
        let text = match addons::http_get(url) {
            Ok(text) => text,
            Err(e) => {
                self.push_error(format!("{url}: {e}"));
                return;
            }
        };
        let value = match serde_json::from_str::<Value>(&text) {
            Ok(value) => value,
            Err(e) => {
                self.push_error(format!("{url}: not JSON ({e})"));
                return;
            }
        };
        self.collect_value(&value);
    }

    fn collect_plugin(&mut self, plugin: &Value) {
        if plugin
            .get("status")
            .and_then(|s| s.as_i64())
            .is_some_and(|status| status == 0)
        {
            return;
        }
        if looks_like_repository_expander(plugin) {
            let before = self.sources.len();
            if let Some(url) = cs3_url(plugin) {
                self.collect_cs3_urls(&url);
                if self.sources.len() > before {
                    return;
                }
            }
        }
        let Some(url) = cs3_url(plugin) else {
            return;
        };
        if !self.plugin_urls.insert(url.clone()) {
            return;
        }
        let name = plugin
            .get("name")
            .or_else(|| plugin.get("internalName"))
            .and_then(|n| n.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("CloudStream plugin");
        let mut obj = serde_json::Map::new();
        obj.insert("name".to_string(), Value::String(name.to_string()));
        obj.insert("format".to_string(), Value::String("cs3".to_string()));
        obj.insert("pluginUrl".to_string(), Value::String(url));
        for key in [
            "internalName",
            "repositoryUrl",
            "language",
            "description",
            "iconUrl",
            "tvTypes",
            "authors",
        ] {
            if let Some(v) = plugin.get(key) {
                obj.insert(key.to_string(), v.clone());
            }
        }
        self.sources.push(Value::Object(obj));
    }

    fn collect_cs3_urls(&mut self, url: &str) {
        if self.cs3_inspected >= 8 {
            return;
        }
        let bytes = match addons::http_get_bytes(url, MAX_CS3_BYTES) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.push_error(format!("{url}: {e}"));
                return;
            }
        };
        self.cs3_inspected += 1;
        for discovered in extract_urls_from_cs3(&bytes) {
            if looks_like_cloudstream_index_url(&discovered) {
                self.collect_json_url(&discovered);
            }
        }
    }

    fn push_error(&mut self, error: String) {
        if self.errors.len() < 5 {
            self.errors.push(error);
        }
    }
}

fn repo_database_url(value: &Value) -> Option<String> {
    let url = if let Some(url) = value.as_str() {
        url
    } else {
        let obj = value.as_object()?;
        if obj
            .keys()
            .any(|key| key.as_str() != "url" && key.as_str() != "verified")
        {
            return None;
        }
        obj.get("url").and_then(|u| u.as_str())?
    };
    (!looks_like_cs3_url(url)).then(|| url.to_string())
}

fn is_cloudstream_repo_database(value: &Value) -> bool {
    value.as_array().is_some_and(|arr| {
        !arr.is_empty()
            && arr.iter().all(|entry| {
                repo_database_url(entry).is_some_and(|url| {
                    let lower = url.to_ascii_lowercase();
                    lower.contains("repo") || lower.ends_with(".json")
                })
            })
            && !arr.iter().any(|entry| entry.get("apiVersion").is_some())
    })
}

fn looks_like_repository_expander(plugin: &Value) -> bool {
    let text = ["name", "internalName", "description"]
        .iter()
        .filter_map(|key| plugin.get(*key).and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    text.contains("megaprovider")
        || (text.contains("repositories") && text.contains("add"))
        || text.contains("all repositories")
}

fn extract_urls_from_cs3(bytes: &[u8]) -> Vec<String> {
    let mut urls = extract_http_urls_from_bytes(bytes);
    if let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(bytes)) {
        for i in 0..archive.len() {
            let Ok(mut file) = archive.by_index(i) else {
                continue;
            };
            if file.size() > MAX_CS3_BYTES {
                continue;
            }
            let name = file.name().to_ascii_lowercase();
            if !(name.ends_with(".json") || name.ends_with(".dex") || name.ends_with(".txt")) {
                continue;
            }
            let mut buf = Vec::new();
            if file.read_to_end(&mut buf).is_ok() {
                urls.extend(extract_http_urls_from_bytes(&buf));
            }
        }
    }
    urls.sort();
    urls.dedup();
    urls
}

fn extract_http_urls_from_bytes(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut urls = Vec::new();
    let mut start = 0;
    while let Some(pos) = text[start..].find("http") {
        let i = start + pos;
        let rest = &text[i..];
        if !(rest.starts_with("http://") || rest.starts_with("https://")) {
            start = i + 4;
            continue;
        }
        let end = rest
            .char_indices()
            .find_map(|(idx, ch)| {
                (ch.is_whitespace()
                    || ch.is_control()
                    || matches!(
                        ch,
                        '"' | '\'' | '<' | '>' | '\\' | '{' | '}' | '[' | ']' | ')' | '(' | ','
                    ))
                .then_some(idx)
            })
            .unwrap_or(rest.len());
        let url = rest[..end].trim_end_matches(['.', ';', ':']).to_string();
        if url.len() <= MAX_URL_LEN {
            urls.push(url);
        }
        start = i + end.max(4);
    }
    urls
}

fn looks_like_cloudstream_index_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("repos-db")
        || lower.contains("plugins.json")
        || lower.contains("repo.json")
        || lower.ends_with("/repo")
}

/// Recognises the CloudStream plugin-list JSON (the format at a repo's
/// `plugins.json`): an array whose entries point to `.cs3` plugin files or
/// carry CloudStream's plugin fields (`internalName`, `apiVersion`).
fn is_cs3_plugin_list(value: &Value) -> bool {
    value.as_array().is_some_and(|arr| {
        !arr.is_empty()
            && arr.iter().any(|e| {
                e.get("url")
                    .and_then(|u| u.as_str())
                    .is_some_and(looks_like_cs3_url)
                    || e.get("internalName").is_some()
                    || e.get("apiVersion").is_some()
            })
    })
}

/// Alternate spellings accepted for the movie / series URL templates, so a
/// manifest written for a slightly different convention still works.
const MOVIE_KEYS: &[&str] = &["movie", "movieUrl", "movie_url", "movieUrlTemplate"];
const SERIES_KEYS: &[&str] = &["series", "tv", "seriesUrl", "series_url", "tv_url", "show"];
/// A single template that serves both (a generic `url`/`template`).
const GENERIC_KEYS: &[&str] = &["url", "template", "urlTemplate"];
/// CloudStream repository entries point at compiled `.cs3` packages.
const CS3_KEYS: &[&str] = &["cs3", "pluginUrl", "plugin_url", "packageUrl"];

/// First non-empty string among `keys` on `s`.
fn first_template(s: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| s.get(*k).and_then(|v| v.as_str()))
        .filter(|t| !t.is_empty())
        .map(String::from)
}

fn looks_like_cs3_url(url: &str) -> bool {
    url.split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase()
        .ends_with(".cs3")
}

fn cs3_url(s: &Value) -> Option<String> {
    first_template(s, CS3_KEYS)
        .or_else(|| first_template(s, &["url"]).filter(|url| looks_like_cs3_url(url)))
}

/// Does this JSON value look like a stream source (declares some URL template)?
/// Used to accept a bare array / single object of sources without a wrapper.
fn looks_like_source(v: &Value) -> bool {
    !v.is_string()
        && (first_template(v, MOVIE_KEYS).is_some()
            || first_template(v, SERIES_KEYS).is_some()
            || first_template(v, GENERIC_KEYS).is_some()
            || cs3_url(v).is_some())
}

/// Parses the `sources` array of a manifest into usable providers. A source is
/// kept only if it declares at least one template (movie or series). Accepts
/// several key spellings (`movie`/`movieUrl`/…, `series`/`tv`/…) and a generic
/// `url`/`template` that serves movies, and series too when it references a
/// `{season}`/`{episode}` placeholder.
fn parse_sources(manifest: &Value) -> Vec<SourceDef> {
    let Some(sources) = manifest.get("sources").and_then(|s| s.as_array()) else {
        return Vec::new();
    };
    sources
        .iter()
        .filter_map(|s| {
            let cs3 = cs3_url(s);
            let generic = first_template(s, GENERIC_KEYS).filter(|url| !looks_like_cs3_url(url));
            let movie = first_template(s, MOVIE_KEYS).or_else(|| generic.clone());
            let series = first_template(s, SERIES_KEYS).or_else(|| {
                // A generic template is a series template only if it can vary by
                // episode — otherwise it's a movies-only source.
                generic
                    .as_ref()
                    .filter(|t| t.contains("{season}") || t.contains("{episode}"))
                    .cloned()
            });
            if movie.is_none() && series.is_none() {
                if cs3.is_none() {
                    return None; // nothing playable or importable
                }
            }
            let format = match s.get("format").and_then(|f| f.as_str()) {
                Some("stremio") => Format::Stremio,
                Some("cs3") | Some("cloudstream-plugin") => Format::Cs3,
                _ if cs3.is_some() && movie.is_none() && series.is_none() => Format::Cs3,
                _ => Format::Cloudstream,
            };
            Some(SourceDef {
                name: s
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("Source")
                    .to_string(),
                movie,
                series,
                plugin: cs3,
                format,
            })
        })
        .collect()
}

/// Parses a CloudStream-style link response: either a bare array of link
/// objects or an object wrapping one under `links` / `sources` / `streams`.
fn parse_cloudstream(json: &str, source_name: &str) -> Vec<Stream> {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let links = value
        .as_array()
        .cloned()
        .or_else(|| {
            ["links", "sources", "streams"]
                .iter()
                .find_map(|k| value.get(k).and_then(|v| v.as_array()).cloned())
        })
        .unwrap_or_default();

    links
        .iter()
        .filter_map(|l| parse_link(l, source_name))
        .collect()
}

fn parse_link(v: &Value, source_name: &str) -> Option<Stream> {
    let str_of = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| v.get(*k).and_then(|x| x.as_str()))
            .map(String::from)
    };
    let url = str_of(&["url", "link", "file"]).filter(|u| !u.is_empty())?;

    let quality = v
        .get("quality")
        .and_then(|q| {
            q.as_str()
                .map(String::from)
                .or_else(|| q.as_i64().map(|n| format!("{n}p")))
        })
        .filter(|q| !q.is_empty());

    let provider =
        str_of(&["name", "source", "provider"]).unwrap_or_else(|| source_name.to_string());
    let name = match &quality {
        Some(q) => format!("{provider} · {q}"),
        None => provider,
    };

    let kind = if url.starts_with("magnet:") {
        StreamKind::Torrent
    } else {
        StreamKind::Direct
    };

    Some(Stream {
        kind,
        url,
        name,
        title: str_of(&["title", "description"]).unwrap_or_default(),
        file_idx: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"{
        "name": "Community Sources",
        "sources": [
            {"name": "VidSrc",
             "movie": "https://host/m/{imdb}",
             "series": "https://host/s/{imdb}/{season}/{episode}",
             "format": "cloudstream"},
            {"name": "Mirror",
             "movie": "https://host/stream/{type}/{id}.json",
             "format": "stremio"},
            {"name": "SeriesOnly",
             "series": "https://host/tv/{imdb}/{season}/{episode}"},
            {"name": "NoTemplate"}
        ]
    }"#;

    fn manifest() -> Value {
        serde_json::from_str(MANIFEST).unwrap()
    }

    #[test]
    fn parses_sources_and_drops_templateless() {
        let defs = parse_sources(&manifest());
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        // "NoTemplate" has neither movie nor series → dropped.
        assert_eq!(names, vec!["VidSrc", "Mirror", "SeriesOnly"]);
        let mirror = defs.iter().find(|d| d.name == "Mirror").unwrap();
        assert!(mirror.format == Format::Stremio);
        assert!(mirror.series.is_none()); // movie-only
        let so = defs.iter().find(|d| d.name == "SeriesOnly").unwrap();
        assert!(so.movie.is_none());
    }

    #[test]
    fn summarize_reports_series_capability_and_enable_state() {
        let states = reconcile_states(&parse_sources(&manifest()), None);
        let stored = Stored {
            id: "u".into(),
            source_url: None,
            manifest: manifest(),
            states,
        };
        let m = summarize(&stored).unwrap();
        assert_eq!(m.name, "Community Sources");
        let vidsrc = m.sources.iter().find(|s| s.name == "VidSrc").unwrap();
        assert!(vidsrc.enabled && vidsrc.series);
        let mirror = m.sources.iter().find(|s| s.name == "Mirror").unwrap();
        assert!(!mirror.series); // movie-only
    }

    #[test]
    fn reconcile_carries_over_enable_and_reachability() {
        let prior = vec![SourceState {
            name: "VidSrc".into(),
            enabled: false,
            reachable: Some(true),
            latency_ms: Some(42),
        }];
        let states = reconcile_states(&parse_sources(&manifest()), Some(&prior));
        let vidsrc = states.iter().find(|s| s.name == "VidSrc").unwrap();
        assert!(!vidsrc.enabled); // carried over
        assert_eq!(vidsrc.latency_ms, Some(42));
        // A source with no prior state defaults to enabled.
        let mirror = states.iter().find(|s| s.name == "Mirror").unwrap();
        assert!(mirror.enabled && mirror.reachable.is_none());
    }

    #[test]
    fn migrates_old_tuple_store_format() {
        let old = format!(r#"[["https://x/manifest.json", {MANIFEST}]]"#);
        let stored = parse_store(&old);
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, "https://x/manifest.json");
        assert_eq!(
            stored[0].source_url.as_deref(),
            Some("https://x/manifest.json")
        );
        // Every migrated source starts enabled.
        assert!(stored[0].states.iter().all(|s| s.enabled));
        assert_eq!(stored[0].states.len(), 3);
    }

    #[test]
    fn probe_url_prefers_movie_then_series() {
        let defs = parse_sources(&manifest());
        let mirror = defs.iter().find(|d| d.name == "Mirror").unwrap();
        assert_eq!(
            probe_url(mirror).unwrap(),
            "https://host/stream/movie/tt0111161.json"
        );
        let so = defs.iter().find(|d| d.name == "SeriesOnly").unwrap();
        assert_eq!(probe_url(so).unwrap(), "https://host/tv/tt0944947/1/1");
    }

    #[test]
    fn fills_movie_and_episode_templates() {
        let movie = TemplateCtx::new("movie", "tt0133093");
        assert_eq!(movie.fill("https://h/m/{imdb}"), "https://h/m/tt0133093");
        assert_eq!(
            movie.fill("https://h/{type}/{id}.json"),
            "https://h/movie/tt0133093.json"
        );

        let ep = TemplateCtx::new("series", "tt0944947:2:5");
        assert_eq!(
            ep.fill("https://h/s/{imdb}/{season}/{episode}"),
            "https://h/s/tt0944947/2/5"
        );
        assert_eq!(ep.imdb, "tt0944947");
    }

    #[test]
    fn parses_cloudstream_array_and_wrapped_forms() {
        let arr = r#"[
            {"name": "CDN", "url": "https://cdn/x.m3u8", "quality": 1080},
            {"source": "Alt", "link": "https://alt/y.mp4", "quality": "720p"},
            {"url": "magnet:?xt=urn:btih:ABC"},
            {"name": "bad"}
        ]"#;
        let s = parse_cloudstream(arr, "Prov");
        assert_eq!(s.len(), 3); // url-less "bad" entry dropped
        assert_eq!(s[0].name, "CDN · 1080p");
        assert_eq!(s[0].kind, StreamKind::Direct);
        assert_eq!(s[1].name, "Alt · 720p");
        assert_eq!(s[2].name, "Prov");
        assert_eq!(s[2].kind, StreamKind::Torrent);

        let wrapped = r#"{"links": [{"url": "https://cdn/z.mp4", "name": "Z"}]}"#;
        let w = parse_cloudstream(wrapped, "Prov");
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].name, "Z");
    }

    #[test]
    fn garbage_and_empty_are_safe() {
        assert!(parse_cloudstream("oops", "P").is_empty());
        assert!(parse_sources(&serde_json::json!({"name": "x"})).is_empty());
        assert!(parse_store("nonsense").is_empty());
    }

    #[test]
    fn imports_cloudstream_cs3_plugin_list_as_metadata() {
        let plugins = r#"[
            {"apiVersion": 1, "repositoryUrl": "https://github.com/recloudstream/extensions",
             "status": 1, "version": 4, "internalName": "DailymotionProvider",
             "url": "https://raw.githubusercontent.com/recloudstream/extensions/builds/DailymotionProvider.cs3",
             "name": "DailymotionProvider"}
        ]"#;
        let value: Value = serde_json::from_str(plugins).unwrap();
        assert!(is_cs3_plugin_list(&value));
        let imported = interpret_manifest(&value).unwrap();
        let defs = parse_sources(&imported);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "DailymotionProvider");
        assert!(!defs[0].playable());
        assert!(defs[0].format == Format::Cs3);

        // A genuine source manifest still passes through.
        assert!(interpret_manifest(&manifest()).is_ok());
        // A plain unrelated array is rejected but not mislabelled as .cs3.
        let plain = serde_json::json!([{"foo": "bar"}]);
        assert!(!is_cs3_plugin_list(&plain));
        assert!(interpret_manifest(&plain).is_err());
    }

    #[test]
    fn extracts_repository_urls_from_cs3_bytes() {
        let bytes = b"classes.dex\0https://raw.githubusercontent.com/recloudstream/cs-repos/master/repos-db.json\0";
        let urls = extract_http_urls_from_bytes(bytes);
        assert_eq!(
            urls,
            vec!["https://raw.githubusercontent.com/recloudstream/cs-repos/master/repos-db.json"]
        );
        assert!(looks_like_cloudstream_index_url(&urls[0]));
    }

    #[test]
    fn accepts_lenient_source_manifest_shapes() {
        // Bare array of source objects (no wrapper).
        let bare = serde_json::json!([{ "name": "A", "movie": "https://h/m/{imdb}" }]);
        assert_eq!(parse_sources(&interpret_manifest(&bare).unwrap()).len(), 1);

        // Single source object (no wrapper).
        let single = serde_json::json!({ "name": "Solo", "series": "https://h/s/{imdb}/{season}/{episode}" });
        let defs = parse_sources(&interpret_manifest(&single).unwrap());
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "Solo");
        assert!(defs[0].series.is_some() && defs[0].movie.is_none());

        // Alternate container key ("providers") + alternate template keys.
        let alt = serde_json::json!({
            "name": "Alt",
            "providers": [{
                "name": "P",
                "movieUrl": "https://h/m/{imdb}",
                "tv": "https://h/t/{imdb}/{season}/{episode}"
            }]
        });
        let defs = parse_sources(&interpret_manifest(&alt).unwrap());
        assert_eq!(defs.len(), 1);
        assert!(defs[0].movie.is_some() && defs[0].series.is_some());

        // A generic `url` serves movies; it's a series template only when it can
        // vary by episode.
        let generic = serde_json::json!({ "sources": [{ "name": "G", "url": "https://h/{type}/{id}.json" }] });
        let defs = parse_sources(&interpret_manifest(&generic).unwrap());
        assert!(defs[0].movie.is_some() && defs[0].series.is_none());

        let generic_ep = serde_json::json!({ "sources": [
            { "name": "G", "url": "https://h/{type}/{imdb}/{season}/{episode}" }
        ] });
        let defs = parse_sources(&interpret_manifest(&generic_ep).unwrap());
        assert!(defs[0].series.is_some());
    }
}
