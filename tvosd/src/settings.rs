//! User settings, persisted as JSON in the TV OS config directory.
//!
//! Defaults:
//!   - `TVOS_CONFIG_DIR` when set.
//!   - repo-local `.tvos/profile/config` when `TVOS_PORTABLE=1`.
//!   - macOS: `~/Library/Application Support/TV OS`.
//!   - other Unix: `~/.config/tvos`.

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
    /// UI accent color as a hex string (e.g. "#4f8cff"); empty = the default.
    #[serde(default)]
    pub accent: String,
    /// Fullscreen output resolution as "WIDTHxHEIGHT" (e.g. "1920x1080");
    /// empty = follow the display's native resolution. Read by the gamescope
    /// launch scripts (see system/tvos-session, system/tvos-app).
    #[serde(default)]
    pub display_resolution: String,
    /// Ask gamescope to enable HDR output on capable displays.
    #[serde(default)]
    pub display_hdr: bool,
    /// YouTube channels to follow (@handles or channel URLs, comma/space
    /// separated). Each becomes a home row via yt-dlp — no API key needed.
    #[serde(default)]
    pub youtube_channels: String,
    /// Use the YouTube account signed in inside the TV OS browser window:
    /// adds the personal "For you" and "Subscriptions" rows (cookie-based).
    #[serde(default)]
    pub youtube_account: bool,
    /// Two-letter country code for game store pricing ("" = US).
    #[serde(default)]
    pub game_region: String,
    /// Two-letter country code for the Live tab: which region's free-to-air
    /// sports channels to surface first and which fixtures to prioritise
    /// ("" = IN, India — the default).
    #[serde(default)]
    pub live_region: String,
    /// Sports the user follows, comma/space separated
    /// (e.g. "cricket, football, tennis, f1"); empty = show all. Filters and
    /// orders the Live tab's per-sport rows.
    #[serde(default)]
    pub live_sports: String,
    /// Optional comma-separated competition ids/names within followed sports.
    #[serde(default)]
    pub live_leagues: String,
    /// Optional comma-separated team ids/names. Matching is normalized by the
    /// live guide so renames and punctuation do not break a follow.
    #[serde(default)]
    pub live_teams: String,
    /// Extra IPTV playlists to fold into the Live tab: M3U/M3U8 URLs,
    /// comma/newline separated. Each channel becomes a live card. The built-in
    /// public catalog (iptv-org) is always included on top of these.
    #[serde(default)]
    pub iptv_playlists: String,
    /// XMLTV program-guide (EPG) URLs, comma/newline separated. Used to match
    /// live sports fixtures to the channel currently carrying them, so a match
    /// card becomes directly playable. IPTV providers ship one next to their M3U.
    #[serde(default)]
    pub epg_urls: String,
    /// Trakt API app credentials (trakt.tv/oauth/applications) + the OAuth
    /// token the device-code flow saves. Watched movies/episodes sync there.
    #[serde(default)]
    pub trakt_client_id: String,
    #[serde(default)]
    pub trakt_client_secret: String,
    #[serde(default)]
    pub trakt_token: String,
    /// AniList access token (implicit grant from your own AniList API client).
    #[serde(default)]
    pub anilist_token: String,
    /// Twitch Helix application credentials and user token. The token is
    /// write-only and grants read access to followed channels/streams.
    #[serde(default)]
    pub twitch_client_id: String,
    #[serde(default)]
    pub twitch_token: String,
    /// MyAnimeList API client id (PKCE flow; token saved by the callback).
    #[serde(default)]
    pub mal_client_id: String,
    #[serde(default)]
    pub mal_token: String,
}

#[derive(Deserialize, Default)]
pub struct SettingsPatch {
    pub enhance: Option<EnhanceMode>,
    pub steam_api_key: Option<String>,
    pub steam_id: Option<String>,
    pub tmdb_key: Option<String>,
    pub accent: Option<String>,
    pub display_resolution: Option<String>,
    pub display_hdr: Option<bool>,
    pub youtube_channels: Option<String>,
    pub youtube_account: Option<bool>,
    pub game_region: Option<String>,
    pub live_region: Option<String>,
    pub live_sports: Option<String>,
    pub live_leagues: Option<String>,
    pub live_teams: Option<String>,
    pub iptv_playlists: Option<String>,
    pub epg_urls: Option<String>,
    pub trakt_client_id: Option<String>,
    pub trakt_client_secret: Option<String>,
    pub trakt_token: Option<String>,
    pub anilist_token: Option<String>,
    pub twitch_client_id: Option<String>,
    pub twitch_token: Option<String>,
    pub mal_client_id: Option<String>,
    pub mal_token: Option<String>,
}

impl Settings {
    /// References to the write-only secret fields, in a fixed order shared with
    /// [`Self::secret_fields_mut`] and [`SECRET_FIELD_NAMES`] so the redacted
    /// read view and the "empty means unchanged" merge stay in sync.
    fn secret_fields(&self) -> [&String; 7] {
        [
            &self.steam_api_key,
            &self.tmdb_key,
            &self.trakt_client_secret,
            &self.trakt_token,
            &self.anilist_token,
            &self.twitch_token,
            &self.mal_token,
        ]
    }

    /// A copy safe to hand to the client: every secret blanked, with a sibling
    /// `<field>_set` boolean the UI can use to show "configured". The real
    /// values never leave the daemon on a GET.
    pub fn redacted(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(obj) = v.as_object_mut() {
            for name in SECRET_FIELD_NAMES {
                let set = obj
                    .get(name)
                    .and_then(|s| s.as_str())
                    .is_some_and(|s| !s.is_empty());
                obj.insert(name.to_string(), serde_json::Value::String(String::new()));
                obj.insert(format!("{name}_set"), serde_json::Value::Bool(set));
            }
        }
        v
    }

    /// Fills in any incoming secret that arrived empty with the value we already
    /// hold, so saving the panel (which never sees real secrets) can't wipe a
    /// stored credential. An explicit non-empty value still overwrites.
    fn merge_secrets_from(&mut self, current: &Settings) {
        let existing = current.secret_fields();
        for (slot, prev) in self.secret_fields_mut().into_iter().zip(existing) {
            if slot.is_empty() {
                *slot = prev.clone();
            }
        }
    }

    fn secret_fields_mut(&mut self) -> [&mut String; 7] {
        [
            &mut self.steam_api_key,
            &mut self.tmdb_key,
            &mut self.trakt_client_secret,
            &mut self.trakt_token,
            &mut self.anilist_token,
            &mut self.twitch_token,
            &mut self.mal_token,
        ]
    }
}

impl SettingsPatch {
    fn apply_to(self, mut current: Settings) -> Settings {
        macro_rules! set_plain {
            ($field:ident) => {
                if let Some(value) = self.$field {
                    current.$field = value;
                }
            };
        }
        macro_rules! set_secret {
            ($field:ident) => {
                if let Some(value) = self.$field {
                    if !value.is_empty() {
                        current.$field = value;
                    }
                }
            };
        }

        if let Some(value) = self.enhance {
            current.enhance = value;
        }
        set_secret!(steam_api_key);
        set_plain!(steam_id);
        set_secret!(tmdb_key);
        set_plain!(accent);
        set_plain!(display_resolution);
        if let Some(value) = self.display_hdr {
            current.display_hdr = value;
        }
        set_plain!(youtube_channels);
        if let Some(value) = self.youtube_account {
            current.youtube_account = value;
        }
        set_plain!(game_region);
        set_plain!(live_region);
        set_plain!(live_sports);
        set_plain!(live_leagues);
        set_plain!(live_teams);
        set_plain!(iptv_playlists);
        set_plain!(epg_urls);
        set_plain!(trakt_client_id);
        set_secret!(trakt_client_secret);
        set_secret!(trakt_token);
        set_secret!(anilist_token);
        set_plain!(twitch_client_id);
        set_secret!(twitch_token);
        set_plain!(mal_client_id);
        set_secret!(mal_token);
        current
    }
}

/// Names of the write-only secret fields (must match `secret_fields`).
const SECRET_FIELD_NAMES: [&str; 7] = [
    "steam_api_key",
    "tmdb_key",
    "trakt_client_secret",
    "trakt_token",
    "anilist_token",
    "twitch_token",
    "mal_token",
];

/// Normalizes a 2-letter country code, falling back to `default` for anything
/// that isn't two ASCII letters.
fn normalize_country(raw: &str, default: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() == 2 && trimmed.chars().all(|c| c.is_ascii_alphabetic()) {
        trimmed.to_ascii_uppercase()
    } else {
        default.to_string()
    }
}

/// Store region for game pricing, defaulting to "US".
fn normalize_region(raw: &str) -> String {
    normalize_country(raw, "US")
}

/// Live-tab region, defaulting to "IN" (India).
fn normalize_live_region(raw: &str) -> String {
    normalize_country(raw, "IN")
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
        self.current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set(&self, mut settings: Settings) -> Result<(), String> {
        // Empty incoming secrets mean "leave as-is" — the settings panel never
        // sees real secret values (they're blanked on GET), so a plain save
        // must not wipe stored credentials.
        {
            let current = self.current.lock().unwrap_or_else(|e| e.into_inner());
            settings.merge_secrets_from(&current);
        }
        settings.game_region = normalize_region(&settings.game_region);
        settings.live_region = normalize_live_region(&settings.live_region);

        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            // Config dir holds secrets — keep it owner-only on Unix.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
        write_private(&self.path, json.as_bytes()).map_err(|e| e.to_string())?;
        *self.current.lock().unwrap_or_else(|e| e.into_inner()) = settings;
        Ok(())
    }

    pub fn patch(&self, patch: SettingsPatch) -> Result<(), String> {
        let current = self.get();
        self.set(patch.apply_to(current))
    }
}

/// Writes `bytes` to `path`, creating the file mode 0600 on Unix so the stored
/// credentials are never world-readable. On other platforms this is a plain
/// truncating write.
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        // .mode() only applies on create; tighten an existing file too.
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        f.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        let mut f = std::fs::File::create(path)?;
        f.write_all(bytes)
    }
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TVOS_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    if portable_enabled() {
        if let Some(repo) = repo_dir() {
            return repo.join(".tvos/profile/config");
        }
    }
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/TV OS")
    } else {
        home.join(".config/tvos")
    }
}

pub fn profile_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TVOS_PROFILE_DIR") {
        return PathBuf::from(dir);
    }
    if portable_enabled() {
        if let Some(repo) = repo_dir() {
            return repo.join(".tvos/profile");
        }
    }
    config_dir()
}

fn portable_enabled() -> bool {
    if matches!(
        std::env::var("TVOS_PORTABLE").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    ) {
        return true;
    }
    if cfg!(test) {
        return false;
    }
    repo_dir().is_some_and(|repo| repo.join(".tvos/portable").is_file())
}

fn repo_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("TVOS_REPO_DIR") {
        let path = PathBuf::from(dir);
        if is_repo_root(&path) {
            return Some(path);
        }
    }
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if is_repo_root(&dir) {
            return Some(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

fn is_repo_root(path: &std::path::Path) -> bool {
    path.join("tvosd/Cargo.toml").is_file() && path.join("shell/package.json").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_is_normalized() {
        assert_eq!(normalize_region("us"), "US");
        assert_eq!(normalize_region(" gb "), "GB");
        assert_eq!(normalize_region(""), "US");
        assert_eq!(normalize_region("USA"), "US");
        assert_eq!(normalize_region("1!"), "US");
    }

    #[test]
    fn empty_incoming_secret_keeps_stored_value() {
        let stored = Settings {
            steam_api_key: "SECRET".to_string(),
            trakt_token: "TOK".to_string(),
            ..Default::default()
        };
        let mut incoming = Settings {
            steam_api_key: String::new(),      // untouched by the panel
            trakt_token: "NEWTOK".to_string(), // explicitly changed
            ..Default::default()
        };
        incoming.merge_secrets_from(&stored);
        assert_eq!(incoming.steam_api_key, "SECRET");
        assert_eq!(incoming.trakt_token, "NEWTOK");
    }

    #[test]
    fn redacted_blanks_secrets_and_flags_configured() {
        let s = Settings {
            steam_api_key: "SECRET".to_string(),
            steam_id: "76561".to_string(),
            ..Default::default()
        };
        let v = s.redacted();
        assert_eq!(v["steam_api_key"], "");
        assert_eq!(v["steam_api_key_set"], true);
        assert_eq!(v["anilist_token_set"], false);
        // Non-secret fields pass through untouched.
        assert_eq!(v["steam_id"], "76561");
    }

    #[test]
    fn patch_only_changes_present_fields() {
        let current = Settings {
            enhance: EnhanceMode::Quality,
            steam_api_key: "SECRET".to_string(),
            steam_id: "76561".to_string(),
            live_region: "GB".to_string(),
            display_hdr: true,
            ..Default::default()
        };
        let patch = SettingsPatch {
            steam_id: Some("newid".to_string()),
            steam_api_key: Some(String::new()),
            display_hdr: Some(false),
            ..Default::default()
        };
        let next = patch.apply_to(current);
        assert_eq!(next.enhance, EnhanceMode::Quality);
        assert_eq!(next.steam_api_key, "SECRET");
        assert_eq!(next.steam_id, "newid");
        assert_eq!(next.live_region, "GB");
        assert!(!next.display_hdr);
    }
}
