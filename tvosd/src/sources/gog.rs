//! GOG via the Heroic Games Launcher.
//!
//! Heroic stores the signed-in GOG library and install state on disk; we read
//! those (no extra auth needed) and launch / install through Heroic's
//! `heroic://launch?appName=…&runner=gog` deep link. Available once you've
//! signed into GOG in Heroic.
//!
//! Installed games show in "Games" (Play); owned-but-not-installed show in
//! "Ready to Install" (Install).

use std::path::PathBuf;

use serde_json::Value;

use crate::install::InstallManager;
use crate::launcher;
use crate::model::{Action, ContentItem, Kind, Row};
use crate::sources::Source;
use crate::util::percent_encode;

pub struct Gog;

struct GogGame {
    app_name: String,
    title: String,
    art: Option<String>,
    installed: bool,
}

impl Source for Gog {
    fn id(&self) -> &'static str {
        "gog"
    }

    fn available(&self) -> bool {
        // Signed into GOG in Heroic = the auth file and the library cache exist.
        heroic_dir().is_some_and(|d| {
            d.join("gog_store/auth.json").exists() && d.join("store_cache/gog_library.json").exists()
        })
    }

    fn rows(&self) -> Vec<Row> {
        let installed_extra = installed_app_names();
        let mut installed = Vec::new();
        let mut not_installed = Vec::new();
        for game in library() {
            let is_installed = game.installed || installed_extra.contains(&game.app_name);
            let action = if is_installed {
                Action::Play
            } else {
                Action::Install
            };
            let item = ContentItem {
                id: format!("gog:{}", game.app_name),
                kind: Kind::Game,
                title: game.title,
                art: game.art,
                action,
            };
            if is_installed {
                installed.push(item);
            } else {
                not_installed.push(item);
            }
        }
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
        deep_link(item_id)
    }

    fn install(&self, item_id: &str, _jobs: &InstallManager) -> Result<(), String> {
        // Unlike Epic (a tracked `legendary install` job), this is a plain
        // hand-off: the deep link opens Heroic on the game and Heroic runs and
        // tracks the download itself. We don't register an install job, so the
        // UI won't show fake progress for something we aren't managing.
        deep_link(item_id)
    }
}

/// Opens Heroic at the game (launches if installed, offers install if not).
fn deep_link(item_id: &str) -> Result<(), String> {
    let app = item_id.strip_prefix("gog:").unwrap_or_default();
    let url = format!("heroic://launch?appName={}&runner=gog", percent_encode(app));
    launcher::open_external(&url).map_err(|e| format!("could not open Heroic: {e}"))
}

fn heroic_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let dir = PathBuf::from(home).join(".config/heroic");
    dir.is_dir().then_some(dir)
}

/// Owned GOG games from Heroic's library cache (skipping DLC/redistributables).
fn library() -> Vec<GogGame> {
    let Some(dir) = heroic_dir() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(dir.join("store_cache/gog_library.json")) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let Some(games) = value.get("games").and_then(|g| g.as_array()) else {
        return Vec::new();
    };
    games.iter().filter_map(parse_game).collect()
}

fn parse_game(g: &Value) -> Option<GogGame> {
    let app_name = g.get("app_name")?.as_str()?.to_string();
    // Skip DLC and the Galaxy redistributables.
    if app_name == "gog-redist"
        || g.get("install")
            .and_then(|i| i.get("is_dlc"))
            .and_then(|d| d.as_bool())
            == Some(true)
    {
        return None;
    }
    let title = g.get("title")?.as_str()?.to_string();
    // Prefer the tall grid art for poster cards, else the wide cover.
    let art = g
        .get("art_square")
        .or_else(|| g.get("art_cover"))
        .and_then(|a| a.as_str())
        .filter(|a| !a.is_empty())
        .map(String::from);
    Some(GogGame {
        app_name,
        title,
        art,
        installed: g.get("is_installed").and_then(|i| i.as_bool()).unwrap_or(false),
    })
}

/// App names Heroic records as installed (authoritative over the library flag).
fn installed_app_names() -> Vec<String> {
    let Some(dir) = heroic_dir() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(dir.join("gog_store/installed.json")) else {
        return Vec::new();
    };
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("installed")?.as_array().cloned())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    e.get("appName")
                        .or_else(|| e.get("app_name"))
                        .and_then(|n| n.as_str())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIB: &str = r#"{"games": [
        {"app_name": "gog-redist", "title": "Redist", "is_installed": true, "install": {"is_dlc": true}},
        {"app_name": "1207658924", "title": "The Witcher 3", "is_installed": false,
         "art_square": "https://x/w3.png", "art_cover": "https://x/w3c.jpg"},
        {"app_name": "1430740694", "title": "Cyberpunk 2077", "is_installed": true,
         "art_cover": "https://x/cp.jpg"}
    ]}"#;

    #[test]
    fn parses_gog_library_skipping_dlc() {
        let value: Value = serde_json::from_str(LIB).unwrap();
        let games: Vec<GogGame> = value
            .get("games")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(parse_game)
            .collect();
        assert_eq!(games.len(), 2); // redist DLC dropped
        assert_eq!(games[0].title, "The Witcher 3");
        assert_eq!(games[0].art.as_deref(), Some("https://x/w3.png")); // square preferred
        assert!(!games[0].installed);
        assert_eq!(games[1].art.as_deref(), Some("https://x/cp.jpg")); // cover fallback
        assert!(games[1].installed);
    }
}
