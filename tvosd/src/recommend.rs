//! Local recommender — no cloud, no accounts. Every launch is appended to an
//! event log (~/.config/tvos/events.jsonl); the home screen gets two ranked
//! rows out of it:
//!
//!   Continue             — most recent distinct items, newest first; mixes
//!                          games and video (the cross-domain row from PLAN.md)
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
const HALF_LIFE_DAYS: f32 = 14.0;
const SAME_DAYPART_BOOST: f32 = 1.5;

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
        let events = std::fs::read_to_string(&path)
            .map(|text| {
                text.lines()
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect()
            })
            .unwrap_or_default();
        Self {
            path,
            events: Mutex::new(events),
        }
    }

    /// Appends a launch event (in memory + one JSON line on disk).
    pub fn record(&self, item: ContentItem) {
        let event = Event { ts: now(), item };
        if let Ok(line) = serde_json::to_string(&event) {
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
        self.events.lock().unwrap().push(event);
    }

    /// The personalized home rows. Empty rows are dropped by the caller.
    pub fn rows(&self) -> Vec<Row> {
        let events = self.events.lock().unwrap().clone();
        vec![
            Row {
                title: "Continue".to_string(),
                items: continue_items(&events),
            },
            Row {
                title: "Recommended for You".to_string(),
                items: recommended_items(&events, now()),
            },
        ]
    }
}

/// Most recent distinct items, newest first.
fn continue_items(events: &[Event]) -> Vec<ContentItem> {
    let mut seen = Vec::new();
    let mut items = Vec::new();
    for event in events.iter().rev() {
        if !seen.contains(&event.item.id) {
            seen.push(event.item.id.clone());
            items.push(event.item.clone());
        }
        if items.len() == ROW_SIZE {
            break;
        }
    }
    items
}

/// Frequency × recency decay × daypart affinity, excluding the single most
/// recent item (it leads the Continue row already).
fn recommended_items(events: &[Event], now: u64) -> Vec<ContentItem> {
    let skip = events.last().map(|e| e.item.id.clone());
    let mut scored: Vec<(f32, ContentItem)> = Vec::new();
    for event in events {
        if Some(&event.item.id) == skip.as_ref() {
            continue;
        }
        let age_days = now.saturating_sub(event.ts) as f32 / 86_400.0;
        let mut weight = 0.5f32.powf(age_days / HALF_LIFE_DAYS);
        if daypart(event.ts) == daypart(now) {
            weight *= SAME_DAYPART_BOOST;
        }
        match scored.iter_mut().find(|(_, item)| item.id == event.item.id) {
            Some((score, item)) => {
                *score += weight;
                *item = event.item.clone(); // keep the freshest metadata
            }
            None => scored.push((weight, event.item.clone())),
        }
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored
        .into_iter()
        .take(ROW_SIZE)
        .map(|(_, item)| item)
        .collect()
}

/// Six-hour buckets of the (UTC) day — only used relatively, so the missing
/// timezone offset cancels out.
fn daypart(ts: u64) -> u64 {
    (ts / 3600) % 24 / 6
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

    const DAY: u64 = 86_400;

    #[test]
    fn continue_is_recent_distinct_newest_first() {
        let events = vec![ev("a", 1), ev("b", 2), ev("a", 3), ev("c", 4)];
        let ids: Vec<String> = continue_items(&events).into_iter().map(|i| i.id).collect();
        assert_eq!(ids, ["c", "a", "b"]);
    }

    #[test]
    fn frequency_beats_a_single_old_event() {
        let now = 100 * DAY;
        // "often" launched 3 times this week; "once" a month ago; "latest" is
        // the most recent event and therefore excluded.
        let events = vec![
            ev("once", now - 30 * DAY),
            ev("often", now - 6 * DAY),
            ev("often", now - 4 * DAY),
            ev("often", now - 2 * DAY),
            ev("latest", now - 1),
        ];
        let ids: Vec<String> = recommended_items(&events, now)
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(ids, ["often", "once"]);
    }

    #[test]
    fn same_daypart_wins_over_other_daypart() {
        let now = 100 * DAY; // daypart 0
        let same = now - DAY; // same daypart, yesterday
        let other = now - DAY + 12 * 3600; // 12h later = daypart 2
        let events = vec![
            ev("evening", other),
            ev("morning", same),
            ev("latest", now - 1),
        ];
        let ids: Vec<String> = recommended_items(&events, now)
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(ids[0], "morning");
    }

    #[test]
    fn most_recent_item_is_left_to_continue_row() {
        let events = vec![ev("a", 1), ev("b", 2)];
        let ids: Vec<String> = recommended_items(&events, 100)
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(ids, ["a"]); // "b" is the most recent → excluded
    }
}
