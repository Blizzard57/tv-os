//! Lists installed Steam games by reading Steam's own on-disk metadata:
//!
//! 1. Find the Steam root (native or flatpak install).
//! 2. Read `steamapps/libraryfolders.vdf` to find every library folder.
//! 3. Each installed game has a `steamapps/appmanifest_<appid>.acf` file
//!    containing its appid and display name.
//!
//! Both files are Valve's KeyValues ("VDF") format. We only ever need flat
//! `"key" "value"` string pairs out of them, so a full VDF parser is overkill.
//!
//! With a Steam Web API key + SteamID in settings (the Settings panel), the
//! source also lists the account's *entire owned library* via the Web API,
//! merged with the installed games. Launching any of them uses steam://, so
//! an owned-but-not-installed game prompts Steam to install it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::model::{Action, ContentItem, Kind, Row};
use crate::sources::Source;
use crate::util::percent_encode;
use crate::{addons, launcher, settings};

const OWNED_TTL: Duration = Duration::from_secs(300);

#[derive(Default)]
pub struct Steam {
    /// Owned library cached per (api_key, resolved steamid); refetched when
    /// those change or the TTL lapses.
    owned: Mutex<Option<OwnedCache>>,
}

struct OwnedCache {
    creds: (String, String),
    at: Instant,
    items: Vec<ContentItem>,
}

impl Steam {
    pub fn new() -> Self {
        Self::default()
    }

    fn owned_games(&self) -> Vec<ContentItem> {
        let s = settings::STORE.get();
        if s.steam_api_key.is_empty() || s.steam_id.is_empty() {
            return Vec::new();
        }
        let Some(steamid) = resolve_steamid(&s.steam_api_key, &s.steam_id) else {
            return Vec::new();
        };
        let creds = (s.steam_api_key.clone(), steamid.clone());

        let mut cache = self.owned.lock().unwrap();
        if let Some(c) = cache.as_ref() {
            if c.creds == creds && c.at.elapsed() < OWNED_TTL {
                return c.items.clone();
            }
        }
        let items = fetch_owned(&creds.0, &creds.1).unwrap_or_default();
        if !items.is_empty() {
            *cache = Some(OwnedCache {
                creds,
                at: Instant::now(),
                items: items.clone(),
            });
        }
        items
    }
}

impl Source for Steam {
    fn id(&self) -> &'static str {
        "steam"
    }

    fn available(&self) -> bool {
        steam_root().is_some() || {
            let s = settings::STORE.get();
            !s.steam_api_key.is_empty() && !s.steam_id.is_empty()
        }
    }

    fn rows(&self) -> Vec<Row> {
        // Installed games first, then everything else the account owns;
        // dedupe so an installed game isn't listed twice.
        let mut items = scan();
        for game in self.owned_games() {
            if !items.iter().any(|i| i.id == game.id) {
                items.push(game);
            }
        }
        items.sort_by_key(|item| item.title.to_lowercase());
        vec![Row {
            title: "Games".to_string(),
            items,
        }]
    }

    fn launch(&self, item_id: &str) -> Result<(), String> {
        let appid = item_id.strip_prefix("steam:").unwrap_or_default();
        let url = format!("steam://rungameid/{appid}");
        // Native install first, flatpak as fallback.
        let attempts: [(&str, Vec<&str>); 2] = [
            ("steam", vec![&url]),
            ("flatpak", vec!["run", "com.valvesoftware.Steam", &url]),
        ];
        for (program, args) in attempts {
            if launcher::spawn_detached(program, &args).is_ok() {
                return Ok(());
            }
        }
        Err("could not start Steam (tried native and flatpak)".to_string())
    }
}

/// Tests the saved Steam credentials, returning the owned-game count or a
/// human error. Used by the Settings panel's "Connect" button.
pub fn connection_test() -> Result<usize, String> {
    let s = settings::STORE.get();
    if s.steam_api_key.is_empty() {
        return Err("Enter your Steam Web API key".to_string());
    }
    if s.steam_id.is_empty() {
        return Err("Enter your SteamID or profile name".to_string());
    }
    let steamid = resolve_steamid(&s.steam_api_key, &s.steam_id)
        .ok_or("Could not resolve that SteamID / profile name")?;
    let games = fetch_owned(&s.steam_api_key, &steamid).map_err(|e| {
        format!("{e} — check the API key, and that the profile's game details are public")
    })?;
    Ok(games.len())
}

/// Accepts a SteamID64 as-is; otherwise treats the input as a vanity name and
/// resolves it via the Web API.
fn resolve_steamid(api_key: &str, input: &str) -> Option<String> {
    let trimmed = input.trim().trim_end_matches('/');
    let candidate = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if candidate.len() == 17 && candidate.chars().all(|c| c.is_ascii_digit()) {
        return Some(candidate.to_string());
    }
    let url = format!(
        "https://api.steampowered.com/ISteamUser/ResolveVanityURL/v1/?key={api_key}&vanityurl={}",
        percent_encode(candidate)
    );
    let json: Value = serde_json::from_str(&addons::http_get(&url).ok()?).ok()?;
    let response = json.get("response")?;
    (response.get("success")?.as_i64()? == 1)
        .then(|| response.get("steamid")?.as_str().map(String::from))
        .flatten()
}

fn fetch_owned(api_key: &str, steamid: &str) -> Result<Vec<ContentItem>, String> {
    let url = format!(
        "https://api.steampowered.com/IPlayerService/GetOwnedGames/v1/?key={api_key}\
         &steamid={steamid}&include_appinfo=1&include_played_free_games=1&format=json"
    );
    let json: Value = serde_json::from_str(&addons::http_get(&url)?)
        .map_err(|e| format!("Steam returned invalid data: {e}"))?;
    let games = json
        .get("response")
        .and_then(|r| r.get("games"))
        .and_then(|g| g.as_array())
        .ok_or("Steam returned no games — is the profile's game list public?")?;
    Ok(games.iter().filter_map(owned_item).collect())
}

fn owned_item(game: &Value) -> Option<ContentItem> {
    let appid = game.get("appid")?.as_i64()?;
    let name = game.get("name")?.as_str()?.to_string();
    Some(ContentItem {
        id: format!("steam:{appid}"),
        kind: Kind::Game,
        title: name,
        art: Some(art_url(appid)),
        action: Action::Play,
    })
}

/// Public Steam CDN poster art; the UI falls back to a placeholder on 404.
fn art_url(appid: impl std::fmt::Display) -> String {
    format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{appid}/library_600x900.jpg")
}

fn scan() -> Vec<ContentItem> {
    let Some(root) = steam_root() else {
        return Vec::new();
    };
    let mut items: Vec<ContentItem> = library_dirs(&root)
        .iter()
        .flat_map(|dir| games_in(dir))
        .collect();
    items.sort_by_key(|item| item.title.to_lowercase());
    items.dedup_by(|a, b| a.id == b.id); // same game installed in two libraries
    items
}

/// Steam install locations, in order of preference.
fn steam_root() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    [
        ".local/share/Steam",
        ".steam/steam",
        ".var/app/com.valvesoftware.Steam/.local/share/Steam", // flatpak
    ]
    .iter()
    .map(|rel| Path::new(&home).join(rel))
    .find(|p| p.join("steamapps").is_dir())
}

/// The root's own `steamapps` plus every extra library from libraryfolders.vdf.
fn library_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![root.join("steamapps")];
    let vdf = root.join("steamapps/libraryfolders.vdf");
    if let Ok(text) = fs::read_to_string(vdf) {
        for path in parse_library_folders(&text) {
            let dir = path.join("steamapps");
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

fn games_in(steamapps: &Path) -> Vec<ContentItem> {
    let Ok(entries) = fs::read_dir(steamapps) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("appmanifest_") && name.ends_with(".acf")
        })
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .filter_map(|text| parse_acf(&text))
        .filter(|(appid, name)| !is_hidden(appid, name))
        .map(|(appid, name)| ContentItem {
            id: format!("steam:{appid}"),
            kind: Kind::Game,
            title: name,
            art: Some(art_url(&appid)),
            action: Action::Play,
        })
        .collect()
}

/// Runtimes and redistributables that show up as "installed apps" but are not games.
fn is_hidden(appid: &str, name: &str) -> bool {
    appid == "228980" // Steamworks Common Redistributables
        || name.starts_with("Proton")
        || name.starts_with("Steam Linux Runtime")
}

/// Extracts `"path"` values from libraryfolders.vdf.
fn parse_library_folders(text: &str) -> Vec<PathBuf> {
    quoted_pairs(text)
        .filter(|(key, _)| *key == "path")
        .map(|(_, value)| PathBuf::from(value))
        .collect()
}

/// Extracts (appid, name) from an appmanifest .acf file.
fn parse_acf(text: &str) -> Option<(String, String)> {
    let mut appid = None;
    let mut name = None;
    for (key, value) in quoted_pairs(text) {
        match key {
            "appid" if appid.is_none() => appid = Some(value.to_string()),
            "name" if name.is_none() => name = Some(value.to_string()),
            _ => {}
        }
    }
    Some((appid?, name?))
}

/// Yields every `"key" "value"` pair found on a single line of VDF text.
/// Keys with nested-block values (no value on the line) are skipped.
fn quoted_pairs(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines().filter_map(|line| {
        let mut parts = line.split('"');
        let _before = parts.next()?;
        let key = parts.next()?;
        let _between = parts.next()?;
        let value = parts.next()?;
        Some((key, value))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACF: &str = r#"
"AppState"
{
	"appid"		"620"
	"Universe"		"1"
	"name"		"Portal 2"
	"StateFlags"		"4"
	"installdir"		"Portal 2"
}
"#;

    const LIBRARY_FOLDERS: &str = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"/home/user/.local/share/Steam"
		"label"		""
	}
	"1"
	{
		"path"		"/mnt/games/SteamLibrary"
		"label"		"big disk"
	}
}
"#;

    #[test]
    fn parses_acf_appid_and_name() {
        assert_eq!(
            parse_acf(ACF),
            Some(("620".to_string(), "Portal 2".to_string()))
        );
    }

    #[test]
    fn parses_all_library_paths() {
        assert_eq!(
            parse_library_folders(LIBRARY_FOLDERS),
            vec![
                PathBuf::from("/home/user/.local/share/Steam"),
                PathBuf::from("/mnt/games/SteamLibrary"),
            ]
        );
    }

    #[test]
    fn acf_without_name_is_rejected() {
        assert_eq!(parse_acf("\"appid\" \"42\""), None);
    }

    #[test]
    fn runtimes_are_hidden() {
        assert!(is_hidden("228980", "Steamworks Common Redistributables"));
        assert!(is_hidden("1628350", "Steam Linux Runtime 3.0 (sniper)"));
        assert!(is_hidden("2805730", "Proton 9.0"));
        assert!(!is_hidden("620", "Portal 2"));
    }

    #[test]
    fn owned_item_maps_appid_and_art() {
        let game = serde_json::json!({"appid": 620, "name": "Portal 2", "playtime_forever": 42});
        let item = owned_item(&game).unwrap();
        assert_eq!(item.id, "steam:620");
        assert_eq!(item.title, "Portal 2");
        assert!(item.art.unwrap().contains("/620/library_600x900.jpg"));
        assert!(owned_item(&serde_json::json!({"appid": 620})).is_none()); // no name
    }

    #[test]
    fn steamid64_is_used_as_is_without_network() {
        // A 17-digit id (or a profile URL containing one) needs no API call.
        assert_eq!(
            resolve_steamid("anykey", "76561197960287930"),
            Some("76561197960287930".to_string())
        );
        assert_eq!(
            resolve_steamid(
                "anykey",
                "https://steamcommunity.com/profiles/76561197960287930/"
            ),
            Some("76561197960287930".to_string())
        );
    }
}
