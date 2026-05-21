use std::fs;

use tauri::{AppHandle, State};

use crate::core::drift::{detect as detect_drift, DriftInfo};
use crate::core::profile_store::ProfileInfo;
use crate::{record_active, tray, AppState};

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
    {
        let engine = state.engine.lock().map_err(|e| e.to_string())?;
        engine.apply_named(&name).map_err(|e| e.to_string())?;
    }
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
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let current = fs::read_to_string(store.target_path()).map_err(|e| e.to_string())?;
    if last_active == "origin" {
        // Apply to origin is handled by its own command; route there for clarity.
        fs::write(store.profile_path("origin"), current).map_err(|e| e.to_string())?;
    } else {
        store
            .write(&last_active, &current)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Resolution: write current `CLAUDE.md` bytes into `CLAUDE.md.origin`,
/// promoting the drift edits as the new default baseline.
#[tauri::command]
pub fn resolve_drift_apply_to_origin(state: State<'_, AppState>) -> Result<(), String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let current = fs::read_to_string(store.target_path()).map_err(|e| e.to_string())?;
    fs::write(store.profile_path("origin"), current).map_err(|e| e.to_string())?;
    Ok(())
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
    {
        let engine = state.engine.lock().map_err(|e| e.to_string())?;
        engine.apply_named(&last_active).map_err(|e| e.to_string())?;
    }
    tray::refresh(&app).map_err(|e| e.to_string())?;
    Ok(())
}
