//! Local recommender — no cloud, no accounts. Every launch is appended to an
//! event log (~/.config/tvos/events.jsonl); the home screen gets its
//! "continue" row out of it:
//!
//!   Continue watching    — most recent distinct items (movies, shows, games
//!                          and YouTube clips together), newest first — the
//!                          single Google-TV cross-domain row
//!   Recommended for You  — frequency × recency decay (half-life 14 days),
//!                          boosted when an item is usually used in the same
//!                          part of day as right now, excluding the item
//!                          currently leading Continue (see [`EventLog::recommended`])
//!
//! That scorer is the *fallback* path: when the on-box embedding model is warm
//! the recommender instead ranks *new* titles by taste similarity (see
//! sources::tmdb::for_you). The row contract stays the same either way.

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
const MAX_PREFS: usize = 5_000;

#[derive(Serialize, Deserialize, Clone)]
pub struct Event {
    pub ts: u64,
    pub item: ContentItem,
}

pub static LOG: LazyLock<EventLog> = LazyLock::new(EventLog::load);

pub struct EventLog {
    path: PathBuf,
    events: Mutex<Vec<Event>>,
    prefs_path: PathBuf,
    prefs: Mutex<Vec<Preference>>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Reaction {
    Like,
    Dislike,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Preference {
    pub ts: u64,
    pub item: ContentItem,
    #[serde(default)]
    pub watchlist: bool,
    #[serde(default)]
    pub watched: bool,
    #[serde(default)]
    pub reaction: Option<Reaction>,
}

#[derive(Serialize, Clone, Default)]
pub struct PreferenceStatus {
    pub watchlist: bool,
    pub watched: bool,
    pub liked: bool,
    pub disliked: bool,
}

#[derive(Clone, Copy)]
pub enum PreferenceAction {
    Watchlist,
    Watched,
    Like,
    Dislike,
}

impl EventLog {
    fn load() -> Self {
        let path = config_dir().join("events.jsonl");
        let prefs_path = config_dir().join("preferences.json");
        let mut events: Vec<Event> = std::fs::read_to_string(&path)
            .map(|text| {
                text.lines()
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect()
            })
            .unwrap_or_default();
        trim(&mut events);
        let mut prefs: Vec<Preference> = std::fs::read_to_string(&prefs_path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        trim_prefs(&mut prefs);
        Self {
            path,
            events: Mutex::new(events),
            prefs_path,
            prefs: Mutex::new(prefs),
        }
    }

    /// Appends a launch event (in memory + on disk), keeping both bounded to
    /// the newest `MAX_EVENTS`. Within the cap this is a cheap one-line append;
    /// only when the cap is exceeded do we drop the oldest and rewrite the file
    /// so it stays in lockstep with memory.
    pub fn record(&self, item: ContentItem) {
        let event = Event { ts: now(), item };
        let mut events = self.events.lock().unwrap_or_else(|e| e.into_inner());
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
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(mut file) => {
                if let Err(e) = writeln!(file, "{line}") {
                    crate::log_error!("events log: append failed: {e}");
                }
            }
            Err(e) => crate::log_error!("events log: open for append failed: {e}"),
        }
    }

    /// Rewrites the whole (already-capped) log, discarding dropped events.
    /// Writes to a sibling temp file and atomically renames it over the log, so
    /// a crash mid-write can never leave a truncated/half-written events file.
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
        let tmp = self.path.with_extension("jsonl.tmp");
        if let Err(e) = std::fs::write(&tmp, &buf) {
            crate::log_error!("events log: temp write failed: {e}");
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, &self.path) {
            crate::log_error!("events log: atomic replace failed: {e}");
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// The single "Continue watching" home row — most recent distinct items,
    /// newest first (see [`continue_rows`]). Actual *recommendations* of new
    /// titles come from the TMDB recommender (see sources::tmdb::for_you and
    /// because_you_watched), seeded by [`recent_items`].
    pub fn rows(&self) -> Vec<Row> {
        let events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        let disliked = self.disliked_ids();
        if disliked.is_empty() {
            return continue_rows(&events);
        }
        let filtered: Vec<Event> = events
            .iter()
            .filter(|e| !disliked.contains(&e.item.id))
            .cloned()
            .collect();
        continue_rows(&filtered)
    }

    /// The newest distinct items (newest first), used to seed recommendations.
    pub fn recent_items(&self, n: usize) -> Vec<ContentItem> {
        let events = self.events.lock().unwrap_or_else(|e| e.into_inner());
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

    /// Recommendation seeds: explicit preferences first (liked/watched/
    /// watchlisted), then play history. This feeds the embedding recommender
    /// and "Because you watched" rows without polluting Continue.
    pub fn recommendation_seeds(&self, n: usize) -> Vec<ContentItem> {
        let prefs = self.prefs.lock().unwrap_or_else(|e| e.into_inner());
        let mut preferred: Vec<&Preference> = prefs
            .iter()
            .filter(|p| p.reaction != Some(Reaction::Dislike))
            .filter(|p| p.reaction == Some(Reaction::Like) || p.watched || p.watchlist)
            .collect();
        preferred.sort_by(|a, b| b.ts.cmp(&a.ts));
        let mut seen = Vec::new();
        let mut items = Vec::new();
        for p in preferred {
            if seen.contains(&p.item.id) {
                continue;
            }
            seen.push(p.item.id.clone());
            items.push(p.item.clone());
            if items.len() == n {
                return items;
            }
        }
        drop(prefs);
        for item in self.recent_items(n) {
            if !seen.contains(&item.id) {
                seen.push(item.id.clone());
                items.push(item);
            }
            if items.len() == n {
                break;
            }
        }
        items
    }

    pub fn preference(&self, id: &str) -> PreferenceStatus {
        self.prefs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|p| p.item.id == id)
            .map(pref_status)
            .unwrap_or_default()
    }

    pub fn set_preference(&self, item: ContentItem, action: PreferenceAction) -> PreferenceStatus {
        let mut prefs = self.prefs.lock().unwrap_or_else(|e| e.into_inner());
        let pref = match prefs.iter_mut().find(|p| p.item.id == item.id) {
            Some(pref) => {
                pref.item = item;
                pref.ts = now();
                pref
            }
            None => {
                prefs.push(Preference {
                    ts: now(),
                    item,
                    watchlist: false,
                    watched: false,
                    reaction: None,
                });
                prefs.last_mut().expect("just pushed a preference")
            }
        };
        match action {
            PreferenceAction::Watchlist => pref.watchlist = !pref.watchlist,
            PreferenceAction::Watched => pref.watched = !pref.watched,
            PreferenceAction::Like => {
                pref.reaction = if pref.reaction == Some(Reaction::Like) {
                    None
                } else {
                    Some(Reaction::Like)
                };
            }
            PreferenceAction::Dislike => {
                pref.reaction = if pref.reaction == Some(Reaction::Dislike) {
                    None
                } else {
                    Some(Reaction::Dislike)
                };
            }
        }
        let status = pref_status(pref);
        trim_prefs(&mut prefs);
        self.write_prefs(&prefs);
        status
    }

    pub fn preference_rows(&self) -> Vec<Row> {
        let prefs = self.prefs.lock().unwrap_or_else(|e| e.into_inner());
        let mut watchlist: Vec<&Preference> = prefs
            .iter()
            .filter(|p| p.watchlist && p.reaction != Some(Reaction::Dislike))
            .collect();
        watchlist.sort_by(|a, b| b.ts.cmp(&a.ts));
        let items: Vec<ContentItem> = watchlist
            .into_iter()
            .take(ROW_SIZE)
            .map(|p| p.item.clone())
            .collect();
        if items.is_empty() {
            Vec::new()
        } else {
            vec![Row {
                title: "Watchlist".to_string(),
                items,
            }]
        }
    }

    pub fn disliked_ids(&self) -> std::collections::HashSet<String> {
        self.prefs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|p| p.reaction == Some(Reaction::Dislike))
            .map(|p| p.item.id.clone())
            .collect()
    }

    fn write_prefs(&self, prefs: &[Preference]) {
        if let Some(dir) = self.prefs_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let Ok(buf) = serde_json::to_string_pretty(prefs) else {
            return;
        };
        let tmp = self.prefs_path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, buf) {
            crate::log_error!("preferences: temp write failed: {e}");
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, &self.prefs_path) {
            crate::log_error!("preferences: atomic replace failed: {e}");
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// The documented "Recommended for You" scorer (the non-embedding fallback):
    /// frequency × recency decay (half-life 14 days) with a time-of-day boost
    /// for items usually used within ±[`TOD_WINDOW_HOURS`] of the current hour,
    /// excluding the item currently leading the Continue row (you're already
    /// resuming that — recommending it back is noise). Returns up to `n` items,
    /// best first. This ranks items *from your own history*; discovery of new
    /// titles is the embedding path's job.
    pub fn recommended(&self, n: usize) -> Vec<ContentItem> {
        let events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        let now = now();
        // The Continue row's lead = the single most recent event's item.
        let continue_lead = events.last().map(|e| e.item.id.clone());

        // Aggregate per distinct item: summed recency-decayed weight, a raw
        // frequency count, and how many plays fall near the current time of day.
        struct Agg {
            item: ContentItem,
            decayed: f64,
            count: u32,
            tod_hits: u32,
        }
        let current_hour = hour_of_day(now);
        let disliked = self.disliked_ids();
        let mut aggs: Vec<Agg> = Vec::new();
        for event in events.iter() {
            if disliked.contains(&event.item.id) {
                continue;
            }
            let age_days = (now.saturating_sub(event.ts)) as f64 / 86_400.0;
            let decay = (-std::f64::consts::LN_2 * age_days / HALF_LIFE_DAYS).exp();
            let near_tod = hours_apart(hour_of_day(event.ts), current_hour) <= TOD_WINDOW_HOURS;
            match aggs.iter_mut().find(|a| a.item.id == event.item.id) {
                Some(a) => {
                    a.decayed += decay;
                    a.count += 1;
                    a.tod_hits += near_tod as u32;
                }
                None => aggs.push(Agg {
                    item: event.item.clone(),
                    decayed: decay,
                    count: 1,
                    tod_hits: near_tod as u32,
                }),
            }
        }
        drop(events);

        let prefs = self.prefs.lock().unwrap_or_else(|e| e.into_inner());
        for pref in prefs.iter().filter(|p| !disliked.contains(&p.item.id)) {
            let mut boost = 0.0;
            if pref.reaction == Some(Reaction::Like) {
                boost += 3.0;
            }
            if pref.watched {
                boost += 2.0;
            }
            if pref.watchlist {
                boost += 1.0;
            }
            if boost == 0.0 {
                continue;
            }
            match aggs.iter_mut().find(|a| a.item.id == pref.item.id) {
                Some(a) => {
                    a.decayed += boost;
                    a.count += boost.ceil() as u32;
                }
                None => aggs.push(Agg {
                    item: pref.item.clone(),
                    decayed: boost,
                    count: boost.ceil() as u32,
                    tod_hits: 0,
                }),
            }
        }

        let mut scored: Vec<(f64, ContentItem)> = aggs
            .into_iter()
            .filter(|a| Some(&a.item.id) != continue_lead.as_ref())
            .map(|a| {
                // frequency factor: gentle (a repeat watch matters, but recency
                // still dominates) — log-scaled so a binge doesn't swamp the row.
                let frequency = 1.0 + FREQ_FACTOR * (a.count as f64).ln_1p();
                // time-of-day boost: proportional to the share of this item's
                // plays that happened around now.
                let tod = 1.0 + TOD_BOOST * (a.tod_hits as f64 / a.count as f64);
                (a.decayed * frequency * tod, a.item)
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored.into_iter().take(n).map(|(_, item)| item).collect()
    }
}

fn pref_status(pref: &Preference) -> PreferenceStatus {
    PreferenceStatus {
        watchlist: pref.watchlist,
        watched: pref.watched,
        liked: pref.reaction == Some(Reaction::Like),
        disliked: pref.reaction == Some(Reaction::Dislike),
    }
}

/// Recency half-life for the fallback scorer (documented in the module header).
const HALF_LIFE_DAYS: f64 = 14.0;
/// How gently repeat plays boost an item's score (log-scaled frequency).
const FREQ_FACTOR: f64 = 0.5;
/// Extra weight for items usually watched near the current time of day.
const TOD_BOOST: f64 = 0.5;
/// "Near the current hour" means within this many hours (wrapping midnight).
const TOD_WINDOW_HOURS: u32 = 2;

/// Local hour-of-day (0–23) for a unix timestamp. Uses the process TZ offset so
/// "this time of day" matches the user's clock; falls back to UTC if unknown.
fn hour_of_day(ts: u64) -> u32 {
    let offset = tz_offset_secs();
    let local = ts as i64 + offset;
    (local.rem_euclid(86_400) / 3_600) as u32
}

/// Local-time UTC offset in seconds, from the TVOS_TZ_OFFSET env (seconds east
/// of UTC) if set, else 0 (UTC). Kept dependency-free; a wrong offset only
/// shifts the time-of-day boost, never breaks the row.
fn tz_offset_secs() -> i64 {
    std::env::var("TVOS_TZ_OFFSET")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

/// Smallest distance between two hours on a 24-hour clock (so 23 and 1 are 2h).
fn hours_apart(a: u32, b: u32) -> u32 {
    let d = a.abs_diff(b);
    d.min(24 - d)
}

/// Drops the oldest events so at most `MAX_EVENTS` (the newest) remain.
fn trim(events: &mut Vec<Event>) {
    if events.len() > MAX_EVENTS {
        events.drain(0..events.len() - MAX_EVENTS);
    }
}

fn trim_prefs(prefs: &mut Vec<Preference>) {
    if prefs.len() > MAX_PREFS {
        prefs.sort_by(|a, b| b.ts.cmp(&a.ts));
        prefs.truncate(MAX_PREFS);
    }
}

/// The single "Continue watching" row, Google-TV style: the most recent
/// distinct items — movies, shows, games and YouTube clips together — newest
/// first. Returned as a `Vec` (empty when there's nothing) so the caller can
/// `extend` it into the home rows unchanged.
fn continue_rows(events: &[Event]) -> Vec<Row> {
    let mut seen = Vec::new();
    let mut items: Vec<ContentItem> = Vec::new();
    for event in events.iter().rev() {
        if seen.contains(&event.item.id) {
            continue;
        }
        seen.push(event.item.id.clone());
        items.push(event.item.clone());
        if items.len() >= ROW_SIZE {
            break;
        }
    }
    if items.is_empty() {
        return Vec::new();
    }
    vec![Row {
        title: "Continue watching".to_string(),
        items,
    }]
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

    /// `TVOS_CONFIG_DIR` is process-global, so tests that mutate it must not run
    /// concurrently or one test's config path leaks into another's `load()`.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
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
                note: None,
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
        assert_eq!(row_ids(&rows, "Continue watching"), ["c", "a", "b"]);
    }

    #[test]
    fn continue_mixes_youtube_with_everything_newest_first() {
        let events = vec![
            ev("strm:movie:tt1", 1),
            ev("yt:aaa", 2),
            ev("steam:620", 3),
            ev("yt:bbb", 4),
        ];
        let rows = continue_rows(&events);
        // One Google-TV "Continue watching" row: movies, games and YouTube
        // clips together, newest first.
        assert_eq!(rows.len(), 1);
        assert_eq!(
            row_ids(&rows, "Continue watching"),
            ["yt:bbb", "steam:620", "yt:aaa", "strm:movie:tt1"]
        );
    }

    #[test]
    fn continue_omits_empty_row() {
        assert!(continue_rows(&[]).is_empty());
        let rows = continue_rows(&[ev("yt:only", 1)]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Continue watching");
    }

    #[test]
    fn hours_apart_wraps_midnight() {
        assert_eq!(hours_apart(23, 1), 2);
        assert_eq!(hours_apart(1, 23), 2);
        assert_eq!(hours_apart(10, 14), 4);
        assert_eq!(hours_apart(0, 0), 0);
    }

    #[test]
    fn recommended_excludes_continue_lead_and_ranks_by_recency() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("tvos-rec-{}", std::process::id()));
        std::env::set_var("TVOS_CONFIG_DIR", &dir);
        let _ = std::fs::remove_dir_all(&dir);

        let now = now();
        let day = 86_400u64;
        let log = EventLog::load();
        // Seed the in-memory log directly so we control timestamps. "old" was
        // watched long ago; "recent" more recently; "lead" is the single newest
        // event (the Continue lead) and must be excluded.
        {
            let mut events = log.events.lock().unwrap();
            events.push(ev("old", now - 20 * day));
            events.push(ev("recent", now - 2 * day));
            events.push(ev("lead", now));
        }

        let recs = log.recommended(10);
        let ids: Vec<&str> = recs.iter().map(|i| i.id.as_str()).collect();
        assert!(!ids.contains(&"lead"), "continue lead must be excluded");
        // Recency decay puts the more-recent item first.
        assert_eq!(ids, ["recent", "old"]);

        std::env::remove_var("TVOS_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn event_log_is_capped_in_memory_and_on_disk() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
