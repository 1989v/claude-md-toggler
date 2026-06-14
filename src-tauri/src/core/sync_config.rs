//! Persisted git-sync configuration (v0.3). A single row describing the linked
//! context repo: its remote URL, the tracked branch, the last successfully
//! synced commit, and the auto-pull / auto-push preferences.
//!
//! Shares the `~/.claude/.toggler-history.db` SQLite file with history and
//! mappings (a different table) so one dotfiles backup covers everything. The
//! table is constrained to exactly one row (`id = 1`) — there is only ever one
//! linked repo. Credentials are NEVER stored here; only the public remote URL.

use std::path::Path;

use chrono::Utc;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use thiserror::Error;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sync_config (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    remote_url      TEXT    NOT NULL,
    branch          TEXT    NOT NULL,
    last_synced_sha TEXT,
    auto_pull       INTEGER NOT NULL DEFAULT 1,
    auto_push       INTEGER NOT NULL DEFAULT 0,
    updated_at      TEXT    NOT NULL
);
"#;

#[derive(Debug, Error)]
pub enum SyncConfigError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("no repo is linked")]
    NotLinked,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SyncConfig {
    pub remote_url: String,
    pub branch: String,
    pub last_synced_sha: Option<String>,
    pub auto_pull: bool,
    pub auto_push: bool,
    pub updated_at: String,
}

pub struct SyncConfigStore {
    conn: Connection,
}

impl SyncConfigStore {
    pub fn open(db_path: &Path) -> Result<Self, SyncConfigError> {
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

    pub fn in_memory() -> Result<Self, SyncConfigError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Link (or re-link) a context repo. Re-linking overwrites the single row and
    /// resets the synced-sha so the next pull re-materializes from scratch.
    /// Defaults: auto_pull on, auto_push off.
    pub fn link(&self, remote_url: &str, branch: &str) -> Result<(), SyncConfigError> {
        let ts = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.conn.execute(
            "INSERT OR REPLACE INTO sync_config
                (id, remote_url, branch, last_synced_sha, auto_pull, auto_push, updated_at)
             VALUES (1, ?1, ?2, NULL, 1, 0, ?3)",
            params![remote_url, branch, ts],
        )?;
        Ok(())
    }

    pub fn get(&self) -> Result<Option<SyncConfig>, SyncConfigError> {
        let cfg = self
            .conn
            .query_row(
                "SELECT remote_url, branch, last_synced_sha, auto_pull, auto_push, updated_at
                 FROM sync_config WHERE id = 1",
                [],
                |row| {
                    Ok(SyncConfig {
                        remote_url: row.get(0)?,
                        branch: row.get(1)?,
                        last_synced_sha: row.get(2)?,
                        auto_pull: row.get::<_, i64>(3)? != 0,
                        auto_push: row.get::<_, i64>(4)? != 0,
                        updated_at: row.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(cfg)
    }

    /// Advance the last-synced commit. Per the partial-pull rule the caller only
    /// calls this once every file in a pull is resolved (fast-forwarded or the
    /// user settled the conflict) so a half-applied pull never moves the baseline.
    pub fn set_synced_sha(&self, sha: &str) -> Result<(), SyncConfigError> {
        let ts = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let n = self.conn.execute(
            "UPDATE sync_config SET last_synced_sha = ?1, updated_at = ?2 WHERE id = 1",
            params![sha, ts],
        )?;
        if n == 0 {
            return Err(SyncConfigError::NotLinked);
        }
        Ok(())
    }

    pub fn set_auto(&self, auto_pull: bool, auto_push: bool) -> Result<(), SyncConfigError> {
        let ts = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let n = self.conn.execute(
            "UPDATE sync_config SET auto_pull = ?1, auto_push = ?2, updated_at = ?3 WHERE id = 1",
            params![auto_pull as i64, auto_push as i64, ts],
        )?;
        if n == 0 {
            return Err(SyncConfigError::NotLinked);
        }
        Ok(())
    }

    /// Unlink the repo (forget the row). Idempotent.
    pub fn clear(&self) -> Result<(), SyncConfigError> {
        self.conn
            .execute("DELETE FROM sync_config WHERE id = 1", [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_returns_none_when_unlinked() {
        let store = SyncConfigStore::in_memory().unwrap();
        assert!(store.get().unwrap().is_none());
    }

    #[test]
    fn link_creates_single_row_with_defaults() {
        let store = SyncConfigStore::in_memory().unwrap();
        store
            .link("https://github.com/me/ctx.git", "main")
            .unwrap();
        let cfg = store.get().unwrap().unwrap();
        assert_eq!(cfg.remote_url, "https://github.com/me/ctx.git");
        assert_eq!(cfg.branch, "main");
        assert_eq!(cfg.last_synced_sha, None);
        assert!(cfg.auto_pull);
        assert!(!cfg.auto_push);
    }

    #[test]
    fn relink_overwrites_and_resets_sha() {
        let store = SyncConfigStore::in_memory().unwrap();
        store.link("https://a/x.git", "main").unwrap();
        store.set_synced_sha("deadbeef").unwrap();
        assert_eq!(store.get().unwrap().unwrap().last_synced_sha.as_deref(), Some("deadbeef"));

        store.link("https://b/y.git", "trunk").unwrap();
        let cfg = store.get().unwrap().unwrap();
        assert_eq!(cfg.remote_url, "https://b/y.git");
        assert_eq!(cfg.branch, "trunk");
        assert_eq!(cfg.last_synced_sha, None, "re-link must reset synced sha");
    }

    #[test]
    fn set_synced_sha_advances_baseline() {
        let store = SyncConfigStore::in_memory().unwrap();
        store.link("https://a/x.git", "main").unwrap();
        store.set_synced_sha("abc123").unwrap();
        assert_eq!(
            store.get().unwrap().unwrap().last_synced_sha.as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn set_synced_sha_errors_when_unlinked() {
        let store = SyncConfigStore::in_memory().unwrap();
        assert!(matches!(
            store.set_synced_sha("x"),
            Err(SyncConfigError::NotLinked)
        ));
    }

    #[test]
    fn set_auto_toggles_preferences() {
        let store = SyncConfigStore::in_memory().unwrap();
        store.link("https://a/x.git", "main").unwrap();
        store.set_auto(false, true).unwrap();
        let cfg = store.get().unwrap().unwrap();
        assert!(!cfg.auto_pull);
        assert!(cfg.auto_push);
    }

    #[test]
    fn clear_unlinks() {
        let store = SyncConfigStore::in_memory().unwrap();
        store.link("https://a/x.git", "main").unwrap();
        store.clear().unwrap();
        assert!(store.get().unwrap().is_none());
    }

    #[test]
    fn single_row_constraint_holds_across_links() {
        // Two links must not create two rows — id is pinned to 1.
        let store = SyncConfigStore::in_memory().unwrap();
        store.link("https://a/x.git", "main").unwrap();
        store.link("https://b/y.git", "main").unwrap();
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM sync_config", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
