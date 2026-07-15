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
    #[serde(default)]
    pub content_id: Option<String>,
    #[serde(default)]
    pub track_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub sequence: Option<i64>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackProgress {
    pub content_id: String,
    pub track_id: String,
    pub session_id: String,
    pub sequence: i64,
    pub position_seconds: f64,
    pub duration_seconds: f64,
    pub remaining_seconds: Option<f64>,
    pub percentage: Option<f64>,
    pub updated_at: i64,
    pub completed: bool,
    pub paused: bool,
    pub season: Option<i64>,
    pub episode: Option<i64>,
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
                 CREATE TABLE IF NOT EXISTS playback_sessions (
                   content_id TEXT PRIMARY KEY, track_id TEXT NOT NULL,
                   session_id TEXT NOT NULL, sequence INTEGER NOT NULL DEFAULT 0,
                   position REAL NOT NULL DEFAULT 0, duration REAL NOT NULL DEFAULT 0,
                   completed INTEGER NOT NULL DEFAULT 0, paused INTEGER NOT NULL DEFAULT 0,
                   season INTEGER, episode INTEGER, updated_at INTEGER NOT NULL
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
        store.migrate_playback_progress();
        store.import_progress_sidecars();
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

    fn migrate_playback_progress(&self) {
        let _ = self.with_conn(|c| {
            c.execute(
                "INSERT OR IGNORE INTO playback_sessions
                 (content_id,track_id,session_id,sequence,position,duration,completed,paused,updated_at)
                 SELECT item_id,item_id,'legacy',0,position,duration,completed,0,updated_at
                 FROM playback_progress",
                [],
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
            if matches!(event.kind, InteractionKind::Play | InteractionKind::Progress | InteractionKind::Pause | InteractionKind::Complete | InteractionKind::Abandon) {
                let content_id = event.content_id.as_deref().unwrap_or(&event.item_id);
                let track_id = event.track_id.as_deref().unwrap_or(&event.item_id);
                let session_id = event.session_id.as_deref().unwrap_or("legacy-api");
                let sequence = event.sequence.unwrap_or(ts);
                let position = event.position.unwrap_or(0.0).max(0.0);
                let duration = event.duration.unwrap_or(0.0).max(0.0);
                let completed = i64::from(
                    matches!(event.kind, InteractionKind::Complete)
                        || (duration > 0.0 && position / duration >= 0.95),
                );
                let paused = i64::from(matches!(event.kind, InteractionKind::Pause));
                let (season, episode) = episode_parts(track_id);
                tx.execute(
                    "INSERT INTO playback_progress(item_id,position,duration,completed,updated_at)
                     VALUES(?1,?2,?3,?4,?5)
                     ON CONFLICT(item_id) DO UPDATE SET
                       position=MAX(position,excluded.position), duration=MAX(duration,excluded.duration),
                       completed=MAX(completed,excluded.completed), updated_at=excluded.updated_at",
                    params![event.item_id, event.position.unwrap_or(0.0), event.duration.unwrap_or(0.0), completed, ts],
                )?;
                tx.execute(
                    "INSERT INTO playback_sessions
                     (content_id,track_id,session_id,sequence,position,duration,completed,paused,season,episode,updated_at)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
                     ON CONFLICT(content_id) DO UPDATE SET
                       track_id=excluded.track_id, session_id=excluded.session_id,
                       sequence=excluded.sequence, position=excluded.position,
                       duration=CASE WHEN excluded.duration>0 THEN excluded.duration ELSE duration END,
                       completed=CASE WHEN excluded.track_id<>track_id THEN excluded.completed
                                      ELSE MAX(completed,excluded.completed) END,
                       paused=excluded.paused,
                       season=excluded.season, episode=excluded.episode, updated_at=excluded.updated_at
                     WHERE (excluded.session_id=session_id AND excluded.sequence>sequence)
                        OR (excluded.session_id<>session_id AND excluded.updated_at>=updated_at)",
                    params![content_id, track_id, session_id, sequence, position, duration, completed, paused, season, episode, ts],
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

    pub fn progress(&self, content_id: &str) -> Option<PlaybackProgress> {
        self.with_conn(|c| c.query_row(
            "SELECT content_id,track_id,session_id,sequence,position,duration,completed,paused,season,episode,updated_at
             FROM playback_sessions WHERE content_id=?1",
            [content_id], progress_from_row,
        ))
    }

    pub fn continue_progress(&self) -> Vec<PlaybackProgress> {
        self.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT content_id,track_id,session_id,sequence,position,duration,completed,paused,season,episode,updated_at
                 FROM playback_sessions
                 WHERE completed=0 AND position>=?1 AND (duration<=0 OR position/duration<0.95)
                 ORDER BY updated_at DESC LIMIT 30",
            )?;
            let progress = stmt
                .query_map([crate::resume::MIN_RESUME_SECS], progress_from_row)?
                .filter_map(Result::ok)
                .collect();
            Ok(progress)
        }).unwrap_or_default()
    }

    /// Import crash-safe mpv sidecars. Re-importing is harmless because the
    /// session/sequence guard rejects stale snapshots.
    pub fn import_progress_sidecars(&self) {
        let dir = config_dir().join("positions");
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.to_string_lossy().ends_with(".progress.json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(event) = serde_json::from_str::<InteractionEvent>(&text) else {
                continue;
            };
            if self.record(&event).is_ok() {
                let _ = std::fs::remove_file(&path);
            }
        }
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
        // Serialize before taking the database mutex. ContentItem's wire form
        // may read canonical progress from this same store.
        let metadata_json = serde_json::to_string(item).unwrap_or_default();
        let _ = self.with_conn(|c| {
            let tx = c.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO canonical_items(id,title,media_kind,metadata_json,updated_at)
                 VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET
                 title=excluded.title,metadata_json=excluded.metadata_json,updated_at=excluded.updated_at",
                params![item.id, item.title, format!("{:?}", item.kind).to_lowercase(), metadata_json, now()],
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

fn episode_parts(track_id: &str) -> (Option<i64>, Option<i64>) {
    let parts: Vec<_> = track_id.split(':').collect();
    if parts.len() >= 5 && matches!(parts.get(1), Some(&"series") | Some(&"tv")) {
        (
            parts[parts.len() - 2].parse().ok(),
            parts[parts.len() - 1].parse().ok(),
        )
    } else {
        (None, None)
    }
}

fn progress_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlaybackProgress> {
    let position: f64 = row.get(4)?;
    let duration: f64 = row.get(5)?;
    Ok(PlaybackProgress {
        content_id: row.get(0)?,
        track_id: row.get(1)?,
        session_id: row.get(2)?,
        sequence: row.get(3)?,
        position_seconds: position,
        duration_seconds: duration,
        remaining_seconds: (duration > 0.0).then_some((duration - position).max(0.0)),
        percentage: (duration > 0.0).then_some((position / duration * 100.0).clamp(0.0, 100.0)),
        completed: row.get::<_, i64>(6)? != 0,
        paused: row.get::<_, i64>(7)? != 0,
        season: row.get(8)?,
        episode: row.get(9)?,
        updated_at: row.get(10)?,
    })
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

    fn test_store() -> ProfileStore {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE interactions(id INTEGER PRIMARY KEY,item_id TEXT,kind TEXT,position REAL,duration REAL,context TEXT,occurred_at INTEGER);
             CREATE TABLE playback_progress(item_id TEXT PRIMARY KEY,position REAL,duration REAL,completed INTEGER,updated_at INTEGER);
             CREATE TABLE playback_sessions(content_id TEXT PRIMARY KEY,track_id TEXT,session_id TEXT,sequence INTEGER,position REAL,duration REAL,completed INTEGER,paused INTEGER,season INTEGER,episode INTEGER,updated_at INTEGER);
             CREATE TABLE recommendation_impressions(item_id TEXT,shelf_id TEXT,shown_count INTEGER,last_shown INTEGER,PRIMARY KEY(item_id,shelf_id));",
        ).unwrap();
        ProfileStore {
            conn: Mutex::new(Some(conn)),
        }
    }

    fn progress_event(sequence: i64, position: f64, track: &str) -> InteractionEvent {
        InteractionEvent {
            item_id: "show:1".into(),
            content_id: Some("show:1".into()),
            track_id: Some(track.into()),
            session_id: Some("session-a".into()),
            sequence: Some(sequence),
            kind: InteractionKind::Progress,
            position: Some(position),
            duration: Some(1000.0),
            context: None,
            ts: Some(100 + sequence),
            reason: Some("test".into()),
        }
    }

    #[test]
    fn progress_accepts_ordered_backward_seek_and_rejects_stale_update() {
        let store = test_store();
        store
            .record(&progress_event(1, 600.0, "strm:series:tt1:2:4"))
            .unwrap();
        store
            .record(&progress_event(2, 120.0, "strm:series:tt1:2:4"))
            .unwrap();
        store
            .record(&progress_event(1, 700.0, "strm:series:tt1:2:4"))
            .unwrap();
        let progress = store.progress("show:1").unwrap();
        assert_eq!(progress.position_seconds, 120.0);
        assert_eq!((progress.season, progress.episode), (Some(2), Some(4)));
    }

    #[test]
    fn a_new_episode_replaces_the_series_continue_identity() {
        let store = test_store();
        let mut completed = progress_event(1, 950.0, "strm:series:tt1:1:8");
        completed.kind = InteractionKind::Complete;
        store.record(&completed).unwrap();
        store
            .record(&progress_event(2, 40.0, "strm:series:tt1:2:1"))
            .unwrap();
        let progress = store.progress("show:1").unwrap();
        assert_eq!(progress.track_id, "strm:series:tt1:2:1");
        assert_eq!((progress.season, progress.episode), (Some(2), Some(1)));
        assert!(!progress.completed);
    }
}
