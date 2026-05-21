use tauri::{AppHandle, State};

use crate::core::profile_store::ProfileInfo;
use crate::{tray, AppState};

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
    // Refresh tray menu so the active marker stays in sync with the window UI.
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
    // Updating the content of the active profile changes the byte-equality test,
    // so refresh to keep the marker correct.
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
