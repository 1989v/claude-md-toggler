//! System tray integration: builds a menu of profiles + an Origin/Quit footer.
//!
//! Menu items use stable IDs so click handlers can route directly to the
//! ToggleEngine without re-parsing labels. The active profile is marked with
//! a leading "● " in its label so users see at a glance which one is live.

use std::sync::Mutex;

use tauri::menu::{Menu, MenuBuilder, MenuItem, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

use crate::AppState;

const PROFILE_PREFIX: &str = "profile:";
const ORIGIN_ID: &str = "profile:origin";
const QUIT_ID: &str = "quit";
const SHOW_ID: &str = "show";

/// Mutable handle to the tray icon, kept in app state so toggle events can
/// rebuild the menu and refresh the active marker.
pub struct TrayState(pub Mutex<Option<TrayIcon>>);

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app)?;
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| {
            tauri::Error::Io(std::io::Error::other(
                "default window icon missing from config",
            ))
        })?;

    let tray = TrayIconBuilder::with_id("main")
        .icon(icon)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(handle_menu_event)
        .on_tray_icon_event(|tray, event| {
            // Left-click on the icon itself surfaces the main window on platforms
            // (Linux) where show_menu_on_left_click does not implicitly do so.
            if let TrayIconEvent::Click { .. } = event {
                if let Some(window) = tray.app_handle().get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .build(app)?;

    app.manage(TrayState(Mutex::new(Some(tray))));
    Ok(())
}

/// Rebuilds the tray menu from the current profile list. Call this after a
/// toggle so the active marker stays in sync.
pub fn refresh(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app)?;
    if let Some(state) = app.try_state::<TrayState>() {
        if let Some(tray) = state.0.lock().ok().and_then(|g| g.clone()) {
            tray.set_menu(Some(menu))?;
        }
    }
    Ok(())
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let state = app.state::<AppState>();
    let (profiles, active) = {
        let store = state.store.lock().map_err(to_tauri_err)?;
        let profiles = store.list().map_err(to_tauri_err)?;
        let active = store.detect_active().map_err(to_tauri_err)?;
        (profiles, active)
    };

    let mut builder = MenuBuilder::new(app);

    let origin_label = label_for("origin", &active);
    let origin_item = MenuItemBuilder::with_id(ORIGIN_ID, origin_label).build(app)?;
    builder = builder.item(&origin_item);

    builder = builder.separator();

    if profiles.is_empty() {
        let empty: MenuItem<Wry> = MenuItemBuilder::with_id("profile:none", "(no profiles)")
            .enabled(false)
            .build(app)?;
        builder = builder.item(&empty);
    } else {
        for prof in profiles {
            if prof.name == "origin" {
                // origin is already pinned at the top; don't repeat it.
                continue;
            }
            let id = format!("{}{}", PROFILE_PREFIX, prof.name);
            let label = label_for(&prof.name, &active);
            let item = MenuItemBuilder::with_id(id, label).build(app)?;
            builder = builder.item(&item);
        }
    }

    builder = builder.separator();

    let show_item = MenuItemBuilder::with_id(SHOW_ID, "Open Profile Editor").build(app)?;
    builder = builder.item(&show_item);

    let quit_item = PredefinedMenuItem::quit(app, Some("Quit"))?;
    builder = builder.item(&quit_item);

    builder.build()
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id().as_ref();
    match id {
        SHOW_ID => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }
        QUIT_ID => app.exit(0),
        other if other.starts_with(PROFILE_PREFIX) => {
            let name = &other[PROFILE_PREFIX.len()..];
            apply_and_refresh(app, name);
        }
        _ => {}
    }
}

fn apply_and_refresh(app: &AppHandle, name: &str) {
    let state = app.state::<AppState>();
    let result = state
        .engine
        .lock()
        .map_err(|e| e.to_string())
        .and_then(|eng| eng.apply_named(name).map_err(|e| e.to_string()));

    if let Err(e) = result {
        eprintln!("[tray] toggle '{}' failed: {}", name, e);
        return;
    }
    if let Err(e) = refresh(app) {
        eprintln!("[tray] menu refresh failed: {}", e);
    }
}

fn label_for(name: &str, active: &str) -> String {
    if name == active {
        format!("● {}", name)
    } else {
        format!("   {}", name)
    }
}

fn to_tauri_err<E: std::fmt::Display>(e: E) -> tauri::Error {
    tauri::Error::Io(std::io::Error::other(e.to_string()))
}
