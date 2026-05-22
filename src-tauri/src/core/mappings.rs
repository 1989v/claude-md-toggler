//! Persisted directory-to-profile mappings. The user registers rules like
//! "when I'm in `/Users/me/work/proj`, the right profile is `quality-first`"
//! and the app applies them on demand.
//!
//! v0.1 is explicit only: the FE passes a directory path to
//! `apply_mapping_for`, the store finds the best matching rule (exact match
//! preferred, otherwise longest-prefix), and the engine applies it. The
//! PRD's deferred auto-detection (v0.2) would call this exact entry point
//! once the OS-specific active-dir watcher is in place.
//!
//! Mappings share the toggle history's SQLite file so a single dotfiles
//! backup picks both up. SQLite handles concurrent connections natively.

use std::path::Path;

use chrono::Utc;
use rusqlite::{params, Connection, OpenFlags};
use serde::Serialize;
use thiserror::Error;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS directory_mappings (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    dir_path     TEXT    NOT NULL UNIQUE,
    target       TEXT    NOT NULL,
    profile_name TEXT    NOT NULL,
    created_at   TEXT    NOT NULL,
    updated_at   TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_directory_mappings_dir ON directory_mappings(dir_path);
"#;

#[derive(Debug, Error)]
pub enum MappingsError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("dir_path is empty")]
    EmptyPath,
    #[error("mapping with id {0} not found")]
    NotFound(i64),
    #[error("mapping for path '{0}' already exists")]
    AlreadyExists(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct DirectoryMapping {
    pub id: i64,
    pub dir_path: String,
    /// Canonical target string — `"global"` or `"memory:{project-id}"`. Same
    /// vocabulary as the history `target` column.
    pub target: String,
    pub profile_name: String,
    pub created_at: String,
    pub updated_at: String,
}

pub struct MappingsStore {
    conn: Connection,
}

impl MappingsStore {
    pub fn open(db_path: &Path) -> Result<Self, MappingsError> {
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

    pub fn in_memory() -> Result<Self, MappingsError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn list(&self) -> Result<Vec<DirectoryMapping>, MappingsError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, dir_path, target, profile_name, created_at, updated_at
             FROM directory_mappings
             ORDER BY length(dir_path) DESC, dir_path ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(DirectoryMapping {
                id: row.get(0)?,
                dir_path: row.get(1)?,
                target: row.get(2)?,
                profile_name: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn add(
        &self,
        dir_path: &str,
        target: &str,
        profile_name: &str,
    ) -> Result<i64, MappingsError> {
        if dir_path.is_empty() {
            return Err(MappingsError::EmptyPath);
        }
        let normalized = normalize_path(dir_path);
        let ts = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        match self.conn.execute(
            "INSERT INTO directory_mappings (dir_path, target, profile_name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![normalized, target, profile_name, ts],
        ) {
            Ok(_) => Ok(self.conn.last_insert_rowid()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(MappingsError::AlreadyExists(normalized))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn update(
        &self,
        id: i64,
        target: &str,
        profile_name: &str,
    ) -> Result<(), MappingsError> {
        let ts = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let n = self.conn.execute(
            "UPDATE directory_mappings
             SET target = ?2, profile_name = ?3, updated_at = ?4
             WHERE id = ?1",
            params![id, target, profile_name, ts],
        )?;
        if n == 0 {
            return Err(MappingsError::NotFound(id));
        }
        Ok(())
    }

    pub fn delete(&self, id: i64) -> Result<(), MappingsError> {
        let n = self
            .conn
            .execute("DELETE FROM directory_mappings WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(MappingsError::NotFound(id));
        }
        Ok(())
    }

    /// Find the best matching mapping for a directory.
    /// Returns the longest `dir_path` that is either equal to or a parent
    /// directory of `query`. Returns `None` if nothing matches.
    pub fn find_match(&self, query: &str) -> Result<Option<DirectoryMapping>, MappingsError> {
        if query.is_empty() {
            return Ok(None);
        }
        let normalized = normalize_path(query);
        let candidates = self.list()?;
        Ok(candidates.into_iter().find(|m| {
            normalized == m.dir_path || normalized.starts_with(&format!("{}/", m.dir_path))
        }))
    }
}

/// Strip a trailing slash so `/Users/me/work` and `/Users/me/work/` produce
/// the same key. Leaves `/` alone.
fn normalize_path(p: &str) -> String {
    let trimmed = p.trim();
    if trimmed.len() > 1 && trimmed.ends_with('/') {
        trimmed[..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_creates_row_with_timestamps() {
        let store = MappingsStore::in_memory().unwrap();
        let id = store
            .add("/Users/me/proj", "global", "quality-first")
            .unwrap();
        assert!(id > 0);
        let rows = store.list().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].dir_path, "/Users/me/proj");
        assert_eq!(rows[0].profile_name, "quality-first");
        assert_eq!(rows[0].created_at, rows[0].updated_at);
    }

    #[test]
    fn add_rejects_duplicate_dir_path() {
        let store = MappingsStore::in_memory().unwrap();
        store.add("/a", "global", "p").unwrap();
        let err = store.add("/a", "global", "q").unwrap_err();
        assert!(matches!(err, MappingsError::AlreadyExists(_)));
    }

    #[test]
    fn add_normalizes_trailing_slash() {
        let store = MappingsStore::in_memory().unwrap();
        store.add("/Users/me/proj/", "global", "p").unwrap();
        let rows = store.list().unwrap();
        assert_eq!(rows[0].dir_path, "/Users/me/proj");
    }

    #[test]
    fn update_changes_target_and_profile_and_bumps_timestamp() {
        let store = MappingsStore::in_memory().unwrap();
        let id = store.add("/a", "global", "p1").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.update(id, "memory:foo", "p2").unwrap();
        let rows = store.list().unwrap();
        assert_eq!(rows[0].target, "memory:foo");
        assert_eq!(rows[0].profile_name, "p2");
        assert_ne!(rows[0].created_at, rows[0].updated_at);
    }

    #[test]
    fn update_returns_not_found_for_unknown_id() {
        let store = MappingsStore::in_memory().unwrap();
        let err = store.update(9999, "global", "p").unwrap_err();
        assert!(matches!(err, MappingsError::NotFound(9999)));
    }

    #[test]
    fn delete_removes_row() {
        let store = MappingsStore::in_memory().unwrap();
        let id = store.add("/a", "global", "p").unwrap();
        store.delete(id).unwrap();
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn find_match_prefers_exact() {
        let store = MappingsStore::in_memory().unwrap();
        store.add("/Users/me", "global", "parent").unwrap();
        store.add("/Users/me/proj", "global", "exact").unwrap();
        let m = store.find_match("/Users/me/proj").unwrap().unwrap();
        assert_eq!(m.profile_name, "exact");
    }

    #[test]
    fn find_match_falls_back_to_longest_prefix() {
        let store = MappingsStore::in_memory().unwrap();
        store.add("/Users/me", "global", "outer").unwrap();
        store
            .add("/Users/me/work", "global", "inner")
            .unwrap();
        let m = store
            .find_match("/Users/me/work/deep/nested")
            .unwrap()
            .unwrap();
        assert_eq!(m.profile_name, "inner");
    }

    #[test]
    fn find_match_returns_none_when_no_rule_applies() {
        let store = MappingsStore::in_memory().unwrap();
        store.add("/a/b", "global", "p").unwrap();
        assert!(store.find_match("/c/d").unwrap().is_none());
    }

    #[test]
    fn find_match_normalizes_query_path() {
        let store = MappingsStore::in_memory().unwrap();
        store.add("/Users/me/proj", "global", "p").unwrap();
        let m = store.find_match("/Users/me/proj/").unwrap().unwrap();
        assert_eq!(m.profile_name, "p");
    }
}
