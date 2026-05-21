mod commands;
mod core;
mod seeding;
mod tray;

use std::path::PathBuf;
use std::sync::Mutex;

use tauri::Manager;

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

const TARGET_NAME: &str = "CLAUDE.md";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let claude_dir = default_claude_dir();
    let store = ProfileStore::new(claude_dir.clone(), TARGET_NAME.to_string());
    let engine = ToggleEngine::new(store.target_path());

    tauri::Builder::default()
        .manage(AppState {
            store: Mutex::new(store),
            engine: Mutex::new(engine),
        })
        .setup(move |app| {
            // Seed pre-defined profiles on first run (idempotent — never overwrites
            // existing user customizations).
            if let Err(e) = seeding::seed_presets(&claude_dir, TARGET_NAME) {
                eprintln!("[seed] failed to seed presets: {}", e);
            }
            // Ensure the origin backup exists from the very first launch so toggle
            // operations always have something to fall back to.
            if let Ok(eng) = app.state::<AppState>().engine.lock() {
                if let Err(e) = eng.ensure_backup() {
                    eprintln!("[seed] failed to ensure origin backup: {}", e);
                }
            }
            tray::setup(app.handle())?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_profiles,
            commands::get_active_profile,
            commands::toggle_profile,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
