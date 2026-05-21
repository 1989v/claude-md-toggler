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
    /// Profile name last successfully toggled into the active target. Used as
    /// the baseline for drift detection — comparison happens against the
    /// matching `CLAUDE.md.{last_active}` file's bytes at check time, so the
    /// baseline tracks profile-file edits made through the editor UI as well
    /// as toggle operations.
    pub last_active: Mutex<Option<String>>,
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

    // Best-effort initial baseline: whichever profile currently matches the
    // target byte-for-byte. Falls back to "modified"/"none" handled later.
    let initial_active = store.detect_active().ok();

    tauri::Builder::default()
        .manage(AppState {
            store: Mutex::new(store),
            engine: Mutex::new(engine),
            last_active: Mutex::new(initial_active),
        })
        .setup(move |app| {
            if let Err(e) = seeding::seed_presets(&claude_dir, TARGET_NAME) {
                eprintln!("[seed] failed to seed presets: {}", e);
            }
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
            commands::read_profile,
            commands::create_profile,
            commands::update_profile,
            commands::delete_profile,
            commands::rename_profile,
            commands::duplicate_profile,
            commands::check_drift,
            commands::resolve_drift_apply_to_active,
            commands::resolve_drift_apply_to_origin,
            commands::resolve_drift_discard,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Helper used by commands and the tray to record the profile name a successful
/// toggle just applied. Keeps the drift baseline in sync.
pub(crate) fn record_active(state: &AppState, name: &str) {
    if let Ok(mut guard) = state.last_active.lock() {
        *guard = Some(name.to_string());
    }
}
