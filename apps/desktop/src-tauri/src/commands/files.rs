//! Files and system: paths, recent archives, windows, shell integration, default apps.

use super::*;

#[derive(Serialize)]
pub struct Classified {
    archives: Vec<String>,
    others: Vec<String>,
}

/// Splits dropped paths into archives and other files (`docs/04-ui-design.md` §5.2).
#[tauri::command]
pub async fn paths_classify(paths: Vec<String>) -> CmdResult<Classified> {
    off_main(move || {
        let (mut archives, mut others) = (Vec::new(), Vec::new());
        for p in paths {
            let path = Path::new(&p);
            if path.is_file()
                && shared()
                    .detect(path)
                    .is_ok_and(|d| shared().engine(d.format).is_some())
            {
                archives.push(p);
            } else {
                others.push(p);
            }
        }
        Classified { archives, others }
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathStats {
    bytes: u64,
    files: u64,
    common_parent: Option<String>,
    suggested_name: String,
}

/// Size of files about to be compressed and a suggested archive name.
#[tauri::command]
pub async fn paths_stats(paths: Vec<String>) -> CmdResult<PathStats> {
    off_main(move || stats(paths)).await
}

fn stats(paths: Vec<String>) -> PathStats {
    let ctx = omnizip_core::JobCtx::default();
    let pb: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    let sources = omnizip_core::source::collect(&pb, &Default::default(), &ctx).unwrap_or_default();
    let parent = pb.first().and_then(|p| p.parent()).map(Path::to_path_buf);
    let suggested = match pb.as_slice() {
        [one] => {
            let n = ops::file_name(one);
            if one.is_dir() {
                n
            } else {
                n.rsplit_once('.').map_or(n.clone(), |(s, _)| s.to_owned())
            }
        }
        _ => parent
            .as_deref()
            .map(ops::file_name)
            .unwrap_or_else(|| "archive".to_owned()),
    };
    PathStats {
        bytes: omnizip_core::source::total_size(&sources),
        files: omnizip_core::source::file_count(&sources),
        common_parent: parent.map(|p| p.to_string_lossy().into_owned()),
        suggested_name: suggested,
    }
}

#[tauri::command]
pub async fn path_reveal(path: String) -> CmdResult<()> {
    off_main(move || {
        let p = PathBuf::from(&path);
        let r = if p.is_dir() {
            tauri_plugin_opener::open_path(&p, None::<&str>)
        } else {
            tauri_plugin_opener::reveal_item_in_dir(&p)
        };
        r.map_err(|e| err(Error::internal(e)))
    })
    .await?
}

#[tauri::command]
pub async fn path_exists(path: String) -> CmdResult<bool> {
    off_main(move || Path::new(&path).exists()).await
}

/// Recent archives that still exist. Looking can take long (a network share that does not
/// answer), so it happens without holding the list.
#[tauri::command]
pub async fn recent_list(app: AppHandle) -> CmdResult<Vec<RecentItem>> {
    with_state(app, |_, state| {
        let items = state
            .recent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let gone: Vec<String> = items
            .into_iter()
            .filter(|i| !Path::new(&i.path).exists())
            .map(|i| i.path)
            .collect();
        let mut r = state.recent.lock().unwrap_or_else(|e| e.into_inner());
        r.retain(|i| !gone.contains(&i.path));
        Ok(r.clone())
    })
    .await
}

#[tauri::command]
pub fn recent_remove(state: State<'_, AppState>, path: String) {
    let mut r = state.recent.lock().unwrap_or_else(|e| e.into_inner());
    r.retain(|i| i.path != path);
    state.save_recent(&r);
}

#[tauri::command]
pub fn recent_clear(state: State<'_, AppState>) {
    let mut r = state.recent.lock().unwrap_or_else(|e| e.into_inner());
    r.clear();
    state.save_recent(&r);
}

/// Shows a window once the UI has painted (prevents a white flash).
#[tauri::command]
pub fn window_ready(window: WebviewWindow) {
    let _ = window.show();
    window_focus(window);
}

/// Brings the calling window to the front with keyboard focus inside its page, so a field the
/// page focused can be typed into right away. Transparent (material) windows on Windows do
/// not hand focus to their WebView by themselves.
#[tauri::command]
pub fn window_focus(window: WebviewWindow) {
    let _ = window.set_focus();
    let _ = AsRef::<tauri::Webview>::as_ref(&window).set_focus();
}

/// The request a standalone dialog window was opened for (`None` for other windows).
#[tauri::command]
pub fn shell_request(window: WebviewWindow, state: State<'_, AppState>) -> Option<LaunchRequest> {
    state
        .shell_requests
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(window.label())
        .cloned()
}

/// Context-menu registration state (docs/07-shell-integration.md §7).
#[tauri::command]
pub async fn shell_status() -> CmdResult<ShellStatus> {
    tauri::async_runtime::spawn_blocking(integration::status)
        .await
        .map_err(|e| err(Error::internal(e)))
}

/// Registers (or re-registers with the current menu settings) the context menu for this user,
/// with the Windows 11 top-level menu where the app ships its package (a few seconds).
#[tauri::command]
pub async fn shell_register() -> CmdResult<ShellStatus> {
    tauri::async_runtime::spawn_blocking(|| integration::register().map(|()| integration::status()))
        .await
        .map_err(|e| err(Error::internal(e)))?
        .map_err(err)
}

#[tauri::command]
pub async fn shell_unregister() -> CmdResult<ShellStatus> {
    tauri::async_runtime::spawn_blocking(|| {
        integration::unregister().map(|()| integration::status())
    })
    .await
    .map_err(|e| err(Error::internal(e)))?
    .map_err(err)
}

/// Opens the project's page in the browser (settings, About). The page cannot open addresses
/// itself (docs/09-security.md §7), so this one is fixed here.
#[tauri::command]
pub fn open_homepage() -> CmdResult<()> {
    tauri_plugin_opener::open_url(env!("CARGO_PKG_REPOSITORY"), None::<&str>)
        .map_err(|e| err(Error::internal(e)))
}

/// Opens the download page of the mount driver (WinFsp), for the settings page.
#[tauri::command]
pub fn open_mount_driver_page() -> CmdResult<()> {
    tauri_plugin_opener::open_url("https://winfsp.dev/rel/", None::<&str>)
        .map_err(|e| err(Error::internal(e)))
}

/// Opens the system "default apps" settings (`docs/07-shell-integration.md` §3.4).
#[tauri::command]
pub fn open_default_apps() -> CmdResult<()> {
    let url = if cfg!(windows) {
        "ms-settings:defaultapps"
    } else if cfg!(target_os = "macos") {
        "x-apple.systempreferences:com.apple.preference.general"
    } else {
        return Ok(());
    };
    tauri_plugin_opener::open_url(url, None::<&str>).map_err(|e| err(Error::internal(e)))
}

/// Which formats OmniZip opens by default, where it sets that itself (Linux,
/// `docs/07-shell-integration.md` §5.1): by format key, as in the settings. `None` where the
/// system decides (Windows, macOS); the switches are then the settings.
#[tauri::command]
pub async fn format_defaults() -> Option<BTreeMap<String, bool>> {
    #[cfg(target_os = "linux")]
    {
        tauri::async_runtime::spawn_blocking(crate::defaults::status)
            .await
            .ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Makes OmniZip open the formats `keys`, or gives them back to the programs that opened them
/// before (Linux); the formats' state afterwards.
#[tauri::command]
pub async fn format_set_default(keys: Vec<String>, on: bool) -> CmdResult<BTreeMap<String, bool>> {
    #[cfg(target_os = "linux")]
    {
        tauri::async_runtime::spawn_blocking(move || {
            crate::defaults::set(&keys, on)?;
            Ok(crate::defaults::status())
        })
        .await
        .map_err(|e| err(Error::internal(e)))?
        .map_err(err)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (keys, on);
        Err(err(
            Error::new(ErrorCode::UnsupportedOperation).with("operation", "default-app")
        ))
    }
}
