//! User settings, persisted as JSON in ~/.config/tvos/settings.json
//! (override the directory with TVOS_CONFIG_DIR).

use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};

/// How aggressively to upscale video. Auto picks per content + GPU.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Default, Debug)]
#[serde(rename_all = "lowercase")]
pub enum EnhanceMode {
    #[default]
    Auto,
    Quality,
    Performance,
    Off,
}

/// Persisted user settings. String credentials use "" to mean "unset" so the
/// settings panel can round-trip them as plain text fields. All fields default,
/// so older settings.json files keep loading as new ones are added.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Settings {
    #[serde(default)]
    pub enhance: EnhanceMode,
    /// Steam Web API key (steamcommunity.com/dev/apikey).
    #[serde(default)]
    pub steam_api_key: String,
    /// SteamID64 or vanity name; resolved to an id when fetching the library.
    #[serde(default)]
    pub steam_id: String,
    /// TMDB API key (themoviedb.org → Settings → API).
    #[serde(default)]
    pub tmdb_key: String,
}

pub struct SettingsStore {
    path: PathBuf,
    current: Mutex<Settings>,
}

/// One store for the whole process; sources and API handlers share it.
pub static STORE: LazyLock<SettingsStore> = LazyLock::new(SettingsStore::load);

impl SettingsStore {
    fn load() -> Self {
        let path = config_dir().join("settings.json");
        let current = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            path,
            current: Mutex::new(current),
        }
    }

    pub fn get(&self) -> Settings {
        self.current.lock().unwrap().clone()
    }

    pub fn set(&self, settings: Settings) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, json).map_err(|e| e.to_string())?;
        *self.current.lock().unwrap() = settings;
        Ok(())
    }
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TVOS_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/tvos")
}
