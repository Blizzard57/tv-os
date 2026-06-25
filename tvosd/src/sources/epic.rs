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

pub struct Epic {
    available: bool,
    owned_cache: Mutex<Option<(Instant, Vec<EpicGame>)>>,
}

#[derive(Clone, PartialEq, Debug)]
struct EpicGame {
    app_name: String,
    title: String,
    art: Option<String>,
}

impl Epic {
    pub fn detect() -> Self {
        let available = Command::new("legendary")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        Self {
            available,
            owned_cache: Mutex::new(None),
        }
    }

    fn owned_games(&self) -> Vec<EpicGame> {
        let mut cache = self.owned_cache.lock().unwrap();
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
        self.available
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
                title: "Games".to_string(),
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
        // Engine-level upscaler defaults (FSR4 on AMD, NVAPI/DLSS on NVIDIA).
        launcher::spawn_detached_env("legendary", &["launch", app], &crate::upscale::game_env())
            .map_err(|e| format!("could not run legendary: {e}"))
    }

    fn install(&self, item_id: &str, jobs: &InstallManager) -> Result<(), String> {
        let app = item_id
            .strip_prefix("epic:")
            .unwrap_or_default()
            .to_string();
        let title = self
            .owned_games()
            .iter()
            .find(|g| g.app_name == app)
            .map_or_else(|| app.clone(), |g| g.title.clone());

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
