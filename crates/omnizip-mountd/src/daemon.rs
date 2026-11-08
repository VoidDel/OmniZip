//! The daemon: hosts mounts and answers on its pipe (`docs/12-mount.md` §4).
//!
//! `omnizip-mountd` is the user's daemon, started by OmniZip when it first mounts. It mounts for
//! any process of the user; the mounts stay until they are unmounted or the user signs out.
//!
//! `omnizip-mountd --elevated --handoff <pipe>` is started through the UAC prompt to mount as
//! administrator: it reads the request from the pipe OmniZip created, then becomes the elevated
//! daemon, or passes the request to the one running already. The elevated daemon lists and
//! unmounts for the user's processes, but mounts only for elevated ones, and only on drive
//! letters: a program of the user must not get archives parsed with administrator rights, nor
//! volumes placed where only administrators may write.
//!
//! A writable mount puts a layer over the archive (`cache/mount/layer-<id>`). At unmount its
//! changes are discarded, or kept for the app to save: the layer then moves to
//! `cache/mount-saving`, which no daemon clears, and the app removes it once saved (when it
//! cannot move, it is copied, or as a last resort saved from where it is). A layer left behind
//! by a daemon that did not exit cleanly is cleared with the other cache files.
//!
//! The elevated daemon keeps its spool files and layers in a folder of its own that only
//! administrators may change (`admin_dir`), never in the user's profile, and hands kept changes
//! over to the user for the app to save.
//!
//! On Linux and macOS there is only the user's daemon, on a Unix socket (`sock`), mounting
//! through FUSE or the loopback NFS server.
//! It unmounts everything when it is asked to stop (`SIGTERM` as the session ends), and at start
//! clears volumes a daemon that did not exit cleanly left behind.
//!
//! Either exits once it has had no mount for five minutes.

#[cfg(windows)]
use std::{fs::File, os::windows::io::OwnedHandle};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use omnizip_core::{Error, ErrorCode, MountClass, appdirs};
use omnizip_vfs::{ArchiveFs, Mount, MountBackend, MountError, MountOptions, OverlayFs, VirtualFs};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::sock::{self, Conn};
#[cfg(windows)]
use crate::{
    client,
    pipe::{self, Access, Conn},
};
use crate::{
    proto::{Level, MountSpec, MountStatus, Request, Response, SavedLayer, saving_dir},
    save::{can_save, keep_layer},
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
        #[cfg(windows)]
        [e, h, handoff_pipe] if e == "--elevated" && h == "--handoff" => handoff(handoff_pipe),
        _ => 2,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn user_daemon() -> i32 {
    let Ok(path) = sock::daemon_socket(Level::User) else {
        return 1;
    };
    match sock::listen(&path) {
        Ok(Some(listener)) => {
            clear_stale();
            serve(Host::new(Level::User), listener)
        }
        // Another daemon serves this user already.
        Ok(None) => 0,
        Err(e) => {
            tracing::warn!("omnizip-mountd: {}: {e}", path.display());
            1
        }
    }
}

#[cfg(windows)]
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

#[cfg(windows)]
fn handoff(handoff_pipe: &str) -> i32 {
    if !pipe::elevated() {
        return 1;
    }
    // Only the app that started this process may hand the mount over: the pipe's name says
    // which process that is, and the user's other programs cannot add instances to it.
    let Some(app) = pipe::handoff_server(handoff_pipe) else {
        return 1;
    };
    let Ok(conn) = pipe::connect(handoff_pipe, pipe::Server::Process(app)) else {
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

fn reply(conn: &Conn, r: &Response) -> bool {
    let mut line = serde_json::to_string(r).unwrap_or_default();
    line.push('\n');
    (&*conn).write_all(line.as_bytes()).is_ok()
}

/// Volumes a daemon of this user that did not exit cleanly left mounted (their file systems
/// lived in it): unmounted, their folders removed.
#[cfg(target_os = "linux")]
fn clear_stale() {
    omnizip_mount_fuse::clear_stale();
}

#[cfg(target_os = "macos")]
fn clear_stale() {
    omnizip_mount_nfs::clear_stale();
}

/// The elevated daemon's spool files and layers: in its own folder, which only administrators
/// may change (`admin_dir`), never in the user's profile. Without one, spooling stays in
/// memory and writable mounts are refused.
fn admin_cache_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        match pipe::user_sid().and_then(crate::admin_dir::user_dir) {
            Ok(d) => Some(d.join("cache")),
            Err(e) => {
                tracing::warn!("omnizip-mountd: no folder of its own: {e}");
                None
            }
        }
    }
    // Only Windows has an elevated daemon.
    #[cfg(not(windows))]
    None
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
    fn finish(self, keep: bool, host: &Host) -> Option<SavedLayer> {
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
            let layer = match host.saving_dir() {
                Some(d) => keep_layer(&layer, &d.join(format!("{}-{id}", std::process::id()))),
                None => layer,
            };
            // The elevated daemon's folder is closed to the user, whose app saves the changes.
            #[cfg(windows)]
            if host.level == Level::Admin
                && let Err(e) =
                    pipe::user_sid().and_then(|u| crate::admin_dir::hand_over(&layer, u))
            {
                tracing::warn!(layer = %layer.display(), "cannot hand the changes over: {e}");
            }
            return Some(SavedLayer {
                archive,
                layer,
                changes,
                password,
                encoding,
            });
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
    watchers: Mutex<Vec<Conn>>,
    /// Since when the daemon has had no mount.
    idle_since: Mutex<Option<Instant>>,
    cache_dir: Option<PathBuf>,
}

impl Host {
    pub fn new(level: Level) -> Arc<Self> {
        let cache_dir = match level {
            Level::User => appdirs::cache_dir().map(|d| d.join("mount")),
            Level::Admin => admin_cache_dir(),
        };
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
        #[cfg(windows)]
        if self.level == Level::Admin
            && let Some(p) = point.as_deref().filter(|p| !is_drive(p))
        {
            return Err(Error::new(ErrorCode::PermissionDenied).with("path", p.display()));
        }
        let (fs, label) = open_volume(&spec, self.cache_dir.clone())?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let overlay = if spec.writable {
            let base = match (&self.cache_dir, self.level) {
                (Some(d), _) => d.clone(),
                (None, Level::User) => std::env::temp_dir(),
                // Never in a folder the user may change (`admin_dir`).
                (None, Level::Admin) => {
                    return Err(Error::new(ErrorCode::PermissionDenied)
                        .with("path", spec.archive.display())
                        .detail("the elevated daemon has no folder of its own for changes"));
                }
            };
            let layer = base.join(format!("layer-{id}"));
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
        #[cfg(windows)]
        let mounted = omnizip_mount_winfsp::WinFsp.mount(volume, &opts);
        #[cfg(target_os = "linux")]
        let mounted = omnizip_mount_fuse::Fuse.mount(volume, &opts);
        #[cfg(target_os = "macos")]
        let mounted = omnizip_mount_nfs::Nfs.mount(volume, &opts);
        let mount = match mounted {
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
        let saved = gone
            .into_iter()
            .filter_map(|h| h.finish(keep, self))
            .collect();
        self.changed();
        saved
    }

    /// Where kept layers wait to be saved: `cache/mount-saving` for the user's daemon, next to
    /// its cache in its own folder for the elevated one. No daemon clears either at start-up.
    fn saving_dir(&self) -> Option<PathBuf> {
        match self.level {
            Level::User => saving_dir(),
            Level::Admin => Some(self.cache_dir.as_deref()?.parent()?.join("saving")),
        }
    }

    fn list(&self) -> Vec<MountStatus> {
        lock(&self.mounts).iter().map(Hosted::status).collect()
    }

    /// Tells the watchers, and starts the idle clock when no mount is left.
    fn changed(&self) {
        let empty = lock(&self.mounts).is_empty();
        *lock(&self.idle_since) = empty.then(Instant::now);
        self.notify();
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

/// Drops volumes gone on their own and exits the process when idle, on a thread of its own.
fn watch_idle(host: &Arc<Host>) {
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

/// Unmounts everything and exits when the daemon is asked to stop. The signals are blocked
/// before any other thread starts, so that only this one receives them.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn stop_on_signals(host: &Arc<Host>) {
    // SAFETY: an initialized set, changed and read by these calls only.
    let set = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::sigaddset(&mut set, sig);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        set
    };
    let host = host.clone();
    std::thread::spawn(move || {
        let mut sig = 0;
        // SAFETY: a set of blocked signals.
        if unsafe { libc::sigwait(&set, &mut sig) } == 0 {
            tracing::info!(sig, "omnizip-mountd: stopping");
            host.unmount(|_| true, false);
            std::process::exit(0);
        }
    });
}

/// Accepts clients forever, one thread each; exits the process when idle or stopped.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn serve(host: Arc<Host>, listener: sock::Listener) -> ! {
    stop_on_signals(&host);
    watch_idle(&host);
    loop {
        match listener.accept() {
            Ok(conn) => {
                let host = host.clone();
                std::thread::spawn(move || connection(&host, conn));
            }
            Err(e) => {
                tracing::warn!("omnizip-mountd: {e}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// Accepts clients forever, one thread each; exits the process when idle.
#[cfg(windows)]
fn serve(host: Arc<Host>, first: OwnedHandle, name: &str, access: Access) -> ! {
    watch_idle(&host);
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
fn connection(host: &Arc<Host>, conn: Conn) {
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
                #[cfg(windows)]
                let may = host.level == Level::User || pipe::client_elevated(&conn);
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                let may = host.level == Level::User;
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
            let backend = if cfg!(windows) {
                "WinFsp"
            } else if cfg!(target_os = "macos") {
                "NFS"
            } else {
                "FUSE"
            };
            Error::new(ErrorCode::MissingDependency).detail(format!("{backend}: {what}"))
        }
        MountError::InUse(p) => Error::new(ErrorCode::AlreadyExists).with("path", p.display()),
        MountError::NoDriveLetter => Error::new(ErrorCode::AlreadyExists).with("path", "D: – Z:"),
        MountError::System(m) => Error::new(ErrorCode::Io).with("path", "").detail(m),
    }
}

/// `Z`, `Z:` or `Z:\`.
#[cfg(windows)]
fn is_drive(p: &Path) -> bool {
    let s = p.to_string_lossy();
    let b = s.trim_end_matches(['\\', '/']).as_bytes();
    (b.len() == 1 || b.len() == 2 && b[1] == b':') && b[0].is_ascii_alphabetic()
}

#[cfg(windows)]
fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
    norm(a) == norm(b)
}

#[cfg(not(windows))]
fn same_path(a: &Path, b: &Path) -> bool {
    a == b
}

#[cfg(all(test, windows))]
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
