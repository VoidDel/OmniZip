//! Window creation (`docs/04-ui-design.md` §3, §4.4, §9).

use tauri::{
    AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    window::{Color, Effect, EffectsBuilder},
};

use crate::{state::AppState, system, theme};

fn canvas_color(app: &AppHandle) -> Color {
    // First paint uses the canvas color of the theme in use, so a window never flashes a
    // color that is not the theme's (`docs/05-theming.md` §8). Windows opened from the file
    // manager have no main window to ask, so "system" mode reads the OS setting directly.
    let state = app.state::<AppState>();
    let s = state.settings();
    let dark = match s.appearance.mode.as_str() {
        "dark" => true,
        "light" => false,
        _ => app
            .get_webview_window("main")
            .and_then(|w| w.theme().ok())
            .map(|t| t == tauri::Theme::Dark)
            .or_else(system::prefers_dark)
            .unwrap_or(false),
    };
    let id = if dark {
        &s.appearance.theme_dark
    } else {
        &s.appearance.theme_light
    };
    let themes = state.themes.lock().unwrap_or_else(|e| e.into_inner());
    let canvas = themes.iter().find(|(t, _)| &t.id == id).map(|(t, _)| {
        let options = theme::ResolveOptions {
            accent: "theme".into(),
            system_accent: None,
            material: false,
        };
        theme::resolve(t, &options).tokens["bg-canvas"].clone()
    });
    match canvas.as_deref().and_then(parse_hex) {
        Some(c) => c,
        None if dark => Color(0x0F, 0x11, 0x15, 0xFF),
        None => Color(0xF5, 0xF7, 0xFB, 0xFF),
    }
}

/// `#RRGGBB` → opaque color.
fn parse_hex(s: &str) -> Option<Color> {
    let h = s.strip_prefix('#').filter(|h| h.len() == 6)?;
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some(Color(byte(0)?, byte(2)?, byte(4)?, 0xFF))
}

/// The window material for the current settings (`None` when off or unsupported).
///
/// Windows 11 offers three (`appearance.material_kind`): Mica (desktop wallpaper, heavily
/// blurred), Mica Alt (the same with a stronger wallpaper tint) and Acrylic (a live blur of
/// whatever is behind the window). macOS always uses vibrancy.
fn current_effects(app: &AppHandle) -> Option<tauri::utils::config::WindowEffectsConfig> {
    let s = app.state::<AppState>().settings();
    if !s.appearance.material || !system::material_supported() {
        return None;
    }
    let effect = if cfg!(target_os = "macos") {
        Effect::Sidebar
    } else {
        match s.appearance.material_kind.as_str() {
            "mica-alt" => Effect::Tabbed,
            "acrylic" => Effect::Acrylic,
            _ => Effect::Mica,
        }
    };
    Some(EffectsBuilder::new().effect(effect).build())
}

/// Background of every window. Where a material (Mica, vibrancy) is available it is fully
/// transparent, whether or not the material is switched on: an opaque color here would be
/// painted by the WebView underneath the page and cover the material, and it cannot be
/// cleared reliably at run time. The page paints the theme canvas itself whenever the
/// material is off (`body` is only transparent under `data-material="true"`), and the window
/// stays hidden until that first paint, so nothing flashes.
fn window_background(app: &AppHandle) -> Color {
    if system::material_supported() {
        Color(0, 0, 0, 0)
    } else {
        canvas_color(app)
    }
}

/// Applies the material settings to every open window (main, progress and dialog windows).
pub fn apply_material(app: &AppHandle) {
    for w in app.webview_windows().values() {
        let _ = w.set_effects(current_effects(app));
    }
}

/// The main window, created hidden; the UI shows it after its first render.
pub fn main_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    if let Some(w) = app.get_webview_window("main") {
        return Ok(w);
    }
    let mut b = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("OmniZip")
        .inner_size(1180.0, 760.0)
        .min_inner_size(720.0, 480.0)
        .center()
        .visible(false)
        .background_color(window_background(app));
    #[cfg(target_os = "macos")]
    {
        // The traffic lights sit in the middle of the sidebar's top row (`--titlebar-h`). The y
        // here is not the buttons' top: it sets the title bar's height to the button height plus
        // y, and the buttons keep their distance from its bottom edge.
        b = b
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true)
            .traffic_light_position(tauri::LogicalPosition::new(13.0, 22.5));
    }
    #[cfg(windows)]
    {
        b = b
            .decorations(false)
            .transparent(system::material_supported());
    }
    // Linux draws its own title bar and window buttons too (the window manager still resizes it
    // from its edges).
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        b = b.decorations(false);
    }
    if let Some(fx) = current_effects(app) {
        b = b.effects(fx);
    }
    b.build()
}

/// A small progress window for jobs started from the file manager (`docs/04-ui-design.md` §4.4).
pub fn mini_window(app: &AppHandle, job_id: u32) -> tauri::Result<WebviewWindow> {
    let label = format!("mini-{job_id}");
    let url = WebviewUrl::App(format!("index.html?mini={job_id}").into());
    let b = WebviewWindowBuilder::new(app, &label, url)
        .title("OmniZip")
        .inner_size(440.0, 200.0)
        .resizable(false)
        .maximizable(false)
        .minimizable(true)
        .center()
        .visible(false)
        .always_on_top(true)
        .background_color(window_background(app));
    // Rebinding per platform keeps every target free of an unused mut.
    #[cfg(target_os = "macos")]
    let b = b
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    #[cfg(windows)]
    let b = b
        .decorations(false)
        .transparent(system::material_supported());
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let b = b.decorations(false);
    with_material(app, b).build()
}

/// A standalone dialog window for "Extract to…" / "Add to archive…" started from the file
/// manager. It shows only the dialog (the UI sizes it to fit) and then the job's progress.
pub fn shell_window(app: &AppHandle, label: &str) -> tauri::Result<WebviewWindow> {
    let url = WebviewUrl::App(format!("index.html?shell={label}").into());
    let b = WebviewWindowBuilder::new(app, label, url)
        .title("OmniZip")
        .inner_size(560.0, 420.0)
        .resizable(false)
        .maximizable(false)
        .minimizable(true)
        .center()
        .visible(false)
        .focused(true)
        .background_color(window_background(app));
    #[cfg(target_os = "macos")]
    let b = b
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    #[cfg(windows)]
    let b = b
        .decorations(false)
        .transparent(system::material_supported());
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let b = b.decorations(false);
    with_material(app, b).build()
}

/// Adds the current material to a window that is being built.
fn with_material<'a>(
    app: &AppHandle,
    b: WebviewWindowBuilder<'a, tauri::Wry, AppHandle>,
) -> WebviewWindowBuilder<'a, tauri::Wry, AppHandle> {
    match current_effects(app) {
        Some(fx) => b.effects(fx),
        None => b,
    }
}

/// Brings the main window to the front.
pub fn focus_main(app: &AppHandle) {
    let created = app.get_webview_window("main").is_none();
    if let Ok(w) = main_window(app) {
        // A process started for a file-manager verb has just got its main window: the
        // notification-area icon belongs to it now.
        if created {
            crate::tray::sync(app);
        }
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}
