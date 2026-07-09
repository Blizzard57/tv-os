//! Resume state — "Continue where you left off, with the same source".
//!
//! Two pieces per item:
//!   * the last **stream** that played it (persisted to ~/.config/tvos/resume.json)
//!     so Continue replays the exact same source rather than re-resolving.
//!   * the last **position**, written by the player (scripts/resume.lua) to a
//!     per-item file under ~/.config/tvos/positions/ and read back here.
//!
//! For series we also remember, per show, a small descriptor of the chosen
//! source so the *next episode* can prefer the same addon/quality.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use crate::media::Stream;
use crate::settings::config_dir;

/// Only resume if we're at least this many seconds in (avoids resuming at ~0).
pub const MIN_RESUME_SECS: f64 = 30.0;

pub static STORE: LazyLock<ResumeStore> = LazyLock::new(ResumeStore::load);

pub struct ResumeStore {
    path: PathBuf,
    /// item id → last stream used. Also keyed by series id (the part before the
    /// episode) so the next episode can find the show's preferred source.
    streams: Mutex<HashMap<String, Stream>>,
}

impl ResumeStore {
    fn load() -> Self {
        let path = config_dir().join("resume.json");
        let streams = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self {
            path,
            streams: Mutex::new(streams),
        }
    }

    /// Remembers the stream used for an item (and, for episodes, for the show).
    pub fn remember(&self, item_id: &str, stream: &Stream) {
        let mut map = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(item_id.to_string(), stream.clone());
        if let Some(series) = series_key(item_id) {
            map.insert(series, stream.clone());
        }
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string(&*map) {
            let _ = std::fs::write(&self.path, json);
        }
    }

    /// The stream last used for this exact item, if any.
    pub fn stream(&self, item_id: &str) -> Option<Stream> {
        self.streams.lock().unwrap_or_else(|e| e.into_inner()).get(item_id).cloned()
    }

    /// The source last used for this item's *show* — used to keep the next
    /// episode on the same addon/quality.
    pub fn series_stream(&self, item_id: &str) -> Option<Stream> {
        let key = series_key(item_id)?;
        self.streams.lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned()
    }
}

/// The per-item position file the player writes to. Ensures the directory
/// exists so the player's resume.lua (which can't mkdir) can write to it.
pub fn position_file(item_id: &str) -> PathBuf {
    let safe: String = item_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let dir = config_dir().join("positions");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(safe)
}

/// The saved position (seconds) for an item, if it's worth resuming from.
pub fn position(item_id: &str) -> Option<f64> {
    let secs: f64 = std::fs::read_to_string(position_file(item_id))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    (secs >= MIN_RESUME_SECS).then_some(secs)
}

/// A series key from an episode item id, e.g. "strm:series:tt123:2:5" →
/// "series:tt123". Returns None for non-series ids.
fn series_key(item_id: &str) -> Option<String> {
    let mut parts = item_id.split(':');
    let _prefix = parts.next()?;
    let kind = parts.next()?;
    if kind != "series" && kind != "tv" {
        return None;
    }
    let show = parts.next()?;
    Some(format!("series:{show}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_key_groups_episodes_of_a_show() {
        assert_eq!(
            series_key("strm:series:tt123:2:5").as_deref(),
            Some("series:tt123")
        );
        assert_eq!(
            series_key("strm:series:tt123").as_deref(),
            Some("series:tt123")
        );
        assert_eq!(series_key("strm:movie:tt9"), None);
        assert_eq!(series_key("steam:620"), None);
    }
}
