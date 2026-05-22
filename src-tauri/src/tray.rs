//! Menu-bar popover tray. Left-click toggles a borderless window positioned
//! just under the tray icon; click-outside (focus lost) hides it again. The
//! tray itself carries only a minimal right-click context menu (Open / Quit)
//! since the entire interaction lives inside the popover.

use std::sync::Mutex;

use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WindowEvent};
use tauri_plugin_positioner::{Position, WindowExt};

const POPOVER_WINDOW: &str = "main";
const QUIT_ID: &str = "quit";
const OPEN_ID: &str = "open";

pub struct TrayState(pub Mutex<Option<TrayIcon>>);

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_context_menu(app)?;
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
        // Treat the icon as a macOS template image so it auto-inverts for
        // dark/light menu bars instead of rendering full-color.
        .icon_as_template(true)
        .menu(&menu)
        // Right-click opens the small context menu; left-click toggles the
        // popover. The plugin needs to see every event to keep the
        // "last tray rect" cache that powers Position::TrayCenter.
        .show_menu_on_left_click(false)
        .on_menu_event(handle_menu_event)
        .on_tray_icon_event(|tray, event| {
            tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_popover(tray.app_handle());
            }
        })
        .build(app)?;

    app.manage(TrayState(Mutex::new(Some(tray))));

    // Hide the popover whenever it loses focus so click-outside dismisses it,
    // matching the muxbar / NSStatusItem popover idiom.
    if let Some(window) = app.get_webview_window(POPOVER_WINDOW) {
        let app_handle = app.clone();
        window.on_window_event(move |event| {
            if let WindowEvent::Focused(false) = event {
                if let Some(w) = app_handle.get_webview_window(POPOVER_WINDOW) {
                    let _ = w.hide();
                }
            }
        });
    }

    Ok(())
}

fn build_context_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let open = MenuItemBuilder::with_id(OPEN_ID, "Open").build(app)?;
    let quit = PredefinedMenuItem::quit(app, Some("Quit"))?;
    MenuBuilder::new(app).items(&[&open, &quit]).build()
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        OPEN_ID => show_popover(app),
        QUIT_ID => app.exit(0),
        _ => {}
    }
}

fn toggle_popover(app: &AppHandle) {
    let Some(window) = app.get_webview_window(POPOVER_WINDOW) else {
        return;
    };
    let visible = window.is_visible().unwrap_or(false);
    if visible {
        let _ = window.hide();
    } else {
        show_popover(app);
    }
}

fn show_popover(app: &AppHandle) {
    let Some(window) = app.get_webview_window(POPOVER_WINDOW) else {
        return;
    };
    // Position::TrayCenter aligns the window's top-edge centered under the
    // tray icon on macOS / Windows / Linux. The positioner plugin needs to
    // have seen at least one tray event for the cache; we call this on every
    // show because the menubar arrangement can change between toggles.
    let _ = window.move_window(Position::TrayBottomCenter);
    let _ = window.show();
    let _ = window.set_focus();
}

/// Re-render hook kept for compatibility with the existing toggle command
/// path. Profile lists live entirely in the popover web view now, so the
/// only state the tray-side needs to refresh is the click cache — nothing
/// to do here.
pub fn refresh(_app: &AppHandle) -> tauri::Result<()> {
    Ok(())
}

/// Convenience used by callers that don't have a Manager import in scope.
#[allow(dead_code)]
pub fn show(app: &AppHandle) {
    show_popover(app);
}

// Kept private so external modules don't accidentally bind to a typed handle
// that would force them to import tauri_plugin_positioner's traits.
#[allow(dead_code)]
fn _hide(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(POPOVER_WINDOW) {
        let _ = window.hide();
    }
}

