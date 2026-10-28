//! OmniZip desktop application (Tauri 2).

pub mod commands;
pub mod draft;
pub mod jobs;
pub mod launch;
pub mod ops;
pub mod preview;
pub mod settings;
pub mod state;
pub mod system;
pub mod theme;
pub mod windows;

use std::path::{Path, PathBuf};

use omnizip_core::{FormatId, Level, OverwriteMode, format::archive_stem};
use tauri::{AppHandle, Emitter, Manager};

use crate::{
    launch::{Invocation, Verb},
    ops::{CompressRequest, ExtractRequest},
    state::AppState,
};

/// Runs a file-manager invocation: background verbs get a mini progress window, dialog verbs a
/// standalone dialog window, the others are handed to the main window.
fn handle_invocation(app: &AppHandle, inv: Invocation, startup: bool) {
    if inv.verb.is_dialog() {
        if let Some(req) = launch::to_request(&inv) {
            let state = app.state::<AppState>();
            let label = state.next_shell_label();
            state
                .shell_requests
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(label.clone(), req);
            if let Err(e) = windows::shell_window(app, &label) {
                tracing::error!("dialog window: {e}");
            }
        }
        return;
    }
    if !inv.verb.is_background() {
        let req = launch::to_request(&inv);
        let state = app.state::<AppState>();
        if startup {
            *state.launch.lock().unwrap_or_else(|e| e.into_inner()) = req;
        } else if let Some(r) = req {
            let _ = app.emit("app://launch", r);
        }
        windows::focus_main(app);
        return;
    }
    let state = app.state::<AppState>();
    let settings = state.settings();
    let paths: Vec<String> = inv
        .paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let parent = inv
        .paths
        .first()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let policy = omnizip_core::SecurityPolicy {
        max_ratio: settings.security.max_ratio.max(10),
        ..Default::default()
    };
    let (kind, title, work) = match inv.verb {
        Verb::ExtractHere | Verb::ExtractSmart | Verb::ExtractEach => {
            let req = ExtractRequest {
                archive_id: None,
                paths,
                nodes: Vec::new(),
                relative_to: None,
                destination: parent.to_string_lossy().into_owned(),
                path_mode: if inv.verb == Verb::ExtractHere {
                    "full".into()
                } else {
                    "smart".into()
                },
                overwrite: OverwriteMode::Ask,
                password: None,
                encoding: None,
                open_after: false,
                delete_after: false,
                keep_broken: false,
                each_to_folder: inv.verb == Verb::ExtractEach,
            };
            match ops::extract_work(app, req) {
                Ok((t, w)) => ("extract", t, w),
                Err(e) => {
                    tracing::error!("{e}");
                    return;
                }
            }
        }
        Verb::Test => {
            let (t, w) = ops::test_work(None, paths, None);
            ("test", t, w)
        }
        Verb::AddZip | Verb::Add7z => {
            let format = if inv.verb == Verb::AddZip {
                FormatId::Zip
            } else {
                FormatId::SevenZ
            };
            let stem = match inv.paths.as_slice() {
                [one] if one.is_dir() => ops::file_name(one),
                [one] => archive_stem(&ops::file_name(one)),
                _ => ops::file_name(&parent),
            };
            let stem = if stem.is_empty() {
                "archive".to_owned()
            } else {
                stem
            };
            let dest = unique(&parent.join(format!("{stem}{}", format.primary_extension())));
            let req = CompressRequest {
                paths,
                destination: dest.to_string_lossy().into_owned(),
                format,
                level: Level::Normal,
                method: None,
                password: None,
                encrypt_names: false,
                solid: true,
                volume_size: None,
                comment: None,
                exclude: Vec::new(),
                delete_source: false,
                verify: false,
                open_after: false,
                draft: None,
            };
            let (t, w) = ops::compress_work(req, None, settings.advanced.threads);
            ("compress", t, w)
        }
        _ => unreachable!("foreground verbs return above"),
    };
    let spec = jobs::JobSpec {
        window: Some("mini".into()),
        ..jobs::JobSpec::new(kind, title, policy)
    };
    let id = state.jobs.spawn(app, spec, work, None);
    if let Err(e) = windows::mini_window(app, id) {
        tracing::error!("mini window: {e}");
    }
}

/// `a.zip` → `a (2).zip` when the name is taken.
fn unique(p: &Path) -> PathBuf {
    if !p.exists() {
        return p.to_owned();
    }
    let name = ops::file_name(p);
    let dir = p.parent().unwrap_or(Path::new("."));
    let (stem, ext) = match name.find('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    (2..1000)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|c| !c.exists())
        .unwrap_or(p.to_owned())
}

/// Entry point.
pub fn run() {
    let cwd = std::env::current_dir().unwrap_or_default();
    let args: Vec<String> = std::env::args().collect();
    let invocation = launch::parse(&args, &cwd);

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(
            |app, argv, cwd| match launch::parse(&argv, Path::new(&cwd)) {
                Some(inv) => handle_invocation(app, inv, false),
                None => windows::focus_main(app),
            },
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_denylist(&["mini-*", "shell-*"])
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::SIZE
                        | tauri_plugin_window_state::StateFlags::POSITION
                        | tauri_plugin_window_state::StateFlags::MAXIMIZED,
                )
                .build(),
        )
        .manage(AppState::new())
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event
                && let Some(state) = window.try_state::<AppState>()
            {
                state
                    .shell_requests
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(window.label());
            }
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            match invocation {
                Some(inv) if inv.verb.is_background() || inv.verb.is_dialog() => {
                    handle_invocation(&handle, inv, true)
                }
                Some(inv) => {
                    handle_invocation(&handle, inv, true);
                    windows::main_window(&handle)?;
                }
                None => {
                    windows::main_window(&handle)?;
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_bootstrap,
            commands::app_take_launch,
            commands::settings_set,
            commands::theme_resolve,
            commands::themes_reload,
            commands::theme_import,
            commands::themes_open_folder,
            commands::archive_open,
            commands::archive_reencode,
            commands::archive_summary,
            commands::archive_close,
            commands::archive_list,
            commands::archive_dirs,
            commands::archive_ancestors,
            commands::archive_node,
            commands::archive_selection_size,
            commands::archive_preview,
            commands::archive_set_password,
            commands::archive_open_external,
            commands::draft_new,
            commands::draft_mkdir,
            commands::job_extract,
            commands::job_compress,
            commands::job_test,
            commands::job_add,
            commands::job_delete,
            commands::job_rename,
            commands::job_cancel,
            commands::job_pause,
            commands::job_resume,
            commands::job_list,
            commands::job_prompts,
            commands::job_clear,
            commands::job_answer,
            commands::paths_classify,
            commands::paths_stats,
            commands::path_reveal,
            commands::path_exists,
            commands::recent_list,
            commands::recent_remove,
            commands::recent_clear,
            commands::window_ready,
            commands::window_focus,
            commands::open_default_apps,
            commands::shell_status,
            commands::shell_request,
            commands::mount_probe,
            commands::shell_register,
            commands::shell_unregister,
        ])
        .run(tauri::generate_context!())
        .expect("error while running OmniZip");
}
