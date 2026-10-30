//! Notification-area icon (`docs/04-ui-design.md` §4.4, `docs/07-shell-integration.md` §5). It
//! exists while the main window hides instead of closing (setting "when closing the main window:
//! hide in the notification area"), and only in the process that has the main window.
//!
//! A click shows the main window. The menu opens OmniZip, lists the mounts (open, unmount),
//! unmounts them all and quits; the tooltip shows the running jobs. A writable mount with unsaved
//! changes is unmounted through the main window, which asks whether to save them.

use std::sync::Mutex;

use omnizip_i18n::{Localizer, tr};
use tauri::{
    AppHandle, Emitter, Manager, Wry,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
};

use crate::{
    jobs::JobState,
    mounts::{self, MountInfo},
    state::AppState,
    windows,
};

const ID: &str = "omnizip";

/// The tooltip last set, so a progress tick that changes nothing visible costs nothing.
static TOOLTIP: Mutex<String> = Mutex::new(String::new());

fn localizer(app: &AppHandle) -> Localizer {
    match app.state::<AppState>().settings().language.ui.as_str() {
        "" | "auto" => Localizer::detect(None),
        l => Localizer::new(l),
    }
}

fn wanted(app: &AppHandle) -> bool {
    app.get_webview_window("main").is_some()
        && app.state::<AppState>().settings().general.close_behavior == "tray"
}

/// Whether the icon is there (the main window then hides instead of closing).
pub fn shown(app: &AppHandle) -> bool {
    app.tray_by_id(ID).is_some()
}

/// Adds or removes the icon as the settings say, with a fresh menu (after the language or the
/// close setting changed, or the main window was created).
pub fn sync(app: &AppHandle) {
    if !wanted(app) {
        if app.remove_tray_by_id(ID).is_some() {
            *TOOLTIP.lock().unwrap_or_else(|e| e.into_inner()) = String::new();
        }
        return;
    }
    if let Some(tray) = app.tray_by_id(ID) {
        set_menu(app, &tray);
        jobs_changed(app);
        return;
    }
    let menu = match menu(app) {
        Ok(m) => m,
        Err(e) => return tracing::error!("tray menu: {e}"),
    };
    let mut b = TrayIconBuilder::with_id(ID)
        .tooltip("OmniZip")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle().clone();
                std::thread::spawn(move || windows::focus_main(&app));
            }
        });
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    match b.build(app) {
        Ok(_) => {
            *TOOLTIP.lock().unwrap_or_else(|e| e.into_inner()) = "OmniZip".into();
            jobs_changed(app);
        }
        Err(e) => tracing::error!("tray icon: {e}"),
    }
}

/// The mounts changed: the menu lists them.
pub fn mounts_changed(app: &AppHandle) {
    if let Some(tray) = app.tray_by_id(ID) {
        set_menu(app, &tray);
    }
}

/// A job started, progressed or ended: the tooltip shows how many run and how far they are.
pub fn jobs_changed(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(ID) else {
        return;
    };
    let running: Vec<_> = app
        .state::<AppState>()
        .jobs
        .list()
        .into_iter()
        .filter(|j| matches!(j.state, JobState::Running | JobState::Paused))
        .collect();
    let text = if running.is_empty() {
        "OmniZip".to_owned()
    } else {
        let percent = running.iter().map(|j| j.progress.fraction()).sum::<f64>() * 100.0
            / running.len() as f64;
        let l = localizer(app);
        tr!(
            l,
            "tray-tooltip-jobs",
            count = running.len(),
            percent = percent.floor() as u32
        )
    };
    let mut last = TOOLTIP.lock().unwrap_or_else(|e| e.into_inner());
    if *last != text {
        let _ = tray.set_tooltip(Some(&text));
        *last = text;
    }
}

fn set_menu(app: &AppHandle, tray: &TrayIcon) {
    match menu(app) {
        Ok(m) => {
            let _ = tray.set_menu(Some(m));
        }
        Err(e) => tracing::error!("tray menu: {e}"),
    }
}

/// Text for a menu item: on Windows `&` marks the access key, so a literal one is doubled.
fn text(s: &str) -> String {
    if cfg!(windows) {
        s.replace('&', "&&")
    } else {
        s.to_owned()
    }
}

fn menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let l = localizer(app);
    let mounts = app.state::<AppState>().mounts.list();
    let item = |id: String, label: String, enabled: bool| {
        MenuItem::with_id(app, id, text(&label), enabled, None::<&str>)
    };
    let menu = Menu::new(app)?;
    menu.append(&item("open".into(), tr!(l, "tray-open"), true)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    if mounts.is_empty() {
        menu.append(&item("none".into(), tr!(l, "tray-no-mounts"), false)?)?;
    }
    for m in &mounts {
        let open = item(format!("reveal:{}", m.id), tr!(l, "tray-mount-open"), true)?;
        let eject = item(format!("eject:{}", m.id), tr!(l, "tray-mount-eject"), true)?;
        let label = text(&format!("{}  {}", m.mount_point, m.name));
        menu.append(&Submenu::with_items(app, label, true, &[&open, &eject])?)?;
    }
    if !mounts.is_empty() {
        menu.append(&item("eject-all".into(), tr!(l, "tray-eject-all"), true)?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&item("quit".into(), tr!(l, "tray-quit"), true)?)?;
    Ok(menu)
}

fn on_menu(app: &AppHandle, event: MenuEvent) {
    let id = event.id().as_ref().to_owned();
    if id == "quit" {
        return app.exit(0);
    }
    // Showing the window, revealing a folder and unmounting can block (a window being built, a
    // file system call in progress): not on the thread that runs the event loop.
    let app = app.clone();
    std::thread::spawn(move || match id.split_once(':') {
        None if id == "open" => windows::focus_main(&app),
        None if id == "eject-all" => {
            let all = app.state::<AppState>().mounts.list();
            unmount(&app, all);
        }
        Some(("reveal", n)) => {
            if let Some(m) = find(&app, n) {
                let _ = tauri_plugin_opener::open_path(mounts::root(&m), None::<&str>);
            }
        }
        Some(("eject", n)) => {
            if let Some(m) = find(&app, n) {
                unmount(&app, vec![m]);
            }
        }
        _ => {}
    });
}

fn find(app: &AppHandle, id: &str) -> Option<MountInfo> {
    let id: u64 = id.parse().ok()?;
    app.state::<AppState>()
        .mounts
        .list()
        .into_iter()
        .find(|m| m.id == id)
}

/// Unmounts right away what has nothing to save; mounts with unsaved changes go to the main
/// window, which asks whether to save them into the archive.
fn unmount(app: &AppHandle, list: Vec<MountInfo>) {
    let state = app.state::<AppState>();
    let (ask, now): (Vec<_>, Vec<_>) = list.into_iter().partition(|m| m.dirty);
    for m in now {
        if let Err(e) = state.mounts.unmount(app, m.id, false) {
            tracing::warn!(
                code = e.code().id(),
                "unmounting {} from the tray",
                m.mount_point
            );
        }
    }
    if !ask.is_empty() {
        windows::focus_main(app);
        for m in ask {
            let _ = app.emit_to("main", "tray://unmount", m.id);
        }
    }
}
