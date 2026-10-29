//! Mounted archives (`docs/12-mount.md`). The app hosts its mounts itself: each one is an
//! [`ArchiveFs`] over its own reader, served by the platform backend from the backend's threads
//! while the app runs. Closing OmniZip unmounts them, unless the settings keep them mounted, in
//! which case closing the main window only hides it.
//!
//! The list of mounts is saved whenever the user mounts or unmounts, so that "restore on start"
//! can mount the same archives again.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use omnizip_core::{
    Error, ErrorCode, FixedPassword, JobCtx, MountClass, NullProgress, OpenOptions,
    encoding::NameEncoding,
};
use omnizip_formats::shared;
use omnizip_vfs::{ArchiveFs, CacheLimits, FsOptions, Mount, MountError, MountOptions};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::{ops, state::AppState};

/// What the UI shows of a mount.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MountInfo {
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
}

/// A mount to restore at start-up.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Saved {
    archive: String,
    mount_point: String,
    label: String,
}

struct Live {
    id: u64,
    archive: PathBuf,
    label: String,
    fs: Arc<ArchiveFs>,
    mount: Box<dyn Mount>,
    warming: Arc<AtomicBool>,
}

impl Live {
    fn info(&self) -> MountInfo {
        let (mem, disk) = self.fs.cache().usage();
        MountInfo {
            id: self.id,
            archive: self.archive.to_string_lossy().into_owned(),
            name: ops::file_name(&self.archive),
            label: self.label.clone(),
            mount_point: self.mount.mount_point().to_string_lossy().into_owned(),
            format: self.fs.info().format.name().to_owned(),
            cache_bytes: mem + disk,
            warming: self.warming.load(Ordering::SeqCst),
        }
    }

    fn saved(&self) -> Saved {
        Saved {
            archive: self.archive.to_string_lossy().into_owned(),
            mount_point: self.mount.mount_point().to_string_lossy().into_owned(),
            label: self.label.clone(),
        }
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

fn backend() -> Result<Box<dyn omnizip_vfs::MountBackend>, Error> {
    #[cfg(windows)]
    {
        Ok(Box::new(omnizip_mount_winfsp::WinFsp))
    }
    #[cfg(not(windows))]
    {
        Err(Error::new(ErrorCode::UnsupportedOperation).with("operation", "mount"))
    }
}

fn mount_error(e: MountError) -> Error {
    match e {
        MountError::NotInstalled(what) => {
            Error::new(ErrorCode::MissingDependency).detail(format!("WinFsp: {what}"))
        }
        MountError::InUse(p) => Error::new(ErrorCode::AlreadyExists).with("path", p.display()),
        MountError::NoDriveLetter => Error::new(ErrorCode::AlreadyExists).with("path", "D: – Z:"),
        MountError::System(m) => Error::new(ErrorCode::Io).with("path", "").detail(m),
    }
}

#[derive(Default)]
pub struct MountManager {
    live: Mutex<Vec<Live>>,
    next: AtomicU64,
}

impl MountManager {
    fn file(state: &AppState) -> Option<PathBuf> {
        state.data_dir.as_ref().map(|d| d.join("mounts.json"))
    }

    fn save(&self, state: &AppState) {
        let Some(f) = Self::file(state) else { return };
        let saved: Vec<Saved> = self
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(Live::saved)
            .collect();
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(text) = serde_json::to_string_pretty(&saved) {
            let _ = std::fs::write(&f, text);
        }
    }

    fn changed(app: &AppHandle) {
        let _ = app.emit("mounts://changed", ());
    }

    /// Live mounts. Volumes the system took down on its own are dropped from the list.
    pub fn list(&self) -> Vec<MountInfo> {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.retain(|l| l.mount.is_alive());
        live.iter().map(Live::info).collect()
    }

    /// Mounts an archive, or returns its mount if it is mounted already.
    pub fn mount(&self, app: &AppHandle, req: MountRequest) -> Result<MountInfo, Error> {
        let state = app.state::<AppState>();
        let path = PathBuf::from(&req.path);
        if let Some(l) = self
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|l| same_path(&l.archive, &path))
        {
            return Ok(l.info());
        }
        let backend = backend()?;
        let settings = state.settings().mount;
        let opened = req.archive_id.and_then(|id| state.archive(id));
        let password = req.password.clone().or_else(|| {
            opened
                .as_ref()
                .and_then(|a| a.password.lock().unwrap_or_else(|e| e.into_inner()).clone())
        });
        let encoding: Option<NameEncoding> = opened.as_ref().and_then(|a| a.encoding);
        let ctx = match &password {
            Some(p) => JobCtx::new(Arc::new(NullProgress), Arc::new(FixedPassword(p.clone()))),
            None => JobCtx::default(),
        };
        let oo = OpenOptions {
            password: password.clone(),
            encoding,
            locale: Some(omnizip_i18n::detect_language(None).to_owned()),
        };
        let reader = shared().open(&path, &oo, &ctx)?;
        // Without a password, encrypted entries could be listed but never read.
        if reader.info().encrypted && password.is_none() {
            return Err(Error::new(ErrorCode::PasswordRequired).with("path", path.display()));
        }
        let label = req
            .label
            .clone()
            .filter(|l| !l.trim().is_empty())
            .unwrap_or_else(|| omnizip_vfs::archivefs::volume_label(reader.info()));
        let fs = Arc::new(
            ArchiveFs::new(
                reader,
                FsOptions {
                    cache: CacheLimits {
                        disk: u64::from(settings.cache_gb.max(1)) << 30,
                        ..CacheLimits::default()
                    },
                    cache_dir: state.cache_dir.as_ref().map(|d| d.join("mount")),
                    label: Some(label.clone()),
                    password,
                    ..FsOptions::default()
                },
            )
            .map_err(|e| Error::io(e, state.cache_dir.clone().unwrap_or_default()))?,
        );
        match fs.check_password() {
            Err(omnizip_vfs::FsError::Access) => {
                return Err(Error::new(ErrorCode::WrongPassword).with("path", path.display()));
            }
            // A damaged entry does not keep the rest of the archive from being mounted.
            Err(e) => tracing::warn!("mount: checking the password: {e}"),
            Ok(()) => {}
        }
        let opts = MountOptions {
            mount_point: req
                .mount_point
                .as_deref()
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
            label: Some(label.clone()),
            read_only: true,
            offline_attribute: settings.offline_attribute,
        };
        let mount = backend.mount(fs.clone(), &opts).map_err(mount_error)?;
        let live = Live {
            id: self.next.fetch_add(1, Ordering::Relaxed) + 1,
            archive: path,
            label,
            fs: fs.clone(),
            mount,
            warming: Arc::new(AtomicBool::new(false)),
        };
        let info = live.info();
        let warm = req.warm.unwrap_or(settings.warm_solid) && fs.mount_class() == MountClass::C;
        let warming = live.warming.clone();
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(live);
        self.save(&state);
        if warm {
            warming.store(true, Ordering::SeqCst);
            let app = app.clone();
            std::thread::spawn(move || {
                if let Err(e) = fs.warm() {
                    tracing::warn!("mount warm-up: {e}");
                }
                warming.store(false, Ordering::SeqCst);
                Self::changed(&app);
            });
        }
        Self::changed(app);
        Ok(MountInfo {
            warming: warm,
            ..info
        })
    }

    /// Unmounts one archive. Waits for file system calls in progress (a solid block being
    /// decoded) to finish.
    pub fn unmount(&self, app: &AppHandle, id: u64) {
        let live = {
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            live.iter().position(|l| l.id == id).map(|i| live.remove(i))
        };
        if live.is_some() {
            self.save(&app.state::<AppState>());
            drop(live);
            Self::changed(app);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    /// Unmounts everything when the app quits; the saved list stays for the next start.
    pub fn unmount_all(&self) {
        let all = std::mem::take(&mut *self.live.lock().unwrap_or_else(|e| e.into_inner()));
        drop(all);
    }

    /// Mounts the archives saved at the last change, where possible at the same mount point.
    /// Archives that need a password or no longer exist are skipped.
    pub fn restore(&self, app: &AppHandle) {
        let state = app.state::<AppState>();
        let Some(saved) = Self::file(&state)
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|t| serde_json::from_str::<Vec<Saved>>(&t).ok())
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

    /// Removes spool files left by a previous run that did not exit cleanly.
    pub fn sweep(state: &AppState) {
        if let Some(d) = state.cache_dir.as_ref().map(|d| d.join("mount"))
            && let Ok(entries) = std::fs::read_dir(&d)
        {
            for e in entries.flatten() {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
        norm(a) == norm(b)
    } else {
        a == b
    }
}
