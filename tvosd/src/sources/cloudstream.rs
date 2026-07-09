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

use std::collections::HashSet;
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
    format: Format,
}

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Stremio,
    Cloudstream,
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
            .ok_or("manifest needs a \"name\" field")?;
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
                .map(|t| {
                    scope.spawn(move || (t.id.clone(), t.name.clone(), addons::probe(&t.url)))
                })
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
/// returns a specific, actionable error. The common mistake is pasting a
/// CloudStream *plugin repository* (a list of compiled `.cs3` plugins) — we
/// detect that and say so, instead of a confusing "no sources" message.
fn interpret_manifest(value: &Value) -> Result<Value, String> {
    // A real source manifest is an object with a "sources" array.
    if value.get("sources").and_then(|s| s.as_array()).is_some() {
        return Ok(value.clone());
    }
    if is_cs3_plugin_list(value) {
        return Err(
            "This is a CloudStream plugin repository — a list of compiled .cs3 plugins \
             (Android/Kotlin extensions) that this app cannot run. Add a source manifest \
             instead: a JSON object with a \"sources\" array, where each source gives a \
             \"movie\"/\"series\" URL template that returns stream links."
                .to_string(),
        );
    }
    Err(
        "not a source manifest — expected a JSON object with a \"sources\" array \
         (each source: a \"movie\"/\"series\" URL template). See the format in Settings."
            .to_string(),
    )
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
                    .is_some_and(|u| u.ends_with(".cs3"))
                    || e.get("internalName").is_some()
                    || e.get("apiVersion").is_some()
            })
    })
}

/// Parses the `sources` array of a manifest into usable providers. A source is
/// kept only if it declares at least one template (movie or series).
fn parse_sources(manifest: &Value) -> Vec<SourceDef> {
    let Some(sources) = manifest.get("sources").and_then(|s| s.as_array()) else {
        return Vec::new();
    };
    sources
        .iter()
        .filter_map(|s| {
            let template = |key: &str| {
                s.get(key)
                    .and_then(|v| v.as_str())
                    .filter(|t| !t.is_empty())
                    .map(String::from)
            };
            let movie = template("movie");
            let series = template("series");
            if movie.is_none() && series.is_none() {
                return None; // nothing playable
            }
            let format = match s.get("format").and_then(|f| f.as_str()) {
                Some("stremio") => Format::Stremio,
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
        assert_eq!(stored[0].source_url.as_deref(), Some("https://x/manifest.json"));
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
    fn rejects_cloudstream_cs3_plugin_repo_with_clear_error() {
        // The real MegaRepo plugin list the user pasted.
        let mega = r#"[
            {"apiVersion": 1, "repositoryUrl": "https://github.com/self-similarity/MegaRepo",
             "status": 1, "version": 2, "internalName": "MegaProvider",
             "url": "https://raw.githubusercontent.com/self-similarity/MegaRepo/builds/MegaProvider.cs3",
             "name": "MegaProvider"}
        ]"#;
        let value: Value = serde_json::from_str(mega).unwrap();
        assert!(is_cs3_plugin_list(&value));
        let err = interpret_manifest(&value).unwrap_err();
        assert!(err.contains(".cs3"), "error should name the .cs3 problem: {err}");

        // A genuine source manifest still passes through.
        assert!(interpret_manifest(&manifest()).is_ok());
        // A plain unrelated array is rejected but not mislabelled as .cs3.
        let plain = serde_json::json!([{"foo": "bar"}]);
        assert!(!is_cs3_plugin_list(&plain));
        assert!(interpret_manifest(&plain).is_err());
    }
}
