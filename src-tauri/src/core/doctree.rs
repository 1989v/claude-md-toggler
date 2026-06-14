//! Domain doc-tree selection + materialization (v0.3 — 21-2).
//!
//! Each `doctrees/{id}/` directory in the linked repo is a domain. The user picks
//! which domains they currently need (additive multi-select); each selected domain
//! contributes ONE `@import` pointer line into the composed active `CLAUDE.md`:
//!
//! ```text
//! @domains/kubernetes/INDEX.md
//! ```
//!
//! The doc-tree is COPIED (not symlinked — Windows-safe) into `~/.claude/domains/{id}/`
//! and Claude Code's recursive `@import` fans the tree out from `INDEX.md`. This is
//! a *content* modifier, orthogonal to #19's domain *level* (low/mid/high) modifier;
//! both pass through `core::composer` so their sentinel regions never collide.
//!
//! The domain `id` is attacker-influenced repo content (it becomes a filesystem
//! path AND an `@import` line), so it is re-validated with both `validate_name`
//! (charset) and `validate_lookup` (path traversal) before any path is built.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::{params, Connection, OpenFlags};
use serde::Serialize;
use thiserror::Error;

use crate::core::profile_store::{validate_lookup, validate_name};

/// Subdirectory under `~/.claude` where selected domain doc-trees are copied.
pub const DOMAINS_DIR_NAME: &str = "domains";
/// The single file the composer points `@import` at; everything else fans out
/// from it via Claude Code's recursive import.
pub const INDEX_NAME: &str = "INDEX.md";

#[derive(Debug, Error)]
pub enum DoctreeError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid domain id '{0}': {1}")]
    InvalidId(String, String),
    #[error("domain '{0}' not found in repo doctrees")]
    NotFound(String),
    #[error("domain '{0}' has no {1}")]
    MissingIndex(String, String),
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS doctree_selection (
    id          TEXT PRIMARY KEY,
    applied_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS project_doctree_selection (
    project_id  TEXT NOT NULL,
    doctree_id  TEXT NOT NULL,
    applied_at  TEXT NOT NULL,
    PRIMARY KEY (project_id, doctree_id)
);
"#;

/// Result of materializing one domain, including any non-fatal warnings (e.g. an
/// `@import` inside INDEX.md that points at a missing file).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DomainApply {
    pub id: String,
    /// The `@import` line contributed to the composed CLAUDE.md.
    pub import_line: String,
    /// Missing 1-hop `@import` targets referenced by INDEX.md — surfaced to the FE.
    pub warnings: Vec<String>,
}

/// Persisted set of currently-selected domain ids. Shares the
/// `.toggler-history.db` file with the other stores.
pub struct DoctreeStore {
    conn: Connection,
}

impl DoctreeStore {
    pub fn open(db_path: &Path) -> Result<Self, DoctreeError> {
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

    pub fn in_memory() -> Result<Self, DoctreeError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn list_selected(&self) -> Result<Vec<String>, DoctreeError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM doctree_selection ORDER BY id ASC")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Replace the whole selection with `ids` (clear + insert) in one transaction.
    pub fn set_selected(&self, ids: &[String]) -> Result<(), DoctreeError> {
        let ts = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.conn.execute("DELETE FROM doctree_selection", [])?;
        for id in ids {
            self.conn.execute(
                "INSERT OR REPLACE INTO doctree_selection (id, applied_at) VALUES (?1, ?2)",
                params![id, ts],
            )?;
        }
        Ok(())
    }

    pub fn clear(&self) -> Result<(), DoctreeError> {
        self.conn.execute("DELETE FROM doctree_selection", [])?;
        Ok(())
    }

    /// Per-project selection (v0.4). Keyed by project id so each project binds
    /// its own domain set without disturbing the global selection or other
    /// projects.
    pub fn list_selected_for(&self, project_id: &str) -> Result<Vec<String>, DoctreeError> {
        let mut stmt = self.conn.prepare(
            "SELECT doctree_id FROM project_doctree_selection
             WHERE project_id = ?1 ORDER BY doctree_id ASC",
        )?;
        let rows = stmt.query_map([project_id], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Replace a SINGLE project's selection (scoped `DELETE WHERE project_id`),
    /// never the global DELETE-all — so binding one project leaves the others
    /// intact.
    pub fn set_selected_for(&self, project_id: &str, ids: &[String]) -> Result<(), DoctreeError> {
        let ts = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.conn.execute(
            "DELETE FROM project_doctree_selection WHERE project_id = ?1",
            params![project_id],
        )?;
        for id in ids {
            self.conn.execute(
                "INSERT OR REPLACE INTO project_doctree_selection
                    (project_id, doctree_id, applied_at) VALUES (?1, ?2, ?3)",
                params![project_id, id, ts],
            )?;
        }
        Ok(())
    }
}

/// Validate a repo-sourced domain id before it is ever turned into a path or an
/// `@import` line. Must pass BOTH the charset rule and the traversal block.
pub fn validate_domain_id(id: &str) -> Result<(), DoctreeError> {
    validate_lookup(id).map_err(|e| DoctreeError::InvalidId(id.to_string(), e.to_string()))?;
    validate_name(id).map_err(|e| DoctreeError::InvalidId(id.to_string(), e.to_string()))?;
    Ok(())
}

/// The `~/.claude/domains/{id}` destination for a domain copy.
pub fn domain_dest(claude_dir: &Path, id: &str) -> PathBuf {
    claude_dir.join(DOMAINS_DIR_NAME).join(id)
}

/// The `@import` line a globally-materialized domain contributes (relative to
/// `~/.claude`, where the global `CLAUDE.md` lives).
pub fn import_line(id: &str) -> String {
    import_line_for(id, "")
}

/// The `@import` line for an arbitrary composition target, where `rel_prefix` is
/// the path from the importing file's directory to the domains root. Claude Code
/// resolves `@import` relative to the file containing it, so a per-project
/// `MEMORY.md` in `.../memory/` pointing at `.../domains/{id}/` uses `rel_prefix=".."`
/// → `@../domains/{id}/INDEX.md`. The global `CLAUDE.md` sits beside `domains/`
/// → empty prefix → `@domains/{id}/INDEX.md`.
pub fn import_line_for(id: &str, rel_prefix: &str) -> String {
    if rel_prefix.is_empty() {
        format!("@{}/{}/{}", DOMAINS_DIR_NAME, id, INDEX_NAME)
    } else {
        format!("@{}/{}/{}/{}", rel_prefix, DOMAINS_DIR_NAME, id, INDEX_NAME)
    }
}

/// Copy one domain doc-tree from the repo mirror into the GLOBAL
/// `~/.claude/domains/{id}` (v0.3 behavior). Delegates to `materialize_domain_into`.
pub fn materialize_domain(
    doctrees_dir: &Path,
    claude_dir: &Path,
    id: &str,
) -> Result<DomainApply, DoctreeError> {
    materialize_domain_into(doctrees_dir, &claude_dir.join(DOMAINS_DIR_NAME), id, "")
}

/// Copy one domain doc-tree from the repo mirror into `domains_root/{id}`,
/// validating the id and INDEX.md presence and walking INDEX.md's direct (1-hop)
/// `@import` references to warn about missing targets. `rel_prefix` shapes the
/// returned `@import` line for the composition target. The destination is
/// replaced wholesale so removing files in the repo propagates.
///
/// Note: this does plain filesystem mutation — the per-project caller must hold
/// that project's swap lock around it so a concurrent apply can't torn-read the
/// tree mid-copy.
pub fn materialize_domain_into(
    doctrees_dir: &Path,
    domains_root: &Path,
    id: &str,
    rel_prefix: &str,
) -> Result<DomainApply, DoctreeError> {
    validate_domain_id(id)?;
    let src = doctrees_dir.join(id);
    if !src.is_dir() {
        return Err(DoctreeError::NotFound(id.to_string()));
    }
    let index = src.join(INDEX_NAME);
    if !index.is_file() {
        return Err(DoctreeError::MissingIndex(id.to_string(), INDEX_NAME.into()));
    }

    let warnings = check_imports(&src, &index)?;

    let dest = domains_root.join(id);
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    copy_dir_all(&src, &dest)?;

    Ok(DomainApply {
        id: id.to_string(),
        import_line: import_line_for(id, rel_prefix),
        warnings,
    })
}

/// Remove GLOBAL `~/.claude/domains/{id}` directories whose id is not in `keep`.
pub fn gc_orphans(claude_dir: &Path, keep: &[String]) -> Result<Vec<String>, DoctreeError> {
    gc_orphans_in(&claude_dir.join(DOMAINS_DIR_NAME), keep)
}

/// Remove `{domains_root}/{id}` directories whose id is not in `keep`. Returns
/// the ids that were garbage-collected. Operates ONLY within `domains_root`, so a
/// per-project gc never touches another project's (or the global) domains.
pub fn gc_orphans_in(domains_root: &Path, keep: &[String]) -> Result<Vec<String>, DoctreeError> {
    if !domains_root.is_dir() {
        return Ok(Vec::new());
    }
    let mut removed = Vec::new();
    for entry in fs::read_dir(domains_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !keep.iter().any(|k| k == &name) {
            fs::remove_dir_all(entry.path())?;
            removed.push(name);
        }
    }
    removed.sort();
    Ok(removed)
}

/// Drop selection ids whose doctree no longer exists in the mirror (deleted from
/// the repo), so a dangling per-project selection can't wedge re-application with
/// a NotFound error. Returns the surviving ids (sorted, order-stable input kept).
pub fn prune_missing(doctrees_dir: &Path, ids: &[String]) -> Vec<String> {
    ids.iter()
        .filter(|id| doctrees_dir.join(id).join(INDEX_NAME).is_file())
        .cloned()
        .collect()
}

/// Scan INDEX.md for direct `@import` lines and return warnings for any whose
/// target file is missing. 1-hop only: we do not recurse into imported files.
fn check_imports(domain_root: &Path, index: &Path) -> Result<Vec<String>, DoctreeError> {
    let text = fs::read_to_string(index)?;
    let mut warnings = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        let Some(rel) = t.strip_prefix('@') else {
            continue;
        };
        let rel = rel.trim();
        if rel.is_empty() {
            continue;
        }
        // Block traversal out of the domain root; ignore such imports defensively.
        if rel.contains("..") {
            warnings.push(format!("ignored traversal import: {}", rel));
            continue;
        }
        let target = domain_root.join(rel);
        if !target.exists() {
            warnings.push(format!("missing import target: {}", rel));
        }
    }
    Ok(warnings)
}

fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else if ty.is_file() {
            fs::copy(&from, &to)?;
        }
        // symlinks in the repo are skipped (not followed) for safety.
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_domain(doctrees: &Path, id: &str, index_body: &str) {
        let d = doctrees.join(id);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(INDEX_NAME), index_body).unwrap();
    }

    #[test]
    fn validate_domain_id_blocks_traversal_and_bad_charset() {
        assert!(validate_domain_id("kubernetes").is_ok());
        for bad in ["../evil", "a/b", "..", "Upper", "with space", "origin", "composed"] {
            assert!(
                validate_domain_id(bad).is_err(),
                "expected '{}' rejected",
                bad
            );
        }
    }

    #[test]
    fn import_line_is_relative_to_claude_dir() {
        assert_eq!(import_line("trading"), "@domains/trading/INDEX.md");
    }

    #[test]
    fn materialize_copies_tree_and_returns_import_line() {
        let dir = tempdir().unwrap();
        let doctrees = dir.path().join("repo/doctrees");
        make_domain(&doctrees, "kubernetes", "# K8s\n@networking.md\n");
        fs::write(doctrees.join("kubernetes/networking.md"), "net").unwrap();

        let claude = dir.path().join(".claude");
        let applied = materialize_domain(&doctrees, &claude, "kubernetes").unwrap();

        assert_eq!(applied.import_line, "@domains/kubernetes/INDEX.md");
        assert!(applied.warnings.is_empty());
        assert!(claude.join("domains/kubernetes/INDEX.md").exists());
        assert!(claude.join("domains/kubernetes/networking.md").exists());
    }

    #[test]
    fn materialize_warns_on_missing_import_target() {
        let dir = tempdir().unwrap();
        let doctrees = dir.path().join("doctrees");
        make_domain(&doctrees, "spring", "# Spring\n@missing.md\n");
        let claude = dir.path().join(".claude");

        let applied = materialize_domain(&doctrees, &claude, "spring").unwrap();
        assert_eq!(applied.warnings.len(), 1);
        assert!(applied.warnings[0].contains("missing.md"));
    }

    #[test]
    fn materialize_errors_when_index_absent() {
        let dir = tempdir().unwrap();
        let doctrees = dir.path().join("doctrees");
        fs::create_dir_all(doctrees.join("noindex")).unwrap();
        let claude = dir.path().join(".claude");
        assert!(matches!(
            materialize_domain(&doctrees, &claude, "noindex"),
            Err(DoctreeError::MissingIndex(_, _))
        ));
    }

    #[test]
    fn materialize_rejects_traversal_id() {
        let dir = tempdir().unwrap();
        let doctrees = dir.path().join("doctrees");
        let claude = dir.path().join(".claude");
        assert!(matches!(
            materialize_domain(&doctrees, &claude, "../etc"),
            Err(DoctreeError::InvalidId(_, _))
        ));
    }

    #[test]
    fn materialize_replaces_existing_dest() {
        let dir = tempdir().unwrap();
        let doctrees = dir.path().join("doctrees");
        make_domain(&doctrees, "k8s", "v2\n");
        let claude = dir.path().join(".claude");
        // pre-existing stale copy with an extra file that must disappear
        let dest = claude.join("domains/k8s");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("stale.md"), "old").unwrap();

        materialize_domain(&doctrees, &claude, "k8s").unwrap();
        assert!(!claude.join("domains/k8s/stale.md").exists());
        assert_eq!(
            fs::read_to_string(claude.join("domains/k8s/INDEX.md")).unwrap(),
            "v2\n"
        );
    }

    #[test]
    fn gc_removes_unselected_domains() {
        let dir = tempdir().unwrap();
        let claude = dir.path().join(".claude");
        for id in ["kubernetes", "trading", "spring"] {
            fs::create_dir_all(claude.join("domains").join(id)).unwrap();
        }
        let removed = gc_orphans(&claude, &["kubernetes".to_string()]).unwrap();
        assert_eq!(removed, vec!["spring".to_string(), "trading".to_string()]);
        assert!(claude.join("domains/kubernetes").exists());
        assert!(!claude.join("domains/trading").exists());
    }

    #[test]
    fn store_persists_and_replaces_selection() {
        let store = DoctreeStore::in_memory().unwrap();
        assert!(store.list_selected().unwrap().is_empty());
        store
            .set_selected(&["kubernetes".into(), "trading".into()])
            .unwrap();
        assert_eq!(
            store.list_selected().unwrap(),
            vec!["kubernetes".to_string(), "trading".to_string()]
        );
        // replace, not append
        store.set_selected(&["spring".into()]).unwrap();
        assert_eq!(store.list_selected().unwrap(), vec!["spring".to_string()]);
    }

    // --- v0.4 per-project ---------------------------------------------------

    #[test]
    fn import_line_for_global_and_project_relative() {
        assert_eq!(import_line_for("k8s", ""), "@domains/k8s/INDEX.md");
        assert_eq!(import_line_for("k8s", ".."), "@../domains/k8s/INDEX.md");
    }

    #[test]
    fn materialize_into_uses_project_root_and_relative_import() {
        let dir = tempdir().unwrap();
        let doctrees = dir.path().join("doctrees");
        make_domain(&doctrees, "kubernetes", "# k8s\n");
        let proj_domains = dir.path().join("projects/p1/domains");

        let a = materialize_domain_into(&doctrees, &proj_domains, "kubernetes", "..").unwrap();
        assert_eq!(a.import_line, "@../domains/kubernetes/INDEX.md");
        assert!(proj_domains.join("kubernetes/INDEX.md").exists());
    }

    #[test]
    fn gc_in_isolates_to_its_root() {
        let dir = tempdir().unwrap();
        let root_a = dir.path().join("a/domains");
        let root_b = dir.path().join("b/domains");
        for r in [&root_a, &root_b] {
            fs::create_dir_all(r.join("kubernetes")).unwrap();
            fs::create_dir_all(r.join("trading")).unwrap();
        }
        let removed = gc_orphans_in(&root_a, &["kubernetes".to_string()]).unwrap();
        assert_eq!(removed, vec!["trading".to_string()]);
        assert!(root_b.join("trading").exists(), "other root untouched");
    }

    #[test]
    fn prune_missing_drops_absent_doctrees() {
        let dir = tempdir().unwrap();
        let doctrees = dir.path().join("doctrees");
        make_domain(&doctrees, "kubernetes", "x");
        let kept = prune_missing(&doctrees, &["kubernetes".into(), "deleted".into()]);
        assert_eq!(kept, vec!["kubernetes".to_string()]);
    }

    #[test]
    fn project_selection_is_scoped_per_project() {
        let store = DoctreeStore::in_memory().unwrap();
        store
            .set_selected_for("projA", &["kubernetes".into(), "trading".into()])
            .unwrap();
        store.set_selected_for("projB", &["spring".into()]).unwrap();
        assert_eq!(
            store.list_selected_for("projA").unwrap(),
            vec!["kubernetes".to_string(), "trading".to_string()]
        );
        assert_eq!(
            store.list_selected_for("projB").unwrap(),
            vec!["spring".to_string()]
        );
        // re-setting A must not disturb B or the global selection.
        store.set_selected_for("projA", &["redis".into()]).unwrap();
        assert_eq!(store.list_selected_for("projA").unwrap(), vec!["redis".to_string()]);
        assert_eq!(store.list_selected_for("projB").unwrap(), vec!["spring".to_string()]);
        assert!(store.list_selected().unwrap().is_empty());
    }
}
