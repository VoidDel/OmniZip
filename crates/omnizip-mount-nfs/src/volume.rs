//! The backend and the lifetime of a mounted volume.
//!
//! A volume is mounted on an empty folder: the one asked for (created when missing, and removed
//! after unmounting only then), or a new folder named after the label below [`mount_root`]. Its
//! server runs on threads of its own until the volume is unmounted. The mount is soft, so that
//! calls fail instead of hanging should the server be gone, and the system unmounts it by itself
//! once the server has not answered for a while (`deadtimeout`). A volume ejected in the Finder
//! or unmounted with `umount` is no longer mounted on its folder, which [`Mount::is_alive`]
//! reports.

use std::{
    ffi::{CStr, CString},
    io,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use nfsserve::tcp::{NFSTcp, NFSTcpListener};
use omnizip_vfs::{
    BackendCaps, BackendHealth, BackendKind, Mount, MountBackend, MountError, MountOptions,
    NameRules, VirtualFs, names::display_name,
};
use tokio::runtime::Runtime;

use crate::fs::NfsVolume;

const MOUNT_NFS: &str = "/sbin/mount_nfs";
const UMOUNT: &str = "/sbin/umount";

/// Held while a mount point is chosen and mounted on, so that two mounts at once (the daemon
/// serves clients in parallel) do not take the same new folder.
static MOUNTING: Mutex<()> = Mutex::new(());

/// The NFS loopback backend.
#[derive(Debug, Default, Clone, Copy)]
pub struct Nfs;

/// Where volumes without a mount point of their own go:
/// `~/Library/Application Support/OmniZip/Volumes` (not in Caches, which cleaning tools empty:
/// that would delete through a writable volume).
pub fn mount_root() -> PathBuf {
    match omnizip_core::appdirs::data_dir() {
        Some(d) => d.join("Volumes"),
        // SAFETY: no arguments, cannot fail.
        None => std::env::temp_dir()
            .join(format!("omnizip-{}", unsafe { libc::getuid() }))
            .join("Volumes"),
    }
}

/// Unmounts the volumes below [`mount_root`] left by a daemon that did not exit cleanly (their
/// servers lived in it), and removes their folders. Called by a daemon starting, which is the
/// only one of its user.
pub fn clear_stale() {
    let Ok(entries) = std::fs::read_dir(mount_root()) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if mounted_from(&p).is_some_and(|from| from.starts_with("127.0.0.1:/")) {
            tracing::info!(point = %p.display(), "unmounting a stale volume");
            let _ = Command::new(UMOUNT).arg("-f").arg(&p).output();
        }
        let _ = std::fs::remove_dir(&p);
    }
}

/// What is mounted on `p` (`127.0.0.1:/…` for a volume of this backend), when an NFS volume is.
fn mounted_from(p: &Path) -> Option<String> {
    let path = CString::new(p.as_os_str().as_bytes()).ok()?;
    // SAFETY: all-zero is a valid statfs (plain integers and arrays).
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated path and a structure of the right size.
    if unsafe { libc::statfs(path.as_ptr(), &mut st) } != 0 {
        return None;
    }
    // SAFETY: the system fills both with NUL-terminated strings.
    let (kind, from, on) = unsafe {
        (
            CStr::from_ptr(st.f_fstypename.as_ptr()),
            CStr::from_ptr(st.f_mntfromname.as_ptr()),
            CStr::from_ptr(st.f_mntonname.as_ptr()),
        )
    };
    // statfs of a folder that is not a mount point describes the volume holding it.
    let here = std::fs::canonicalize(p).ok()?;
    (kind.to_bytes() == b"nfs" && on.to_bytes() == here.as_os_str().as_bytes())
        .then(|| from.to_string_lossy().into_owned())
}

impl MountBackend for Nfs {
    fn kind(&self) -> BackendKind {
        BackendKind::NfsLoopback
    }

    fn capabilities(&self) -> BackendCaps {
        BackendCaps::DIR_MOUNTPOINT | BackendCaps::WRITABLE | BackendCaps::SYMLINKS
    }

    fn health(&self) -> BackendHealth {
        omnizip_vfs::probe(true).health
    }

    fn mount(
        &self,
        fs: Arc<dyn VirtualFs>,
        opts: &MountOptions,
    ) -> Result<Box<dyn Mount>, MountError> {
        let probe = omnizip_vfs::probe(true);
        if probe.health != BackendHealth::Ready {
            return Err(MountError::NotInstalled(
                probe.detail.unwrap_or_else(|| MOUNT_NFS.into()),
            ));
        }
        let system = |e: &dyn std::fmt::Display| MountError::System(e.to_string());
        let stats = fs.statfs().map_err(|e| system(&e))?;
        let label = opts.label.clone().unwrap_or(stats.label);
        let writable = fs.writable();
        let volume = NfsVolume::new(fs, u64::from(stats.serial));

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(16)
            .thread_name("omnizip-nfs")
            .enable_all()
            .build()
            .map_err(|e| system(&e))?;
        let secret = format!("{:016x}{:016x}", random(), random());
        let listener = runtime
            .block_on(async {
                let mut l = NFSTcpListener::bind("127.0.0.1:0", volume.clone()).await?;
                l.with_export_name(&secret);
                io::Result::Ok(l)
            })
            .map_err(|e| system(&e))?;
        let port = listener.get_listen_port();
        runtime.spawn(async move {
            if let Err(e) = listener.handle_forever().await {
                tracing::warn!("mount: NFS server stopped: {e}");
            }
        });

        let _mounting = MOUNTING.lock().unwrap_or_else(|e| e.into_inner());
        let (point, created) = match &opts.mount_point {
            Some(p) => prepare_dir(p)?,
            None => (new_dir(&label)?, true),
        };
        // From here on, dropping `mount` undoes whatever was done.
        let mut mount = NfsMount {
            runtime: Some(runtime),
            volume,
            point,
            created,
            source: format!("127.0.0.1:/{secret}"),
            mounted: false,
        };
        let mut options = format!(
            "vers=3,tcp,port={port},mountport={port},soft,intr,timeo=10,retrans=3,\
             deadtimeout=20,locallocks,nfc,rsize={io},wsize={io},actimeo=1,nosuid,nodev",
            io = 1 << 20,
        );
        if !writable {
            options.push_str(",rdonly");
        }
        let out = Command::new(MOUNT_NFS)
            .args(["-o", &options, &mount.source])
            .arg(&mount.point)
            .output()
            .map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => MountError::NotInstalled(MOUNT_NFS.into()),
                _ => system(&e),
            })?;
        if !out.status.success() {
            return Err(MountError::System(format!(
                "{}: {}",
                mount.point.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        mount.mounted = true;
        tracing::info!(point = %mount.point.display(), writable, "mounted");
        Ok(Box::new(mount))
    }
}

/// Bits that differ between calls (not cryptographic on their own: the export path they make
/// guards the one MOUNT, while handles carry a keyed tag).
fn random() -> u64 {
    use std::hash::{BuildHasher, RandomState};
    RandomState::new().hash_one(std::time::SystemTime::now())
}

/// A folder asked for as a mount point: an empty one, or one that is created (`true`).
fn prepare_dir(p: &Path) -> Result<(PathBuf, bool), MountError> {
    let in_use = || MountError::InUse(p.to_owned());
    match std::fs::symlink_metadata(p) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir_all(p).map_err(|e| MountError::System(e.to_string()))?;
            Ok((p.to_owned(), true))
        }
        Err(_) => Err(in_use()),
        Ok(m) if !m.is_dir() || is_mount_point(p, &m) => Err(in_use()),
        Ok(_) => {
            let empty = std::fs::read_dir(p)
                .map_err(|e| MountError::System(e.to_string()))?
                .next()
                .is_none();
            if empty {
                Ok((p.to_owned(), false))
            } else {
                Err(in_use())
            }
        }
    }
}

/// A new folder below [`mount_root`] named after `label`, `label (2)` and so on when taken.
fn new_dir(label: &str) -> Result<PathBuf, MountError> {
    let root = mount_root();
    std::fs::create_dir_all(&root).map_err(|e| MountError::System(e.to_string()))?;
    let label = label.trim();
    let name = display_name(
        if label.is_empty() { "archive" } else { label },
        NameRules::current(),
    );
    for n in 1..1000 {
        let p = if n == 1 {
            root.join(&name)
        } else {
            root.join(format!("{name} ({n})"))
        };
        match std::fs::create_dir(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                // An empty folder left behind is taken again.
                if let Ok(m) = std::fs::symlink_metadata(&p)
                    && m.is_dir()
                    && !is_mount_point(&p, &m)
                    && std::fs::read_dir(&p).is_ok_and(|mut d| d.next().is_none())
                {
                    return Ok(p);
                }
            }
            Err(e) => return Err(MountError::System(format!("{}: {e}", p.display()))),
        }
    }
    Err(MountError::InUse(root.join(name)))
}

/// Whether a folder is the root of a mounted volume (another device than its parent's).
fn is_mount_point(p: &Path, m: &std::fs::Metadata) -> bool {
    p.parent()
        .and_then(|parent| std::fs::metadata(parent).ok())
        .is_some_and(|pm| pm.dev() != m.dev())
}

struct NfsMount {
    runtime: Option<Runtime>,
    volume: NfsVolume,
    point: PathBuf,
    /// The folder was made for the volume, and goes with it.
    created: bool,
    /// What the volume is mounted from, `127.0.0.1:/<secret>`.
    source: String,
    mounted: bool,
}

impl Mount for NfsMount {
    fn mount_point(&self) -> &Path {
        &self.point
    }

    fn is_alive(&self) -> bool {
        mounted_from(&self.point).is_some_and(|from| from == self.source)
    }
}

impl Drop for NfsMount {
    fn drop(&mut self) {
        if self.mounted && self.is_alive() {
            // Files open on the volume keep a plain unmount from working: then it is forced,
            // and they fail from then on, as on the other platforms.
            let plain = Command::new(UMOUNT).arg(&self.point).output();
            if !plain.is_ok_and(|o| o.status.success()) {
                let _ = Command::new(UMOUNT).arg("-f").arg(&self.point).output();
            }
        }
        // The server answers the unmount, then stops.
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_timeout(Duration::from_secs(2));
        }
        self.volume.0.close_all();
        if self.created {
            let _ = std::fs::remove_dir(&self.point);
        }
        tracing::info!(point = %self.point.display(), "unmounted");
    }
}
