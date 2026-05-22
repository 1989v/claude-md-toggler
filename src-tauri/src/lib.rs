mod commands;
mod core;
mod file_watcher;
mod seeding;
mod tray;

use std::path::PathBuf;
use std::sync::Mutex;

use tauri::Manager;

use core::history::{default_db_path, HistoryStore};
use core::mappings::MappingsStore;
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
    /// Persistent append-only log of toggle/drift-resolution actions.
    pub history: Mutex<HistoryStore>,
    /// Persistent directory → profile rules. Shares the same SQLite file as
    /// `history` (different table) so a single backup covers both.
    pub mappings: Mutex<MappingsStore>,
}

pub(crate) fn default_claude_dir() -> PathBuf {
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

    // Open (or create) the history database at ~/.claude/.toggler-history.db.
    // A failure here is non-fatal — fall back to in-memory so the app keeps
    // working even when disk persistence is broken (e.g. permissions issue).
    let db_path = default_db_path(&claude_dir);
    let history = HistoryStore::open(&db_path).unwrap_or_else(|e| {
        eprintln!(
            "[history] failed to open {} — events will not persist: {}",
            db_path.display(),
            e
        );
        HistoryStore::in_memory().expect("in-memory sqlite must always succeed")
    });
    let mappings = MappingsStore::open(&db_path).unwrap_or_else(|e| {
        eprintln!(
            "[mappings] failed to open {} — rules will not persist: {}",
            db_path.display(),
            e
        );
        MappingsStore::in_memory().expect("in-memory sqlite must always succeed")
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_positioner::init())
        .manage(AppState {
            store: Mutex::new(store),
            engine: Mutex::new(engine),
            last_active: Mutex::new(initial_active),
            history: Mutex::new(history),
            mappings: Mutex::new(mappings),
        })
        .setup(move |app| {
            if let Err(e) = seeding::seed_presets(&claude_dir, TARGET_NAME) {
                eprintln!("[seed] failed to seed presets: {}", e);
            }
            let target_path = {
                let state = app.state::<AppState>();
                let eng = state.engine.lock().expect("engine mutex poisoned");
                if let Err(e) = eng.ensure_backup() {
                    eprintln!("[seed] failed to ensure origin backup: {}", e);
                }
                eng.target().to_path_buf()
            };
            tray::setup(app.handle())?;
            match file_watcher::start(app.handle().clone(), target_path) {
                Ok(handle) => {
                    // Keep the watcher alive for the app lifetime by parking it
                    // in app state — `manage` of a separate type so it doesn't
                    // collide with AppState.
                    app.manage(handle);
                }
                Err(e) => eprintln!("[watcher] start failed: {}", e),
            }
            // Apply native vibrancy under the webview so the popover blurs the
            // wallpaper / windows underneath, matching the muxbar / standard
            // NSPopover look. The webview body must keep a transparent
            // background for this effect to show through.
            #[cfg(target_os = "macos")]
            {
                use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState};
                if let Some(win) = app.get_webview_window("main") {
                    let _ = apply_vibrancy(
                        &win,
                        NSVisualEffectMaterial::HudWindow,
                        Some(NSVisualEffectState::Active),
                        Some(12.0),
                    );
                }
            }
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
            commands::list_history,
            commands::memory_list_projects,
            commands::memory_list_profiles,
            commands::memory_get_active_profile,
            commands::memory_toggle_profile,
            commands::memory_read_profile,
            commands::memory_create_profile,
            commands::memory_update_profile,
            commands::memory_delete_profile,
            commands::memory_rename_profile,
            commands::memory_duplicate_profile,
            commands::list_mappings,
            commands::add_mapping,
            commands::update_mapping,
            commands::delete_mapping,
            commands::apply_mapping_for,
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
