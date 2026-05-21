use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Information about a single profile file (`CLAUDE.md.{name}`).
#[derive(Debug, Clone, Serialize)]
pub struct ProfileInfo {
    pub name: String,
    pub path: PathBuf,
    pub is_active: bool,
}

/// Scans a directory for profile files matching `{target_name}.{suffix}` and
/// reports which one (if any) has identical contents to the active `{target_name}`.
pub struct ProfileStore {
    base_dir: PathBuf,
    target_name: String,
}

impl ProfileStore {
    pub fn new(base_dir: impl Into<PathBuf>, target_name: impl Into<String>) -> Self {
        Self {
            base_dir: base_dir.into(),
            target_name: target_name.into(),
        }
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn target_path(&self) -> PathBuf {
        self.base_dir.join(&self.target_name)
    }

    pub fn profile_path(&self, name: &str) -> PathBuf {
        self.base_dir.join(format!("{}.{}", self.target_name, name))
    }

    /// List all profile files in `base_dir` matching the prefix `{target_name}.`,
    /// excluding `.tmp.*` swap files. Each entry's `is_active` is true when the
    /// file's contents are byte-identical to the current `{target_name}`.
    pub fn list(&self) -> io::Result<Vec<ProfileInfo>> {
        if !self.base_dir.exists() {
            return Ok(Vec::new());
        }

        let prefix = format!("{}.", self.target_name);
        let active_bytes = fs::read(self.target_path()).ok();

        let mut out = Vec::new();
        for entry in fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let file_name = entry.file_name();
            let Some(name_str) = file_name.to_str() else {
                continue;
            };
            let Some(suffix) = name_str.strip_prefix(&prefix) else {
                continue;
            };
            // Skip the in-flight swap files used by the toggle engine.
            if suffix.starts_with("tmp.") {
                continue;
            }

            let path = entry.path();
            let is_active = match (&active_bytes, fs::read(&path).ok()) {
                (Some(a), Some(b)) => a == &b,
                _ => false,
            };

            out.push(ProfileInfo {
                name: suffix.to_string(),
                path,
                is_active,
            });
        }

        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Detect which profile (if any) the current `{target_name}` matches.
    /// Returns the profile name (e.g. "origin", "quality-first") or "modified"
    /// when no profile matches and "none" when the target file is absent.
    pub fn detect_active(&self) -> io::Result<String> {
        let target = self.target_path();
        if !target.exists() {
            return Ok("none".to_string());
        }
        for prof in self.list()? {
            if prof.is_active {
                return Ok(prof.name);
            }
        }
        Ok("modified".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn list_returns_empty_when_base_dir_missing() {
        let dir = tempdir().unwrap();
        let store = ProfileStore::new(dir.path().join("absent"), "CLAUDE.md");
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn list_finds_profiles_by_prefix_and_skips_tmp_files() {
        let dir = tempdir().unwrap();
        write(dir.path(), "CLAUDE.md", "active");
        write(dir.path(), "CLAUDE.md.origin", "active"); // matches active
        write(dir.path(), "CLAUDE.md.quality-first", "quality");
        write(dir.path(), "CLAUDE.md.tmp.12345", "swap"); // must be ignored
        write(dir.path(), "NOTES.md", "unrelated"); // wrong prefix
        write(dir.path(), "OTHER.md.origin", "wrong target name"); // wrong target

        let store = ProfileStore::new(dir.path().to_path_buf(), "CLAUDE.md");
        let names: Vec<_> = store
            .list()
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["origin", "quality-first"]);
    }

    #[test]
    fn list_marks_matching_profile_as_active() {
        let dir = tempdir().unwrap();
        write(dir.path(), "CLAUDE.md", "speed-content");
        write(dir.path(), "CLAUDE.md.origin", "default");
        write(dir.path(), "CLAUDE.md.speed-first", "speed-content");

        let store = ProfileStore::new(dir.path().to_path_buf(), "CLAUDE.md");
        let profs = store.list().unwrap();
        let speed = profs.iter().find(|p| p.name == "speed-first").unwrap();
        let origin = profs.iter().find(|p| p.name == "origin").unwrap();
        assert!(speed.is_active);
        assert!(!origin.is_active);
    }

    #[test]
    fn detect_active_returns_modified_when_no_profile_matches() {
        let dir = tempdir().unwrap();
        write(dir.path(), "CLAUDE.md", "hand-edited content");
        write(dir.path(), "CLAUDE.md.origin", "default");

        let store = ProfileStore::new(dir.path().to_path_buf(), "CLAUDE.md");
        assert_eq!(store.detect_active().unwrap(), "modified");
    }

    #[test]
    fn detect_active_returns_none_when_target_missing() {
        let dir = tempdir().unwrap();
        write(dir.path(), "CLAUDE.md.origin", "default");

        let store = ProfileStore::new(dir.path().to_path_buf(), "CLAUDE.md");
        assert_eq!(store.detect_active().unwrap(), "none");
    }

    #[test]
    fn detect_active_returns_profile_name_on_match() {
        let dir = tempdir().unwrap();
        write(dir.path(), "CLAUDE.md", "default");
        write(dir.path(), "CLAUDE.md.origin", "default");

        let store = ProfileStore::new(dir.path().to_path_buf(), "CLAUDE.md");
        assert_eq!(store.detect_active().unwrap(), "origin");
    }
}
