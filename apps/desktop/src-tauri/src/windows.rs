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

fn effects_for(material: bool) -> Option<tauri::utils::config::WindowEffectsConfig> {
    if !material || !system::material_supported() {
        return None;
    }
    let effect = if cfg!(target_os = "macos") {
        Effect::Sidebar
    } else {
        Effect::Mica
    };
    Some(EffectsBuilder::new().effect(effect).build())
}

/// Applies or removes the window material at run time.
pub fn set_material(w: &WebviewWindow, material: bool) {
    let _ = w.set_effects(effects_for(material));
}

/// The main window, created hidden; the UI shows it after its first render.
pub fn main_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    if let Some(w) = app.get_webview_window("main") {
        return Ok(w);
    }
    let material = app.state::<AppState>().settings().appearance.material;
    let transparent = system::material_supported();
    let mut b = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("OmniZip")
        .inner_size(1180.0, 760.0)
        .min_inner_size(720.0, 480.0)
        .center()
        .visible(false)
        .background_color(canvas_color(app));
    #[cfg(target_os = "macos")]
    {
        b = b
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true);
    }
    #[cfg(windows)]
    {
        b = b.decorations(false).transparent(transparent);
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let _ = transparent;
    }
    if let Some(fx) = effects_for(material) {
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
        .inner_size(440.0, 196.0)
        .resizable(false)
        .maximizable(false)
        .minimizable(true)
        .center()
        .visible(false)
        .always_on_top(true)
        .background_color(canvas_color(app));
    // Rebinding per platform keeps every target free of an unused mut (Linux has no tweak).
    #[cfg(target_os = "macos")]
    let b = b
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    #[cfg(windows)]
    let b = b.decorations(false);
    b.build()
}

/// Brings the main window to the front.
pub fn focus_main(app: &AppHandle) {
    if let Ok(w) = main_window(app) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}
