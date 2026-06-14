//! Git transport for the linked context repo (v0.3 — 21-1).
//!
//! One HTTPS git repo holds the shareable toggler artifacts. It is cloned into a
//! hidden working tree `~/.claude/.toggler-sync/` (never `~/.claude` itself — a
//! stray `git clean` there would wipe unrelated Claude state). Profiles live under
//! `profiles/`, domain doc-trees under `doctrees/`, and a plain-text `manifest.toml`
//! indexes both plus the syncable slice of SQLite.
//!
//! Sync model is **repo-as-peer**: the working tree is a read-mostly mirror; the
//! user's real edits live in the flat `~/.claude/CLAUDE.md.{name}` files. Pull =
//! fetch + hard-reset the mirror to the remote head, then *materialize* mirror →
//! flat with per-file conflict classification (never a silent clobber). Push =
//! copy flat → mirror, commit, push (non-fast-forward is rejected, never forced).
//!
//! Credentials are resolved lazily in the git2 callback: first the OS credential
//! helper (reuses the token gh already stored in the keychain — zero config for
//! the common case), then a fine-grained PAT from the OS keychain. Nothing
//! credential-related is ever written to SQLite, the manifest, or the repo.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use git2::{
    build::CheckoutBuilder, build::RepoBuilder, CredentialType, FetchOptions, PushOptions,
    RemoteCallbacks, Repository, ResetType,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::core::profile_store::{validate_name, COMPOSED_NAME};

/// Hidden working-tree directory under `~/.claude`.
pub const SYNC_DIR_NAME: &str = ".toggler-sync";
pub const PROFILES_SUBDIR: &str = "profiles";
pub const DOCTREES_SUBDIR: &str = "doctrees";
pub const MANIFEST_NAME: &str = "manifest.toml";
/// Keychain service name for stored PATs.
const KEYRING_SERVICE: &str = "claude-md-toggler";

#[derive(Debug, Error)]
pub enum GitSyncError {
    #[error("git error: {0}")]
    Git(#[from] git2::Error),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("manifest parse error: {0}")]
    ManifestParse(String),
    #[error("not a linked working tree: {0}")]
    NotLinked(PathBuf),
}

// --- manifest -------------------------------------------------------------

/// Plain-text index committed to the repo as `manifest.toml`. Mirrors the
/// syncable slice of local SQLite (directory mappings, profile inventory,
/// doctree catalog). The binary `.toggler-history.db` is never committed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default, rename = "profile")]
    pub profiles: Vec<ProfileEntry>,
    #[serde(default, rename = "doctree")]
    pub doctrees: Vec<DoctreeEntry>,
    #[serde(default, rename = "mapping")]
    pub mappings: Vec<MappingEntry>,
}

fn default_schema_version() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileEntry {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctreeEntry {
    pub id: String,
    #[serde(default)]
    pub display: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub est_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MappingEntry {
    pub dir_path: String,
    pub target: String,
    pub profile_name: String,
}

pub fn parse_manifest(text: &str) -> Result<Manifest, GitSyncError> {
    toml::from_str(text).map_err(|e| GitSyncError::ManifestParse(e.to_string()))
}

pub fn render_manifest(manifest: &Manifest) -> Result<String, GitSyncError> {
    toml::to_string_pretty(manifest).map_err(|e| GitSyncError::ManifestParse(e.to_string()))
}

// --- materialize classification ------------------------------------------

/// Per-profile outcome when materializing the mirror onto the flat namespace.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum MaterializeOutcome {
    /// Flat file did not exist — written from the mirror.
    Created,
    /// Flat file already equals the mirror — nothing to do.
    Unchanged,
    /// Remote changed, local untouched since last sync — flat updated silently.
    FastForward,
    /// Both sides changed since last sync — surfaced to the drift dialog, flat
    /// left untouched until the user resolves it.
    Conflict {
        local: String,
        remote: String,
    },
    /// Mirror file name failed validation (e.g. uppercase, traversal, reserved) —
    /// skipped, never written to the flat namespace.
    Skipped {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct MaterializeEntry {
    pub name: String,
    pub outcome: MaterializeOutcome,
}

/// Classify a single profile against the flat namespace. `old_mirror` is the
/// mirror's content for this profile *before* the pull reset — used as the
/// per-file baseline to tell a remote-only change (fast-forward) from a true
/// two-sided conflict. `None` means the profile is new in the mirror.
fn classify_profile(
    flat: Option<&str>,
    new_mirror: &str,
    old_mirror: Option<&str>,
) -> MaterializeOutcome {
    match flat {
        None => MaterializeOutcome::Created,
        Some(flat) if flat == new_mirror => MaterializeOutcome::Unchanged,
        Some(flat) => {
            let local_untouched = old_mirror.map_or(false, |old| old == flat);
            if local_untouched {
                // Local matches the previously-synced version, only remote moved.
                MaterializeOutcome::FastForward
            } else {
                MaterializeOutcome::Conflict {
                    local: flat.to_string(),
                    remote: new_mirror.to_string(),
                }
            }
        }
    }
}

/// Derive a flat profile name from a mirror file name `CLAUDE.md.{name}`, or
/// `None` if it doesn't match the target prefix. Reserved / invalid / traversal
/// names are returned as `Some(Err(reason))` so the caller can record a Skip.
fn mirror_profile_name<'a>(
    file_name: &'a str,
    target_name: &str,
) -> Option<Result<&'a str, String>> {
    let prefix = format!("{}.", target_name);
    let suffix = file_name.strip_prefix(&prefix)?;
    if suffix.starts_with("tmp.") {
        return Some(Err("swap file".into()));
    }
    // origin and composed are per-machine and must never be materialized from
    // a remote (origin would overwrite the local pristine backup).
    if suffix == "origin" || suffix == COMPOSED_NAME {
        return Some(Err(format!("reserved name '{}'", suffix)));
    }
    match validate_name(suffix) {
        Ok(name) => Some(Ok(name)),
        Err(e) => Some(Err(e.to_string())),
    }
}

// --- git operations -------------------------------------------------------

pub struct GitSync {
    /// Working-tree path, e.g. `~/.claude/.toggler-sync`.
    worktree: PathBuf,
}

impl GitSync {
    pub fn new(claude_dir: &Path) -> Self {
        Self {
            worktree: claude_dir.join(SYNC_DIR_NAME),
        }
    }

    pub fn worktree(&self) -> &Path {
        &self.worktree
    }

    pub fn profiles_dir(&self) -> PathBuf {
        self.worktree.join(PROFILES_SUBDIR)
    }

    pub fn doctrees_dir(&self) -> PathBuf {
        self.worktree.join(DOCTREES_SUBDIR)
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.worktree.join(MANIFEST_NAME)
    }

    pub fn is_linked(&self) -> bool {
        Repository::open(&self.worktree).is_ok()
    }

    /// Clone the remote into the working tree, or open it if already present.
    /// Re-cloning over an existing different remote is the caller's decision; this
    /// opens whatever is there if a `.git` exists.
    pub fn clone_or_open(&self, remote_url: &str, branch: &str) -> Result<Repository, GitSyncError> {
        if self.worktree.join(".git").exists() {
            return Ok(Repository::open(&self.worktree)?);
        }
        if let Some(parent) = self.worktree.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut cbs = RemoteCallbacks::new();
        set_credentials(&mut cbs);
        let mut fo = FetchOptions::new();
        fo.remote_callbacks(cbs);
        let mut builder = RepoBuilder::new();
        builder.branch(branch);
        builder.fetch_options(fo);
        let repo = builder.clone(remote_url, &self.worktree)?;
        Ok(repo)
    }

    fn open(&self) -> Result<Repository, GitSyncError> {
        Repository::open(&self.worktree).map_err(|_| GitSyncError::NotLinked(self.worktree.clone()))
    }

    pub fn head_sha(&self) -> Result<String, GitSyncError> {
        let repo = self.open()?;
        let head = repo.head()?.peel_to_commit()?;
        Ok(head.id().to_string())
    }

    /// Fetch `origin/{branch}` and return the remote head sha (without touching
    /// the working tree). Caller decides whether to fast-forward.
    pub fn fetch_remote_head(&self, branch: &str) -> Result<String, GitSyncError> {
        let repo = self.open()?;
        let mut remote = repo.find_remote("origin")?;
        let mut cbs = RemoteCallbacks::new();
        set_credentials(&mut cbs);
        let mut fo = FetchOptions::new();
        fo.remote_callbacks(cbs);
        remote.fetch(&[branch], Some(&mut fo), None)?;
        let oid = repo.refname_to_id(&format!("refs/remotes/origin/{}", branch))?;
        Ok(oid.to_string())
    }

    /// Hard-reset the working tree (and the local branch ref) to the given commit.
    /// Safe because the mirror is never hand-edited — real edits live in the flat
    /// namespace. Returns the new head sha.
    pub fn reset_hard_to(&self, sha: &str) -> Result<String, GitSyncError> {
        let repo = self.open()?;
        let oid = git2::Oid::from_str(sha)?;
        let obj = repo.find_object(oid, None)?;
        repo.reset(&obj, ResetType::Hard, Some(CheckoutBuilder::new().force()))?;
        Ok(oid.to_string())
    }

    /// Stage everything in the working tree and commit on the current branch.
    /// Returns the new commit sha. No-op commit (empty tree delta) is allowed —
    /// the caller decides whether to push.
    pub fn commit_all(&self, message: &str) -> Result<String, GitSyncError> {
        let repo = self.open()?;
        let mut index = repo.index()?;
        index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
        index.write()?;
        let tree_oid = index.write_tree()?;
        let tree = repo.find_tree(tree_oid)?;
        let sig = repo
            .signature()
            .or_else(|_| git2::Signature::now("claude-md-toggler", "toggler@localhost"))?;
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let oid = repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?;
        Ok(oid.to_string())
    }

    /// Push the local branch to origin. Non-fast-forward pushes are rejected by
    /// the remote and surfaced as an error — never force-pushed.
    pub fn push(&self, branch: &str) -> Result<(), GitSyncError> {
        let repo = self.open()?;
        let mut remote = repo.find_remote("origin")?;
        let mut cbs = RemoteCallbacks::new();
        set_credentials(&mut cbs);
        let mut po = PushOptions::new();
        po.remote_callbacks(cbs);
        let refspec = format!("refs/heads/{0}:refs/heads/{0}", branch);
        remote.push(&[refspec.as_str()], Some(&mut po))?;
        Ok(())
    }

    /// Snapshot the current mirror profile contents (name → bytes) before a reset
    /// so the materialize step can use them as the per-file fast-forward baseline.
    pub fn snapshot_mirror_profiles(&self, target_name: &str) -> Vec<(String, String)> {
        read_profiles_dir(&self.profiles_dir(), target_name)
    }

    /// Copy the flat `~/.claude/CLAUDE.md.{name}` profiles into the mirror's
    /// `profiles/` dir ahead of a commit + push. `read_profiles_dir` already
    /// excludes origin / composed / swap / invalid names, so the per-machine
    /// backup and baseline never leave the machine.
    pub fn stage_flat_profiles(&self, flat_dir: &Path, target_name: &str) -> io::Result<()> {
        let profiles_dir = self.profiles_dir();
        fs::create_dir_all(&profiles_dir)?;
        for (name, content) in read_profiles_dir(flat_dir, target_name) {
            fs::write(
                profiles_dir.join(format!("{}.{}", target_name, name)),
                content,
            )?;
        }
        Ok(())
    }

    /// Classify how each mirror profile maps onto the flat namespace, and apply
    /// the non-conflicting ones (Created / FastForward) by writing the flat file.
    /// Conflicts are returned untouched for the FE drift dialog.
    pub fn materialize_profiles(
        &self,
        flat_dir: &Path,
        target_name: &str,
        old_mirror: &[(String, String)],
    ) -> Result<Vec<MaterializeEntry>, GitSyncError> {
        let profiles_dir = self.profiles_dir();
        if !profiles_dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&profiles_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let file_name = entry.file_name();
            let Some(name_str) = file_name.to_str() else {
                continue;
            };
            let Some(name_res) = mirror_profile_name(name_str, target_name) else {
                continue; // not a profile file
            };
            let name = match name_res {
                Ok(n) => n.to_string(),
                Err(reason) => {
                    out.push(MaterializeEntry {
                        name: name_str.to_string(),
                        outcome: MaterializeOutcome::Skipped { reason },
                    });
                    continue;
                }
            };
            let new_mirror = fs::read_to_string(entry.path())?;
            let flat_path = flat_dir.join(format!("{}.{}", target_name, name));
            let flat = fs::read_to_string(&flat_path).ok();
            let old = old_mirror
                .iter()
                .find(|(n, _)| n == &name)
                .map(|(_, c)| c.as_str());
            let outcome = classify_profile(flat.as_deref(), &new_mirror, old);
            match &outcome {
                MaterializeOutcome::Created | MaterializeOutcome::FastForward => {
                    fs::create_dir_all(flat_dir)?;
                    fs::write(&flat_path, &new_mirror)?;
                }
                _ => {}
            }
            out.push(MaterializeEntry { name, outcome });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }
}

/// Read all `{target_name}.{suffix}` profile files (excluding swap/origin/composed)
/// from a directory into (name, content) pairs.
fn read_profiles_dir(dir: &Path, target_name: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let file_name = entry.file_name();
        let Some(name_str) = file_name.to_str() else {
            continue;
        };
        let Some(Ok(name)) = mirror_profile_name(name_str, target_name) else {
            continue;
        };
        if let Ok(content) = fs::read_to_string(entry.path()) {
            out.push((name.to_string(), content));
        }
    }
    out
}

// --- credentials ----------------------------------------------------------

/// Configure the lazy credential resolution used by every authenticated git op.
/// Capped at two attempts per operation so a bad credential can't loop forever.
fn set_credentials(cbs: &mut RemoteCallbacks) {
    let mut attempts = 0;
    cbs.credentials(move |url, username_from_url, allowed| {
        attempts += 1;
        if attempts > 2 {
            return Err(git2::Error::from_str(
                "authentication failed (credential helper / keychain PAT exhausted)",
            ));
        }
        if allowed.contains(CredentialType::USER_PASS_PLAINTEXT) {
            // 1) OS credential helper — reuses the token gh stored in the keychain.
            if let Ok(cfg) = git2::Config::open_default() {
                if let Ok(cred) = git2::Cred::credential_helper(&cfg, url, username_from_url) {
                    return Ok(cred);
                }
            }
            // 2) Fine-grained PAT from the OS keychain.
            if let Some(pat) = pat_for(url) {
                return git2::Cred::userpass_plaintext(
                    username_from_url.unwrap_or("x-access-token"),
                    &pat,
                );
            }
        }
        if allowed.contains(CredentialType::DEFAULT) {
            return git2::Cred::default();
        }
        Err(git2::Error::from_str("no usable credential type offered"))
    });
}

/// Look up a stored PAT for `remote_url` in the OS keychain. Never logs the value.
pub fn pat_for(remote_url: &str) -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, remote_url)
        .ok()?
        .get_password()
        .ok()
}

/// Store a fine-grained PAT for `remote_url` in the OS keychain.
pub fn store_pat(remote_url: &str, pat: &str) -> Result<(), GitSyncError> {
    keyring::Entry::new(KEYRING_SERVICE, remote_url)
        .and_then(|e| e.set_password(pat))
        .map_err(|e| GitSyncError::ManifestParse(format!("keychain: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // --- manifest ---------------------------------------------------------

    #[test]
    fn manifest_roundtrips_through_toml() {
        let m = Manifest {
            schema_version: 3,
            profiles: vec![ProfileEntry {
                name: "quality-first".into(),
            }],
            doctrees: vec![DoctreeEntry {
                id: "kubernetes".into(),
                display: "Kubernetes".into(),
                tags: vec!["infra".into()],
                est_tokens: 4200,
            }],
            mappings: vec![MappingEntry {
                dir_path: "/Users/me/work/k8s".into(),
                target: "global".into(),
                profile_name: "quality-first".into(),
            }],
        };
        let text = render_manifest(&m).unwrap();
        let back = parse_manifest(&text).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn manifest_defaults_apply_to_sparse_toml() {
        let text = r#"
            [[profile]]
            name = "token-save"

            [[doctree]]
            id = "trading"
        "#;
        let m = parse_manifest(text).unwrap();
        assert_eq!(m.schema_version, 3);
        assert_eq!(m.profiles.len(), 1);
        assert_eq!(m.doctrees[0].id, "trading");
        assert_eq!(m.doctrees[0].est_tokens, 0);
        assert!(m.mappings.is_empty());
    }

    // --- classification ---------------------------------------------------

    #[test]
    fn classify_new_profile_is_created() {
        assert_eq!(
            classify_profile(None, "remote", None),
            MaterializeOutcome::Created
        );
    }

    #[test]
    fn classify_identical_is_unchanged() {
        assert_eq!(
            classify_profile(Some("same"), "same", Some("same")),
            MaterializeOutcome::Unchanged
        );
    }

    #[test]
    fn classify_remote_only_change_is_fast_forward() {
        // local still equals the previously-synced mirror; only remote moved.
        assert_eq!(
            classify_profile(Some("v1"), "v2", Some("v1")),
            MaterializeOutcome::FastForward
        );
    }

    #[test]
    fn classify_both_sides_changed_is_conflict() {
        let out = classify_profile(Some("local-edit"), "remote-edit", Some("v1"));
        assert_eq!(
            out,
            MaterializeOutcome::Conflict {
                local: "local-edit".into(),
                remote: "remote-edit".into()
            }
        );
    }

    #[test]
    fn mirror_profile_name_skips_reserved_and_invalid() {
        assert!(matches!(
            mirror_profile_name("CLAUDE.md.origin", "CLAUDE.md"),
            Some(Err(_))
        ));
        assert!(matches!(
            mirror_profile_name("CLAUDE.md.composed", "CLAUDE.md"),
            Some(Err(_))
        ));
        assert!(matches!(
            mirror_profile_name("CLAUDE.md.Uppercase", "CLAUDE.md"),
            Some(Err(_))
        ));
        assert!(matches!(
            mirror_profile_name("CLAUDE.md.tmp.123", "CLAUDE.md"),
            Some(Err(_))
        ));
        assert_eq!(
            mirror_profile_name("CLAUDE.md.quality-first", "CLAUDE.md"),
            Some(Ok("quality-first"))
        );
        assert_eq!(mirror_profile_name("NOTES.md.x", "CLAUDE.md"), None);
    }

    #[test]
    fn materialize_writes_created_and_fast_forward_but_not_conflict() {
        let dir = tempdir().unwrap();
        let claude = dir.path().join(".claude");
        let gs = GitSync::new(&claude);
        let profiles = gs.profiles_dir();
        fs::create_dir_all(&profiles).unwrap();

        // mirror has three profiles
        fs::write(profiles.join("CLAUDE.md.fresh"), "new-remote").unwrap();
        fs::write(profiles.join("CLAUDE.md.ff"), "remote-v2").unwrap();
        fs::write(profiles.join("CLAUDE.md.conflict"), "remote-edit").unwrap();
        fs::write(profiles.join("CLAUDE.md.origin"), "should be skipped").unwrap();

        // flat namespace pre-state
        let flat = claude.clone();
        fs::create_dir_all(&flat).unwrap();
        // "ff": local equals the OLD mirror version → fast-forward
        fs::write(flat.join("CLAUDE.md.ff"), "remote-v1").unwrap();
        // "conflict": local diverged from old mirror → conflict
        fs::write(flat.join("CLAUDE.md.conflict"), "local-edit").unwrap();

        let old = vec![
            ("ff".to_string(), "remote-v1".to_string()),
            ("conflict".to_string(), "v1".to_string()),
        ];
        let report = gs.materialize_profiles(&flat, "CLAUDE.md", &old).unwrap();

        let by_name = |n: &str| {
            report
                .iter()
                .find(|e| e.name == n)
                .map(|e| e.outcome.clone())
        };
        assert_eq!(by_name("fresh"), Some(MaterializeOutcome::Created));
        assert_eq!(by_name("ff"), Some(MaterializeOutcome::FastForward));
        assert!(matches!(
            by_name("conflict"),
            Some(MaterializeOutcome::Conflict { .. })
        ));
        // origin must be skipped, never written into the flat namespace beyond
        // what was already there.
        assert!(report.iter().any(|e| e.name == "CLAUDE.md.origin"
            && matches!(e.outcome, MaterializeOutcome::Skipped { .. })));

        // Created + FastForward written; Conflict left untouched.
        assert_eq!(
            fs::read_to_string(flat.join("CLAUDE.md.fresh")).unwrap(),
            "new-remote"
        );
        assert_eq!(
            fs::read_to_string(flat.join("CLAUDE.md.ff")).unwrap(),
            "remote-v2"
        );
        assert_eq!(
            fs::read_to_string(flat.join("CLAUDE.md.conflict")).unwrap(),
            "local-edit",
            "conflict must not clobber the local edit"
        );
    }

    // --- git roundtrip against a local bare remote ------------------------

    /// Build a bare "remote" seeded with one commit on `main` containing a
    /// profile file and a manifest. Returns the bare repo path.
    fn seed_bare_remote(root: &Path) -> PathBuf {
        let remote = root.join("remote.git");
        Repository::init_bare(&remote).unwrap();

        // Seed via a temporary working clone.
        let seed = root.join("seed");
        let repo = Repository::clone(remote.to_str().unwrap(), &seed).unwrap();
        let profiles = seed.join("profiles");
        fs::create_dir_all(&profiles).unwrap();
        fs::write(profiles.join("CLAUDE.md.quality-first"), "remote quality\n").unwrap();
        fs::write(seed.join("manifest.toml"), "schema_version = 3\n").unwrap();

        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("seed", "seed@localhost").unwrap();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, "seed", &tree, &[])
            .unwrap();
        // Ensure the branch is named `main`.
        repo.branch("main", &repo.find_commit(oid).unwrap(), true)
            .unwrap();
        repo.set_head("refs/heads/main").unwrap();
        let mut remote_conn = repo.find_remote("origin").unwrap();
        remote_conn
            .push(&["refs/heads/main:refs/heads/main"], None)
            .unwrap();
        remote
    }

    #[test]
    fn clone_fetch_commit_push_roundtrip_local() {
        let dir = tempdir().unwrap();
        let remote = seed_bare_remote(dir.path());

        // Clone into the toggler sync worktree.
        let claude = dir.path().join(".claude");
        let gs = GitSync::new(&claude);
        gs.clone_or_open(remote.to_str().unwrap(), "main").unwrap();
        assert!(gs.is_linked());
        assert!(gs
            .profiles_dir()
            .join("CLAUDE.md.quality-first")
            .exists());

        let first_sha = gs.head_sha().unwrap();

        // Modify the mirror, commit, push.
        fs::write(
            gs.profiles_dir().join("CLAUDE.md.quality-first"),
            "edited from this machine\n",
        )
        .unwrap();
        let new_sha = gs.commit_all("edit quality-first").unwrap();
        assert_ne!(first_sha, new_sha);
        gs.push("main").unwrap();

        // A fresh clone of the same remote must see the pushed change.
        let claude2 = dir.path().join(".claude2");
        let gs2 = GitSync::new(&claude2);
        gs2.clone_or_open(remote.to_str().unwrap(), "main").unwrap();
        assert_eq!(
            fs::read_to_string(gs2.profiles_dir().join("CLAUDE.md.quality-first")).unwrap(),
            "edited from this machine\n"
        );
        assert_eq!(gs2.head_sha().unwrap(), new_sha);
    }

    #[test]
    fn fetch_remote_head_then_reset_fast_forwards_mirror() {
        let dir = tempdir().unwrap();
        let remote = seed_bare_remote(dir.path());

        let claude_a = dir.path().join(".claudeA");
        let a = GitSync::new(&claude_a);
        a.clone_or_open(remote.to_str().unwrap(), "main").unwrap();

        let claude_b = dir.path().join(".claudeB");
        let b = GitSync::new(&claude_b);
        b.clone_or_open(remote.to_str().unwrap(), "main").unwrap();

        // A pushes a change.
        fs::write(a.profiles_dir().join("CLAUDE.md.new-one"), "from A\n").unwrap();
        let a_sha = a.commit_all("add new-one").unwrap();
        a.push("main").unwrap();

        // B fetches + resets and now has A's commit + file.
        let head = b.fetch_remote_head("main").unwrap();
        assert_eq!(head, a_sha);
        b.reset_hard_to(&head).unwrap();
        assert_eq!(b.head_sha().unwrap(), a_sha);
        assert!(b.profiles_dir().join("CLAUDE.md.new-one").exists());
    }
}
