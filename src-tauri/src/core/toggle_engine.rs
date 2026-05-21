use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToggleError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("target file has no parent directory: {0}")]
    NoParent(PathBuf),
    #[error("profile file not found: {0}")]
    ProfileNotFound(PathBuf),
}

/// Atomically swaps the contents of a target file (e.g. `~/.claude/CLAUDE.md`)
/// with a profile file living next to it (e.g. `~/.claude/CLAUDE.md.quality-first`).
///
/// Backup of the original target is kept at `{target}.origin`. The backup is created
/// once on first toggle (idempotent) and preserved across subsequent toggles.
///
/// All writes go through a `{target}.tmp.{pid}` file then `fs::rename` to the final
/// path — POSIX `rename(2)` and Windows `MoveFileExW` are atomic when source and
/// destination live on the same filesystem.
pub struct ToggleEngine {
    target: PathBuf,
    backup: PathBuf,
}

impl ToggleEngine {
    pub fn new(target: impl Into<PathBuf>) -> Self {
        let target = target.into();
        let backup = sibling_with_suffix(&target, "origin");
        Self { target, backup }
    }

    pub fn target(&self) -> &Path {
        &self.target
    }

    pub fn backup(&self) -> &Path {
        &self.backup
    }

    /// Build the path for a named profile (e.g. "quality-first" -> `~/.claude/CLAUDE.md.quality-first`).
    pub fn profile_path(&self, name: &str) -> PathBuf {
        sibling_with_suffix(&self.target, name)
    }

    /// Create `{target}.origin` from current target contents, only if missing.
    /// Idempotent — safe to call on every app start.
    pub fn ensure_backup(&self) -> Result<(), ToggleError> {
        if self.backup.exists() {
            return Ok(());
        }
        if !self.target.exists() {
            // No target yet — nothing to back up. Caller may create one later.
            return Ok(());
        }
        let parent = self
            .backup
            .parent()
            .ok_or_else(|| ToggleError::NoParent(self.backup.clone()))?;
        fs::create_dir_all(parent)?;
        atomic_copy(&self.target, &self.backup)?;
        Ok(())
    }

    /// Apply the contents of `profile_path` to the target file via atomic rename.
    /// Caller is responsible for ensuring `ensure_backup()` has been called at least once.
    pub fn apply_profile(&self, profile_path: &Path) -> Result<(), ToggleError> {
        if !profile_path.exists() {
            return Err(ToggleError::ProfileNotFound(profile_path.to_path_buf()));
        }
        self.ensure_backup()?;
        atomic_copy(profile_path, &self.target)
    }

    /// Toggle by profile name. "origin" restores from backup; any other name
    /// resolves to `{target}.{name}`.
    pub fn apply_named(&self, name: &str) -> Result<(), ToggleError> {
        let path = if name == "origin" {
            self.backup.clone()
        } else {
            self.profile_path(name)
        };
        self.apply_profile(&path)
    }
}

/// Place `{name}` as a suffix on the target's filename. So
/// `/x/CLAUDE.md` + "origin" = `/x/CLAUDE.md.origin`.
fn sibling_with_suffix(target: &Path, suffix: &str) -> PathBuf {
    let parent = target.parent().unwrap_or_else(|| Path::new(""));
    let file_name = target
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    parent.join(format!("{}.{}", file_name, suffix))
}

/// Copy `src` to `dst` atomically: write to `{dst}.tmp.{pid}` then `fs::rename`.
fn atomic_copy(src: &Path, dst: &Path) -> Result<(), ToggleError> {
    let parent = dst
        .parent()
        .ok_or_else(|| ToggleError::NoParent(dst.to_path_buf()))?;
    let file_name = dst
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = parent.join(format!("{}.tmp.{}", file_name, std::process::id()));

    // Best-effort cleanup if a previous run crashed mid-write.
    let _ = fs::remove_file(&tmp);

    fs::copy(src, &tmp)?;
    match fs::rename(&tmp, dst) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn sibling_suffix_appends_after_full_filename() {
        let p = sibling_with_suffix(Path::new("/x/CLAUDE.md"), "origin");
        assert_eq!(p, PathBuf::from("/x/CLAUDE.md.origin"));
        let q = sibling_with_suffix(Path::new("/x/CLAUDE.md"), "quality-first");
        assert_eq!(q, PathBuf::from("/x/CLAUDE.md.quality-first"));
    }

    #[test]
    fn ensure_backup_creates_origin_once() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        fs::write(&target, "default").unwrap();

        let engine = ToggleEngine::new(target.clone());
        engine.ensure_backup().unwrap();

        let backup = dir.path().join("CLAUDE.md.origin");
        assert!(backup.exists());
        assert_eq!(read(&backup), "default");

        // Re-write target then call ensure_backup again — backup must NOT be overwritten.
        fs::write(&target, "modified").unwrap();
        engine.ensure_backup().unwrap();
        assert_eq!(
            read(&backup),
            "default",
            "ensure_backup must be idempotent and never overwrite existing origin"
        );
    }

    #[test]
    fn ensure_backup_noop_when_target_missing() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        let engine = ToggleEngine::new(target);
        engine.ensure_backup().unwrap();
        assert!(!dir.path().join("CLAUDE.md.origin").exists());
    }

    #[test]
    fn apply_named_swaps_target_with_profile_contents() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        let profile = dir.path().join("CLAUDE.md.quality-first");
        fs::write(&target, "default content").unwrap();
        fs::write(&profile, "quality content").unwrap();

        let engine = ToggleEngine::new(target.clone());
        engine.apply_named("quality-first").unwrap();

        assert_eq!(read(&target), "quality content");
        // Backup was created from the pre-toggle state.
        assert_eq!(read(&dir.path().join("CLAUDE.md.origin")), "default content");
    }

    #[test]
    fn apply_named_origin_restores_from_backup() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        let profile = dir.path().join("CLAUDE.md.speed-first");
        fs::write(&target, "default").unwrap();
        fs::write(&profile, "speed").unwrap();

        let engine = ToggleEngine::new(target.clone());
        engine.apply_named("speed-first").unwrap();
        assert_eq!(read(&target), "speed");

        engine.apply_named("origin").unwrap();
        assert_eq!(read(&target), "default", "origin must restore from backup");
    }

    #[test]
    fn apply_named_missing_profile_returns_error() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        fs::write(&target, "default").unwrap();

        let engine = ToggleEngine::new(target);
        let err = engine.apply_named("nonexistent").unwrap_err();
        assert!(matches!(err, ToggleError::ProfileNotFound(_)));
    }

    #[test]
    fn apply_named_leaves_no_tmp_file_after_success() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        let profile = dir.path().join("CLAUDE.md.unlimited");
        fs::write(&target, "x").unwrap();
        fs::write(&profile, "y").unwrap();

        let engine = ToggleEngine::new(target);
        engine.apply_named("unlimited").unwrap();

        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no .tmp.* files should remain after successful toggle"
        );
    }
}
