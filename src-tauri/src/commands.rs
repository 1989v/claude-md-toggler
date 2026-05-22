use std::fs;

use tauri::{AppHandle, State};

use crate::core::drift::{detect as detect_drift, DriftInfo};
use crate::core::history::{target_for_memory, Action, HistoryEntry, TARGET_GLOBAL};
use crate::core::mappings::DirectoryMapping;
use crate::core::memory::{self, MemoryProject};
use crate::core::profile_store::ProfileInfo;
use crate::{default_claude_dir, record_active, tray, AppState};

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
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let target = store.target_path();
    let profile_path = store.profile_path(&name);
    Ok(detect_drift(&name, &target, &profile_path))
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
    let engine = memory::engine_for(&default_claude_dir(), &project_id);
    // Best-effort backup creation before the first toggle.
    if let Err(e) = engine.ensure_backup() {
        return Err(e.to_string());
    }
    let result = engine.apply_named(&name).map_err(|e| e.to_string());
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
    } else {
        Err(format!("unknown mapping target: {}", mapping.target))
    }
}
