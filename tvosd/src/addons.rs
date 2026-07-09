//! Stremio-compatible addon management.
//!
//! An addon is an HTTP service described by a manifest.json (the open Stremio
//! addon protocol — stremio.github.io/stremio-addon-sdk). We use two of its
//! resources: `catalog` (browse rows) and `stream` (resolve something to
//! playable URLs). Installed addons are persisted with their manifest in
//! ~/.config/tvos/addons.json, so the home screen works offline at boot.

use std::net::{IpAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use crate::settings::config_dir;

/// One installed addon, distilled from its manifest.
#[derive(Serialize, Clone)]
pub struct Addon {
    /// The manifest URL the user installed (unique key).
    pub url: String,
    /// Resource base: the manifest URL without "/manifest.json".
    pub base: String,
    pub name: String,
    pub catalogs: Vec<Catalog>,
    /// True if the addon serves the `stream` resource.
    pub streams: bool,
    /// True if the addon serves the `meta` resource (e.g. Cinemeta).
    pub meta: bool,
    /// A /configure page, if the addon advertises one (debrid keys, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configure_url: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct Catalog {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub name: String,
    /// Fetchable as a plain row (no required extra parameters).
    pub browse: bool,
    /// Supports the `search` extra — usable for catalog search.
    pub search: bool,
}

pub static STORE: LazyLock<AddonStore> = LazyLock::new(AddonStore::load);

pub struct AddonStore {
    path: PathBuf,
    addons: Mutex<Vec<(String, Value)>>, // (manifest url, manifest json)
}

impl AddonStore {
    fn load() -> Self {
        let path = config_dir().join("addons.json");
        let addons = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Vec<(String, Value)>>(&text).ok())
            .unwrap_or_default();
        Self {
            path,
            addons: Mutex::new(addons),
        }
    }

    pub fn list(&self) -> Vec<Addon> {
        self.addons
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(|(url, manifest)| parse_manifest(url, manifest))
            .collect()
    }

    /// Fetches and validates a manifest, then installs (or updates) the addon.
    pub fn install(&self, manifest_url: &str) -> Result<Addon, String> {
        if manifest_url.len() > MAX_URL_LEN {
            return Err("addon URL is too long".to_string());
        }
        if !manifest_url.ends_with("/manifest.json") {
            return Err("addon URL must end in /manifest.json".to_string());
        }
        // SSRF guard: only fetch from public hosts (localhost allowed for dev).
        validate_fetch_url(manifest_url)?;
        let text = http_get(manifest_url)?;
        let manifest: Value =
            serde_json::from_str(&text).map_err(|e| format!("manifest is not JSON: {e}"))?;
        let addon = parse_manifest(manifest_url, &manifest)
            .ok_or("manifest is missing required fields (id, name)")?;

        let mut addons = self.addons.lock().unwrap_or_else(|e| e.into_inner());
        addons.retain(|(url, _)| url != manifest_url);
        addons.push((manifest_url.to_string(), manifest));
        self.persist(&addons)
            .map_err(|e| format!("could not save addons: {e}"))?;
        Ok(addon)
    }

    pub fn remove(&self, manifest_url: &str) -> Result<(), String> {
        let mut addons = self.addons.lock().unwrap_or_else(|e| e.into_inner());
        let before = addons.len();
        addons.retain(|(url, _)| url != manifest_url);
        if addons.len() == before {
            return Err("no such addon installed".to_string());
        }
        self.persist(&addons)
            .map_err(|e| format!("could not save addons: {e}"))
    }

    fn persist(&self, addons: &[(String, Value)]) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(addons).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, json).map_err(|e| e.to_string())
    }
}

pub fn http_get(url: &str) -> Result<String, String> {
    http_get_within(url, Duration::from_secs(10))
}

/// Short-budget GET for latency-sensitive paths (search): one dead addon
/// must not stall the whole merged result.
pub fn http_get_quick(url: &str) -> Result<String, String> {
    http_get_within(url, Duration::from_secs(4))
}

fn http_get_within(url: &str, timeout: Duration) -> Result<String, String> {
    validate_fetch_url(url)?;
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        // Cap redirects so a public URL can't bounce us into a private host.
        .redirect(reqwest::redirect::Policy::limited(3))
        // Several public APIs (CheapShark among them) reject requests with
        // no User-Agent, which is reqwest's default.
        .user_agent(concat!("tvos/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("request failed: {}", crate::util::scrub_secrets(&e.to_string())))?
        .text()
        .map_err(|e| e.to_string())
}

/// Reachability probe for a source URL: a short GET that treats *any* HTTP
/// response below 500 as "reachable" (a 404 for a probe title still means the
/// host is up and serving), and a network error / timeout / 5xx as unreachable.
/// Returns the round-trip latency in milliseconds when reachable. SSRF-guarded
/// like every other outbound fetch.
pub fn probe(url: &str) -> Option<u64> {
    if validate_fetch_url(url).is_err() {
        return None;
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(6))
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent(concat!("tvos/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    let start = Instant::now();
    let resp = client.get(url).send().ok()?;
    let ms = start.elapsed().as_millis() as u64;
    (resp.status().as_u16() < 500).then_some(ms)
}

/// Upper bound on any user-supplied URL we accept (addon manifests, play URLs).
pub const MAX_URL_LEN: usize = 2048;

/// SSRF guard for outbound fetches. Requires https (http only when the host is
/// an explicit localhost dev target) and refuses any URL whose host resolves to
/// a private, loopback, or link-local address — except that localhost dev case.
///
/// NB: this is a best-effort check at request time; DNS can still be re-resolved
/// to a different IP by reqwest (TOCTOU). Combined with the redirect cap it
/// blocks the common metadata-endpoint / internal-service SSRF vectors.
fn validate_fetch_url(url: &str) -> Result<(), String> {
    if url.len() > MAX_URL_LEN {
        return Err("URL is too long".to_string());
    }
    let (scheme, rest) = url
        .split_once("://")
        .ok_or("URL must include a scheme (http/https)")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(format!("unsupported URL scheme: {scheme}"));
    }
    // authority = everything up to the first '/', '?' or '#'.
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    // Drop any userinfo, then split host[:port].
    let hostport = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(stripped) = hostport.strip_prefix('[') {
        // IPv6 literal: [::1]:port
        stripped.split(']').next().unwrap_or(stripped)
    } else {
        hostport.split(':').next().unwrap_or(hostport)
    };
    if host.is_empty() {
        return Err("URL has no host".to_string());
    }

    let is_localhost_name = host.eq_ignore_ascii_case("localhost");
    // Resolve the host to its IPs (a literal IP resolves to itself).
    let addrs: Vec<IpAddr> = (host, 0u16)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve host {host}: {e}"))?
        .map(|s| s.ip())
        .collect();
    if addrs.is_empty() {
        return Err(format!("host {host} did not resolve"));
    }

    let all_loopback = addrs.iter().all(is_loopback);
    let is_dev_localhost = all_loopback && (is_localhost_name || host.parse::<IpAddr>().is_ok());

    if scheme == "http" && !is_dev_localhost {
        return Err("plain http is only allowed for localhost; use https".to_string());
    }
    for ip in &addrs {
        if is_private_ip(ip) && !is_dev_localhost {
            return Err(format!(
                "refusing to fetch {host}: resolves to a non-public address"
            ));
        }
    }
    Ok(())
}

fn is_loopback(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Whether an IP is in a private / loopback / link-local / reserved range that
/// a public-internet fetch should never reach.
fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                // Carrier-grade NAT 100.64.0.0/10.
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // Unique local fc00::/7.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // Link-local fe80::/10.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // IPv4-mapped: check the embedded v4.
                || v6.to_ipv4_mapped().map(|m| is_private_ip(&IpAddr::V4(m))).unwrap_or(false)
        }
    }
}

/// Distills a Stremio manifest. Tolerant of both resource forms the protocol
/// allows: `"stream"` and `{"name": "stream", "types": […]}`. Catalogs that
/// require extra parameters (e.g. a mandatory genre) are skipped — we can't
/// supply them from a top-level row.
fn parse_manifest(url: &str, manifest: &Value) -> Option<Addon> {
    manifest.get("id")?.as_str()?;
    let name = manifest.get("name")?.as_str()?.to_string();

    let has_resource = |name: &str| {
        manifest
            .get("resources")
            .and_then(|r| r.as_array())
            .is_some_and(|resources| {
                resources.iter().any(|r| {
                    r.as_str() == Some(name) || r.get("name").and_then(|n| n.as_str()) == Some(name)
                })
            })
    };
    let streams = has_resource("stream");
    let meta = has_resource("meta");

    // A configurable addon advertises behaviorHints.configurable and serves a
    // /configure page where the user sets options (e.g. debrid keys).
    let base = url.trim_end_matches("/manifest.json").to_string();
    let configurable = manifest
        .get("behaviorHints")
        .and_then(|h| h.get("configurable"))
        .and_then(|c| c.as_bool())
        .unwrap_or(false);
    let configure_url = configurable.then(|| format!("{base}/configure"));

    let catalogs = manifest
        .get("catalogs")
        .and_then(|c| c.as_array())
        .map(|catalogs| {
            catalogs
                .iter()
                .filter_map(|c| {
                    let browse = !requires_extra(c);
                    let search = supports_search(c);
                    // A catalog we can neither browse nor search is useless to us.
                    if !browse && !search {
                        return None;
                    }
                    Some(Catalog {
                        kind: c.get("type")?.as_str()?.to_string(),
                        id: c.get("id")?.as_str()?.to_string(),
                        name: c
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("Catalog")
                            .to_string(),
                        browse,
                        search,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Some(Addon {
        url: url.to_string(),
        base,
        name,
        catalogs,
        streams,
        meta,
        configure_url,
    })
}

fn requires_extra(catalog: &Value) -> bool {
    catalog
        .get("extra")
        .and_then(|e| e.as_array())
        .is_some_and(|extras| {
            extras
                .iter()
                .any(|e| e.get("isRequired").and_then(|r| r.as_bool()) == Some(true))
        })
        // Legacy manifest form.
        || catalog
            .get("extraRequired")
            .and_then(|e| e.as_array())
            .is_some_and(|extras| !extras.is_empty())
}

/// Whether the catalog accepts the `search` extra (either manifest form).
fn supports_search(catalog: &Value) -> bool {
    let names_search = |v: &Value| v.as_str() == Some("search")
        || v.get("name").and_then(|n| n.as_str()) == Some("search");
    ["extra", "extraSupported", "extraRequired"].iter().any(|key| {
        catalog
            .get(key)
            .and_then(|e| e.as_array())
            .is_some_and(|extras| extras.iter().any(names_search))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"{
        "id": "org.example.films",
        "name": "Example Films",
        "resources": ["catalog", {"name": "stream", "types": ["movie"]}],
        "types": ["movie"],
        "catalogs": [
            {"type": "movie", "id": "top", "name": "Top Films",
             "extra": [{"name": "search", "isRequired": false}]},
            {"type": "movie", "id": "bygenre", "name": "By Genre",
             "extra": [{"name": "genre", "isRequired": true}]},
            {"type": "movie", "id": "searchonly", "name": "Lookup",
             "extra": [{"name": "search", "isRequired": true}]}
        ]
    }"#;

    #[test]
    fn parses_manifest_catalogs_with_browse_and_search_flags() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        let addon = parse_manifest("https://x.example/manifest.json", &manifest).unwrap();
        assert_eq!(addon.name, "Example Films");
        assert_eq!(addon.base, "https://x.example");
        assert!(addon.streams);
        // "bygenre" needs a mandatory genre and can't search — dropped.
        assert_eq!(addon.catalogs.len(), 2);
        assert!(addon.catalogs[0].browse && addon.catalogs[0].search);
        assert_eq!(addon.catalogs[0].id, "top");
        // Search-only catalogs are kept for search but not browsed as rows.
        assert_eq!(addon.catalogs[1].id, "searchonly");
        assert!(!addon.catalogs[1].browse && addon.catalogs[1].search);
    }

    #[test]
    fn legacy_extra_supported_marks_searchable() {
        let manifest: Value = serde_json::from_str(
            r#"{"id": "a", "name": "Legacy", "resources": ["catalog"],
                "catalogs": [{"type": "movie", "id": "top", "name": "Top",
                              "extraSupported": ["search", "genre"]}]}"#,
        )
        .unwrap();
        let addon = parse_manifest("https://x/manifest.json", &manifest).unwrap();
        assert!(addon.catalogs[0].browse && addon.catalogs[0].search);
    }

    #[test]
    fn manifest_without_stream_resource() {
        let manifest: Value = serde_json::from_str(
            r#"{"id": "a", "name": "Catalog Only", "resources": ["catalog"], "catalogs": []}"#,
        )
        .unwrap();
        let addon = parse_manifest("https://x/manifest.json", &manifest).unwrap();
        assert!(!addon.streams);
    }

    #[test]
    fn invalid_manifest_is_rejected() {
        let manifest: Value = serde_json::from_str(r#"{"no": "fields"}"#).unwrap();
        assert!(parse_manifest("https://x/manifest.json", &manifest).is_none());
    }

    #[test]
    fn ssrf_guard_blocks_private_and_bad_schemes() {
        // Loopback literals are only reachable as the explicit dev case (http).
        assert!(validate_fetch_url("http://127.0.0.1:8484/manifest.json").is_ok());
        assert!(validate_fetch_url("http://localhost:8484/manifest.json").is_ok());
        // Private ranges are refused.
        assert!(validate_fetch_url("http://192.168.1.10/manifest.json").is_err());
        assert!(validate_fetch_url("https://10.0.0.5/manifest.json").is_err());
        assert!(validate_fetch_url("http://169.254.169.254/latest/meta-data").is_err());
        // Non-http schemes are refused.
        assert!(validate_fetch_url("file:///etc/passwd").is_err());
        assert!(validate_fetch_url("ftp://example.com/x").is_err());
        // Plain http to a public host is refused (https required).
        assert!(validate_fetch_url("http://example.com/manifest.json").is_err());
    }
}
