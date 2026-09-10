//! Read-only inventory of the prompt layers that reach an agent in a given
//! working directory (v0.5).
//!
//! Every other `core` module writes — this one only reads. It answers "what
//! does my agent actually load when I work here", split by engine (Claude Code
//! / Codex CLI), so the report layer can hand a model something to critique.
//!
//! Four things here are deliberate and easy to get wrong:
//!
//! - **Token estimates split CJK from ASCII.** `bytes/4` — what
//!   `claude-md-analyzer`'s `collect-layers.sh` uses — under-counts a
//!   Korean-heavy harness by roughly 28% (measured on one global CLAUDE.md:
//!   3,599 vs 4,605). Korean is 3 bytes per char in UTF-8 but nowhere near
//!   3/4 of a token, so byte-based estimates rank the wrong files as the
//!   expensive ones.
//!
//! - **`@import` lines are expanded.** The toggler itself composes
//!   `@domains/{id}/INDEX.md` (v0.3) and `@../domains/{id}/INDEX.md` (v0.4)
//!   into the active file. Without expansion the scanner cannot see the
//!   context the toggler added — it would under-report its own work.
//!
//! - **Not every layer is prompt text.** `settings.json` and Codex's
//!   `config.toml` shape behavior (permissions, hooks, model, sandbox) but
//!   their bytes never enter the prompt. They carry `in_prompt: false` and a
//!   zero token estimate so totals stay honest.
//!
//! - **Owner classification is configuration, not code.** Which git owners
//!   count as "company" is the user's business and this repo is public, so
//!   the lists live in a local file and default to empty. With no file every
//!   path is `unknown` and the feature simply offers no warning.

// `core` is a private module, so nothing here is reachable until the report
// layer consumes it. Drop this once `report.rs` calls `scan`.
#![allow(dead_code)]

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

/// Filename Claude Code loads as project/user instructions.
const CLAUDE_MD: &str = "CLAUDE.md";
/// Filename Codex CLI loads as project/user instructions.
const AGENTS_MD: &str = "AGENTS.md";
/// How deep below the working directory to look for nested instruction files.
const SUBTREE_MAX_DEPTH: usize = 3;
/// Import expansion depth ceiling. Cycles are caught by the visited set; this
/// only bounds legitimately deep chains.
const IMPORT_MAX_DEPTH: usize = 5;
/// Directories never worth walking for nested instruction files.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".gradle",
    ".idea",
    "vendor",
    ".venv",
];
/// Optional local file mapping git owners to a trust class. Absent by default.
const ORIGIN_CLASSES_FILE: &str = ".toggler-origin-classes.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Claude,
    Codex,
}

/// Which engines a scan covers. `Both` is the default because cross-engine
/// drift (a stale `AGENTS.md` beside a current `CLAUDE.md`) is only visible
/// when both axes are present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineScope {
    Claude,
    Codex,
    Both,
}

impl EngineScope {
    fn covers(self, engine: Engine) -> bool {
        matches!(
            (self, engine),
            (EngineScope::Both, _)
                | (EngineScope::Claude, Engine::Claude)
                | (EngineScope::Codex, Engine::Codex)
        )
    }
}

/// When a layer reaches the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoadTiming {
    /// Loaded at session start, every session in this directory.
    Always,
    /// Loaded only when the agent touches files in that subtree.
    OnDemand,
    /// Shapes behavior without being prompt text, or loads on a trigger.
    Conditional,
}

/// Trust class of the repository a path belongs to, from the local owner map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OriginClass {
    Company,
    Personal,
    /// In a git repo, but the owner is not in the local map (or there is no
    /// map). The default — never assume a classification we were not given.
    Unknown,
    NotARepo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayerKind {
    ManagedPolicy,
    UserGlobal,
    UserConfig,
    Ancestor,
    Project,
    ProjectLocal,
    AutoMemory,
    Subtree,
    Settings,
    Import,
}

/// One file that reaches (or configures) the agent.
///
/// The first five fields keep `claude-md-analyzer`'s `exported-modules.md` §1
/// shape so that contract still holds; everything after is v0.5 additive.
#[derive(Debug, Clone, Serialize)]
pub struct Layer {
    pub path: String,
    /// Coarse bucket from the analyzer contract: `global` | `user` | `project`
    /// | `ancestor` | `local`.
    pub layer: String,
    pub bytes: u64,
    pub modified: String,
    pub token_estimate: u64,

    pub engine: Engine,
    /// Composition order within the engine. Lower loads earlier.
    pub rank: u8,
    pub kind: LayerKind,
    pub load: LoadTiming,
    pub origin_class: OriginClass,
    /// False for a broken `@import` target — the only case a missing file is
    /// listed at all, because a dangling import is a defect worth showing.
    pub exists: bool,
    /// True when the file's bytes actually become prompt text. Settings and
    /// config files are `false` and contribute zero tokens.
    pub in_prompt: bool,
    /// Set on imported layers: the file whose `@` line pulled this one in.
    pub imported_by: Option<String>,
}

/// A skill / agent / hook / plugin. Counted by name and source only — their
/// prompt cost is real but not measurable from disk, so no token estimate is
/// claimed. See PRD #27 §7-B.
#[derive(Debug, Clone, Serialize)]
pub struct Capability {
    pub engine: Engine,
    pub kind: String,
    pub name: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Inventory {
    pub cwd: String,
    pub scanned_at: String,
    pub layers: Vec<Layer>,
    pub capabilities: Vec<Capability>,
}

/// Filesystem roots a scan reads from. Passed in rather than resolved inside
/// so tests can point the whole scan at a tempdir.
#[derive(Debug, Clone)]
pub struct ScanRoots {
    pub claude_dir: PathBuf,
    pub codex_dir: PathBuf,
    /// Enterprise policy file. `None` on platforms/installs without one.
    pub managed_policy: Option<PathBuf>,
}

// Entry points for the command layer; tests build `ScanRoots` directly
// against a tempdir.
impl ScanRoots {
    pub fn from_home(home: &Path) -> Self {
        Self {
            claude_dir: home.join(".claude"),
            codex_dir: home.join(".codex"),
            managed_policy: managed_policy_path(),
        }
    }

    pub fn detect() -> Option<Self> {
        dirs::home_dir().as_deref().map(Self::from_home)
    }
}

#[cfg(target_os = "macos")]
fn managed_policy_path() -> Option<PathBuf> {
    Some(PathBuf::from(
        "/Library/Application Support/ClaudeCode/managed-settings.json",
    ))
}

#[cfg(target_os = "linux")]
fn managed_policy_path() -> Option<PathBuf> {
    Some(PathBuf::from("/etc/claude-code/managed-settings.json"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn managed_policy_path() -> Option<PathBuf> {
    None
}

// ---------------------------------------------------------------------------
// Token estimation
// ---------------------------------------------------------------------------

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0xAC00..=0xD7A3      // Hangul syllables
        | 0x1100..=0x11FF    // Hangul jamo
        | 0x3130..=0x318F    // Hangul compatibility jamo
        | 0x4E00..=0x9FFF    // CJK unified ideographs
        | 0x3040..=0x30FF    // Hiragana + katakana
        | 0xFF00..=0xFFEF    // Halfwidth/fullwidth forms
    )
}

/// Rough token count that does not collapse under Korean text.
///
/// ASCII-ish runs keep the conventional ~4 chars per token; CJK characters are
/// counted at 1.15 tokens each. Both halves are approximations — the exact
/// coefficient is open (PRD #27 §7-B) and every surfaced value is labelled an
/// estimate. What matters is that the two scripts are not averaged together,
/// which is what makes `bytes/4` rank files wrongly.
pub fn estimate_tokens(text: &str) -> u64 {
    let mut cjk = 0u64;
    let mut other = 0u64;
    for c in text.chars() {
        if is_cjk(c) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    (other as f64 / 4.0 + cjk as f64 * 1.15).round() as u64
}

// ---------------------------------------------------------------------------
// Origin classification
// ---------------------------------------------------------------------------

/// Owner → trust class map, read from `{claude_dir}/.toggler-origin-classes.json`.
///
/// Deliberately empty by default: this repo is public and whose owners are
/// "company" is the user's information, not the app's.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OriginClasses {
    #[serde(default)]
    pub company: Vec<String>,
    #[serde(default)]
    pub personal: Vec<String>,
}

impl OriginClasses {
    pub fn load(claude_dir: &Path) -> Self {
        let path = claude_dir.join(ORIGIN_CLASSES_FILE);
        let Ok(raw) = fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    fn classify_owner(&self, owner: &str) -> OriginClass {
        if self.company.iter().any(|o| o.eq_ignore_ascii_case(owner)) {
            return OriginClass::Company;
        }
        if self.personal.iter().any(|o| o.eq_ignore_ascii_case(owner)) {
            return OriginClass::Personal;
        }
        OriginClass::Unknown
    }
}

/// Extract the owner segment from a git remote URL.
///
/// Handles `https://host/owner/repo.git`, `https://user@host/owner/repo`,
/// `git@host:owner/repo.git` and `ssh://git@host/owner/repo`.
pub fn remote_owner(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);

    // Drop the scheme, then any `user@` prefix, so both URL and scp forms
    // reduce to `host[:/]owner/repo`.
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let after_user = after_scheme
        .split_once('@')
        .map(|(_, r)| r)
        .unwrap_or(after_scheme);
    // scp-style separates host from path with `:` instead of `/`.
    let normalized = after_user.replacen(':', "/", 1);

    let mut segments = normalized.split('/').filter(|s| !s.is_empty());
    let _host = segments.next()?;
    let owner = segments.next()?;
    if owner.is_empty() {
        None
    } else {
        Some(owner.to_string())
    }
}

/// Classify the repository `path` lives in, using the local owner map.
///
/// Uses the already-vendored libgit2 — no shell-out, no network.
pub fn classify_origin(path: &Path, classes: &OriginClasses) -> OriginClass {
    let Ok(repo) = git2::Repository::discover(path) else {
        return OriginClass::NotARepo;
    };
    let Ok(remote) = repo.find_remote("origin") else {
        return OriginClass::Unknown;
    };
    let Some(owner) = remote.url().and_then(remote_owner) else {
        return OriginClass::Unknown;
    };
    classes.classify_owner(&owner)
}

/// Working-tree root of the repository containing `path`, if any.
///
/// Codex treats this as a hard ceiling for `AGENTS.md` discovery, so the
/// scanner needs it to avoid listing files that never load.
fn git_root(path: &Path) -> Option<PathBuf> {
    let repo = git2::Repository::discover(path).ok()?;
    let wd = repo.workdir()?.to_path_buf();
    Some(fs::canonicalize(&wd).unwrap_or(wd))
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Claude Code's on-disk project id: the absolute path with `/` replaced by `-`.
fn escape_cwd(cwd: &Path) -> String {
    cwd.to_string_lossy().replace('/', "-")
}

/// Resolve the project id for `cwd`, verifying against disk rather than
/// trusting the escape function.
///
/// If the computed id has no directory we fall back to matching
/// `memory::list_projects` labels, so an escaping rule we got wrong shows up
/// as a miss to correct rather than a silently absent memory layer.
fn project_id_for(claude_dir: &Path, cwd: &Path) -> Option<String> {
    let escaped = escape_cwd(cwd);
    if claude_dir.join("projects").join(&escaped).is_dir() {
        return Some(escaped);
    }
    // Claude Code derives the id from whatever path it was launched with, which
    // may be the pre-symlink form. Match on disk, comparing both shapes.
    crate::core::memory::list_projects(claude_dir)
        .ok()?
        .into_iter()
        .find(|p| {
            let label = Path::new(&p.label);
            label == cwd
                || fs::canonicalize(label)
                    .map(|c| c == cwd)
                    .unwrap_or(false)
        })
        .map(|p| p.id)
}

fn iso8601(t: std::time::SystemTime) -> String {
    DateTime::<Utc>::from(t).to_rfc3339_opts(SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// Layer construction
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn build_layer(
    path: &Path,
    layer: &str,
    engine: Engine,
    rank: u8,
    kind: LayerKind,
    load: LoadTiming,
    in_prompt: bool,
    classes: &OriginClasses,
) -> Option<Layer> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let bytes = meta.len();
    let modified = meta.modified().map(iso8601).unwrap_or_default();
    let token_estimate = if in_prompt {
        fs::read_to_string(path)
            .map(|t| estimate_tokens(&t))
            .unwrap_or(0)
    } else {
        0
    };
    Some(Layer {
        path: path.to_string_lossy().to_string(),
        layer: layer.to_string(),
        bytes,
        modified,
        token_estimate,
        engine,
        rank,
        kind,
        load,
        origin_class: classify_origin(path, classes),
        exists: true,
        in_prompt,
        imported_by: None,
    })
}

// ---------------------------------------------------------------------------
// Import expansion
// ---------------------------------------------------------------------------

/// Pull `@path` import targets out of markdown, ignoring fenced code blocks.
///
/// Fence tracking is not optional: a Kotlin snippet with `@Transactional` at
/// the start of a line would otherwise be read as an import. Outside fences we
/// still require the token to look like a path (contains `/` or ends `.md`) so
/// bare annotations and `@mentions` are left alone.
pub fn import_targets(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix('@') else {
            continue;
        };
        let token = rest.split_whitespace().next().unwrap_or("");
        if token.is_empty() {
            continue;
        }
        if token.contains('/') || token.ends_with(".md") {
            out.push(token.to_string());
        }
    }
    out
}

/// Resolve an import token against the importing file's directory.
///
/// Claude Code resolves `@` imports relative to the file that contains them,
/// which is why the v0.4 per-project form is `@../domains/{id}/INDEX.md`.
fn resolve_import(token: &str, importing_file: &Path, home: &Path) -> PathBuf {
    if let Some(rest) = token.strip_prefix("~/") {
        return home.join(rest);
    }
    let p = Path::new(token);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    importing_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(p)
}

/// Walk the import graph from `seeds`, appending every reachable target.
///
/// Missing targets are emitted with `exists: false` — a dangling import is a
/// defect the report should show, and it is the only case a non-existent file
/// appears in the inventory at all.
fn expand_imports(
    seeds: &[Layer],
    home: &Path,
    classes: &OriginClasses,
    visited: &mut HashSet<PathBuf>,
) -> Vec<Layer> {
    let mut out = Vec::new();
    let mut frontier: Vec<(PathBuf, usize)> = seeds
        .iter()
        .filter(|l| l.in_prompt && l.exists)
        .map(|l| (PathBuf::from(&l.path), 0usize))
        .collect();

    while let Some((file, depth)) = frontier.pop() {
        if depth >= IMPORT_MAX_DEPTH {
            continue;
        }
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        for token in import_targets(&content) {
            let target = resolve_import(&token, &file, home);
            let key = fs::canonicalize(&target).unwrap_or_else(|_| target.clone());
            if !visited.insert(key) {
                continue;
            }
            match build_layer(
                &target,
                "import",
                Engine::Claude,
                9,
                LayerKind::Import,
                LoadTiming::Always,
                true,
                classes,
            ) {
                Some(mut layer) => {
                    layer.imported_by = Some(file.to_string_lossy().to_string());
                    out.push(layer);
                    frontier.push((target, depth + 1));
                }
                None => out.push(Layer {
                    path: target.to_string_lossy().to_string(),
                    layer: "import".to_string(),
                    bytes: 0,
                    modified: String::new(),
                    token_estimate: 0,
                    engine: Engine::Claude,
                    rank: 9,
                    kind: LayerKind::Import,
                    load: LoadTiming::Always,
                    origin_class: OriginClass::NotARepo,
                    exists: false,
                    in_prompt: true,
                    imported_by: Some(file.to_string_lossy().to_string()),
                }),
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Subtree + capability discovery
// ---------------------------------------------------------------------------

fn collect_subtree(root: &Path, filename: &str, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > SUBTREE_MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            collect_subtree(&path, filename, depth + 1, out);
        } else if ft.is_file() && entry.file_name() == filename && depth > 0 {
            out.push(path);
        }
    }
}

fn collect_capabilities(dir: &Path, engine: Engine, kind: &str, out: &mut Vec<Capability>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        out.push(Capability {
            engine,
            kind: kind.to_string(),
            name,
            source: dir.to_string_lossy().to_string(),
        });
    }
}

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

/// Enumerate every prompt layer reaching `cwd`, for the requested engines.
///
/// Read-only: nothing on disk is created or modified.
pub fn scan(cwd: &Path, home: &Path, roots: &ScanRoots, scope: EngineScope) -> io::Result<Inventory> {
    // Normalize once, here, so both engine axes emit the same string for the
    // same directory. They used to differ — one canonicalized and one did not
    // — and on macOS, where /var is a symlink to /private/var, that silently
    // broke every cross-engine pairing.
    let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let cwd = cwd.as_path();

    let classes = OriginClasses::load(&roots.claude_dir);
    let mut layers: Vec<Layer> = Vec::new();
    let mut capabilities: Vec<Capability> = Vec::new();

    if scope.covers(Engine::Claude) {
        scan_claude(cwd, home, roots, &classes, &mut layers, &mut capabilities);
    }
    if scope.covers(Engine::Codex) {
        scan_codex(cwd, roots, &classes, &mut layers, &mut capabilities);
    }

    layers.sort_by(|a, b| {
        (a.engine as u8, a.rank, a.path.clone()).cmp(&(b.engine as u8, b.rank, b.path.clone()))
    });

    Ok(Inventory {
        cwd: cwd.to_string_lossy().to_string(),
        scanned_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        layers,
        capabilities,
    })
}

fn scan_claude(
    cwd: &Path,
    home: &Path,
    roots: &ScanRoots,
    classes: &OriginClasses,
    layers: &mut Vec<Layer>,
    capabilities: &mut Vec<Capability>,
) {
    let push = |path: PathBuf,
                    layer: &str,
                    rank: u8,
                    kind: LayerKind,
                    load: LoadTiming,
                    in_prompt: bool,
                    layers: &mut Vec<Layer>| {
        if let Some(l) = build_layer(
            &path,
            layer,
            Engine::Claude,
            rank,
            kind,
            load,
            in_prompt,
            classes,
        ) {
            layers.push(l);
        }
    };

    // rank 0 — enterprise policy
    if let Some(policy) = &roots.managed_policy {
        push(
            policy.clone(),
            "global",
            0,
            LayerKind::ManagedPolicy,
            LoadTiming::Conditional,
            false,
            layers,
        );
    }

    // rank 1 — user global instructions
    push(
        roots.claude_dir.join(CLAUDE_MD),
        "global",
        1,
        LayerKind::UserGlobal,
        LoadTiming::Always,
        true,
        layers,
    );

    // rank 2 — ancestors, outermost first
    let mut ancestors: Vec<PathBuf> = Vec::new();
    let mut parent = cwd.parent();
    while let Some(p) = parent {
        ancestors.push(p.join(CLAUDE_MD));
        parent = p.parent();
    }
    for path in ancestors.into_iter().rev() {
        push(
            path,
            "ancestor",
            2,
            LayerKind::Ancestor,
            LoadTiming::Always,
            true,
            layers,
        );
    }

    // rank 3/4 — the project itself
    push(
        cwd.join(CLAUDE_MD),
        "project",
        3,
        LayerKind::Project,
        LoadTiming::Always,
        true,
        layers,
    );
    push(
        cwd.join(".claude").join(CLAUDE_MD),
        "project",
        3,
        LayerKind::Project,
        LoadTiming::Always,
        true,
        layers,
    );
    push(
        cwd.join("CLAUDE.local.md"),
        "local",
        4,
        LayerKind::ProjectLocal,
        LoadTiming::Always,
        true,
        layers,
    );

    // rank 5 — per-project auto-memory
    if let Some(id) = project_id_for(&roots.claude_dir, cwd) {
        let memory_dir = crate::core::memory::memory_dir_for(&roots.claude_dir, &id);
        push(
            memory_dir.join(crate::core::memory::MEMORY_TARGET_NAME),
            "user",
            5,
            LayerKind::AutoMemory,
            LoadTiming::Always,
            true,
            layers,
        );
    }

    // rank 6 — nested instruction files, loaded only when that subtree is touched
    let mut subtree = Vec::new();
    collect_subtree(cwd, CLAUDE_MD, 0, &mut subtree);
    subtree.sort();
    for path in subtree {
        push(
            path,
            "local",
            6,
            LayerKind::Subtree,
            LoadTiming::OnDemand,
            true,
            layers,
        );
    }

    // rank 7 — settings. Behavior, not prompt text.
    for (path, bucket) in [
        (roots.claude_dir.join("settings.json"), "global"),
        (cwd.join(".claude").join("settings.json"), "project"),
        (cwd.join(".claude").join("settings.local.json"), "local"),
    ] {
        push(
            path,
            bucket,
            7,
            LayerKind::Settings,
            LoadTiming::Conditional,
            false,
            layers,
        );
    }

    // Imports reachable from anything already collected.
    let mut visited: HashSet<PathBuf> = layers
        .iter()
        .map(|l| {
            let p = PathBuf::from(&l.path);
            fs::canonicalize(&p).unwrap_or(p)
        })
        .collect();
    let imported = expand_imports(layers, home, classes, &mut visited);
    layers.extend(imported);

    // rank 8 — capabilities, named but not costed
    for (dir, kind) in [
        (roots.claude_dir.join("skills"), "skill"),
        (roots.claude_dir.join("agents"), "agent"),
        (cwd.join(".claude").join("skills"), "skill"),
        (cwd.join(".claude").join("agents"), "agent"),
        (cwd.join(".claude").join("hooks"), "hook"),
    ] {
        collect_capabilities(&dir, Engine::Claude, kind, capabilities);
    }
}

fn scan_codex(
    cwd: &Path,
    roots: &ScanRoots,
    classes: &OriginClasses,
    layers: &mut Vec<Layer>,
    capabilities: &mut Vec<Capability>,
) {
    let push = |path: PathBuf,
                    layer: &str,
                    rank: u8,
                    kind: LayerKind,
                    load: LoadTiming,
                    in_prompt: bool,
                    layers: &mut Vec<Layer>| {
        if let Some(l) = build_layer(
            &path,
            layer,
            Engine::Codex,
            rank,
            kind,
            load,
            in_prompt,
            classes,
        ) {
            layers.push(l);
        }
    };

    // rank 0 — config.toml: model, reasoning effort, sandbox, project trust,
    // marketplaces, plugins. Shapes everything, is not prompt text.
    push(
        roots.codex_dir.join("config.toml"),
        "global",
        0,
        LayerKind::UserConfig,
        LoadTiming::Conditional,
        false,
        layers,
    );

    // rank 1 — user global instructions
    push(
        roots.codex_dir.join(AGENTS_MD),
        "global",
        1,
        LayerKind::UserGlobal,
        LoadTiming::Always,
        true,
        layers,
    );

    // ranks 2/3 — the project chain, bounded by the git root.
    //
    // Measured against codex-cli 0.153.4 with marker files and
    // `codex debug prompt-input`:
    //
    //   - Outside a git repository NO project `AGENTS.md` loads at all; only
    //     the user-global one does.
    //   - Inside one, every `AGENTS.md` from the git root down to the working
    //     directory is merged, outermost first — not just the nearest.
    //   - An `AGENTS.md` above the git root never loads. The root is a hard
    //     ceiling, which is why walking to `/` would list files that are not
    //     read. (`~/AGENTS.md` is outside `~/IdeaProjects/msa` and is not
    //     loaded when working there, though a naive ancestor walk lists it.)
    //
    // Claude Code's chain is walked separately in `scan_claude` and is NOT
    // bounded this way.
    if let Some(root) = git_root(cwd) {
        let here = cwd.to_path_buf();
        let mut chain: Vec<PathBuf> = Vec::new();
        if here.starts_with(&root) {
            let mut cur = Some(here.as_path());
            while let Some(dir) = cur {
                chain.push(dir.to_path_buf());
                if dir == root {
                    break;
                }
                cur = dir.parent();
            }
            chain.reverse();
        } else {
            chain.push(here.clone());
        }
        let last = chain.len() - 1;
        for (i, dir) in chain.into_iter().enumerate() {
            let (bucket, rank, kind) = if i == last {
                ("project", 3, LayerKind::Project)
            } else {
                ("ancestor", 2, LayerKind::Ancestor)
            };
            push(
                dir.join(AGENTS_MD),
                bucket,
                rank,
                kind,
                LoadTiming::Always,
                true,
                layers,
            );
        }
    }

    // rank 6 — nested AGENTS.md below the working directory.
    //
    // Measured absent from the session-start prompt. Whether Codex picks one
    // up on touching that subtree is not something `prompt-input` can show, so
    // these are listed as `Conditional` rather than claiming `OnDemand`.
    let mut subtree = Vec::new();
    collect_subtree(cwd, AGENTS_MD, 0, &mut subtree);
    subtree.sort();
    for path in subtree {
        push(
            path,
            "local",
            6,
            LayerKind::Subtree,
            LoadTiming::Conditional,
            true,
            layers,
        );
    }

    collect_capabilities(
        &roots.codex_dir.join("agents"),
        Engine::Codex,
        "agent",
        capabilities,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    /// home + an empty ~/.claude and ~/.codex, with no managed policy so the
    /// host machine's real one can never leak into a test.
    fn fixture() -> (TempDir, PathBuf, ScanRoots) {
        let dir = tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();
        let roots = ScanRoots {
            claude_dir: home.join(".claude"),
            codex_dir: home.join(".codex"),
            managed_policy: None,
        };
        (dir, home, roots)
    }

    fn paths(inv: &Inventory) -> Vec<String> {
        inv.layers.iter().map(|l| l.path.clone()).collect()
    }

    /// Codex only reads project `AGENTS.md` inside a git repository, so any
    /// Codex fixture has to be one.
    fn init_repo(path: &Path) {
        fs::create_dir_all(path).unwrap();
        git2::Repository::init(path).unwrap();
    }

    fn codex_layers(inv: &Inventory) -> Vec<&Layer> {
        inv.layers.iter().filter(|l| l.engine == Engine::Codex).collect()
    }

    // --- token estimation ---

    #[test]
    fn ascii_estimate_matches_quarter_of_length() {
        assert_eq!(estimate_tokens(&"a".repeat(400)), 100);
    }

    #[test]
    fn korean_costs_far_more_than_bytes_over_four() {
        // 100 Hangul syllables are 300 bytes; bytes/4 would say 75 tokens.
        let text = "가".repeat(100);
        assert_eq!(text.len(), 300);
        assert_eq!(estimate_tokens(&text), 115);
        assert!(
            estimate_tokens(&text) > (text.len() as u64) / 4,
            "CJK must not be under-counted the way bytes/4 does"
        );
    }

    #[test]
    fn mixed_script_counts_each_half_separately() {
        // 40 ASCII (=10) + 20 Hangul (=23)
        let text = format!("{}{}", "a".repeat(40), "한".repeat(20));
        assert_eq!(estimate_tokens(&text), 33);
    }

    #[test]
    fn empty_text_is_zero_tokens() {
        assert_eq!(estimate_tokens(""), 0);
    }

    // --- origin classification ---

    #[test]
    fn remote_owner_handles_every_url_shape() {
        assert_eq!(
            remote_owner("https://github.com/acme/repo.git").as_deref(),
            Some("acme")
        );
        // The toggler's own remote embeds userinfo.
        assert_eq!(
            remote_owner("https://1989v@github.com/1989v/claude-md-toggler.git").as_deref(),
            Some("1989v")
        );
        assert_eq!(
            remote_owner("git@github.com:acme/repo.git").as_deref(),
            Some("acme")
        );
        assert_eq!(
            remote_owner("ssh://git@github.com/acme/repo").as_deref(),
            Some("acme")
        );
        assert_eq!(remote_owner("not a url"), None);
    }

    #[test]
    fn owner_map_classifies_three_ways() {
        let classes = OriginClasses {
            company: vec!["acme".into()],
            personal: vec!["myhandle".into()],
        };
        assert_eq!(classes.classify_owner("acme"), OriginClass::Company);
        assert_eq!(classes.classify_owner("ACME"), OriginClass::Company);
        assert_eq!(classes.classify_owner("myhandle"), OriginClass::Personal);
        assert_eq!(classes.classify_owner("stranger"), OriginClass::Unknown);
    }

    #[test]
    fn missing_owner_map_classifies_nothing() {
        let dir = tempdir().unwrap();
        let classes = OriginClasses::load(dir.path());
        assert!(classes.company.is_empty() && classes.personal.is_empty());
        assert_eq!(classes.classify_owner("acme"), OriginClass::Unknown);
    }

    #[test]
    fn non_repo_path_is_not_a_repo() {
        let dir = tempdir().unwrap();
        let classes = OriginClasses::default();
        assert_eq!(
            classify_origin(dir.path(), &classes),
            OriginClass::NotARepo
        );
    }

    // --- import extraction ---

    #[test]
    fn path_shaped_tokens_inside_a_fence_are_not_imports() {
        // The token inside the fence must be path-shaped, or the shape filter
        // catches it first and this asserts nothing about fence tracking.
        let md = "\
@real/one.md
```markdown
@fenced/example/INDEX.md
```
@real/two.md
";
        let found = import_targets(md);
        assert_eq!(
            found,
            vec!["real/one.md", "real/two.md"],
            "a documented import example inside a fence must not be followed"
        );
    }

    #[test]
    fn bare_annotations_and_mentions_are_not_imports() {
        let md = "\
@domains/kubernetes/INDEX.md
@Override
@Transactional
text @not/an/import trailing
@~/notes/other.md
";
        let found = import_targets(md);
        assert_eq!(
            found,
            vec!["domains/kubernetes/INDEX.md", "~/notes/other.md"],
            "only line-leading path-shaped tokens are imports"
        );
    }

    #[test]
    fn import_cycle_terminates() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        // a -> b -> a
        write(&cwd.join(CLAUDE_MD), "@b.md\n");
        write(&cwd.join("b.md"), "@CLAUDE.md\n");

        let inv = scan(&cwd, &home, &roots, EngineScope::Claude).unwrap();
        let b = inv
            .layers
            .iter()
            .filter(|l| l.path.ends_with("b.md"))
            .count();
        assert_eq!(b, 1, "a cycle must yield each file once, not loop");
    }

    #[test]
    fn transitive_import_is_followed_and_attributed() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        write(&cwd.join(CLAUDE_MD), "@one.md\n");
        write(&cwd.join("one.md"), "@nested/two.md\n");
        write(&cwd.join("nested").join("two.md"), "leaf\n");

        let inv = scan(&cwd, &home, &roots, EngineScope::Claude).unwrap();
        let two = inv
            .layers
            .iter()
            .find(|l| l.path.ends_with("two.md"))
            .expect("transitive import must be reached");
        assert_eq!(two.kind, LayerKind::Import);
        assert!(two.imported_by.as_ref().unwrap().ends_with("one.md"));
    }

    #[test]
    fn dangling_import_is_reported_as_missing() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        write(&cwd.join(CLAUDE_MD), "@gone/missing.md\n");

        let inv = scan(&cwd, &home, &roots, EngineScope::Claude).unwrap();
        let missing = inv
            .layers
            .iter()
            .find(|l| l.path.ends_with("missing.md"))
            .expect("a broken import is a defect and must be listed");
        assert!(!missing.exists);
        assert_eq!(missing.bytes, 0);
    }

    #[test]
    fn toggler_composed_domain_import_is_visible() {
        // The scanner must see the block the toggler itself composes, or it
        // under-reports its own work.
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        write(
            &roots.claude_dir.join(CLAUDE_MD),
            "# base\n@domains/kubernetes/INDEX.md\n",
        );
        write(
            &roots.claude_dir.join("domains").join("kubernetes").join("INDEX.md"),
            "k8s notes\n",
        );

        let inv = scan(&cwd, &home, &roots, EngineScope::Claude).unwrap();
        assert!(
            paths(&inv).iter().any(|p| p.ends_with("kubernetes/INDEX.md")),
            "composed @domains import must appear in the inventory"
        );
    }

    // --- layer discovery ---

    #[test]
    fn ancestor_chain_is_collected_outermost_first() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("a").join("b").join("c");
        fs::create_dir_all(&cwd).unwrap();
        write(&home.join("a").join(CLAUDE_MD), "outer\n");
        write(&home.join("a").join("b").join(CLAUDE_MD), "inner\n");

        let inv = scan(&cwd, &home, &roots, EngineScope::Claude).unwrap();
        let ancestors: Vec<_> = inv
            .layers
            .iter()
            .filter(|l| l.kind == LayerKind::Ancestor)
            .map(|l| l.path.clone())
            .collect();
        assert_eq!(ancestors.len(), 2);
        assert!(ancestors[0].ends_with("/a/CLAUDE.md"));
        assert!(ancestors[1].ends_with("/a/b/CLAUDE.md"));
    }

    #[test]
    fn absent_files_are_omitted_entirely() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("bare");
        fs::create_dir_all(&cwd).unwrap();

        let inv = scan(&cwd, &home, &roots, EngineScope::Both).unwrap();
        assert!(
            inv.layers.is_empty(),
            "nothing on disk means nothing listed, got {:?}",
            paths(&inv)
        );
    }

    #[test]
    fn subtree_files_are_on_demand_and_skip_vendor_dirs() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        write(&cwd.join(CLAUDE_MD), "root\n");
        write(&cwd.join("svc").join(CLAUDE_MD), "service\n");
        write(&cwd.join("node_modules").join("pkg").join(CLAUDE_MD), "no\n");
        write(&cwd.join(".git").join(CLAUDE_MD), "no\n");

        let inv = scan(&cwd, &home, &roots, EngineScope::Claude).unwrap();
        let subtree: Vec<_> = inv
            .layers
            .iter()
            .filter(|l| l.kind == LayerKind::Subtree)
            .collect();
        assert_eq!(subtree.len(), 1);
        assert!(subtree[0].path.ends_with("svc/CLAUDE.md"));
        assert_eq!(subtree[0].load, LoadTiming::OnDemand);

        let root = inv
            .layers
            .iter()
            .find(|l| l.kind == LayerKind::Project)
            .unwrap();
        assert_eq!(root.load, LoadTiming::Always);
    }

    #[test]
    fn subtree_depth_is_bounded() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        write(&cwd.join("a").join("b").join("c").join("d").join(CLAUDE_MD), "deep\n");

        let inv = scan(&cwd, &home, &roots, EngineScope::Claude).unwrap();
        assert!(
            inv.layers.is_empty(),
            "depth beyond the ceiling must not be walked, got {:?}",
            paths(&inv)
        );
    }

    #[test]
    fn settings_are_listed_but_contribute_no_tokens() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        fs::create_dir_all(&cwd).unwrap();
        write(&roots.claude_dir.join("settings.json"), "{\"a\":1}\n");

        let inv = scan(&cwd, &home, &roots, EngineScope::Claude).unwrap();
        let s = inv
            .layers
            .iter()
            .find(|l| l.kind == LayerKind::Settings)
            .unwrap();
        assert!(!s.in_prompt);
        assert_eq!(s.token_estimate, 0);
        assert!(s.bytes > 0, "size is still reported, only the token cost is not claimed");
    }

    #[test]
    fn auto_memory_is_found_for_the_working_directory() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        fs::create_dir_all(&cwd).unwrap();
        let id = escape_cwd(&cwd);
        write(
            &roots.claude_dir.join("projects").join(&id).join("memory").join("MEMORY.md"),
            "remembered\n",
        );

        let inv = scan(&cwd, &home, &roots, EngineScope::Claude).unwrap();
        assert!(inv
            .layers
            .iter()
            .any(|l| l.kind == LayerKind::AutoMemory && l.in_prompt));
    }

    // --- engine split ---

    #[test]
    fn scope_selects_the_requested_engine_only() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        init_repo(&cwd);
        write(&cwd.join(CLAUDE_MD), "claude\n");
        write(&cwd.join(AGENTS_MD), "codex\n");

        let claude = scan(&cwd, &home, &roots, EngineScope::Claude).unwrap();
        assert!(claude.layers.iter().all(|l| l.engine == Engine::Claude));

        let codex = scan(&cwd, &home, &roots, EngineScope::Codex).unwrap();
        assert!(codex.layers.iter().all(|l| l.engine == Engine::Codex));

        let both = scan(&cwd, &home, &roots, EngineScope::Both).unwrap();
        assert_eq!(both.layers.len(), claude.layers.len() + codex.layers.len());
    }

    #[test]
    fn codex_config_is_behavior_not_prompt_text() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        fs::create_dir_all(&cwd).unwrap();
        write(&roots.codex_dir.join("config.toml"), "model = \"x\"\n");

        let inv = scan(&cwd, &home, &roots, EngineScope::Codex).unwrap();
        let cfg = inv
            .layers
            .iter()
            .find(|l| l.kind == LayerKind::UserConfig)
            .unwrap();
        assert!(!cfg.in_prompt);
        assert_eq!(cfg.token_estimate, 0);
    }

    #[test]
    fn both_engines_see_their_own_project_file() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        init_repo(&cwd);
        write(&cwd.join(CLAUDE_MD), "# claude\n");
        write(&cwd.join(AGENTS_MD), "# codex\n");

        let inv = scan(&cwd, &home, &roots, EngineScope::Both).unwrap();
        let claude_project = inv
            .layers
            .iter()
            .find(|l| l.engine == Engine::Claude && l.kind == LayerKind::Project)
            .unwrap();
        let codex_project = inv
            .layers
            .iter()
            .find(|l| l.engine == Engine::Codex && l.kind == LayerKind::Project)
            .unwrap();
        assert!(claude_project.path.ends_with("CLAUDE.md"));
        assert!(codex_project.path.ends_with("AGENTS.md"));
    }

    // --- Codex discovery, measured against codex-cli 0.153.4 ---

    #[test]
    fn codex_ignores_project_agents_outside_a_git_repo() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("loose");
        write(&cwd.join(AGENTS_MD), "# not in a repo\n");
        write(&roots.codex_dir.join(AGENTS_MD), "# global\n");

        let inv = scan(&cwd, &home, &roots, EngineScope::Codex).unwrap();
        let kinds: Vec<_> = codex_layers(&inv).iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![LayerKind::UserGlobal],
            "outside a repo only the user-global AGENTS.md loads"
        );
    }

    #[test]
    fn codex_merges_the_whole_chain_from_the_git_root_down() {
        let (_d, home, roots) = fixture();
        let root = home.join("repo");
        init_repo(&root);
        let cwd = root.join("a").join("b");
        fs::create_dir_all(&cwd).unwrap();
        write(&root.join(AGENTS_MD), "root\n");
        write(&root.join("a").join(AGENTS_MD), "mid\n");
        // `b` itself has none — the chain must still carry the two above it.

        let inv = scan(&cwd, &home, &roots, EngineScope::Codex).unwrap();
        let chain: Vec<_> = codex_layers(&inv)
            .iter()
            .filter(|l| matches!(l.kind, LayerKind::Ancestor | LayerKind::Project))
            .map(|l| l.path.clone())
            .collect();
        assert_eq!(chain.len(), 2, "got {chain:?}");
        assert!(chain[0].ends_with("/repo/AGENTS.md"), "outermost first");
        assert!(chain[1].ends_with("/repo/a/AGENTS.md"));
    }

    #[test]
    fn codex_never_reads_above_the_git_root() {
        let (_d, home, roots) = fixture();
        let root = home.join("repo");
        init_repo(&root);
        // Sitting directly above the repo — measured NOT to load.
        write(&home.join(AGENTS_MD), "above\n");
        write(&root.join(AGENTS_MD), "inside\n");

        let inv = scan(&root, &home, &roots, EngineScope::Codex).unwrap();
        let listed: Vec<_> = codex_layers(&inv).iter().map(|l| l.path.clone()).collect();
        assert!(
            listed.iter().any(|p| p.ends_with("/repo/AGENTS.md")),
            "the repo root file loads: {listed:?}"
        );
        assert!(
            !listed.iter().any(|p| p.ends_with("/home/AGENTS.md")),
            "the git root is a hard ceiling: {listed:?}"
        );
    }

    #[test]
    fn codex_nested_agents_are_conditional_not_session_start() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("repo");
        init_repo(&cwd);
        write(&cwd.join(AGENTS_MD), "root\n");
        write(&cwd.join("svc").join(AGENTS_MD), "nested\n");

        let inv = scan(&cwd, &home, &roots, EngineScope::Codex).unwrap();
        let nested = codex_layers(&inv)
            .into_iter()
            .find(|l| l.kind == LayerKind::Subtree)
            .expect("nested AGENTS.md is still listed");
        assert_eq!(
            nested.load,
            LoadTiming::Conditional,
            "measured absent at session start, so Always/OnDemand would overclaim"
        );
    }

    // --- capabilities ---

    #[test]
    fn capabilities_are_named_but_never_costed() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        fs::create_dir_all(roots.claude_dir.join("skills").join("my-skill")).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        write(&roots.codex_dir.join("agents").join("val.toml"), "x\n");

        let inv = scan(&cwd, &home, &roots, EngineScope::Both).unwrap();
        assert!(inv
            .capabilities
            .iter()
            .any(|c| c.kind == "skill" && c.name == "my-skill" && c.engine == Engine::Claude));
        assert!(inv
            .capabilities
            .iter()
            .any(|c| c.kind == "agent" && c.name == "val.toml" && c.engine == Engine::Codex));
    }

    // --- real machine ---

    /// Scan this machine and print the inventory.
    ///
    /// Ignored by default: the result depends on whose laptop runs it, so it
    /// asserts only invariants that must hold anywhere and leaves the numbers
    /// to the reader. Tempdir tests cannot tell you whether the scanner finds
    /// a real installation at all.
    ///
    ///   cargo test --lib core::scan::tests::smoke -- --ignored --nocapture
    #[test]
    #[ignore]
    fn smoke_scan_this_machine() {
        let home = dirs::home_dir().expect("home");
        let cwd = std::env::var("SCAN_CWD")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::current_dir().unwrap());
        let roots = ScanRoots::from_home(&home);
        let inv = scan(&cwd, &home, &roots, EngineScope::Both).unwrap();

        for engine in [Engine::Claude, Engine::Codex] {
            let mine: Vec<_> = inv.layers.iter().filter(|l| l.engine == engine).collect();
            let always: u64 = mine
                .iter()
                .filter(|l| l.in_prompt && l.load == LoadTiming::Always)
                .map(|l| l.token_estimate)
                .sum();
            println!("\n=== {engine:?} — always-loaded ~{always} tok ===");
            for l in &mine {
                println!(
                    "{:>6} {:>8}B ~{:<7} {:?}/{:?} {}{}",
                    l.rank,
                    l.bytes,
                    l.token_estimate,
                    l.kind,
                    l.load,
                    l.path,
                    if l.exists { "" } else { "  [MISSING]" }
                );
            }
        }
        println!("\ncapabilities: {}", inv.capabilities.len());

        // Invariants that hold on any machine.
        let mut seen = HashSet::new();
        for l in &inv.layers {
            assert!(
                seen.insert((l.engine, l.path.clone())),
                "duplicate layer: {}",
                l.path
            );
            if l.in_prompt && l.exists && l.bytes > 0 {
                assert!(l.token_estimate > 0, "prompt text with zero tokens: {}", l.path);
            }
            if !l.in_prompt {
                assert_eq!(l.token_estimate, 0, "non-prompt layer claims tokens: {}", l.path);
            }
        }
    }

    // --- contract with claude-md-analyzer ---

    #[test]
    fn analyzer_contract_fields_survive_serialization() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        write(&cwd.join(CLAUDE_MD), "# hi\n");

        let inv = scan(&cwd, &home, &roots, EngineScope::Claude).unwrap();
        let json = serde_json::to_value(&inv.layers[0]).unwrap();
        for field in ["path", "layer", "bytes", "modified", "token_estimate"] {
            assert!(
                json.get(field).is_some(),
                "exported-modules.md §1 requires `{field}`"
            );
        }
        assert_eq!(json["layer"], "project");
    }
}
