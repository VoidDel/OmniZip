//! Window creation (`docs/04-ui-design.md` §3, §4.4, §9).

use tauri::{
    AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    window::{Color, Effect, EffectsBuilder},
};

use crate::{state::AppState, system};

fn canvas_color(app: &AppHandle) -> Color {
    // First paint uses the canvas color of the theme in use, so the window never flashes white
    // in dark mode (`docs/05-theming.md` §8).
    let state = app.state::<AppState>();
    let s = state.settings();
    let dark = match s.appearance.mode.as_str() {
        "dark" => true,
        "light" => false,
        _ => app
            .get_webview_window("main")
            .and_then(|w| w.theme().ok())
            .map(|t| t == tauri::Theme::Dark)
            .unwrap_or(false),
    };
    if dark {
        Color(0x0F, 0x11, 0x15, 0xFF)
    } else {
        Color(0xF5, 0xF7, 0xFB, 0xFF)
    }
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
    let mut b = WebviewWindowBuilder::new(app, &label, url)
        .title("OmniZip")
        .inner_size(440.0, 196.0)
        .resizable(false)
        .maximizable(false)
        .minimizable(true)
        .center()
        .visible(false)
        .always_on_top(true)
        .background_color(canvas_color(app));
    #[cfg(target_os = "macos")]
    {
        b = b
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true);
    }
    #[cfg(windows)]
    {
        b = b.decorations(false);
    }
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
