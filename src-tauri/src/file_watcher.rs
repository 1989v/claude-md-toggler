//! Background file watcher: emits a Tauri event whenever the active target
//! file changes on disk. The frontend listens and re-queries `check_drift` so
//! the UI surfaces external edits in real time without having to poll.
//!
//! Uses `notify-debouncer-full` so that a single editor save (which can emit
//! several Modify/Metadata events in quick succession) collapses into one event
//! to the frontend.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

pub const EVENT_NAME: &str = "claude-md:changed";

/// Payload for `claude-md:changed`. Currently a single path string; reserved
/// shape so we can expand later (e.g. with the new mtime, content hash) without
/// breaking the FE contract.
#[derive(Debug, Clone, Serialize)]
pub struct ChangedPayload {
    pub path: PathBuf,
}

/// Owns the debouncer for the lifetime of the app. Dropping this stops the
/// watcher thread.
pub struct WatcherHandle {
    _debouncer: Debouncer<notify::RecommendedWatcher, RecommendedCache>,
}

/// Start watching the directory containing `target` (non-recursive). Emits
/// `claude-md:changed` whenever any debounced fs event references `target`.
///
/// We watch the parent directory rather than the file directly because atomic
/// rename operations (the toggle engine's own write path) replace the inode,
/// which a direct file-level watcher would lose track of on some platforms.
pub fn start(app: AppHandle, target: PathBuf) -> notify::Result<WatcherHandle> {
    let watch_dir = target
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let target_for_handler = target.clone();
    let app_for_handler = app.clone();

    let mut debouncer = new_debouncer(
        Duration::from_millis(250),
        None,
        move |result: DebounceEventResult| match result {
            Ok(events) => {
                let touches_target = events
                    .iter()
                    .flat_map(|e| e.paths.iter())
                    .any(|p| p == &target_for_handler);
                if !touches_target {
                    return;
                }
                let payload = ChangedPayload {
                    path: target_for_handler.clone(),
                };
                if let Err(e) = app_for_handler.emit(EVENT_NAME, payload) {
                    eprintln!("[watcher] emit failed: {}", e);
                }
            }
            Err(errs) => {
                for e in errs {
                    eprintln!("[watcher] notify error: {}", e);
                }
            }
        },
    )?;

    debouncer.watch(&watch_dir, RecursiveMode::NonRecursive)?;

    Ok(WatcherHandle {
        _debouncer: debouncer,
    })
}
