//! The daemon: hosts mounts and answers on its pipe (`docs/12-mount.md` §4).
//!
//! `omnizip-mountd` is the user's daemon, started by OmniZip when it first mounts. It mounts for
//! any process of the user and keeps its list in `mounts.json` for "restore on start".
//!
//! `omnizip-mountd --elevated --handoff <pipe>` is started through the UAC prompt to mount as
//! administrator: it reads the request from the pipe OmniZip created, then becomes the elevated
//! daemon, or passes the request to the one running already. The elevated daemon lists and
//! unmounts for the user's processes, but mounts only for elevated ones, and only on drive
//! letters: a program of the user must not get archives parsed with administrator rights, nor
//! volumes placed where only administrators may write.
//!
//! A writable mount puts a layer over the archive (`cache/mount*/layer-<id>`). At unmount its
//! changes are discarded, or kept for the app to save: the layer then moves to
//! `cache/mount-saving`, which no daemon clears, and the app removes it once saved. A layer left
//! behind by a daemon that did not exit cleanly is cleared with the other cache files.
//!
//! Either exits once it has had no mount for five minutes.

use std::{
    fs::File,
    io::{BufRead, BufReader, Write},
    os::windows::io::OwnedHandle,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use omnizip_core::{Error, ErrorCode, MountClass, appdirs};
use omnizip_vfs::{ArchiveFs, Mount, MountBackend, MountError, MountOptions, OverlayFs, VirtualFs};

use crate::{
    client,
    pipe::{self, Access},
    proto::{
        Level, MountSpec, MountStatus, Request, Response, SavedLayer, SavedMount, saved_list_path,
        saving_dir,
    },
    save::can_save,
    volume::open_volume,
};

/// How long a daemon without mounts waits for one before it exits (`OMNIZIP_MOUNTD_IDLE_SECS`
/// overrides it, for tests).
fn idle_limit() -> Duration {
    let secs = std::env::var("OMNIZIP_MOUNTD_IDLE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    Duration::from_secs(secs)
}

/// Runs the daemon the arguments ask for; the exit code.
pub fn run(args: &[String]) -> i32 {
    match args {
        [] => user_daemon(),
        [e, h, handoff_pipe] if e == "--elevated" && h == "--handoff" => handoff(handoff_pipe),
        _ => 2,
    }
}

fn user_daemon() -> i32 {
    let Ok(name) = pipe::daemon_pipe(Level::User) else {
        return 1;
    };
    // Another daemon serves this user already.
    let Ok(first) = pipe::server(&name, true, Access::User) else {
        return 0;
    };
    serve(Host::new(Level::User), first, &name, Access::User)
}

fn handoff(handoff_pipe: &str) -> i32 {
    if !pipe::elevated() {
        return 1;
    }
    let Ok(conn) = pipe::connect(handoff_pipe) else {
        return 1;
    };
    let mut line = String::new();
    if BufReader::new(&conn).read_line(&mut line).is_err() {
        return 1;
    }
    let Ok(Request::Mount { spec }) = serde_json::from_str(&line) else {
        return 1;
    };
    let Ok(name) = pipe::daemon_pipe(Level::Admin) else {
        return 1;
    };
    match pipe::server(&name, true, Access::UserFromBelow) {
        Ok(first) => {
            let host = Host::new(Level::Admin);
            reply(&conn, &host.handle(Request::Mount { spec }, true));
            drop(conn);
            serve(host, first, &name, Access::UserFromBelow)
        }
        Err(_) => {
            // The elevated daemon runs already: ask it, as the elevated process this is.
            let r = client::call(Level::Admin, &Request::Mount { spec })
                .unwrap_or_else(|e| Response::failed(&e));
            reply(&conn, &r);
            0
        }
    }
}

fn reply(conn: &File, r: &Response) -> bool {
    let mut line = serde_json::to_string(r).unwrap_or_default();
    line.push('\n');
    (&*conn).write_all(line.as_bytes()).is_ok()
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Hosted {
    id: u64,
    archive: PathBuf,
    label: String,
    fs: Arc<ArchiveFs>,
    /// The writable layer over `fs`, which is then what is mounted.
    overlay: Option<Arc<OverlayFs>>,
    /// As mounted, for saving the changes.
    password: Option<String>,
    encoding: Option<String>,
    mount: Box<dyn Mount>,
    warming: Arc<AtomicBool>,
}

impl Hosted {
    /// Unmounts, waiting for file system calls in progress, and keeps the changes of a writable
    /// mount for saving (`keep`, when there are any) or discards them.
    fn finish(self, keep: bool) -> Option<SavedLayer> {
        let Hosted {
            id,
            archive,
            overlay,
            password,
            encoding,
            mount,
            ..
        } = self;
        // Closes every handle of the volume, so the layer can move.
        drop(mount);
        let overlay = overlay?;
        let layer = overlay.layer_dir().to_owned();
        let changes = overlay.changes();
        drop(overlay);
        if keep && !changes.is_empty() {
            let kept = saving_dir().and_then(|d| {
                let dest = d.join(format!("{}-{id}", std::process::id()));
                std::fs::create_dir_all(&d).ok()?;
                std::fs::rename(&layer, &dest).ok()?;
                Some(dest)
            });
            match kept {
                Some(layer) => {
                    return Some(SavedLayer {
                        archive,
                        layer,
                        changes,
                        password,
                        encoding,
                    });
                }
                None => tracing::warn!(layer = %layer.display(), "could not keep the changes"),
            }
        }
        let _ = std::fs::remove_dir_all(&layer);
        None
    }

    fn dirty(&self) -> bool {
        self.overlay.as_ref().is_some_and(|o| o.is_dirty())
    }

    fn status(&self) -> MountStatus {
        let (mem, disk) = self.fs.cache().usage();
        MountStatus {
            id: self.id,
            archive: self.archive.clone(),
            label: self.label.clone(),
            mount_point: self.mount.mount_point().to_owned(),
            format: self.fs.info().format.name().to_owned(),
            cache_bytes: mem + disk,
            warming: self.warming.load(Ordering::SeqCst),
            global: self.mount.global(),
            writable: self.overlay.is_some(),
            dirty: self.dirty(),
            saveable: self.overlay.is_some() && can_save(self.fs.info()),
        }
    }
}

/// The mounts of one daemon and the clients watching them.
pub struct Host {
    level: Level,
    mounts: Mutex<Vec<Hosted>>,
    next: AtomicU64,
    watchers: Mutex<Vec<File>>,
    /// Since when the daemon has had no mount.
    idle_since: Mutex<Option<Instant>>,
    cache_dir: Option<PathBuf>,
}

impl Host {
    pub fn new(level: Level) -> Arc<Self> {
        let cache_dir = appdirs::cache_dir().map(|d| {
            d.join(match level {
                Level::User => "mount",
                Level::Admin => "mount-admin",
            })
        });
        // Spool files of a daemon that did not exit cleanly.
        if let Some(d) = &cache_dir
            && let Ok(entries) = std::fs::read_dir(d)
        {
            for e in entries.flatten() {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
        Arc::new(Self {
            level,
            mounts: Mutex::new(Vec::new()),
            next: AtomicU64::new(1),
            watchers: Mutex::new(Vec::new()),
            idle_since: Mutex::new(Some(Instant::now())),
            cache_dir,
        })
    }

    /// Answers one request; `may_mount` is false for a client the elevated daemon does not
    /// mount for.
    pub fn handle(self: &Arc<Self>, req: Request, may_mount: bool) -> Response {
        let r = match req {
            Request::Mount { spec } if !may_mount => {
                Err(Error::new(ErrorCode::PermissionDenied).with("path", spec.archive.display()))
            }
            Request::Mount { spec } => self.mount(spec).map(|mount| Response::Mounted { mount }),
            Request::Unmount { id, keep_changes } => Ok(Response::Unmounted {
                saved: self.unmount(|h| h.id == id, keep_changes).pop(),
            }),
            Request::UnmountAll { keep_changed } => {
                self.unmount(|h| !(keep_changed && h.dirty()), false);
                Ok(Response::Done)
            }
            Request::List => Ok(Response::Mounts {
                mounts: self.list(),
            }),
            Request::Watch => Ok(Response::Done),
        };
        r.unwrap_or_else(|e| Response::failed(&e))
    }

    fn mount(self: &Arc<Self>, spec: MountSpec) -> Result<MountStatus, Error> {
        if let Some(h) = lock(&self.mounts)
            .iter()
            .find(|h| same_path(&h.archive, &spec.archive))
        {
            return Ok(h.status());
        }
        let point = spec
            .mount_point
            .clone()
            .filter(|p| !p.as_os_str().is_empty());
        if self.level == Level::Admin
            && let Some(p) = point.as_deref().filter(|p| !is_drive(p))
        {
            return Err(Error::new(ErrorCode::PermissionDenied).with("path", p.display()));
        }
        let (fs, label) = open_volume(&spec, self.cache_dir.clone())?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let overlay = if spec.writable {
            let layer = self
                .cache_dir
                .clone()
                .unwrap_or_else(std::env::temp_dir)
                .join(format!("layer-{id}"));
            let _ = std::fs::remove_dir_all(&layer);
            let o = OverlayFs::new(fs.clone(), layer.clone()).map_err(|e| {
                Error::new(ErrorCode::Io)
                    .with("path", layer.display())
                    .detail(e.to_string())
            })?;
            Some(Arc::new(o))
        } else {
            None
        };
        let opts = MountOptions {
            mount_point: point,
            label: Some(label.clone()),
            read_only: overlay.is_none(),
            offline_attribute: spec.offline_attribute,
        };
        let volume: Arc<dyn VirtualFs> = match &overlay {
            Some(o) => o.clone(),
            None => fs.clone(),
        };
        let mount = match omnizip_mount_winfsp::WinFsp.mount(volume, &opts) {
            Ok(m) => m,
            Err(e) => {
                if let Some(o) = &overlay {
                    let _ = std::fs::remove_dir_all(o.layer_dir());
                }
                return Err(mount_error(e));
            }
        };
        // Solid archives and single-stream ones (tar.gz) decode from their start anyway.
        let warm = spec.warm && matches!(fs.mount_class(), MountClass::C | MountClass::D);
        let hosted = Hosted {
            id,
            archive: spec.archive.clone(),
            label,
            fs: fs.clone(),
            overlay,
            password: spec.password.clone(),
            encoding: spec.encoding.clone(),
            mount,
            warming: Arc::new(AtomicBool::new(warm)),
        };
        let status = hosted.status();
        let warming = hosted.warming.clone();
        lock(&self.mounts).push(hosted);
        self.changed();
        if warm {
            let host = self.clone();
            std::thread::spawn(move || {
                if let Err(e) = fs.warm() {
                    tracing::warn!("mount warm-up: {e}");
                }
                warming.store(false, Ordering::SeqCst);
                host.notify();
            });
        }
        Ok(status)
    }

    /// Unmounts the mounts `which` selects, keeping their changes when `keep` is set. Waits
    /// for file system calls in progress (a solid block being decoded).
    fn unmount(&self, which: impl Fn(&Hosted) -> bool, keep: bool) -> Vec<SavedLayer> {
        let gone: Vec<Hosted> = {
            let mut mounts = lock(&self.mounts);
            let (gone, stay) = std::mem::take(&mut *mounts).into_iter().partition(&which);
            *mounts = stay;
            gone
        };
        if gone.is_empty() {
            return Vec::new();
        }
        let saved = gone.into_iter().filter_map(|h| h.finish(keep)).collect();
        self.changed();
        saved
    }

    fn list(&self) -> Vec<MountStatus> {
        lock(&self.mounts).iter().map(Hosted::status).collect()
    }

    /// Saves the list, tells the watchers, and starts the idle clock when no mount is left.
    fn changed(&self) {
        let empty = lock(&self.mounts).is_empty();
        *lock(&self.idle_since) = empty.then(Instant::now);
        self.save();
        self.notify();
    }

    fn save(&self) {
        if self.level != Level::User {
            return;
        }
        let Some(f) = saved_list_path() else { return };
        let saved: Vec<SavedMount> = lock(&self.mounts)
            .iter()
            .map(|h| SavedMount {
                archive: h.archive.to_string_lossy().into_owned(),
                mount_point: h.mount.mount_point().to_string_lossy().into_owned(),
                label: h.label.clone(),
                writable: h.overlay.is_some(),
            })
            .collect();
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(text) = serde_json::to_string_pretty(&saved) {
            let _ = std::fs::write(&f, text);
        }
    }

    fn notify(&self) {
        lock(&self.watchers).retain(|w| reply(w, &Response::Changed));
    }

    /// Drops volumes the system took down on its own; `true` when the daemon should exit.
    fn tick(&self) -> bool {
        let dead = lock(&self.mounts).iter().any(|h| !h.mount.is_alive());
        if dead {
            // Nobody is there to save what such a volume had changed.
            self.unmount(|h| !h.mount.is_alive(), false);
        }
        lock(&self.idle_since).is_some_and(|t| t.elapsed() >= idle_limit())
    }
}

/// Accepts clients forever, one thread each; exits the process when idle.
fn serve(host: Arc<Host>, first: OwnedHandle, name: &str, access: Access) -> ! {
    {
        let host = host.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1).min(idle_limit()));
                if host.tick() {
                    std::process::exit(0);
                }
            }
        });
    }
    let mut next = first;
    loop {
        if pipe::accept(&next).is_ok() {
            let conn = File::from(next);
            let host = host.clone();
            std::thread::spawn(move || connection(&host, conn));
        }
        next = loop {
            match pipe::server(name, false, access) {
                Ok(h) => break h,
                Err(e) => {
                    tracing::warn!("omnizip-mountd: {e}");
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        };
    }
}

/// Serves one client: requests line by line, until it disconnects. A `Watch` connection is
/// kept for notifications.
fn connection(host: &Arc<Host>, conn: File) {
    let mut reader = BufReader::new(&conn);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let r = match serde_json::from_str::<Request>(&line) {
            Ok(Request::Watch) => {
                drop(reader);
                lock(&host.watchers).push(conn);
                return;
            }
            // The client's identity is known once it has written to the pipe.
            Ok(req @ Request::Mount { .. }) => {
                let may = host.level == Level::User || pipe::client_elevated(&conn);
                host.handle(req, may)
            }
            Ok(req) => host.handle(req, false),
            Err(e) => Response::failed(&Error::invalid_argument(e)),
        };
        if !reply(&conn, &r) {
            return;
        }
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

/// `Z`, `Z:` or `Z:\`.
fn is_drive(p: &Path) -> bool {
    let s = p.to_string_lossy();
    let b = s.trim_end_matches(['\\', '/']).as_bytes();
    (b.len() == 1 || b.len() == 2 && b[1] == b':') && b[0].is_ascii_alphabetic()
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drives_and_paths() {
        assert!(is_drive(Path::new("Z:")));
        assert!(is_drive(Path::new("z:\\")));
        assert!(is_drive(Path::new("Q")));
        assert!(!is_drive(Path::new("C:\\mnt")));
        assert!(same_path(
            Path::new("D:/A/b.zip"),
            Path::new("d:\\a\\B.ZIP")
        ));
    }
}
