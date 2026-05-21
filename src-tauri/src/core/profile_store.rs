use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

/// Reserved suffixes that must never be used as user-facing profile names.
/// `origin` is the backup of the default state; `tmp` is the in-flight swap prefix.
pub const RESERVED_NAMES: &[&str] = &["origin", "tmp"];

const MAX_NAME_LEN: usize = 64;

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("profile name is empty")]
    EmptyName,
    #[error("profile name '{0}' exceeds {1} characters")]
    NameTooLong(String, usize),
    #[error(
        "profile name '{0}' contains invalid characters (only a-z, 0-9, '-' allowed, lowercase)"
    )]
    InvalidName(String),
    #[error("'{0}' is a reserved profile name")]
    ReservedName(String),
    #[error("profile '{0}' already exists")]
    AlreadyExists(String),
    #[error("profile '{0}' not found")]
    NotFound(String),
}

/// Validates a user-supplied profile name. Returns the name back on success.
/// Allowed: 1..=64 chars of [a-z0-9-]. Reserved: "origin", "tmp" and any name
/// starting with "tmp." (used internally by the toggle engine for swap files).
pub fn validate_name(name: &str) -> Result<&str, ProfileError> {
    if name.is_empty() {
        return Err(ProfileError::EmptyName);
    }
    if name.len() > MAX_NAME_LEN {
        return Err(ProfileError::NameTooLong(name.to_string(), MAX_NAME_LEN));
    }
    // Reserved check runs before character validation so callers that pass in
    // an internal swap-file pattern ("tmp.{pid}") get a semantic ReservedName
    // error rather than an InvalidName error caused by the embedded dot.
    if RESERVED_NAMES.contains(&name) || name.starts_with("tmp.") {
        return Err(ProfileError::ReservedName(name.to_string()));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(ProfileError::InvalidName(name.to_string()));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(ProfileError::InvalidName(name.to_string()));
    }
    Ok(name)
}

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

    /// Read the raw contents of a profile. `name` may be "origin" or any user
    /// profile suffix; path-traversal characters are rejected.
    pub fn read(&self, name: &str) -> Result<String, ProfileError> {
        validate_lookup(name)?;
        let path = self.profile_path(name);
        if !path.exists() {
            return Err(ProfileError::NotFound(name.to_string()));
        }
        Ok(fs::read_to_string(&path)?)
    }

    /// Create a new user profile file. Rejects reserved names, invalid characters,
    /// and existing profiles. Does not touch the active target — only creates the
    /// `CLAUDE.md.{name}` file; the user must explicitly toggle it.
    pub fn create(&self, name: &str, content: &str) -> Result<(), ProfileError> {
        validate_name(name)?;
        let path = self.profile_path(name);
        if path.exists() {
            return Err(ProfileError::AlreadyExists(name.to_string()));
        }
        fs::create_dir_all(&self.base_dir)?;
        fs::write(&path, content)?;
        Ok(())
    }

    /// Overwrite an existing profile's contents. Rejects writes to "origin"
    /// (use `ToggleEngine` to update the backup snapshot intentionally) and
    /// missing profiles.
    pub fn write(&self, name: &str, content: &str) -> Result<(), ProfileError> {
        validate_name(name)?;
        let path = self.profile_path(name);
        if !path.exists() {
            return Err(ProfileError::NotFound(name.to_string()));
        }
        fs::write(&path, content)?;
        Ok(())
    }

    /// Delete a user profile. Reserved names cannot be deleted through this API.
    pub fn delete(&self, name: &str) -> Result<(), ProfileError> {
        validate_name(name)?;
        let path = self.profile_path(name);
        if !path.exists() {
            return Err(ProfileError::NotFound(name.to_string()));
        }
        fs::remove_file(&path)?;
        Ok(())
    }

    /// Rename an existing user profile.
    pub fn rename(&self, old: &str, new: &str) -> Result<(), ProfileError> {
        validate_name(old)?;
        validate_name(new)?;
        let old_path = self.profile_path(old);
        let new_path = self.profile_path(new);
        if !old_path.exists() {
            return Err(ProfileError::NotFound(old.to_string()));
        }
        if new_path.exists() {
            return Err(ProfileError::AlreadyExists(new.to_string()));
        }
        fs::rename(&old_path, &new_path)?;
        Ok(())
    }

    /// Copy an existing profile to a new name. `source` may be "origin"; `new`
    /// must be a fully-valid user profile name.
    pub fn duplicate(&self, source: &str, new: &str) -> Result<(), ProfileError> {
        validate_lookup(source)?;
        validate_name(new)?;
        let source_path = self.profile_path(source);
        let new_path = self.profile_path(new);
        if !source_path.exists() {
            return Err(ProfileError::NotFound(source.to_string()));
        }
        if new_path.exists() {
            return Err(ProfileError::AlreadyExists(new.to_string()));
        }
        fs::copy(&source_path, &new_path)?;
        Ok(())
    }
}

/// Looser validation that permits "origin" and any name passing `validate_name`,
/// but always blocks path separators and parent-traversal sequences.
fn validate_lookup(name: &str) -> Result<&str, ProfileError> {
    if name.is_empty() {
        return Err(ProfileError::EmptyName);
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(ProfileError::InvalidName(name.to_string()));
    }
    if name == "origin" {
        return Ok(name);
    }
    validate_name(name)
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

    #[test]
    fn validate_name_accepts_lowercase_alnum_and_hyphen() {
        assert!(validate_name("quality-first").is_ok());
        assert!(validate_name("a").is_ok());
        assert!(validate_name("p1").is_ok());
        assert!(validate_name("a-b-c-1-2-3").is_ok());
    }

    #[test]
    fn validate_name_rejects_reserved() {
        assert!(matches!(
            validate_name("origin"),
            Err(ProfileError::ReservedName(_))
        ));
        assert!(matches!(
            validate_name("tmp"),
            Err(ProfileError::ReservedName(_))
        ));
        assert!(matches!(
            validate_name("tmp.12345"),
            Err(ProfileError::ReservedName(_))
        ));
    }

    #[test]
    fn validate_name_rejects_invalid_characters() {
        for bad in [
            "Quality",
            "quality_first",
            "quality first",
            "quality.first",
            "한글",
            "-leading",
            "trailing-",
        ] {
            assert!(
                matches!(validate_name(bad), Err(ProfileError::InvalidName(_))),
                "expected '{}' to be rejected as invalid",
                bad
            );
        }
    }

    #[test]
    fn validate_name_rejects_empty_and_too_long() {
        assert!(matches!(validate_name(""), Err(ProfileError::EmptyName)));
        let long = "a".repeat(65);
        assert!(matches!(
            validate_name(&long),
            Err(ProfileError::NameTooLong(_, _))
        ));
    }

    #[test]
    fn create_writes_new_profile_file() {
        let dir = tempdir().unwrap();
        let store = ProfileStore::new(dir.path().to_path_buf(), "CLAUDE.md");
        store.create("my-profile", "# hello").unwrap();
        let path = dir.path().join("CLAUDE.md.my-profile");
        assert_eq!(fs::read_to_string(path).unwrap(), "# hello");
    }

    #[test]
    fn create_rejects_existing_profile() {
        let dir = tempdir().unwrap();
        let store = ProfileStore::new(dir.path().to_path_buf(), "CLAUDE.md");
        store.create("dup", "x").unwrap();
        let err = store.create("dup", "y").unwrap_err();
        assert!(matches!(err, ProfileError::AlreadyExists(_)));
    }

    #[test]
    fn write_updates_existing_profile() {
        let dir = tempdir().unwrap();
        let store = ProfileStore::new(dir.path().to_path_buf(), "CLAUDE.md");
        store.create("p", "old").unwrap();
        store.write("p", "new").unwrap();
        assert_eq!(store.read("p").unwrap(), "new");
    }

    #[test]
    fn write_rejects_missing_profile() {
        let dir = tempdir().unwrap();
        let store = ProfileStore::new(dir.path().to_path_buf(), "CLAUDE.md");
        let err = store.write("absent", "x").unwrap_err();
        assert!(matches!(err, ProfileError::NotFound(_)));
    }

    #[test]
    fn delete_removes_profile_file() {
        let dir = tempdir().unwrap();
        let store = ProfileStore::new(dir.path().to_path_buf(), "CLAUDE.md");
        store.create("p", "x").unwrap();
        store.delete("p").unwrap();
        assert!(!dir.path().join("CLAUDE.md.p").exists());
    }

    #[test]
    fn delete_rejects_reserved_names() {
        let dir = tempdir().unwrap();
        // pre-create CLAUDE.md.origin so that deletion would otherwise have something to remove
        write(dir.path(), "CLAUDE.md.origin", "default");
        let store = ProfileStore::new(dir.path().to_path_buf(), "CLAUDE.md");
        let err = store.delete("origin").unwrap_err();
        assert!(matches!(err, ProfileError::ReservedName(_)));
        // file must still exist
        assert!(dir.path().join("CLAUDE.md.origin").exists());
    }

    #[test]
    fn rename_moves_profile() {
        let dir = tempdir().unwrap();
        let store = ProfileStore::new(dir.path().to_path_buf(), "CLAUDE.md");
        store.create("old", "x").unwrap();
        store.rename("old", "new").unwrap();
        assert!(!dir.path().join("CLAUDE.md.old").exists());
        assert!(dir.path().join("CLAUDE.md.new").exists());
    }

    #[test]
    fn duplicate_copies_origin_to_user_profile() {
        let dir = tempdir().unwrap();
        write(dir.path(), "CLAUDE.md.origin", "default");
        let store = ProfileStore::new(dir.path().to_path_buf(), "CLAUDE.md");
        store.duplicate("origin", "my-copy").unwrap();
        assert_eq!(store.read("my-copy").unwrap(), "default");
    }
}
