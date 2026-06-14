use std::fs;

use tauri::{AppHandle, State};

use crate::core::composer;
use crate::core::doctree::{self, DomainApply};
use crate::core::drift::{detect as detect_drift, DriftInfo};
use crate::core::git_sync::{self, MaterializeEntry, MaterializeOutcome};
use crate::core::history::{
    target_for_memory, target_for_project_doctree, Action, HistoryEntry, TARGET_GLOBAL,
};
use crate::core::mappings::DirectoryMapping;
use crate::core::memory::{self, MemoryProject};
use crate::core::profile_store::{ProfileInfo, COMPOSED_NAME};
use crate::core::session_lock::{self, SessionGuard};
use crate::{default_claude_dir, record_active, set_composed, tray, AppState, TARGET_NAME};

/// Best-effort history write — never fails the parent command. We log the
/// error to stderr and move on, because losing a history row is strictly less
/// bad than failing a user-initiated toggle.
fn record_history(
    state: &AppState,
    action: Action,
    from: Option<&str>,
    to: Option<&str>,
    target: &str,
    result: Result<(), &str>,
) {
    if let Ok(history) = state.history.lock() {
        if let Err(e) = history.record(action, from, to, target, result) {
            eprintln!("[history] record failed: {}", e);
        }
    }
}

#[tauri::command]
pub fn list_profiles(state: State<'_, AppState>) -> Result<Vec<ProfileInfo>, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    store.list().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_active_profile(state: State<'_, AppState>) -> Result<String, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    store.detect_active().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn toggle_profile(
    name: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    let from = state
        .last_active
        .lock()
        .ok()
        .and_then(|g| g.clone());
    let apply_result: Result<(), String> = {
        let engine = state.engine.lock().map_err(|e| e.to_string())?;
        engine.apply_named(&name).map_err(|e| e.to_string())
    };
    match &apply_result {
        Ok(()) => record_history(
            &state,
            Action::Toggle,
            from.as_deref(),
            Some(&name),
            TARGET_GLOBAL,
            Ok(()),
        ),
        Err(msg) => record_history(
            &state,
            Action::Toggle,
            from.as_deref(),
            Some(&name),
            TARGET_GLOBAL,
            Err(msg.as_str()),
        ),
    }
    apply_result?;
    record_active(&state, &name);
    // A flat toggle replaces the whole active file with a single profile — the
    // active file is no longer composed.
    set_composed(&state, false);
    tray::refresh(&app).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn read_profile(name: String, state: State<'_, AppState>) -> Result<String, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    store.read(&name).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn create_profile(
    name: String,
    content: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        store.create(&name, &content).map_err(|e| e.to_string())?;
    }
    tray::refresh(&app).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn update_profile(
    name: String,
    content: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        store.write(&name, &content).map_err(|e| e.to_string())?;
    }
    tray::refresh(&app).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn delete_profile(
    name: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        store.delete(&name).map_err(|e| e.to_string())?;
    }
    tray::refresh(&app).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn rename_profile(
    old_name: String,
    new_name: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        store
            .rename(&old_name, &new_name)
            .map_err(|e| e.to_string())?;
    }
    tray::refresh(&app).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn duplicate_profile(
    source: String,
    new_name: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        store
            .duplicate(&source, &new_name)
            .map_err(|e| e.to_string())?;
    }
    tray::refresh(&app).map_err(|e| e.to_string())?;
    Ok(())
}

/// Returns Some(DriftInfo) when CLAUDE.md differs from the last-activated profile
/// file. Returns None when content matches, when there's no baseline (no toggle
/// yet this session and detect_active() returned "modified"), or when either
/// file is unreadable.
#[tauri::command]
pub fn check_drift(state: State<'_, AppState>) -> Result<Option<DriftInfo>, String> {
    let last_active = {
        let guard = state.last_active.lock().map_err(|e| e.to_string())?;
        guard.clone()
    };
    let Some(name) = last_active else {
        return Ok(None);
    };
    // "modified" / "none" baselines aren't comparable — only real profile names.
    if name == "modified" || name == "none" {
        return Ok(None);
    }
    let is_composed = state
        .active_is_composed
        .lock()
        .map(|g| *g)
        .unwrap_or(false);
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let target = store.target_path();
    // When the active file is composed (base + modifier regions), it matches no
    // flat profile — compare it against the composed baseline instead, otherwise
    // every FileWatcher tick would report false drift.
    let expected_path = if is_composed {
        store.profile_path(COMPOSED_NAME)
    } else {
        store.profile_path(&name)
    };
    Ok(detect_drift(&name, &target, &expected_path))
}

/// Resolution: write the current `CLAUDE.md` bytes back into the profile file
/// that was last activated. Effectively turns the drift edits into the new
/// canonical version of that profile.
#[tauri::command]
pub fn resolve_drift_apply_to_active(state: State<'_, AppState>) -> Result<(), String> {
    let last_active = {
        let guard = state.last_active.lock().map_err(|e| e.to_string())?;
        guard.clone().ok_or_else(|| "no active profile baseline".to_string())?
    };
    let result: Result<(), String> = (|| {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        let current = fs::read_to_string(store.target_path()).map_err(|e| e.to_string())?;
        if last_active == "origin" {
            fs::write(store.profile_path("origin"), current).map_err(|e| e.to_string())?;
        } else {
            store
                .write(&last_active, &current)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    match &result {
        Ok(()) => record_history(
            &state,
            Action::DriftApplyToActive,
            Some(&last_active),
            Some(&last_active),
            TARGET_GLOBAL,
            Ok(()),
        ),
        Err(msg) => record_history(
            &state,
            Action::DriftApplyToActive,
            Some(&last_active),
            Some(&last_active),
            TARGET_GLOBAL,
            Err(msg.as_str()),
        ),
    }
    result
}

/// Resolution: write current `CLAUDE.md` bytes into `CLAUDE.md.origin`,
/// promoting the drift edits as the new default baseline.
#[tauri::command]
pub fn resolve_drift_apply_to_origin(state: State<'_, AppState>) -> Result<(), String> {
    let result: Result<(), String> = (|| {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        let current = fs::read_to_string(store.target_path()).map_err(|e| e.to_string())?;
        fs::write(store.profile_path("origin"), current).map_err(|e| e.to_string())?;
        Ok(())
    })();
    match &result {
        Ok(()) => record_history(
            &state,
            Action::DriftApplyToOrigin,
            None,
            Some("origin"),
            TARGET_GLOBAL,
            Ok(()),
        ),
        Err(msg) => record_history(
            &state,
            Action::DriftApplyToOrigin,
            None,
            Some("origin"),
            TARGET_GLOBAL,
            Err(msg.as_str()),
        ),
    }
    result
}

/// Resolution: discard drift edits by re-applying the bytes of the last-active
/// profile file back onto `CLAUDE.md`.
#[tauri::command]
pub fn resolve_drift_discard(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    let last_active = {
        let guard = state.last_active.lock().map_err(|e| e.to_string())?;
        guard.clone().ok_or_else(|| "no active profile baseline".to_string())?
    };
    let result: Result<(), String> = {
        let engine = state.engine.lock().map_err(|e| e.to_string())?;
        engine.apply_named(&last_active).map_err(|e| e.to_string())
    };
    match &result {
        Ok(()) => record_history(
            &state,
            Action::DriftDiscard,
            None,
            Some(&last_active),
            TARGET_GLOBAL,
            Ok(()),
        ),
        Err(msg) => record_history(
            &state,
            Action::DriftDiscard,
            None,
            Some(&last_active),
            TARGET_GLOBAL,
            Err(msg.as_str()),
        ),
    }
    result?;
    // Discard re-applies the flat last-active profile, so the active file is no
    // longer composed.
    set_composed(&state, false);
    tray::refresh(&app).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn list_history(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<HistoryEntry>, String> {
    let history = state.history.lock().map_err(|e| e.to_string())?;
    history.list(limit.unwrap_or(100)).map_err(|e| e.to_string())
}

// --- Per-project MEMORY.md commands ---------------------------------------
//
// These mirror the global flow but instantiate a ProfileStore + ToggleEngine
// on the fly for the given project. AppState stays single-target; the FE
// passes the project id explicitly on every call.

#[tauri::command]
pub fn memory_list_projects() -> Result<Vec<MemoryProject>, String> {
    memory::list_projects(&default_claude_dir()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn memory_list_profiles(project_id: String) -> Result<Vec<ProfileInfo>, String> {
    let store = memory::store_for(&default_claude_dir(), &project_id);
    store.list().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn memory_get_active_profile(project_id: String) -> Result<String, String> {
    let store = memory::store_for(&default_claude_dir(), &project_id);
    store.detect_active().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn memory_toggle_profile(
    project_id: String,
    name: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let claude = default_claude_dir();
    let selection = {
        let dstore = state.doctree.lock().map_err(|e| e.to_string())?;
        dstore
            .list_selected_for(&project_id)
            .map_err(|e| e.to_string())?
    };
    let engine = memory::engine_for(&claude, &project_id);
    // Best-effort backup creation before the first toggle.
    if let Err(e) = engine.ensure_backup() {
        return Err(e.to_string());
    }
    let result: Result<(), String> = if selection.is_empty() {
        // No domains bound — flat toggle (v0.1 behavior).
        engine.apply_named(&name).map_err(|e| e.to_string())
    } else {
        // Domains bound to this project — compose the toggled-to base profile
        // with the preserved domain imports so the toggle does not silently drop
        // the binding (single MEMORY.md slot would otherwise clobber it).
        let store = memory::store_for(&claude, &project_id);
        let doctrees_dir = state.git.doctrees_dir();
        let domains_root = memory::domains_dir_for(&claude, &project_id);
        let kept = doctree::prune_missing(&doctrees_dir, &selection);
        let _guard =
            session_lock::acquire_blocking(engine.lock_path()).map_err(|e| e.to_string())?;
        let mut imports = Vec::new();
        for id in &kept {
            if let Ok(a) = doctree::materialize_domain_into(&doctrees_dir, &domains_root, id, "..") {
                imports.push(a.import_line);
            }
        }
        let _ = doctree::gc_orphans_in(&domains_root, &kept);
        let base_body = store.read(&name).unwrap_or_default();
        composer::compose_and_apply_locked(&engine, &base_body, &imports, None)
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    let target = target_for_memory(&project_id);
    match &result {
        Ok(()) => record_history(&state, Action::Toggle, None, Some(&name), &target, Ok(())),
        Err(msg) => record_history(
            &state,
            Action::Toggle,
            None,
            Some(&name),
            &target,
            Err(msg.as_str()),
        ),
    }
    result
}

#[tauri::command]
pub fn memory_read_profile(project_id: String, name: String) -> Result<String, String> {
    let store = memory::store_for(&default_claude_dir(), &project_id);
    store.read(&name).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn memory_create_profile(
    project_id: String,
    name: String,
    content: String,
) -> Result<(), String> {
    let store = memory::store_for(&default_claude_dir(), &project_id);
    store.create(&name, &content).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn memory_update_profile(
    project_id: String,
    name: String,
    content: String,
) -> Result<(), String> {
    let store = memory::store_for(&default_claude_dir(), &project_id);
    store.write(&name, &content).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn memory_delete_profile(project_id: String, name: String) -> Result<(), String> {
    let store = memory::store_for(&default_claude_dir(), &project_id);
    store.delete(&name).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn memory_rename_profile(
    project_id: String,
    old_name: String,
    new_name: String,
) -> Result<(), String> {
    let store = memory::store_for(&default_claude_dir(), &project_id);
    store
        .rename(&old_name, &new_name)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn memory_duplicate_profile(
    project_id: String,
    source: String,
    new_name: String,
) -> Result<(), String> {
    let store = memory::store_for(&default_claude_dir(), &project_id);
    store
        .duplicate(&source, &new_name)
        .map_err(|e| e.to_string())
}

// --- Directory-to-profile mappings (T7) -----------------------------------

#[tauri::command]
pub fn list_mappings(state: State<'_, AppState>) -> Result<Vec<DirectoryMapping>, String> {
    let store = state.mappings.lock().map_err(|e| e.to_string())?;
    store.list().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_mapping(
    dir_path: String,
    target: String,
    profile_name: String,
    state: State<'_, AppState>,
) -> Result<i64, String> {
    let store = state.mappings.lock().map_err(|e| e.to_string())?;
    store
        .add(&dir_path, &target, &profile_name)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_mapping(
    id: i64,
    target: String,
    profile_name: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let store = state.mappings.lock().map_err(|e| e.to_string())?;
    store
        .update(id, &target, &profile_name)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_mapping(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    let store = state.mappings.lock().map_err(|e| e.to_string())?;
    store.delete(id).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
pub struct ApplyMappingResult {
    /// `Some(mapping)` when a rule matched and was applied. `None` when no
    /// registered mapping matched — the FE can show a "no rule" message.
    pub matched: Option<DirectoryMapping>,
}

/// Find the best matching mapping for `dir_path` and apply it. For a "global"
/// target the global toggle engine runs through the standard recording path
/// (with history + tray refresh + drift baseline update). For a memory target
/// the per-project engine is instantiated on the fly, history is recorded
/// against the memory target, and no tray refresh is needed (the tray only
/// renders the global state).
#[tauri::command]
pub fn apply_mapping_for(
    dir_path: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<ApplyMappingResult, String> {
    let matched = {
        let store = state.mappings.lock().map_err(|e| e.to_string())?;
        store.find_match(&dir_path).map_err(|e| e.to_string())?
    };
    let Some(mapping) = matched else {
        return Ok(ApplyMappingResult { matched: None });
    };

    if mapping.target == TARGET_GLOBAL {
        // Route through the existing global flow so history/baseline/tray
        // stay consistent.
        let result = toggle_profile(mapping.profile_name.clone(), state, app);
        match result {
            Ok(()) => Ok(ApplyMappingResult {
                matched: Some(mapping),
            }),
            Err(e) => Err(e),
        }
    } else if let Some(project_id) = mapping.target.strip_prefix("memory:") {
        let engine = memory::engine_for(&default_claude_dir(), project_id);
        engine.ensure_backup().map_err(|e| e.to_string())?;
        let apply_result = engine
            .apply_named(&mapping.profile_name)
            .map_err(|e| e.to_string());
        let target = target_for_memory(project_id);
        match &apply_result {
            Ok(()) => record_history(
                &state,
                Action::Toggle,
                None,
                Some(&mapping.profile_name),
                &target,
                Ok(()),
            ),
            Err(msg) => record_history(
                &state,
                Action::Toggle,
                None,
                Some(&mapping.profile_name),
                &target,
                Err(msg.as_str()),
            ),
        }
        apply_result?;
        Ok(ApplyMappingResult {
            matched: Some(mapping),
        })
    } else if let Some(project_id) = mapping.target.strip_prefix("doctree:") {
        // Per-project domain binding: the domain ids live in
        // project_doctree_selection (not the mapping row), composed into the
        // project's MEMORY.md — never the global CLAUDE.md.
        let project_id = project_id.to_string();
        let ids = {
            let dstore = state.doctree.lock().map_err(|e| e.to_string())?;
            dstore
                .list_selected_for(&project_id)
                .map_err(|e| e.to_string())?
        };
        apply_project_doctrees(project_id, ids, state)?;
        Ok(ApplyMappingResult {
            matched: Some(mapping),
        })
    } else {
        Err(format!("unknown mapping target: {}", mapping.target))
    }
}

// --- v0.3 Connected Context: git sync (21-1) + domain doc-trees (21-2) -----

#[derive(serde::Serialize)]
pub struct SyncStatus {
    pub linked: bool,
    pub remote_url: Option<String>,
    pub branch: Option<String>,
    pub last_synced_sha: Option<String>,
    pub head_sha: Option<String>,
    pub auto_pull: bool,
    pub auto_push: bool,
}

#[derive(serde::Serialize)]
pub struct PullReport {
    pub entries: Vec<MaterializeEntry>,
    pub head_sha: String,
    /// Whether `last_synced_sha` advanced (true only when there were no conflicts).
    pub advanced: bool,
    pub conflicts: usize,
}

#[derive(serde::Serialize)]
pub struct DoctreeInfo {
    pub id: String,
    pub display: String,
    pub tags: Vec<String>,
    pub est_tokens: u32,
    pub selected: bool,
}

#[derive(serde::Serialize)]
pub struct ApplyDoctreesResult {
    pub applied: Vec<DomainApply>,
    pub composed: bool,
}

/// Acquire the long-running git lock (separate from the swap lock) next to the
/// active target. Held only across git I/O — never while a swap-lock apply runs,
/// to keep the lock ordering deadlock-free.
fn acquire_git_lock(state: &AppState) -> Result<SessionGuard, String> {
    let target = {
        let engine = state.engine.lock().map_err(|e| e.to_string())?;
        engine.target().to_path_buf()
    };
    let lock_path = session_lock::default_git_lock_path(&target);
    session_lock::acquire_blocking(&lock_path).map_err(|e| e.to_string())
}

/// The slice of local SQLite state that is safe to publish into the shared
/// (possibly public) context repo's manifest. PRIVACY CHOKEPOINT — v0.4 MVP
/// returns empty: directory_mappings carry per-machine absolute `dir_path`s
/// (and `memory:`/`doctree:` targets), so syncing them needs an explicit
/// user opt-in + a dir_path privacy model (deferred to v0.5). Because the
/// manifest is regenerated on EVERY push, flipping this on would silently
/// egress paths on routine profile pushes.
fn syncable_mappings(_state: &AppState) -> Vec<crate::core::git_sync::MappingEntry> {
    Vec::new()
}

/// The drift-comparable baseline profile name, or `None` for the non-comparable
/// "modified" / "none" sentinels.
fn real_last_active(state: &AppState) -> Option<String> {
    let guard = state.last_active.lock().ok()?;
    match guard.clone() {
        Some(n) if n != "modified" && n != "none" => Some(n),
        _ => None,
    }
}

#[tauri::command]
pub fn get_sync_status(state: State<'_, AppState>) -> Result<SyncStatus, String> {
    let cfg = {
        let store = state.sync_config.lock().map_err(|e| e.to_string())?;
        store.get().map_err(|e| e.to_string())?
    };
    let head_sha = if state.git.is_linked() {
        state.git.head_sha().ok()
    } else {
        None
    };
    Ok(match cfg {
        Some(c) => SyncStatus {
            linked: true,
            remote_url: Some(c.remote_url),
            branch: Some(c.branch),
            last_synced_sha: c.last_synced_sha,
            head_sha,
            auto_pull: c.auto_pull,
            auto_push: c.auto_push,
        },
        None => SyncStatus {
            linked: false,
            remote_url: None,
            branch: None,
            last_synced_sha: None,
            head_sha: None,
            auto_pull: true,
            auto_push: false,
        },
    })
}

/// Store a fine-grained PAT for a remote in the OS keychain (never persisted to
/// SQLite/manifest/repo). Used when the OS credential helper can't supply a token.
#[tauri::command]
pub fn set_repo_pat(remote_url: String, pat: String) -> Result<(), String> {
    git_sync::store_pat(&remote_url, &pat).map_err(|e| e.to_string())
}

/// Link (or re-link) a context repo: clone it into the hidden mirror and
/// materialize its profiles onto the flat namespace. Conflicts (a flat profile
/// that already differs from the repo) are reported, never clobbered.
#[tauri::command]
pub fn link_repo(
    remote_url: String,
    branch: String,
    pat: Option<String>,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<SyncStatus, String> {
    if let Some(pat) = pat.as_deref() {
        if !pat.is_empty() {
            git_sync::store_pat(&remote_url, pat).map_err(|e| e.to_string())?;
        }
    }
    let claude = default_claude_dir();
    let (report, head) = {
        let _git = acquire_git_lock(&state)?;
        state
            .git
            .clone_or_open(&remote_url, &branch)
            .map_err(|e| e.to_string())?;
        {
            let store = state.sync_config.lock().map_err(|e| e.to_string())?;
            store.link(&remote_url, &branch).map_err(|e| e.to_string())?;
        }
        let report = state
            .git
            .materialize_profiles(&claude, TARGET_NAME, &[])
            .map_err(|e| e.to_string())?;
        let head = state.git.head_sha().map_err(|e| e.to_string())?;
        (report, head)
    };
    let conflicts = count_conflicts(&report);
    if conflicts == 0 {
        if let Ok(store) = state.sync_config.lock() {
            let _ = store.set_synced_sha(&head);
        }
    }
    record_history(&state, Action::GitPull, None, Some("link"), TARGET_GLOBAL, Ok(()));
    tray::refresh(&app).map_err(|e| e.to_string())?;
    get_sync_status(state)
}

/// Fetch + fast-forward the mirror, then materialize onto the flat namespace.
/// Per the partial-pull rule, `last_synced_sha` only advances when there are no
/// conflicts. If a fast-forwarded profile is the active one, the active file is
/// re-applied (or recomposed) so it never desyncs from its profile.
#[tauri::command]
pub fn fetch_repo(state: State<'_, AppState>, app: AppHandle) -> Result<PullReport, String> {
    let report = pull_once(&state)?;
    tray::refresh(&app).map_err(|e| e.to_string())?;
    Ok(report)
}

/// The pull pipeline shared by the `fetch_repo` command and the startup
/// auto-pull hook: fetch + hard-reset the mirror, materialize onto the flat
/// namespace, advance the synced sha only when conflict-free, and re-apply the
/// active file if its profile moved. Tray refresh is the caller's responsibility.
pub(crate) fn pull_once(state: &AppState) -> Result<PullReport, String> {
    let cfg = {
        let store = state.sync_config.lock().map_err(|e| e.to_string())?;
        store
            .get()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "no repo linked".to_string())?
    };
    let claude = default_claude_dir();
    // Git I/O under the git lock only — released before any swap-lock apply.
    let (report, head) = {
        let _git = acquire_git_lock(state)?;
        let old = state.git.snapshot_mirror_profiles(TARGET_NAME);
        // Snapshot local doctree dirs so a hard reset that discards a local-only
        // (un-pushed) doctree can restore it — the concurrent-authoring loser
        // keeps its work and fast-forwards on the next push.
        let local_doctrees = state.git.snapshot_mirror_doctrees();
        let head = state
            .git
            .fetch_remote_head(&cfg.branch)
            .map_err(|e| e.to_string())?;
        state.git.reset_hard_to(&head).map_err(|e| e.to_string())?;
        state
            .git
            .restore_missing_doctrees(&local_doctrees)
            .map_err(|e| e.to_string())?;
        let report = state
            .git
            .materialize_profiles(&claude, TARGET_NAME, &old)
            .map_err(|e| e.to_string())?;
        (report, head)
    };
    let conflicts = count_conflicts(&report);
    if conflicts == 0 {
        if let Ok(store) = state.sync_config.lock() {
            let _ = store.set_synced_sha(&head);
        }
    }
    reapply_active_after_pull(state, &report)?;
    record_history(state, Action::GitPull, None, Some("fetch"), TARGET_GLOBAL, Ok(()));
    Ok(PullReport {
        entries: report,
        head_sha: head,
        advanced: conflicts == 0,
        conflicts,
    })
}

/// Copy the flat profiles into the mirror, commit, and push. Non-fast-forward
/// pushes are rejected by the remote (never forced) — the user pulls + reconciles
/// then retries.
#[tauri::command]
pub fn push_repo(state: State<'_, AppState>) -> Result<(), String> {
    let cfg = {
        let store = state.sync_config.lock().map_err(|e| e.to_string())?;
        store
            .get()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "no repo linked".to_string())?
    };
    let claude = default_claude_dir();
    let mappings = syncable_mappings(&state);
    let _git = acquire_git_lock(&state)?;
    state
        .git
        .stage_flat_profiles(&claude, TARGET_NAME)
        .map_err(|e| e.to_string())?;
    // Regenerate manifest as a derived artifact (read-existing-first) so newly
    // authored profiles/doctrees are registered, then stage ONLY the intended
    // paths (no add_all glob) and commit.
    state
        .git
        .regenerate_manifest(TARGET_NAME, &mappings, &[])
        .map_err(|e| e.to_string())?;
    state
        .git
        .stage_paths(&["profiles/", "doctrees/", "manifest.toml"])
        .map_err(|e| e.to_string())?;
    state
        .git
        .commit_tree("toggler: sync profiles + manifest")
        .map_err(|e| e.to_string())?;
    let result = state.git.push(&cfg.branch).map_err(|e| e.to_string());
    match &result {
        Ok(()) => {
            if let Ok(head) = state.git.head_sha() {
                if let Ok(store) = state.sync_config.lock() {
                    let _ = store.set_synced_sha(&head);
                }
            }
            record_history(&state, Action::GitPush, None, Some(&cfg.branch), TARGET_GLOBAL, Ok(()));
        }
        Err(msg) => record_history(
            &state,
            Action::GitPush,
            None,
            Some(&cfg.branch),
            TARGET_GLOBAL,
            Err(msg.as_str()),
        ),
    }
    result
}

#[tauri::command]
pub fn set_sync_auto(
    auto_pull: bool,
    auto_push: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let store = state.sync_config.lock().map_err(|e| e.to_string())?;
    store.set_auto(auto_pull, auto_push).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn unlink_repo(state: State<'_, AppState>) -> Result<(), String> {
    let store = state.sync_config.lock().map_err(|e| e.to_string())?;
    store.clear().map_err(|e| e.to_string())
}

/// List the domain doc-trees declared in the linked repo's manifest, each flagged
/// with whether it is currently part of the active selection.
#[tauri::command]
pub fn list_doctrees(state: State<'_, AppState>) -> Result<Vec<DoctreeInfo>, String> {
    let manifest_path = state.git.manifest_path();
    let entries = if manifest_path.exists() {
        let text = std::fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
        git_sync::parse_manifest(&text)
            .map_err(|e| e.to_string())?
            .doctrees
    } else {
        Vec::new()
    };
    let selected = {
        let store = state.doctree.lock().map_err(|e| e.to_string())?;
        store.list_selected().map_err(|e| e.to_string())?
    };
    Ok(entries
        .into_iter()
        .map(|d| DoctreeInfo {
            selected: selected.iter().any(|s| s == &d.id),
            id: d.id,
            display: d.display,
            tags: d.tags,
            est_tokens: d.est_tokens,
        })
        .collect())
}

/// Read a domain's INDEX.md from the mirror for preview. The id is traversal-checked.
#[tauri::command]
pub fn read_doctree_index(id: String, state: State<'_, AppState>) -> Result<String, String> {
    doctree::validate_domain_id(&id).map_err(|e| e.to_string())?;
    let path = state
        .git
        .doctrees_dir()
        .join(&id)
        .join(doctree::INDEX_NAME);
    std::fs::read_to_string(path).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
pub struct CreateDoctreeResult {
    pub id: String,
    /// True when the new doctree was pushed to the remote. False means it was
    /// committed locally but the push was rejected (e.g. non-fast-forward) — the
    /// FE shows a 'pull then retry' hint and the local work is preserved.
    pub pushed: bool,
    pub push_error: Option<String>,
}

fn default_index_body(id: &str, display: &str) -> String {
    let title = if display.is_empty() { id } else { display };
    format!(
        "# {}\n\n<!-- Domain doc-tree '{}'. Fan out to detail files with @relative.md imports (max 4 hops). -->\n",
        title, id
    )
}

/// Author a new domain doc-tree: scaffold `doctrees/{id}/INDEX.md` in the mirror,
/// register it in the manifest (regenerated as a derived artifact), and push.
/// Id-uniqueness is checked against the MANIFEST (repo truth), so an id already
/// present from a prior pull yields a clear "pull to use / pick another id"
/// message rather than a confusing overwrite error. Never touches global active
/// state; serialized with other git I/O on the single git lock.
#[tauri::command]
pub fn create_doctree(
    id: String,
    display: String,
    tags: Vec<String>,
    est_tokens: u32,
    index_body: Option<String>,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<CreateDoctreeResult, String> {
    doctree::validate_domain_id(&id).map_err(|e| e.to_string())?;
    let cfg = {
        let store = state.sync_config.lock().map_err(|e| e.to_string())?;
        store
            .get()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "no repo linked".to_string())?
    };
    let mappings = syncable_mappings(&state);
    let body = index_body.unwrap_or_else(|| default_index_body(&id, &display));

    let _git = acquire_git_lock(&state)?;

    // Id uniqueness against the manifest (repo truth), distinct from on-disk.
    let manifest_path = state.git.manifest_path();
    if manifest_path.exists() {
        let text = std::fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
        let m = git_sync::parse_manifest(&text).map_err(|e| e.to_string())?;
        if m.doctrees.iter().any(|d| d.id == id) {
            return Err(format!(
                "doctree '{}' already exists in the repo — pull to use it, or pick a different id",
                id
            ));
        }
    }

    state
        .git
        .scaffold_doctree(&id, &body)
        .map_err(|e| e.to_string())?;
    let overrides = vec![git_sync::DoctreeEntry {
        id: id.clone(),
        display,
        tags,
        est_tokens,
    }];
    state
        .git
        .regenerate_manifest(TARGET_NAME, &mappings, &overrides)
        .map_err(|e| e.to_string())?;
    state
        .git
        .stage_paths(&["doctrees/", "manifest.toml", "profiles/"])
        .map_err(|e| e.to_string())?;
    state
        .git
        .commit_tree(&format!("toggler: author doctree {}", id))
        .map_err(|e| e.to_string())?;

    let (pushed, push_error) = match state.git.push(&cfg.branch) {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e.to_string())),
    };
    record_history(
        &state,
        Action::DoctreeCreate,
        None,
        Some(&id),
        TARGET_GLOBAL,
        if pushed { Ok(()) } else { Err("push rejected") },
    );
    tray::refresh(&app).map_err(|e| e.to_string())?;
    Ok(CreateDoctreeResult {
        id,
        pushed,
        push_error,
    })
}

// --- v0.4 per-project domain binding (feature B) --------------------------

#[derive(serde::Serialize)]
pub struct ApplyProjectDoctreesResult {
    pub applied: Vec<DomainApply>,
    pub composed: bool,
    /// Ids dropped because their doctree no longer exists in the repo.
    pub pruned: Vec<String>,
}

/// The base body for a per-project compose: the project's active MEMORY profile
/// bytes when there is one, else the stripped current MEMORY.md (recovers the
/// base from an already-composed file, or empty for a fresh project).
fn memory_base_body(
    store: &crate::core::profile_store::ProfileStore,
    engine: &crate::core::toggle_engine::ToggleEngine,
) -> String {
    if let Ok(active) = store.detect_active() {
        if active != "modified" && active != "none" {
            if let Ok(content) = store.read(&active) {
                return content;
            }
        }
    }
    std::fs::read_to_string(engine.target())
        .map(|c| composer::strip_blocks(&c))
        .unwrap_or_default()
}

/// Bind an additive domain selection to ONE project, composing it into that
/// project's `~/.claude/projects/{id}/memory/MEMORY.md` — never the global
/// CLAUDE.md. The per-project swap lock is held across materialize + gc +
/// compose+apply so a concurrent apply of the same project can't torn-read the
/// domains root. Global slots (active_is_composed/last_active/engine) are untouched.
#[tauri::command]
pub fn apply_project_doctrees(
    project_id: String,
    ids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<ApplyProjectDoctreesResult, String> {
    let claude = default_claude_dir();
    let doctrees_dir = state.git.doctrees_dir();
    // Drop ids whose doctree was deleted from the repo so a dangling selection
    // can't wedge re-apply with NotFound.
    let kept = doctree::prune_missing(&doctrees_dir, &ids);
    let pruned: Vec<String> = ids.into_iter().filter(|i| !kept.contains(i)).collect();

    let engine = memory::engine_for(&claude, &project_id);
    let store = memory::store_for(&claude, &project_id);
    let domains_root = memory::domains_dir_for(&claude, &project_id);

    let mut applied = Vec::new();
    let mut imports = Vec::new();
    {
        let _guard =
            session_lock::acquire_blocking(engine.lock_path()).map_err(|e| e.to_string())?;
        for id in &kept {
            let a = doctree::materialize_domain_into(&doctrees_dir, &domains_root, id, "..")
                .map_err(|e| e.to_string())?;
            imports.push(a.import_line.clone());
            applied.push(a);
        }
        doctree::gc_orphans_in(&domains_root, &kept).map_err(|e| e.to_string())?;
        let base_body = memory_base_body(&store, &engine);
        composer::compose_and_apply_locked(&engine, &base_body, &imports, None)
            .map_err(|e| e.to_string())?;
    }

    {
        let dstore = state.doctree.lock().map_err(|e| e.to_string())?;
        dstore
            .set_selected_for(&project_id, &kept)
            .map_err(|e| e.to_string())?;
    }
    record_history(
        &state,
        Action::DoctreeApply,
        None,
        Some(&format!("{} domain(s)", kept.len())),
        &target_for_project_doctree(&project_id),
        Ok(()),
    );
    Ok(ApplyProjectDoctreesResult {
        applied,
        composed: !kept.is_empty(),
        pruned,
    })
}

/// The repo's doctrees flagged with whether each is in THIS project's binding.
#[tauri::command]
pub fn list_project_doctrees(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<DoctreeInfo>, String> {
    let manifest_path = state.git.manifest_path();
    let entries = if manifest_path.exists() {
        let text = std::fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
        git_sync::parse_manifest(&text)
            .map_err(|e| e.to_string())?
            .doctrees
    } else {
        Vec::new()
    };
    let selected = {
        let dstore = state.doctree.lock().map_err(|e| e.to_string())?;
        dstore
            .list_selected_for(&project_id)
            .map_err(|e| e.to_string())?
    };
    Ok(entries
        .into_iter()
        .map(|d| DoctreeInfo {
            selected: selected.iter().any(|s| s == &d.id),
            id: d.id,
            display: d.display,
            tags: d.tags,
            est_tokens: d.est_tokens,
        })
        .collect())
}

/// Apply an additive domain selection: materialize each selected doc-tree into
/// `~/.claude/domains/{id}`, GC the now-unselected ones, and recompose the active
/// CLAUDE.md (base + `@import` lines) through the shared composer. An empty
/// selection reverts to the flat base profile.
#[tauri::command]
pub fn apply_doctrees(
    ids: Vec<String>,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<ApplyDoctreesResult, String> {
    let claude = default_claude_dir();
    let doctrees_dir = state.git.doctrees_dir();

    let mut applied = Vec::new();
    let mut imports = Vec::new();
    for id in &ids {
        let a = doctree::materialize_domain(&doctrees_dir, &claude, id).map_err(|e| e.to_string())?;
        imports.push(a.import_line.clone());
        applied.push(a);
    }
    doctree::gc_orphans(&claude, &ids).map_err(|e| e.to_string())?;

    let base_name = real_last_active(&state);
    {
        let engine = state.engine.lock().map_err(|e| e.to_string())?;
        if ids.is_empty() {
            // Reverting to the plain base: re-apply the exact flat profile bytes
            // when we have one (avoids any composed-vs-flat newline drift).
            if let Some(name) = &base_name {
                engine.apply_named(name).map_err(|e| e.to_string())?;
            } else {
                let current = std::fs::read_to_string(engine.target()).unwrap_or_default();
                let base = composer::strip_blocks(&current);
                composer::compose_and_apply(&engine, &base, &[], None)
                    .map_err(|e| e.to_string())?;
            }
        } else {
            let current = std::fs::read_to_string(engine.target()).unwrap_or_default();
            let base = composer::strip_blocks(&current);
            composer::compose_and_apply(&engine, &base, &imports, None)
                .map_err(|e| e.to_string())?;
        }
    }

    {
        let store = state.doctree.lock().map_err(|e| e.to_string())?;
        store.set_selected(&ids).map_err(|e| e.to_string())?;
    }
    let composed = !ids.is_empty();
    set_composed(&state, composed);
    if let Some(name) = &base_name {
        record_active(&state, name);
    }
    record_history(
        &state,
        Action::DoctreeApply,
        None,
        Some(&format!("{} domain(s)", ids.len())),
        TARGET_GLOBAL,
        Ok(()),
    );
    tray::refresh(&app).map_err(|e| e.to_string())?;
    Ok(ApplyDoctreesResult { applied, composed })
}

fn count_conflicts(report: &[MaterializeEntry]) -> usize {
    report
        .iter()
        .filter(|e| matches!(e.outcome, MaterializeOutcome::Conflict { .. }))
        .count()
}

/// After a pull, if the active profile's flat file was updated (Created /
/// FastForward), re-apply it so the live CLAUDE.md follows the change instead of
/// silently desyncing. The git lock must already be released (this takes the swap
/// lock). Recomposes when the active file is composed.
fn reapply_active_after_pull(
    state: &AppState,
    report: &[MaterializeEntry],
) -> Result<(), String> {
    let Some(name) = real_last_active(state) else {
        return Ok(());
    };
    let changed = report.iter().any(|e| {
        e.name == name
            && matches!(
                e.outcome,
                MaterializeOutcome::Created | MaterializeOutcome::FastForward
            )
    });
    if !changed {
        return Ok(());
    }
    let composed = state
        .active_is_composed
        .lock()
        .map(|g| *g)
        .unwrap_or(false);
    if composed {
        recompose_current_selection(state)?;
    } else {
        let engine = state.engine.lock().map_err(|e| e.to_string())?;
        engine.apply_named(&name).map_err(|e| e.to_string())?;
    }
    record_active(state, &name);
    Ok(())
}

/// Rebuild the composed active file from the persisted domain selection on top of
/// the current (stripped) base. Used after a pull touches the active base.
fn recompose_current_selection(state: &AppState) -> Result<(), String> {
    let ids = {
        let store = state.doctree.lock().map_err(|e| e.to_string())?;
        store.list_selected().map_err(|e| e.to_string())?
    };
    let claude = default_claude_dir();
    let doctrees_dir = state.git.doctrees_dir();
    let mut imports = Vec::new();
    for id in &ids {
        if let Ok(a) = doctree::materialize_domain(&doctrees_dir, &claude, id) {
            imports.push(a.import_line);
        }
    }
    let engine = state.engine.lock().map_err(|e| e.to_string())?;
    let current = std::fs::read_to_string(engine.target()).unwrap_or_default();
    let base = composer::strip_blocks(&current);
    composer::compose_and_apply(&engine, &base, &imports, None).map_err(|e| e.to_string())?;
    Ok(())
}
