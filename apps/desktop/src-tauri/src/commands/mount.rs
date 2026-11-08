//! Mounts (docs/12-mount.md §9).

use super::*;

/// Mount backend state (docs/12-mount.md §9): whether the driver is installed and whether
/// this build can mount at all.
#[tauri::command]
pub fn mount_probe() -> omnizip_vfs::Probe {
    mounts::probe()
}

#[tauri::command]
pub fn mount_list(state: State<'_, AppState>) -> Vec<MountInfo> {
    state.mounts.list()
}

/// Drive letters free for a mount, from Z: downwards.
#[tauri::command]
pub fn mount_free_letters() -> Vec<String> {
    mounts::free_letters()
}

/// What the mount dialog needs of an archive that is not open in the browser (the dialog window
/// of the file manager's "Mount").
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MountTarget {
    path: String,
    name: String,
    format: FormatId,
    mount_class: omnizip_core::MountClass,
    /// A password is needed to mount it.
    encrypted: bool,
}

/// Detects an archive's format and mount class without listing it, so the mount dialog opens
/// at once. Single compressed streams (tar.gz and the like), which could only be listed by
/// decoding them whole, are not opened: they are neither solid nor encrypted. Archives with
/// encrypted headers count as not solid.
#[tauri::command]
pub async fn mount_target(path: String) -> CmdResult<MountTarget> {
    tauri::async_runtime::spawn_blocking(move || {
        let p = PathBuf::from(&path);
        let format = shared().detect(&p)?.format;
        let (solid, encrypted) = if format.mount_class(false) == omnizip_core::MountClass::D {
            (false, false)
        } else {
            match shared().open_as(&p, format, &Default::default(), &Default::default()) {
                Ok(r) => (r.info().solid, r.info().encrypted),
                Err(e) if e.code() == ErrorCode::PasswordRequired => (false, true),
                Err(e) => return Err(e),
            }
        };
        Ok(MountTarget {
            name: ops::file_name(&p),
            path,
            format,
            mount_class: format.mount_class(solid),
            encrypted,
        })
    })
    .await
    .map_err(|e| err(Error::internal(e)))?
    .map_err(err)
}

/// Mounts an archive (docs/12-mount.md §2). `PasswordRequired` lets the UI ask and retry.
#[tauri::command]
pub async fn mount_start(app: AppHandle, request: MountRequest) -> CmdResult<MountInfo> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<AppState>().mounts.mount(&app, request)
    })
    .await
    .map_err(|e| err(Error::internal(e)))?
    .map_err(err)
}

/// Unmounts; waits for a file system call in progress (a solid block being decoded). The
/// changes of a writable mount are saved into the archive (`save`) or discarded; saving runs
/// as a job, whose id is returned (`0` when there is nothing to save), owned by `window` (a
/// dialog window of the file manager, which then shows its progress).
#[tauri::command]
pub async fn mount_stop(
    app: AppHandle,
    id: u64,
    save: bool,
    window: Option<String>,
) -> CmdResult<u32> {
    let saved = {
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            app.state::<AppState>().mounts.unmount(&app, id, save)
        })
        .await
        .map_err(|e| err(Error::internal(e)))?
        .map_err(err)?
    };
    let Some(saved) = saved else { return Ok(0) };
    let state = app.state::<AppState>();
    let (title, work, finish) = ops::save_mount_work(&app, saved);
    let spec = JobSpec {
        window,
        ..JobSpec::new("mountSave", title, policy(&state))
    };
    Ok(state.jobs.spawn(&app, spec, work, Some(finish)))
}
