//! Epic Games Store via the `legendary` CLI (https://github.com/derrod/legendary).
//!
//! legendary handles Epic auth, downloads, and launching (with Wine/Proton on
//! Linux); we drive it and parse its JSON output:
//!
//!   legendary list --json            → every game the account owns
//!   legendary list-installed --json  → what's installed locally
//!   legendary install <app> -y      → download/install (a tracked job)
//!   legendary launch <app>          → run the game
//!
//! Installed games show in "Games"; owned-but-not-installed games show in
//! "Ready to Install". The owned list is an Epic API call, so it's cached;
//! the installed list is local and read fresh so install state is never stale.

use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::install::InstallManager;
use crate::launcher;
use crate::model::{Action, ContentItem, Kind, Row};
use crate::sources::Source;

const OWNED_CACHE_TTL: Duration = Duration::from_secs(300);
/// `available()` shells out to `legendary --version`; a short TTL keeps that
/// off the hot path (rows/library refresh) while still noticing an install or
/// sign-in within a few seconds.
const AVAILABLE_CACHE_TTL: Duration = Duration::from_secs(15);

pub struct Epic {
    owned_cache: Mutex<Option<(Instant, Vec<EpicGame>)>>,
    available_cache: Mutex<Option<(Instant, bool)>>,
}

#[derive(Clone, PartialEq, Debug)]
struct EpicGame {
    app_name: String,
    title: String,
    art: Option<String>,
}

impl Epic {
    pub fn detect() -> Self {
        Self {
            owned_cache: Mutex::new(None),
            available_cache: Mutex::new(None),
        }
    }

    fn owned_games(&self) -> Vec<EpicGame> {
        let mut cache = self.owned_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, games)) = cache.as_ref() {
            if at.elapsed() < OWNED_CACHE_TTL {
                return games.clone();
            }
        }
        let games = legendary_json(&["list", "--json"])
            .map(|j| parse_games(&j))
            .unwrap_or_default();
        *cache = Some((Instant::now(), games.clone()));
        games
    }

    fn installed_games(&self) -> Vec<EpicGame> {
        legendary_json(&["list-installed", "--json"])
            .map(|j| parse_games(&j))
            .unwrap_or_default()
    }
}

impl Source for Epic {
    fn id(&self) -> &'static str {
        "epic"
    }

    fn available(&self) -> bool {
        // Detected live (not cached at startup) so installing legendary or
        // signing in is picked up without restarting the daemon. "Connected"
        // means legendary is present *and* you're logged in. Cached with a
        // short TTL so a library refresh doesn't spawn a subprocess each call.
        let mut cache = self.available_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, ok)) = cache.as_ref() {
            if at.elapsed() < AVAILABLE_CACHE_TTL {
                return *ok;
            }
        }
        let ok = legendary_present() && legendary_logged_in();
        *cache = Some((Instant::now(), ok));
        ok
    }

    fn rows(&self) -> Vec<Row> {
        let mut installed = self.installed_games();
        let owned = self.owned_games();

        // `list-installed` carries no artwork; borrow it from the owned list.
        for game in &mut installed {
            if game.art.is_none() {
                game.art = owned
                    .iter()
                    .find(|o| o.app_name == game.app_name)
                    .and_then(|o| o.art.clone());
            }
        }
        let not_installed: Vec<EpicGame> = owned
            .into_iter()
            .filter(|g| !installed.iter().any(|i| i.app_name == g.app_name))
            .collect();

        vec![
            Row {
                title: "Ready to Play".to_string(),
                items: installed
                    .into_iter()
                    .map(|g| g.into_item(Action::Play))
                    .collect(),
            },
            Row {
                title: "Ready to Install".to_string(),
                items: not_installed
                    .into_iter()
                    .map(|g| g.into_item(Action::Install))
                    .collect(),
            },
        ]
    }

    fn launch(&self, item_id: &str) -> Result<(), String> {
        let app = item_id.strip_prefix("epic:").unwrap_or_default();
        // The app id becomes a legendary CLI argument, so it must not look like
        // a flag or be empty — reject before it reaches the process.
        let app = validate_app_name(app)?;
        // Engine-level upscaler defaults (FSR4 on AMD, NVAPI/DLSS on NVIDIA).
        launcher::spawn_detached_env("legendary", &["launch", app], &crate::upscale::game_env())
            .map_err(|e| format!("could not run legendary: {e}"))
    }

    fn install(&self, item_id: &str, jobs: &InstallManager) -> Result<(), String> {
        let app = item_id.strip_prefix("epic:").unwrap_or_default();
        let app = validate_app_name(app)?.to_string();
        // Prefer an app id we actually know the account owns; fall back to the
        // validated id (it may just be missing from a stale owned cache).
        let owned = self.owned_games();
        let known = owned.iter().find(|g| g.app_name == app);
        let title = known.map_or_else(|| app.clone(), |g| g.title.clone());

        let mut cmd = Command::new("legendary");
        cmd.args(["install", &app, "-y"]);
        jobs.start_command(item_id, &title, cmd)
    }
}

impl EpicGame {
    fn into_item(self, action: Action) -> ContentItem {
        ContentItem {
            id: format!("epic:{}", self.app_name),
            kind: Kind::Game,
            title: self.title,
            art: self.art,
            action,
        }
    }
}

/// Rejects empty or flag-like Epic app ids before they become legendary CLI
/// arguments (argument injection). Real Epic app names are alphanumeric with
/// underscores; a leading `-` would be parsed as an option.
fn validate_app_name(app: &str) -> Result<&str, String> {
    if app.is_empty() {
        return Err("missing Epic app id".to_string());
    }
    if app.starts_with('-') {
        return Err(format!("invalid Epic app id '{app}'"));
    }
    Ok(app)
}

/// Is the legendary CLI installed (on PATH)?
fn legendary_present() -> bool {
    Command::new("legendary")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Has the user signed in (legendary stores credentials in user.json)?
/// Honors legendary's own config-path overrides so a non-default setup is
/// still detected: LEGENDARY_CONFIG_PATH points straight at the config dir,
/// otherwise XDG_CONFIG_HOME (falling back to ~/.config).
fn legendary_logged_in() -> bool {
    legendary_config_dir()
        .map(|dir| dir.join("user.json").exists())
        .unwrap_or(false)
}

/// The directory legendary keeps its config (incl. user.json) in.
fn legendary_config_dir() -> Option<std::path::PathBuf> {
    if let Ok(explicit) = std::env::var("LEGENDARY_CONFIG_PATH") {
        if !explicit.is_empty() {
            return Some(std::path::PathBuf::from(explicit));
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(std::path::PathBuf::from(xdg).join("legendary"));
        }
    }
    std::env::var("HOME")
        .ok()
        .map(|h| std::path::Path::new(&h).join(".config/legendary"))
}

fn legendary_json(args: &[&str]) -> Option<String> {
    let output = Command::new("legendary").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parses the game arrays both `list --json` and `list-installed --json`
/// produce. Tolerant of missing fields — a game without art still lists.
fn parse_games(json: &str) -> Vec<EpicGame> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(entries) = value.as_array() else {
        return Vec::new();
    };
    let mut games: Vec<EpicGame> = entries
        .iter()
        .filter_map(|g| {
            Some(EpicGame {
                app_name: g.get("app_name")?.as_str()?.to_string(),
                title: g
                    .get("app_title")
                    .or_else(|| g.get("title"))?
                    .as_str()?
                    .to_string(),
                art: poster_url(g),
            })
        })
        .collect();
    games.sort_by_key(|game| game.title.to_lowercase());
    games
}

/// Tall box art from the game's Epic metadata, if present.
fn poster_url(game: &serde_json::Value) -> Option<String> {
    game.get("metadata")?
        .get("keyImages")?
        .as_array()?
        .iter()
        .find(|img| img.get("type").and_then(|t| t.as_str()) == Some("DieselGameBoxTall"))?
        .get("url")?
        .as_str()
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNED: &str = r#"[
        {
            "app_name": "Sugar",
            "app_title": "Alan Wake",
            "metadata": {
                "keyImages": [
                    {"type": "DieselGameBox", "url": "https://cdn.example/wide.jpg"},
                    {"type": "DieselGameBoxTall", "url": "https://cdn.example/tall.jpg"}
                ]
            }
        },
        {"app_name": "Salt", "app_title": "Celeste", "metadata": {}}
    ]"#;

    #[test]
    fn parses_owned_games_with_optional_art() {
        let games = parse_games(OWNED);
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].title, "Alan Wake");
        assert_eq!(
            games[0].art.as_deref(),
            Some("https://cdn.example/tall.jpg")
        );
        assert_eq!(games[1].title, "Celeste");
        assert_eq!(games[1].art, None);
    }

    #[test]
    fn garbage_json_yields_no_games() {
        assert!(parse_games("not json").is_empty());
        assert!(parse_games("{}").is_empty());
    }
}
