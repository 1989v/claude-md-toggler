use tauri::State;

use crate::core::profile_store::ProfileInfo;
use crate::AppState;

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
pub fn toggle_profile(name: String, state: State<'_, AppState>) -> Result<(), String> {
    let engine = state.engine.lock().map_err(|e| e.to_string())?;
    engine.apply_named(&name).map_err(|e| e.to_string())
}
