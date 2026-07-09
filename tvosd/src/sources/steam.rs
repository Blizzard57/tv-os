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

use crate::install::InstallManager;
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

        let mut cache = self.owned.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = cache.as_ref() {
            if c.creds == creds && c.at.elapsed() < OWNED_TTL {
                return c.items.clone();
            }
        }
        // Cache a *successful* fetch even when the library is genuinely empty,
        // so an empty account doesn't re-hit the network on every call; only a
        // hard error (network/API failure) skips caching and retries next time.
        match fetch_owned(&creds.0, &creds.1) {
            Ok(items) => {
                *cache = Some(OwnedCache {
                    creds,
                    at: Instant::now(),
                    items: items.clone(),
                });
                items
            }
            Err(_) => Vec::new(),
        }
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
        // Installed (on-disk) games are ready to play; everything else the
        // account owns is ready to install.
        let mut installed = scan();
        let installed_ids: Vec<String> = installed.iter().map(|i| i.id.clone()).collect();
        let mut not_installed: Vec<ContentItem> = self
            .owned_games()
            .into_iter()
            .filter(|g| !installed_ids.contains(&g.id))
            .map(|mut g| {
                g.action = Action::Install;
                g
            })
            .collect();
        installed.sort_by_key(|item| item.title.to_lowercase());
        not_installed.sort_by_key(|item| item.title.to_lowercase());
        vec![
            Row {
                title: "Ready to Play".to_string(),
                items: installed,
            },
            Row {
                title: "Ready to Install".to_string(),
                items: not_installed,
            },
        ]
    }

    fn launch(&self, item_id: &str) -> Result<(), String> {
        let appid = valid_appid(item_id)?;
        run_steam_url(&format!("steam://rungameid/{appid}"))
    }

    fn install(&self, item_id: &str, _jobs: &InstallManager) -> Result<(), String> {
        let appid = valid_appid(item_id)?;
        // Steam owns the download; this opens its install dialog.
        run_steam_url(&format!("steam://install/{appid}"))
    }
}

/// The numeric appid from a `steam:<appid>` id, or an error. Steam appids are
/// always positive integers; a non-numeric or empty id would otherwise fire a
/// malformed steam:// URL (silent no-op).
fn valid_appid(item_id: &str) -> Result<&str, String> {
    let appid = item_id.strip_prefix("steam:").unwrap_or_default();
    if !appid.is_empty() && appid.bytes().all(|b| b.is_ascii_digit()) {
        Ok(appid)
    } else {
        Err(format!("invalid Steam appid in '{item_id}'"))
    }
}

/// Opens a steam:// URL via the native client, falling back to the flatpak.
fn run_steam_url(url: &str) -> Result<(), String> {
    let attempts: [(&str, Vec<&str>); 2] = [
        ("steam", vec![url]),
        ("flatpak", vec!["run", "com.valvesoftware.Steam", url]),
    ];
    for (program, args) in attempts {
        if launcher::spawn_detached(program, &args).is_ok() {
            return Ok(());
        }
    }
    Err("could not start Steam (tried native and flatpak)".to_string())
}

/// Store-page summary for a game's details page (public Steam storefront API,
/// no key needed). Best-effort — returns None if the call fails.
pub fn store_meta(appid: &str) -> Option<crate::media::Meta> {
    let url = format!("https://store.steampowered.com/api/appdetails?appids={appid}&l=english");
    parse_store_meta(appid, &addons::http_get(&url).ok()?)
}

fn parse_store_meta(appid: &str, json: &str) -> Option<crate::media::Meta> {
    let value: Value = serde_json::from_str(json).ok()?;
    let data = value.get(appid)?.get("data")?;
    Some(crate::media::Meta {
        id: format!("steam:{appid}"),
        kind: "game".to_string(),
        title: data
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or_default()
            .to_string(),
        poster: Some(art_url(appid)),
        background: data
            .get("background_raw")
            .and_then(|b| b.as_str())
            .map(String::from),
        description: data
            .get("short_description")
            .and_then(|d| d.as_str())
            .filter(|d| !d.is_empty())
            .map(clean_html),
        // Release year/date, e.g. "12 Mar, 2020".
        release_info: data
            .get("release_date")
            .and_then(|r| r.get("date"))
            .and_then(|d| d.as_str())
            .filter(|d| !d.is_empty())
            .map(String::from),
        // Metacritic score (the details page shows it as "Metacritic NN").
        rating: data
            .get("metacritic")
            .and_then(|m| m.get("score"))
            .and_then(|s| s.as_i64())
            .map(|s| s.to_string()),
        developer: first_of(data.get("developers")),
        publisher: first_of(data.get("publishers")),
        genres: names(data.get("genres")),
        // Feature categories: Single-player, Co-op, Full controller support…
        tags: data
            .get("categories")
            .and_then(|c| c.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.get("description").and_then(|d| d.as_str()))
                    .filter(|d| !d.is_empty())
                    .take(6)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        // A handful of large screenshots for the gallery.
        screenshots: data
            .get("screenshots")
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.get("path_full").and_then(|p| p.as_str()))
                    .take(8)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        ..Default::default()
    })
}

/// Steam store text is HTML-ish (entities like `&quot;`, the odd `<br>`).
/// Strip tags and decode the common entities so it reads as plain prose.
fn clean_html(s: &str) -> String {
    let mut stripped = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => stripped.push(c),
            _ => {}
        }
    }
    decode_entities(&stripped)
}

/// Decodes the handful of HTML entities Steam descriptions actually use, plus
/// numeric (`&#39;` / `&#x2026;`) ones.
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        if let Some(semi) = after.find(';').filter(|&p| p <= 10) {
            let decoded = match &after[1..semi] {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                "hellip" => Some('…'),
                "mdash" => Some('—'),
                "ndash" => Some('–'),
                "reg" => Some('®'),
                "trade" => Some('™'),
                "copy" => Some('©'),
                num => num.strip_prefix('#').and_then(|n| {
                    let cp = match n.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => n.parse::<u32>().ok(),
                    };
                    cp.and_then(char::from_u32)
                }),
            };
            if let Some(c) = decoded {
                out.push(c);
                rest = &after[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = &after[1..];
    }
    out.push_str(rest);
    out
}

/// First string of a JSON string-array (developers/publishers), if any.
fn first_of(array: Option<&Value>) -> Option<String> {
    array?
        .as_array()?
        .iter()
        .find_map(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// The `description` field of every object in a JSON array (genres/categories).
fn names(array: Option<&Value>) -> Vec<String> {
    array
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|g| g.get("description").and_then(|d| d.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
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
pub fn art_url(appid: impl std::fmt::Display) -> String {
    format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{appid}/library_600x900.jpg")
}

// ---- Per-game stats for the details page (playtime, achievements) ----------

static PLAYTIME_CACHE: std::sync::LazyLock<
    Mutex<Option<(std::time::Instant, std::collections::HashMap<String, i64>)>>,
> = std::sync::LazyLock::new(Mutex::default);

/// Minutes played across all time, from GetOwnedGames (cached ten minutes).
pub fn playtime_minutes(appid: &str) -> Option<i64> {
    const TTL: std::time::Duration = std::time::Duration::from_secs(600);
    {
        let cache = PLAYTIME_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, map)) = &*cache {
            if at.elapsed() < TTL {
                return map.get(appid).copied();
            }
        }
    }
    let s = settings::STORE.get();
    if s.steam_api_key.is_empty() || s.steam_id.is_empty() {
        return None;
    }
    let steamid = resolve_steamid(&s.steam_api_key, &s.steam_id)?;
    let url = format!(
        "https://api.steampowered.com/IPlayerService/GetOwnedGames/v1/?key={}\
         &steamid={steamid}&include_played_free_games=1&format=json",
        s.steam_api_key
    );
    let json: Value = serde_json::from_str(&addons::http_get(&url).ok()?).ok()?;
    let map: std::collections::HashMap<String, i64> = json
        .get("response")?
        .get("games")?
        .as_array()?
        .iter()
        .filter_map(|g| {
            Some((
                g.get("appid")?.as_i64()?.to_string(),
                g.get("playtime_forever")?.as_i64()?,
            ))
        })
        .collect();
    let minutes = map.get(appid).copied();
    *PLAYTIME_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((std::time::Instant::now(), map));
    minutes
}

#[derive(Clone, serde::Serialize)]
pub struct Achievement {
    pub name: String,
    pub description: String,
    pub icon: String,
    /// Unix time when earned; None = still locked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unlocked_at: Option<i64>,
}

/// (unlocked, locked) achievements for a game, or None when the game has
/// none / the profile hides them. Schema gives names+icons, the player call
/// gives earned state; joined by api name.
pub fn achievements(appid: &str) -> Option<(Vec<Achievement>, Vec<Achievement>)> {
    let s = settings::STORE.get();
    if s.steam_api_key.is_empty() || s.steam_id.is_empty() {
        return None;
    }
    let steamid = resolve_steamid(&s.steam_api_key, &s.steam_id)?;

    let schema_url = format!(
        "https://api.steampowered.com/ISteamUserStats/GetSchemaForGame/v2/?key={}&appid={appid}&l=english",
        s.steam_api_key
    );
    let schema: Value = serde_json::from_str(&addons::http_get(&schema_url).ok()?).ok()?;
    let defs = schema
        .get("game")?
        .get("availableGameStats")?
        .get("achievements")?
        .as_array()?
        .clone();
    if defs.is_empty() {
        return None;
    }

    let player_url = format!(
        "https://api.steampowered.com/ISteamUserStats/GetPlayerAchievements/v1/?key={}&steamid={steamid}&appid={appid}",
        s.steam_api_key
    );
    // Private profiles / never-launched games 400 here — show all as locked.
    let earned: std::collections::HashMap<String, i64> = addons::http_get(&player_url)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| {
            Some(
                v.get("playerstats")?
                    .get("achievements")?
                    .as_array()?
                    .iter()
                    .filter_map(|a| {
                        if a.get("achieved")?.as_i64()? == 1 {
                            Some((
                                a.get("apiname")?.as_str()?.to_string(),
                                a.get("unlocktime").and_then(|t| t.as_i64()).unwrap_or(0),
                            ))
                        } else {
                            None
                        }
                    })
                    .collect(),
            )
        })
        .unwrap_or_default();

    let mut unlocked = Vec::new();
    let mut locked = Vec::new();
    for def in &defs {
        let api = def.get("name").and_then(|n| n.as_str()).unwrap_or_default();
        let display = def
            .get("displayName")
            .and_then(|n| n.as_str())
            .unwrap_or(api)
            .to_string();
        let description = def
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_string();
        match earned.get(api) {
            Some(&t) => unlocked.push(Achievement {
                name: display,
                description,
                icon: def.get("icon").and_then(|i| i.as_str()).unwrap_or_default().to_string(),
                unlocked_at: Some(t),
            }),
            None => locked.push(Achievement {
                name: display,
                description,
                // The grayed-out variant reads as "not yet earned".
                icon: def
                    .get("icongray")
                    .or_else(|| def.get("icon"))
                    .and_then(|i| i.as_str())
                    .unwrap_or_default()
                    .to_string(),
                unlocked_at: None,
            }),
        }
    }
    unlocked.sort_by_key(|a| std::cmp::Reverse(a.unlocked_at.unwrap_or(0)));
    Some((unlocked, locked))
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
        .filter(|(key, _)| key.as_str() == "path")
        .map(|(_, value)| PathBuf::from(value))
        .collect()
}

/// Extracts (appid, name) from an appmanifest .acf file.
fn parse_acf(text: &str) -> Option<(String, String)> {
    let mut appid = None;
    let mut name = None;
    for (key, value) in quoted_pairs(text) {
        match key.as_str() {
            "appid" if appid.is_none() => appid = Some(value),
            "name" if name.is_none() => name = Some(value),
            _ => {}
        }
    }
    Some((appid?, name?))
}

/// Yields every `"key" "value"` pair found on a single line of VDF text.
/// Keys with nested-block values (no value on the line) are skipped. VDF
/// escapes `\"` and `\\` inside quoted strings, so a naive split on `"` would
/// truncate a name like `Sid Meier's "Civilization"`; this unescapes them.
fn quoted_pairs(text: &str) -> impl Iterator<Item = (String, String)> + '_ {
    text.lines().filter_map(|line| {
        let mut chars = line.chars().peekable();
        let key = next_quoted(&mut chars)?;
        let value = next_quoted(&mut chars)?;
        Some((key, value))
    })
}

/// Reads the next `"…"` token from `chars`, decoding VDF `\"` / `\\` escapes.
/// Returns None if there is no further quoted token on the line.
fn next_quoted(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<String> {
    // Advance to the opening quote.
    for c in chars.by_ref() {
        if c == '"' {
            break;
        }
    }
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => return Some(out),
            },
            _ => out.push(c),
        }
    }
    None
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
    fn parses_acf_name_with_escaped_quotes() {
        // VDF escapes an embedded quote as \" — the value must keep it.
        let acf = "\t\"appid\"\t\t\"8930\"\n\t\"name\"\t\t\"Sid Meier's \\\"Civilization\\\" V\"\n";
        assert_eq!(
            parse_acf(acf),
            Some(("8930".to_string(), "Sid Meier's \"Civilization\" V".to_string()))
        );
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
    fn parses_steam_store_meta() {
        let json = r#"{"620": {"success": true, "data": {
            "name": "Portal 2",
            "short_description": "The &quot;acclaimed&quot; <b>sequel</b>.",
            "background_raw": "https://x/bg.jpg",
            "developers": ["Valve"],
            "publishers": ["Valve"],
            "release_date": {"coming_soon": false, "date": "18 Apr, 2011"},
            "metacritic": {"score": 95},
            "genres": [{"description": "Action"}, {"description": "Puzzle"}],
            "categories": [{"description": "Single-player"}, {"description": "Co-op"},
                           {"description": "Full controller support"}],
            "screenshots": [
                {"id": 0, "path_full": "https://x/ss0.jpg"},
                {"id": 1, "path_full": "https://x/ss1.jpg"}
            ]
        }}}"#;
        let m = parse_store_meta("620", json).unwrap();
        assert_eq!(m.id, "steam:620");
        assert_eq!(m.title, "Portal 2");
        assert_eq!(m.description.as_deref(), Some("The \"acclaimed\" sequel."));
        assert_eq!(m.genres, vec!["Action", "Puzzle"]);
        assert_eq!(m.developer.as_deref(), Some("Valve"));
        assert_eq!(m.publisher.as_deref(), Some("Valve"));
        assert_eq!(m.release_info.as_deref(), Some("18 Apr, 2011"));
        assert_eq!(m.rating.as_deref(), Some("95"));
        assert_eq!(m.tags, vec!["Single-player", "Co-op", "Full controller support"]);
        assert_eq!(m.screenshots, vec!["https://x/ss0.jpg", "https://x/ss1.jpg"]);
        assert!(parse_store_meta("620", r#"{"620": {"success": false}}"#).is_none());
    }

    #[test]
    fn cleans_html_from_store_text() {
        assert_eq!(
            clean_html("Build &amp; battle &mdash; it&#39;s a &quot;classic&quot;<br>now"),
            "Build & battle — it's a \"classic\"now"
        );
        assert_eq!(clean_html("plain text"), "plain text");
        assert_eq!(clean_html("100% &amp; rising"), "100% & rising");
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
