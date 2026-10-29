//! Mounted archives (`docs/12-mount.md`). They live in `omnizip-mountd`, the user's mount
//! daemon, which OmniZip starts on the first mount: they stay mounted after OmniZip exits,
//! unless the settings unmount them on exit. "Mount as administrator" goes to the elevated
//! daemon, started through the UAC prompt (§4).
//!
//! This module is their client: it lists both daemons, routes unmounts to the right one, and
//! turns their change notifications into `mounts://changed` events. The user's daemon saves its
//! list for "restore on start". A writable mount's changes, kept at unmount, come back as a
//! [`SavedLayer`] for a job to save into the archive (`ops::save_mount_work`).

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Mutex,
};

use omnizip_core::{Error, ErrorCode};
use omnizip_mountd::{Level, MountSpec, MountStatus, Request, Response, SavedLayer, SavedMount};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::{ops, state::AppState};

/// What the UI shows of a mount.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MountInfo {
    /// Which daemon and which of its mounts (see [`app_id`]).
    pub id: u64,
    pub archive: String,
    /// File name of the archive.
    pub name: String,
    pub label: String,
    /// `Z:` or a directory.
    pub mount_point: String,
    pub format: String,
    /// Bytes decoded into the cache (memory and disk).
    pub cache_bytes: u64,
    /// A solid archive is being decoded into the cache.
    pub warming: bool,
    /// Mounted as administrator.
    pub elevated: bool,
    /// A drive visible to every session (registered with the Mount Manager).
    pub global: bool,
    /// Writes go to a temporary layer.
    pub writable: bool,
    /// The layer holds changes not saved into the archive.
    pub dirty: bool,
    /// The changes can be saved into the archive (not RAR, not split, not self-extracting).
    pub saveable: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountRequest {
    pub path: String,
    /// The archive open in the browser, whose password and name encoding are reused.
    pub archive_id: Option<u32>,
    /// `Z:` or a directory; the next free drive letter when absent.
    pub mount_point: Option<String>,
    pub label: Option<String>,
    /// Decode a solid archive into the cache right away (the setting when absent).
    pub warm: Option<bool>,
    pub password: Option<String>,
    /// Mount through the elevated daemon (asks for administrator rights).
    #[serde(default)]
    pub elevated: bool,
    /// With a temporary writable layer (the setting when absent).
    pub writable: Option<bool>,
}

/// Mount ids are per daemon; the app's id also says which daemon (odd: the elevated one).
fn app_id(level: Level, id: u64) -> u64 {
    id * 2 + u64::from(level == Level::Admin)
}

fn daemon_id(id: u64) -> (Level, u64) {
    let level = if id % 2 == 1 {
        Level::Admin
    } else {
        Level::User
    };
    (level, id / 2)
}

fn info(level: Level, s: MountStatus) -> MountInfo {
    MountInfo {
        id: app_id(level, s.id),
        name: ops::file_name(&s.archive),
        archive: s.archive.to_string_lossy().into_owned(),
        label: s.label,
        mount_point: s.mount_point.to_string_lossy().into_owned(),
        format: s.format,
        cache_bytes: s.cache_bytes,
        warming: s.warming,
        elevated: level == Level::Admin,
        global: s.global,
        writable: s.writable,
        dirty: s.dirty,
        saveable: s.saveable,
    }
}

/// Whether this build can mount on this system.
pub fn probe() -> omnizip_vfs::Probe {
    #[cfg(windows)]
    {
        omnizip_vfs::probe(omnizip_mount_winfsp::BUILT)
    }
    #[cfg(not(windows))]
    {
        omnizip_vfs::probe(false)
    }
}

/// Drive letters a mount can use (empty where mounts are not drive letters).
pub fn free_letters() -> Vec<String> {
    #[cfg(windows)]
    {
        omnizip_mount_winfsp::free_drive_letters()
            .into_iter()
            .map(|c| format!("{c}:"))
            .collect()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

#[cfg(windows)]
use omnizip_mountd::client as daemons;

/// No mount daemon on this platform yet: none runs, and none can be started.
#[cfg(not(windows))]
mod daemons {
    use std::path::Path;

    use omnizip_core::{Error, ErrorCode};
    use omnizip_mountd::{Level, MountSpec, MountStatus, Request, Response};

    fn unsupported() -> Error {
        Error::new(ErrorCode::UnsupportedOperation).with("operation", "mount")
    }

    pub fn call(_: Level, _: &Request) -> Result<Response, Error> {
        Err(unsupported())
    }

    pub fn running(_: Level) -> bool {
        false
    }

    pub fn watch(_: Level, _: impl FnMut()) -> Result<(), Error> {
        Err(unsupported())
    }

    pub fn ensure_user_daemon(_: &Path) -> Result<(), Error> {
        Err(unsupported())
    }

    pub fn mount_elevated(_: &Path, _: &MountSpec) -> Result<MountStatus, Error> {
        Err(unsupported())
    }
}

/// `omnizip-mountd`, installed next to the app.
fn daemon_exe() -> Result<PathBuf, Error> {
    let exe = std::env::current_exe()?
        .with_file_name(format!("omnizip-mountd{}", std::env::consts::EXE_SUFFIX));
    if exe.is_file() {
        Ok(exe)
    } else {
        Err(Error::new(ErrorCode::MissingDependency)
            .with("path", exe.display())
            .detail("omnizip-mountd"))
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
pub struct MountManager {
    /// Daemons whose notifications a thread forwards.
    watching: Mutex<HashSet<Level>>,
}

impl MountManager {
    fn changed(app: &AppHandle) {
        let _ = app.emit("mounts://changed", ());
    }

    /// The mounts of both daemons.
    pub fn list(&self) -> Vec<MountInfo> {
        [Level::User, Level::Admin]
            .into_iter()
            .filter_map(|level| match daemons::call(level, &Request::List) {
                Ok(Response::Mounts { mounts }) => {
                    Some(mounts.into_iter().map(move |m| info(level, m)))
                }
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// Forwards a daemon's notifications as `mounts://changed`, until it exits.
    fn watch(&self, app: &AppHandle, level: Level) {
        if !lock(&self.watching).insert(level) {
            return;
        }
        let app = app.clone();
        std::thread::spawn(move || {
            let _ = daemons::watch(level, || Self::changed(&app));
            lock(&app.state::<AppState>().mounts.watching).remove(&level);
            // The daemon exited: its mounts are gone.
            Self::changed(&app);
        });
    }

    /// Watches the daemons that run already (mounts kept from an earlier OmniZip).
    pub fn watch_running(&self, app: &AppHandle) {
        for level in [Level::User, Level::Admin] {
            if daemons::running(level) {
                self.watch(app, level);
            }
        }
    }

    /// Mounts an archive, or returns its mount if it is mounted already.
    pub fn mount(&self, app: &AppHandle, req: MountRequest) -> Result<MountInfo, Error> {
        let path = PathBuf::from(&req.path);
        if let Some(m) = self
            .list()
            .into_iter()
            .find(|m| same_path(Path::new(&m.archive), &path))
        {
            return Ok(m);
        }
        let state = app.state::<AppState>();
        let settings = state.settings().mount;
        let opened = req.archive_id.and_then(|id| state.archive(id));
        let password = req
            .password
            .clone()
            .or_else(|| opened.as_ref().and_then(|a| lock(&a.password).clone()));
        let spec = MountSpec {
            archive: path,
            mount_point: req
                .mount_point
                .as_deref()
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
            label: req.label.clone(),
            password,
            encoding: opened
                .as_ref()
                .and_then(|a| a.encoding)
                .map(|e| e.label().to_owned()),
            offline_attribute: settings.offline_attribute,
            cache_bytes: u64::from(settings.cache_gb.max(1)) << 30,
            warm: req.warm.unwrap_or(settings.warm_solid),
            writable: req.writable.unwrap_or(settings.writable),
        };
        let exe = daemon_exe()?;
        let (level, status) = if req.elevated {
            // A missing or wrong password is reported before the UAC prompt, not after it.
            omnizip_mountd::open_volume(&spec, None)?;
            (Level::Admin, daemons::mount_elevated(&exe, &spec)?)
        } else {
            daemons::ensure_user_daemon(&exe)?;
            match daemons::call(Level::User, &Request::Mount { spec })? {
                Response::Mounted { mount } => (Level::User, mount),
                r => return Err(Error::internal(format!("unexpected answer {r:?}"))),
            }
        };
        self.watch(app, level);
        Self::changed(app);
        Ok(info(level, status))
    }

    /// Unmounts one archive. Waits for file system calls in progress (a solid block being
    /// decoded) to finish. The changes of a writable mount are discarded, or kept for saving
    /// (`keep_changes`) and returned.
    pub fn unmount(
        &self,
        app: &AppHandle,
        id: u64,
        keep_changes: bool,
    ) -> Result<Option<SavedLayer>, Error> {
        let (level, id) = daemon_id(id);
        let r = daemons::call(level, &Request::Unmount { id, keep_changes });
        Self::changed(app);
        match r? {
            Response::Unmounted { saved } => Ok(saved),
            r => Err(Error::internal(format!("unexpected answer {r:?}"))),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.list().is_empty()
    }

    /// Unmounts everything (OmniZip quits and the settings say so), except writable mounts
    /// with changes: they wait for the user to save or discard them.
    pub fn unmount_all(&self) {
        for level in [Level::User, Level::Admin] {
            if daemons::running(level) {
                let _ = daemons::call(level, &Request::UnmountAll { keep_changed: true });
            }
        }
    }

    /// Mounts the archives the user's daemon had at its last change, where possible at the same
    /// mount point. Mounted ones are left alone; archives that need a password or no longer
    /// exist are skipped.
    pub fn restore(&self, app: &AppHandle) {
        let Some(saved) = omnizip_mountd::saved_list_path()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|t| serde_json::from_str::<Vec<SavedMount>>(&t).ok())
        else {
            return;
        };
        for s in saved {
            if !Path::new(&s.archive).is_file() {
                continue;
            }
            let req = |mount_point| MountRequest {
                path: s.archive.clone(),
                archive_id: None,
                mount_point,
                label: Some(s.label.clone()),
                warm: None,
                password: None,
                elevated: false,
                writable: Some(s.writable),
            };
            let r = self
                .mount(app, req(Some(s.mount_point.clone())))
                .or_else(|e| match e.code() {
                    // The mount point was taken meanwhile: take another drive letter.
                    ErrorCode::AlreadyExists => self.mount(app, req(None)),
                    _ => Err(e),
                });
            if let Err(e) = r {
                tracing::warn!(archive = %s.archive, code = e.code().id(), "restoring a mount");
            }
        }
    }
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
        norm(a) == norm(b)
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_say_which_daemon() {
        for (level, id) in [(Level::User, 1), (Level::Admin, 1), (Level::Admin, 7)] {
            assert_eq!(daemon_id(app_id(level, id)), (level, id));
        }
        assert_ne!(app_id(Level::User, 3), app_id(Level::Admin, 3));
    }
}
