//! Drift detection: compares the active `CLAUDE.md` against the profile file
//! that was last activated. A non-empty diff means the user has hand-edited
//! the file outside the toggler — the FE surfaces this as a 4-button dialog
//! before applying any further toggle.

use std::fs;
use std::path::Path;

use serde::Serialize;
use similar::TextDiff;

#[derive(Debug, Clone, Serialize)]
pub struct DriftInfo {
    /// Profile name that was last activated (e.g. "origin", "quality-first").
    pub last_active: String,
    /// Current bytes of the active target file.
    pub current_content: String,
    /// Bytes of the profile file that was last applied — the expected state.
    pub expected_content: String,
    /// Unified diff (expected → current), suitable for direct display.
    pub unified_diff: String,
}

/// Returns `Some(DriftInfo)` when the contents of `active_target` differ from
/// `expected_profile_path`. Both files must exist. Returns `None` when contents
/// match or when either file cannot be read.
pub fn detect(
    last_active: &str,
    active_target: &Path,
    expected_profile_path: &Path,
) -> Option<DriftInfo> {
    let current = fs::read_to_string(active_target).ok()?;
    let expected = fs::read_to_string(expected_profile_path).ok()?;
    if current == expected {
        return None;
    }
    let diff = TextDiff::from_lines(&expected, &current);
    let unified_diff = diff
        .unified_diff()
        .header(
            &format!("expected ({})", last_active),
            "current (CLAUDE.md)",
        )
        .to_string();
    Some(DriftInfo {
        last_active: last_active.to_string(),
        current_content: current,
        expected_content: expected,
        unified_diff,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
    }

    #[test]
    fn detect_returns_none_when_files_match() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        let profile = dir.path().join("CLAUDE.md.q");
        write(&target, "same content\n");
        write(&profile, "same content\n");
        assert!(detect("q", &target, &profile).is_none());
    }

    #[test]
    fn detect_returns_some_with_diff_when_target_drifted() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        let profile = dir.path().join("CLAUDE.md.q");
        write(&target, "edited line one\nedited line two\n");
        write(&profile, "original line one\noriginal line two\n");

        let info = detect("q", &target, &profile).unwrap();
        assert_eq!(info.last_active, "q");
        assert!(info.unified_diff.contains("-original"));
        assert!(info.unified_diff.contains("+edited"));
        assert_eq!(info.current_content, "edited line one\nedited line two\n");
    }

    #[test]
    fn detect_returns_none_when_target_missing() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        let profile = dir.path().join("CLAUDE.md.q");
        write(&profile, "x");
        assert!(detect("q", &target, &profile).is_none());
    }

    #[test]
    fn detect_returns_none_when_profile_missing() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        let profile = dir.path().join("CLAUDE.md.q");
        write(&target, "x");
        assert!(detect("q", &target, &profile).is_none());
    }
}
