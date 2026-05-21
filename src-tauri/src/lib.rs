mod core;
mod commands;

use std::path::PathBuf;
use std::sync::Mutex;

use core::profile_store::ProfileStore;
use core::toggle_engine::ToggleEngine;

pub struct AppState {
    pub store: Mutex<ProfileStore>,
    pub engine: Mutex<ToggleEngine>,
}

fn default_claude_dir() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".claude"))
        .unwrap_or_else(|| PathBuf::from(".claude"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let claude_dir = default_claude_dir();
    let target_name = "CLAUDE.md".to_string();
    let store = ProfileStore::new(claude_dir.clone(), target_name.clone());
    let engine = ToggleEngine::new(store.target_path());

    tauri::Builder::default()
        .manage(AppState {
            store: Mutex::new(store),
            engine: Mutex::new(engine),
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_profiles,
            commands::get_active_profile,
            commands::toggle_profile,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
