//! Persistent toggle history. Every successful or failed apply (whether
//! initiated from the tray menu, the main window, or a drift-resolution flow)
//! lands in SQLite so the user can see what happened and when.
//!
//! Storage path defaults to `~/.claude/.toggler-history.db`. Schema is created
//! on first connect; subsequent runs reuse the existing file. Rows are never
//! mutated — history is append-only.

use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::{params, Connection, OpenFlags};
use serde::Serialize;
use thiserror::Error;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS toggle_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts          TEXT    NOT NULL,
    action      TEXT    NOT NULL,
    from_name   TEXT,
    to_name     TEXT,
    ok          INTEGER NOT NULL,
    error       TEXT
);
CREATE INDEX IF NOT EXISTS idx_toggle_events_ts ON toggle_events(ts DESC);
"#;

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Distinct action types recorded in history. Stored as the lowercase variant
/// name so SQLite queries stay readable.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Toggle,
    DriftApplyToActive,
    DriftApplyToOrigin,
    DriftDiscard,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Action::Toggle => "toggle",
            Action::DriftApplyToActive => "drift-apply-to-active",
            Action::DriftApplyToOrigin => "drift-apply-to-origin",
            Action::DriftDiscard => "drift-discard",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryEntry {
    pub id: i64,
    /// ISO-8601 UTC timestamp, e.g. "2026-05-22T03:14:15.123Z".
    pub ts: String,
    pub action: String,
    pub from_name: Option<String>,
    pub to_name: Option<String>,
    pub ok: bool,
    pub error: Option<String>,
}

pub struct HistoryStore {
    conn: Connection,
}

impl HistoryStore {
    /// Opens or creates the history database at `db_path`. Parent directory
    /// is created if missing.
    pub fn open(db_path: &Path) -> Result<Self, HistoryError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
        )?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// In-memory store. Used in tests, and as a fallback if the on-disk file
    /// cannot be opened at startup (so the rest of the app keeps working even
    /// when history persistence is unavailable).
    pub fn in_memory() -> Result<Self, HistoryError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn record(
        &self,
        action: Action,
        from: Option<&str>,
        to: Option<&str>,
        result: Result<(), &str>,
    ) -> Result<i64, HistoryError> {
        let ts = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let (ok, error) = match result {
            Ok(()) => (1i64, None::<&str>),
            Err(e) => (0, Some(e)),
        };
        self.conn.execute(
            "INSERT INTO toggle_events (ts, action, from_name, to_name, ok, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![ts, action.as_str(), from, to, ok, error],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Most-recent-first, clamped to a sensible upper bound so a runaway FE
    /// query can't accidentally pull megabytes of rows.
    pub fn list(&self, limit: usize) -> Result<Vec<HistoryEntry>, HistoryError> {
        let limit = limit.min(1000) as i64;
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, action, from_name, to_name, ok, error
             FROM toggle_events
             ORDER BY id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(HistoryEntry {
                id: row.get(0)?,
                ts: row.get(1)?,
                action: row.get(2)?,
                from_name: row.get(3)?,
                to_name: row.get(4)?,
                ok: row.get::<_, i64>(5)? != 0,
                error: row.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

/// Default path under `~/.claude/`; co-located with the toggle files so a
/// dotfiles backup picks it up alongside the profiles.
pub fn default_db_path(base_dir: &Path) -> PathBuf {
    base_dir.join(".toggler-history.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_persists_a_successful_toggle() {
        let store = HistoryStore::in_memory().unwrap();
        let id = store
            .record(Action::Toggle, Some("origin"), Some("quality-first"), Ok(()))
            .unwrap();
        assert!(id > 0);
        let rows = store.list(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, "toggle");
        assert_eq!(rows[0].from_name.as_deref(), Some("origin"));
        assert_eq!(rows[0].to_name.as_deref(), Some("quality-first"));
        assert!(rows[0].ok);
        assert!(rows[0].error.is_none());
    }

    #[test]
    fn record_captures_error_message_on_failure() {
        let store = HistoryStore::in_memory().unwrap();
        store
            .record(Action::Toggle, Some("origin"), Some("nope"), Err("not found"))
            .unwrap();
        let rows = store.list(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].ok);
        assert_eq!(rows[0].error.as_deref(), Some("not found"));
    }

    #[test]
    fn list_returns_most_recent_first() {
        let store = HistoryStore::in_memory().unwrap();
        for name in ["a", "b", "c"] {
            store
                .record(Action::Toggle, Some("origin"), Some(name), Ok(()))
                .unwrap();
        }
        let rows = store.list(10).unwrap();
        let names: Vec<_> = rows.iter().map(|r| r.to_name.clone().unwrap()).collect();
        assert_eq!(names, vec!["c", "b", "a"]);
    }

    #[test]
    fn list_clamps_overly_large_limit() {
        let store = HistoryStore::in_memory().unwrap();
        for i in 0..50 {
            store
                .record(Action::Toggle, None, Some(&format!("p{}", i)), Ok(()))
                .unwrap();
        }
        // Asking for u64::MAX should still come back without exploding;
        // upper bound is implementation-defined (currently 1000).
        let rows = store.list(usize::MAX).unwrap();
        assert_eq!(rows.len(), 50);
    }

    #[test]
    fn action_serializes_to_kebab_case() {
        assert_eq!(Action::DriftApplyToActive.as_str(), "drift-apply-to-active");
        assert_eq!(Action::DriftDiscard.as_str(), "drift-discard");
    }
}
