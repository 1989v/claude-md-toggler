//! Per-project memory toggling. The Claude Code memory file lives at
//! `~/.claude/projects/{project-id}/memory/MEMORY.md`, where `project-id` is
//! the escaped working-directory path. We reuse the suffix convention from
//! the global flow: `MEMORY.md.origin` for the backup and `MEMORY.md.{name}`
//! for each user-defined profile.
//!
//! This module is responsible for two things:
//!   1. Discovering which projects on disk currently have a memory file.
//!   2. Building a `ProfileStore` + `ToggleEngine` pair for a given project,
//!      so the existing IPC plumbing can drive memory toggling without any
//!      new state types in `AppState`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::profile_store::ProfileStore;
use crate::core::toggle_engine::ToggleEngine;

pub const MEMORY_TARGET_NAME: &str = "MEMORY.md";

#[derive(Debug, Clone, Serialize)]
pub struct MemoryProject {
    /// Escaped CWD identifier as it appears on disk (e.g.
    /// `-Users-gideok-kwon-IdeaProjects-msa`).
    pub id: String,
    /// Best-effort human label derived by un-escaping the id back to a path.
    /// e.g. `/Users/gideok-kwon/IdeaProjects/msa`.
    pub label: String,
    /// Absolute path of the project's `MEMORY.md` file.
    pub memory_path: PathBuf,
    /// True when `memory/MEMORY.md` actually exists. Projects without a
    /// memory file are still listed so the user can create one — but the FE
    /// can choose to filter them out if it prefers.
    pub has_memory_file: bool,
}

/// Lists every immediate subdirectory of `{claude_dir}/projects/` that looks
/// like a Claude Code project. Returns an empty list if the directory itself
/// is missing.
pub fn list_projects(claude_dir: &Path) -> io::Result<Vec<MemoryProject>> {
    let projects_dir = claude_dir.join("projects");
    if !projects_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&projects_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        let memory_path = entry.path().join("memory").join(MEMORY_TARGET_NAME);
        let has_memory_file = memory_path.exists();
        out.push(MemoryProject {
            label: unescape_id(&id),
            id,
            memory_path,
            has_memory_file,
        });
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(out)
}

/// Build the memory directory path for a project, without verifying its
/// existence on disk.
pub fn memory_dir_for(claude_dir: &Path, project_id: &str) -> PathBuf {
    claude_dir
        .join("projects")
        .join(project_id)
        .join("memory")
}

/// `ProfileStore` aimed at the project's `MEMORY.md`.
pub fn store_for(claude_dir: &Path, project_id: &str) -> ProfileStore {
    ProfileStore::new(memory_dir_for(claude_dir, project_id), MEMORY_TARGET_NAME)
}

/// `ToggleEngine` aimed at the project's `MEMORY.md`. Picks up its own lock
/// path under the memory directory, so memory toggling can't conflict with
/// the global `CLAUDE.md` lock.
pub fn engine_for(claude_dir: &Path, project_id: &str) -> ToggleEngine {
    ToggleEngine::new(memory_dir_for(claude_dir, project_id).join(MEMORY_TARGET_NAME))
}

/// Reverse the Claude Code path-escaping: the on-disk id is the absolute path
/// with `/` replaced by `-` (and a leading `-` standing in for the root slash).
/// We can't perfectly recover the original (a literal `-` in the path becomes
/// ambiguous), but for the standard macOS/Linux layout `/-...` round-trips to
/// the obvious path which is what users want to see.
fn unescape_id(id: &str) -> String {
    if id.starts_with('-') {
        let body = &id[1..];
        format!("/{}", body.replace('-', "/"))
    } else {
        id.replace('-', "/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn list_projects_returns_empty_when_dir_missing() {
        let dir = tempdir().unwrap();
        let out = list_projects(dir.path()).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn list_projects_discovers_projects_with_or_without_memory_file() {
        let dir = tempdir().unwrap();
        let projects = dir.path().join("projects");
        fs::create_dir_all(projects.join("-Users-alice-foo/memory")).unwrap();
        fs::write(
            projects.join("-Users-alice-foo/memory/MEMORY.md"),
            "alice's memory",
        )
        .unwrap();
        // Project without a memory file yet.
        fs::create_dir_all(projects.join("-tmp-empty")).unwrap();

        let out = list_projects(dir.path()).unwrap();
        assert_eq!(out.len(), 2);

        let alice = out.iter().find(|p| p.id == "-Users-alice-foo").unwrap();
        assert!(alice.has_memory_file);
        assert_eq!(alice.label, "/Users/alice/foo");

        let empty = out.iter().find(|p| p.id == "-tmp-empty").unwrap();
        assert!(!empty.has_memory_file);
    }

    #[test]
    fn engine_for_uses_memory_md_as_target() {
        let dir = tempdir().unwrap();
        let engine = engine_for(dir.path(), "-Users-bob");
        let expected = dir
            .path()
            .join("projects")
            .join("-Users-bob")
            .join("memory")
            .join("MEMORY.md");
        assert_eq!(engine.target(), expected);
    }

    #[test]
    fn unescape_id_recovers_root_path() {
        assert_eq!(unescape_id("-Users-bob-proj"), "/Users/bob/proj");
        assert_eq!(unescape_id("relative-path"), "relative/path");
    }
}
