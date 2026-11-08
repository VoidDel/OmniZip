//! Bootstrap, settings, themes.

use super::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    settings: Settings,
    themes: Vec<ThemeSummary>,
    system: SystemInfo,
    launch: Option<LaunchRequest>,
    formats: Vec<FormatSupport>,
    write_methods: BTreeMap<String, Vec<&'static str>>,
    encodings: Vec<&'static str>,
    languages: Vec<(&'static str, &'static str)>,
    config_path: Option<String>,
}

#[tauri::command]
pub fn app_bootstrap(state: State<'_, AppState>) -> Bootstrap {
    let themes = theme::summaries(&state.themes.lock().unwrap_or_else(|e| e.into_inner()));
    let write_methods = [FormatId::Zip, FormatId::SevenZ]
        .iter()
        .map(|f| {
            (
                f.name().to_owned(),
                omnizip_formats::write_methods(*f).to_vec(),
            )
        })
        .collect();
    Bootstrap {
        settings: state.settings(),
        themes,
        system: system::info(),
        launch: state
            .launch
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take(),
        formats: shared().support(),
        write_methods,
        encodings: encoding::all_labels(),
        languages: omnizip_i18n::LANGUAGES.to_vec(),
        config_path: state
            .config_file
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
    }
}

/// Pending launch request (a file opened while the window was starting). The main window asks
/// once it listens for `app://launch`; later requests go there.
#[tauri::command]
pub fn app_take_launch(state: State<'_, AppState>) -> Option<LaunchRequest> {
    let mut pending = state.launch.lock().unwrap_or_else(|e| e.into_inner());
    state.launch_ready.store(true, Ordering::SeqCst);
    pending.take()
}

#[tauri::command]
pub fn settings_set(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: Settings,
) -> CmdResult<Settings> {
    let previous = state.settings();
    let material_changed = previous.appearance.material != settings.appearance.material
        || previous.appearance.material_kind != settings.appearance.material_kind;
    let menu_changed = previous.shell != settings.shell;
    let tray_changed = previous.general.close_behavior != settings.general.close_behavior
        || previous.language.ui != settings.language.ui;
    if let Some(p) = &state.config_file {
        settings::save(p, &settings).map_err(|e| err(Error::io(e, p)))?;
    }
    *state.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings.clone();
    if material_changed {
        windows::apply_material(&app);
    }
    // Flat versus submenu layout lives in the registry: apply it right away.
    if menu_changed {
        integration::refresh_classic();
    }
    if tray_changed {
        crate::tray::sync(&app);
    }
    Ok(settings)
}

#[tauri::command]
pub fn theme_resolve(
    state: State<'_, AppState>,
    id: String,
    options: ResolveOptions,
) -> CmdResult<ResolvedTheme> {
    let themes = state.themes.lock().unwrap_or_else(|e| e.into_inner());
    let t = themes
        .iter()
        .find(|(t, _)| t.id == id)
        .or_else(|| themes.first())
        .ok_or_else(|| err(Error::internal("no themes")))?;
    let options = ResolveOptions {
        material: options.material && system::material_supported(),
        ..options
    };
    Ok(theme::resolve(&t.0, &options))
}

#[tauri::command]
pub fn themes_reload(state: State<'_, AppState>) -> Vec<ThemeSummary> {
    let all = theme::load_all(state.themes_dir.as_deref());
    let out = theme::summaries(&all);
    *state.themes.lock().unwrap_or_else(|e| e.into_inner()) = all;
    out
}

/// Validates and copies a theme file into the user theme folder.
#[tauri::command]
pub fn theme_import(state: State<'_, AppState>, path: String) -> CmdResult<Vec<ThemeSummary>> {
    let text = std::fs::read_to_string(&path).map_err(|e| err(Error::io(e, &path)))?;
    let t = theme::parse(&text).map_err(|m| err(Error::invalid_argument(m)))?;
    let dir = state
        .themes_dir
        .clone()
        .ok_or_else(|| err(Error::internal("no config dir")))?;
    std::fs::create_dir_all(&dir).map_err(|e| err(Error::io(e, &dir)))?;
    let dest = dir.join(format!("{}.json", t.id));
    std::fs::write(&dest, text).map_err(|e| err(Error::io(e, &dest)))?;
    Ok(themes_reload(state))
}

#[tauri::command]
pub fn themes_open_folder(state: State<'_, AppState>) -> CmdResult<()> {
    let dir = state
        .themes_dir
        .clone()
        .ok_or_else(|| err(Error::internal("no config dir")))?;
    std::fs::create_dir_all(&dir).map_err(|e| err(Error::io(e, &dir)))?;
    tauri_plugin_opener::open_path(&dir, None::<&str>).map_err(|e| err(Error::internal(e)))
}
