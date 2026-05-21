//! Seed pre-defined profile files into the user's Claude directory on first run.
//!
//! The preset content is embedded into the binary at compile time, then written to
//! `~/.claude/CLAUDE.md.{name}` only when the target file does not already exist —
//! so users who customize a preset will never have their edits clobbered.

use std::fs;
use std::io;
use std::path::Path;

const PRESETS: &[(&str, &str)] = &[
    (
        "token-save",
        include_str!("../../presets/CLAUDE.md.token-save"),
    ),
    (
        "speed-first",
        include_str!("../../presets/CLAUDE.md.speed-first"),
    ),
    (
        "quality-first",
        include_str!("../../presets/CLAUDE.md.quality-first"),
    ),
    (
        "unlimited",
        include_str!("../../presets/CLAUDE.md.unlimited"),
    ),
];

/// For each preset, write `{base_dir}/{target_name}.{preset}` if it does not exist.
/// Returns the list of newly created profile names.
pub fn seed_presets(base_dir: &Path, target_name: &str) -> io::Result<Vec<String>> {
    fs::create_dir_all(base_dir)?;
    let mut created = Vec::new();
    for (name, content) in PRESETS {
        let path = base_dir.join(format!("{}.{}", target_name, name));
        if path.exists() {
            continue;
        }
        fs::write(&path, content)?;
        created.push((*name).to_string());
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn seed_writes_all_four_presets_on_first_run() {
        let dir = tempdir().unwrap();
        let created = seed_presets(dir.path(), "CLAUDE.md").unwrap();
        assert_eq!(created.len(), 4);
        for name in ["token-save", "speed-first", "quality-first", "unlimited"] {
            let path = dir.path().join(format!("CLAUDE.md.{}", name));
            assert!(path.exists(), "expected {} to exist", path.display());
            let content = fs::read_to_string(&path).unwrap();
            assert!(!content.is_empty(), "preset {} should not be empty", name);
        }
    }

    #[test]
    fn seed_skips_existing_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md.quality-first");
        fs::write(&path, "user customization").unwrap();

        let created = seed_presets(dir.path(), "CLAUDE.md").unwrap();
        assert!(!created.contains(&"quality-first".to_string()));
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(
            content, "user customization",
            "existing user file must not be overwritten"
        );
    }

    #[test]
    fn seed_creates_missing_base_dir() {
        let parent = tempdir().unwrap();
        let base = parent.path().join("nested/.claude");
        seed_presets(&base, "CLAUDE.md").unwrap();
        assert!(base.exists());
    }
}
