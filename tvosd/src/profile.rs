//! Durable personalization state.
//!
//! The legacy event/preference files remain readable by `recommend`, but this
//! database is the canonical home for richer playback events, external ids,
//! imported history, impressions, interests and retryable tracker writes.

use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::model::ContentItem;
use crate::settings::config_dir;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionKind {
    Impression,
    Focus,
    Play,
    Pause,
    Progress,
    Complete,
    Abandon,
    Search,
    Like,
    Dislike,
    Watchlist,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionEvent {
    pub item_id: String,
    pub kind: InteractionKind,
    #[serde(default)]
    pub position: Option<f64>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub ts: Option<i64>,
}

pub struct ProfileStore {
    conn: Mutex<Option<Connection>>,
}

pub static STORE: LazyLock<ProfileStore> = LazyLock::new(ProfileStore::open);

impl ProfileStore {
    fn open() -> Self {
        let path = config_dir().join("profile.sqlite3");
        let conn = (|| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok()?;
            }
            let conn = Connection::open(path).ok()?;
            conn.execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA foreign_keys=ON;
                 CREATE TABLE IF NOT EXISTS canonical_items (
                   id TEXT PRIMARY KEY, title TEXT, media_kind TEXT,
                   metadata_json TEXT, updated_at INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS external_ids (
                   namespace TEXT NOT NULL, external_id TEXT NOT NULL,
                   item_id TEXT NOT NULL, PRIMARY KEY(namespace, external_id)
                 );
                 CREATE TABLE IF NOT EXISTS interactions (
                   id INTEGER PRIMARY KEY, item_id TEXT NOT NULL, kind TEXT NOT NULL,
                   position REAL, duration REAL, context TEXT, occurred_at INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS interactions_item_time
                   ON interactions(item_id, occurred_at DESC);
                 CREATE TABLE IF NOT EXISTS playback_progress (
                   item_id TEXT PRIMARY KEY, position REAL NOT NULL DEFAULT 0,
                   duration REAL NOT NULL DEFAULT 0, completed INTEGER NOT NULL DEFAULT 0,
                   updated_at INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS preferences (
                   item_id TEXT PRIMARY KEY, reaction INTEGER NOT NULL DEFAULT 0,
                   watchlist INTEGER NOT NULL DEFAULT 0, watched INTEGER NOT NULL DEFAULT 0,
                   updated_at INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS external_history (
                   service TEXT NOT NULL, external_id TEXT NOT NULL, item_id TEXT,
                   progress REAL, score REAL, status TEXT, watched_at INTEGER,
                   payload_json TEXT, PRIMARY KEY(service, external_id)
                 );
                 CREATE TABLE IF NOT EXISTS recommendation_impressions (
                   item_id TEXT NOT NULL, shelf_id TEXT NOT NULL,
                   shown_count INTEGER NOT NULL DEFAULT 0, last_shown INTEGER NOT NULL,
                   PRIMARY KEY(item_id, shelf_id)
                 );
                 CREATE TABLE IF NOT EXISTS sports_interests (
                   kind TEXT NOT NULL, value TEXT NOT NULL, weight REAL NOT NULL DEFAULT 1,
                   PRIMARY KEY(kind, value)
                 );
                 CREATE TABLE IF NOT EXISTS sync_outbox (
                   id INTEGER PRIMARY KEY, service TEXT NOT NULL, dedupe_key TEXT NOT NULL UNIQUE,
                   payload_json TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
                   next_attempt INTEGER NOT NULL DEFAULT 0, last_error TEXT
                 );
                 CREATE TABLE IF NOT EXISTS migrations (name TEXT PRIMARY KEY, applied_at INTEGER);",
            ).ok()?;
            Some(conn)
        })();
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate_legacy();
        store
    }

    fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Option<T> {
        let guard = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().and_then(|conn| f(conn).ok())
    }

    fn migrate_legacy(&self) {
        let already = self
            .with_conn(|c| {
                c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM migrations WHERE name='legacy-json-v1')",
                    [],
                    |r| r.get::<_, i64>(0),
                )
            })
            .unwrap_or(0)
            != 0;
        if already {
            return;
        }
        let events = config_dir().join("events.jsonl");
        if let Ok(text) = std::fs::read_to_string(events) {
            for line in text.lines() {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                let Some(id) = v.pointer("/item/id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let ts = v.get("ts").and_then(|v| v.as_i64()).unwrap_or_else(now);
                let _ = self.with_conn(|c| c.execute(
                    "INSERT INTO interactions(item_id,kind,occurred_at,context) VALUES(?1,'play',?2,'legacy')",
                    params![id, ts],
                ));
            }
        }
        let _ = self.with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO migrations(name,applied_at) VALUES('legacy-json-v1',?1)",
                [now()],
            )
        });
    }

    pub fn record(&self, event: &InteractionEvent) -> Result<(), String> {
        if event.item_id.trim().is_empty() {
            return Err("item_id is required".into());
        }
        let kind = serde_json::to_value(&event.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "focus".into());
        let ts = event.ts.unwrap_or_else(now);
        self.with_conn(|c| {
            let tx = c.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO interactions(item_id,kind,position,duration,context,occurred_at)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![event.item_id, kind, event.position, event.duration, event.context, ts],
            )?;
            if matches!(event.kind, InteractionKind::Progress | InteractionKind::Pause | InteractionKind::Complete) {
                let completed = i64::from(matches!(event.kind, InteractionKind::Complete));
                tx.execute(
                    "INSERT INTO playback_progress(item_id,position,duration,completed,updated_at)
                     VALUES(?1,?2,?3,?4,?5)
                     ON CONFLICT(item_id) DO UPDATE SET
                       position=MAX(position,excluded.position), duration=MAX(duration,excluded.duration),
                       completed=MAX(completed,excluded.completed), updated_at=excluded.updated_at",
                    params![event.item_id, event.position.unwrap_or(0.0), event.duration.unwrap_or(0.0), completed, ts],
                )?;
            }
            if matches!(event.kind, InteractionKind::Impression) {
                tx.execute(
                    "INSERT INTO recommendation_impressions(item_id,shelf_id,shown_count,last_shown)
                     VALUES(?1,COALESCE(?2,'unknown'),1,?3)
                     ON CONFLICT(item_id,shelf_id) DO UPDATE SET shown_count=shown_count+1,last_shown=excluded.last_shown",
                    params![event.item_id, event.context, ts],
                )?;
            }
            tx.commit()
        }).ok_or_else(|| "personalization database is unavailable".to_string())?;
        Ok(())
    }

    pub fn status(&self) -> serde_json::Value {
        let (events, pending) = self
            .with_conn(|c| {
                let events = c.query_row("SELECT COUNT(*) FROM interactions", [], |r| {
                    r.get::<_, i64>(0)
                })?;
                let pending = c.query_row("SELECT COUNT(*) FROM sync_outbox", [], |r| {
                    r.get::<_, i64>(0)
                })?;
                Ok((events, pending))
            })
            .unwrap_or((0, 0));
        serde_json::json!({ "ready": self.conn.lock().map(|c| c.is_some()).unwrap_or(false), "interactions": events, "pending_sync": pending })
    }

    pub fn impression_count(&self, item_id: &str) -> i64 {
        self.with_conn(|c| {
            c.query_row(
            "SELECT COALESCE(SUM(shown_count),0) FROM recommendation_impressions WHERE item_id=?1",
            [item_id], |r| r.get::<_, i64>(0),
        )
        })
        .unwrap_or(0)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn import_history(
        &self,
        service: &str,
        external_id: &str,
        item: &ContentItem,
        progress: Option<f64>,
        score: Option<f64>,
        status: Option<&str>,
        watched_at: Option<i64>,
        payload: &serde_json::Value,
    ) {
        let _ = self.with_conn(|c| {
            let tx = c.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO canonical_items(id,title,media_kind,metadata_json,updated_at)
                 VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET
                 title=excluded.title,metadata_json=excluded.metadata_json,updated_at=excluded.updated_at",
                params![item.id, item.title, format!("{:?}", item.kind).to_lowercase(), serde_json::to_string(item).unwrap_or_default(), now()],
            )?;
            tx.execute(
                "INSERT INTO external_ids(namespace,external_id,item_id) VALUES(?1,?2,?3)
                 ON CONFLICT(namespace,external_id) DO UPDATE SET item_id=excluded.item_id",
                params![service, external_id, item.id],
            )?;
            tx.execute(
                "INSERT INTO external_history(service,external_id,item_id,progress,score,status,watched_at,payload_json)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(service,external_id) DO UPDATE SET
                 item_id=excluded.item_id,progress=MAX(COALESCE(progress,0),COALESCE(excluded.progress,0)),
                 score=COALESCE(excluded.score,score),
                 status=CASE WHEN status='completed' THEN status ELSE excluded.status END,
                 watched_at=MAX(COALESCE(watched_at,0),COALESCE(excluded.watched_at,0)),
                 payload_json=excluded.payload_json",
                params![service, external_id, item.id, progress, score, status, watched_at, payload.to_string()],
            )?;
            tx.commit()
        });
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interaction_kind_uses_wire_names() {
        assert_eq!(
            serde_json::to_string(&InteractionKind::Complete).unwrap(),
            "\"complete\""
        );
    }
}
