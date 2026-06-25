//! Lists installed Steam games by reading Steam's own on-disk metadata:
//!
//! 1. Find the Steam root (native or flatpak install).
//! 2. Read `steamapps/libraryfolders.vdf` to find every library folder.
//! 3. Each installed game has a `steamapps/appmanifest_<appid>.acf` file
//!    containing its appid and display name.
//!
//! Both files are Valve's KeyValues ("VDF") format. We only ever need flat
//! `"key" "value"` string pairs out of them, so a full VDF parser is overkill.

use std::fs;
use std::path::{Path, PathBuf};

use crate::launcher;
use crate::model::{Action, ContentItem, Kind, Row};
use crate::sources::Source;

pub struct Steam;

impl Source for Steam {
    fn id(&self) -> &'static str {
        "steam"
    }

    fn available(&self) -> bool {
        steam_root().is_some()
    }

    fn rows(&self) -> Vec<Row> {
        vec![Row {
            title: "Games".to_string(),
            items: scan(),
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
            // Public Steam CDN poster art; the UI falls back to a placeholder on 404.
            art: Some(format!(
                "https://cdn.cloudflare.steamstatic.com/steam/apps/{appid}/library_600x900.jpg"
            )),
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
}
