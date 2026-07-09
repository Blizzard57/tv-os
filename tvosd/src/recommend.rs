//! Local recommender — no cloud, no accounts. Every launch is appended to an
//! event log (~/.config/tvos/events.jsonl); the home screen gets its
//! "continue" rows out of it:
//!
//!   Continue Watching    — most recent distinct movies/shows/games, newest
//!                          first (the cross-domain row from PLAN.md)
//!   Continue on YouTube  — the same, but for YouTube clips, kept in their own
//!                          row so a binge of shorts doesn't bury the film or
//!                          game you were half-way through
//!   Recommended for You  — frequency × recency decay (half-life 14 days),
//!                          boosted when an item is usually used in the same
//!                          part of day as right now
//!
//! Embedding-based similarity can replace the scorer later; the row contract
//! stays the same.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::{ContentItem, Row};
use crate::settings::config_dir;

const ROW_SIZE: usize = 12;

/// Cap on how many events we keep — in memory and on disk. The daemon runs for
/// weeks on a TV box and every launch appended an event forever, so both the
/// in-memory `Vec` (cloned on every /api/library call) and events.jsonl grew
/// without bound. The newest `MAX_EVENTS` are far more than the recommender's
/// recency-weighted scoring can use, so older ones are dropped.
const MAX_EVENTS: usize = 5_000;

#[derive(Serialize, Deserialize, Clone)]
pub struct Event {
    pub ts: u64,
    pub item: ContentItem,
}

pub static LOG: LazyLock<EventLog> = LazyLock::new(EventLog::load);

pub struct EventLog {
    path: PathBuf,
    events: Mutex<Vec<Event>>,
}

impl EventLog {
    fn load() -> Self {
        let path = config_dir().join("events.jsonl");
        let mut events: Vec<Event> = std::fs::read_to_string(&path)
            .map(|text| {
                text.lines()
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect()
            })
            .unwrap_or_default();
        trim(&mut events);
        Self {
            path,
            events: Mutex::new(events),
        }
    }

    /// Appends a launch event (in memory + on disk), keeping both bounded to
    /// the newest `MAX_EVENTS`. Within the cap this is a cheap one-line append;
    /// only when the cap is exceeded do we drop the oldest and rewrite the file
    /// so it stays in lockstep with memory.
    pub fn record(&self, item: ContentItem) {
        let event = Event { ts: now(), item };
        let mut events = self.events.lock().unwrap();
        events.push(event.clone());
        if events.len() > MAX_EVENTS {
            trim(&mut events);
            self.rewrite(&events);
        } else {
            self.append(&event);
        }
    }

    /// Appends a single event as one JSON line.
    fn append(&self, event: &Event) {
        let Ok(line) = serde_json::to_string(event) else {
            return;
        };
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    /// Rewrites the whole (already-capped) log, discarding dropped events.
    fn rewrite(&self, events: &[Event]) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut buf = String::new();
        for event in events {
            if let Ok(line) = serde_json::to_string(event) {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        let _ = std::fs::write(&self.path, buf);
    }

    /// The "Continue" home rows — most recent distinct items, newest first,
    /// split so YouTube gets its own row (see [`continue_rows`]). Actual
    /// *recommendations* of new titles come from the TMDB recommender (see
    /// sources::tmdb::for_you), seeded by [`recent_items`].
    pub fn rows(&self) -> Vec<Row> {
        let events = self.events.lock().unwrap();
        continue_rows(&events)
    }

    /// The newest distinct items (newest first), used to seed recommendations.
    pub fn recent_items(&self, n: usize) -> Vec<ContentItem> {
        let events = self.events.lock().unwrap();
        let mut seen = Vec::new();
        let mut items = Vec::new();
        for event in events.iter().rev() {
            if !seen.contains(&event.item.id) {
                seen.push(event.item.id.clone());
                items.push(event.item.clone());
            }
            if items.len() == n {
                break;
            }
        }
        items
    }
}

/// Drops the oldest events so at most `MAX_EVENTS` (the newest) remain.
fn trim(events: &mut Vec<Event>) {
    if events.len() > MAX_EVENTS {
        events.drain(0..events.len() - MAX_EVENTS);
    }
}

/// The Continue rows: most recent distinct items, newest first, with YouTube
/// clips split into their own row so they don't crowd out the movie, show or
/// game you were part-way through. Empty rows are simply omitted.
fn continue_rows(events: &[Event]) -> Vec<Row> {
    let mut seen = Vec::new();
    let mut main: Vec<ContentItem> = Vec::new();
    let mut youtube: Vec<ContentItem> = Vec::new();
    for event in events.iter().rev() {
        if seen.contains(&event.item.id) {
            continue;
        }
        seen.push(event.item.id.clone());
        let bucket = if is_youtube(&event.item) {
            &mut youtube
        } else {
            &mut main
        };
        if bucket.len() < ROW_SIZE {
            bucket.push(event.item.clone());
        }
        if main.len() >= ROW_SIZE && youtube.len() >= ROW_SIZE {
            break;
        }
    }
    let mut rows = Vec::new();
    if !main.is_empty() {
        rows.push(Row {
            title: "Continue Watching".to_string(),
            items: main,
        });
    }
    if !youtube.is_empty() {
        rows.push(Row {
            title: "Continue on YouTube".to_string(),
            items: youtube,
        });
    }
    rows
}

/// YouTube items own the `yt:` id prefix (see sources::youtube).
fn is_youtube(item: &ContentItem) -> bool {
    item.id.starts_with("yt:")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Action, Kind};

    fn ev(id: &str, ts: u64) -> Event {
        Event {
            ts,
            item: ContentItem {
                id: id.to_string(),
                kind: Kind::Game,
                title: id.to_string(),
                art: None,
                action: Action::Play,
            },
        }
    }

    fn row_ids(rows: &[Row], title: &str) -> Vec<String> {
        rows.iter()
            .find(|r| r.title == title)
            .map(|r| r.items.iter().map(|i| i.id.clone()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn continue_is_recent_distinct_newest_first() {
        let events = vec![ev("a", 1), ev("b", 2), ev("a", 3), ev("c", 4)];
        let rows = continue_rows(&events);
        assert_eq!(row_ids(&rows, "Continue Watching"), ["c", "a", "b"]);
    }

    #[test]
    fn continue_splits_youtube_into_its_own_row() {
        let events = vec![
            ev("strm:movie:tt1", 1),
            ev("yt:aaa", 2),
            ev("steam:620", 3),
            ev("yt:bbb", 4),
        ];
        let rows = continue_rows(&events);
        // YouTube clips never appear in the main Continue row…
        assert_eq!(
            row_ids(&rows, "Continue Watching"),
            ["steam:620", "strm:movie:tt1"]
        );
        // …they get their own row, newest first.
        assert_eq!(row_ids(&rows, "Continue on YouTube"), ["yt:bbb", "yt:aaa"]);
    }

    #[test]
    fn continue_omits_empty_rows() {
        let rows = continue_rows(&[ev("yt:only", 1)]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Continue on YouTube");
    }

    #[test]
    fn event_log_is_capped_in_memory_and_on_disk() {
        let dir = std::env::temp_dir().join(format!("tvos-events-{}", std::process::id()));
        std::env::set_var("TVOS_CONFIG_DIR", &dir);
        let _ = std::fs::remove_dir_all(&dir);

        let log = EventLog::load();
        for n in 0..(MAX_EVENTS + 250) {
            log.record(ev(&format!("item{n}"), n as u64).item);
        }

        // Memory is bounded to the newest MAX_EVENTS…
        assert_eq!(log.events.lock().unwrap().len(), MAX_EVENTS);
        // …and so is the file the next boot would load.
        let on_disk = EventLog::load().events.lock().unwrap().len();
        assert_eq!(on_disk, MAX_EVENTS);

        std::env::remove_var("TVOS_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
