//! The backend and the lifetime of a mounted volume.
//!
//! A volume is mounted on an empty folder: the one asked for (created when missing, removed
//! again after unmounting only then), or a new folder named after the label below
//! [`mount_root`]. Unmounting detaches the volume at once even when files on it are open
//! (`fusermount3 -u -z`); should the kernel still keep the connection for them after a few
//! seconds, it is aborted, and those files fail from then on, as on Windows. A volume
//! unmounted from outside (`fusermount3 -u`, a file manager's "Unmount") ends its session,
//! which [`Mount::is_alive`] reports.

use std::{
    io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use fuser::{BackgroundSession, Config, MountOption};
use omnizip_vfs::{
    BackendCaps, BackendHealth, BackendKind, Mount, MountBackend, MountError, MountOptions,
    NameRules, VirtualFs, names::display_name,
};

use crate::fs::Volume;

/// Held while a mount point is chosen and mounted on, so that two mounts at once (the daemon
/// serves clients in parallel) do not take the same new folder.
static MOUNTING: Mutex<()> = Mutex::new(());

/// The FUSE3 backend.
#[derive(Debug, Default, Clone, Copy)]
pub struct Fuse;

/// Where volumes without a mount point of their own go: `$XDG_RUNTIME_DIR/omnizip/mnt`, which
/// only the user may enter, or a folder of the user's in the temporary directory.
pub fn mount_root() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d).join("omnizip").join("mnt"),
        // SAFETY: no arguments, cannot fail.
        None => std::env::temp_dir()
            .join(format!("omnizip-{}", unsafe { libc::getuid() }))
            .join("mnt"),
    }
}

/// Unmounts the volumes below [`mount_root`] whose daemon is gone (their folders answer "not
/// connected"), and removes their folders. Live volumes, whoever hosts them, stay.
pub fn clear_stale() {
    let Ok(entries) = std::fs::read_dir(mount_root()) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if std::fs::symlink_metadata(&p).is_err_and(|e| e.raw_os_error() == Some(libc::ENOTCONN)) {
            tracing::info!(point = %p.display(), "unmounting a stale volume");
            fusermount_detach(&p);
            let _ = std::fs::remove_dir(&p);
        }
    }
}

fn fusermount_detach(p: &Path) {
    let _ = Command::new("fusermount3")
        .args(["-u", "-q", "-z", "--"])
        .arg(p)
        .output();
}

impl MountBackend for Fuse {
    fn kind(&self) -> BackendKind {
        BackendKind::Fuse
    }

    fn capabilities(&self) -> BackendCaps {
        BackendCaps::DIR_MOUNTPOINT
            | BackendCaps::WRITABLE
            | BackendCaps::SYMLINKS
            | BackendCaps::CASE_SENSITIVE
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
                probe.detail.unwrap_or_else(|| "FUSE".into()),
            ));
        }
        let stats = fs.statfs().map_err(|e| MountError::System(e.to_string()))?;
        let label = opts.label.clone().unwrap_or(stats.label);
        let writable = fs.writable();
        let _mounting = MOUNTING.lock().unwrap_or_else(|e| e.into_inner());
        let (point, created) = match &opts.mount_point {
            Some(p) => prepare_dir(p)?,
            None => (new_dir(&label)?, true),
        };
        // From here on, dropping `mount` undoes whatever was done.
        let mut mount = FuseMount {
            session: Mutex::new(None),
            point,
            created,
            conn: None,
        };
        let mut options = vec![
            // `mount` and `df` show the label as the source; commas would split the options.
            MountOption::FSName(label.replace([',', '\\'], "_")),
            MountOption::Subtype("omnizip".into()),
            MountOption::DefaultPermissions,
            MountOption::NoSuid,
            MountOption::NoDev,
            MountOption::NoAtime,
        ];
        if !writable {
            options.push(MountOption::RO);
        }
        let mut cfg = Config::default();
        cfg.mount_options = options;
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
        cfg.n_threads = Some(threads);
        cfg.clone_fd = threads > 1;
        let session =
            fuser::spawn_mount(Volume::new(fs), &mount.point, &cfg).map_err(|e| {
                match e.kind() {
                    io::ErrorKind::NotFound => MountError::NotInstalled("fusermount3".into()),
                    _ => MountError::System(format!("{}: {e}", mount.point.display())),
                }
            })?;
        *mount.session.get_mut().unwrap_or_else(|e| e.into_inner()) = Some(session);
        // The connection's number, for aborting it (`/sys/fs/fuse/connections/<n>`).
        mount.conn = std::fs::metadata(&mount.point)
            .ok()
            .map(|m| libc::minor(m.dev()) as u64);
        tracing::info!(point = %mount.point.display(), writable, "mounted");
        Ok(Box::new(mount))
    }
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
    if let Some(top) = root.parent() {
        let _ = std::fs::set_permissions(top, std::fs::Permissions::from_mode(0o700));
    }
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

/// Whether something is mounted on the folder `p`: it lies on another device than its parent.
fn is_mount_point(p: &Path, m: &std::fs::Metadata) -> bool {
    let parent = p.parent().unwrap_or(Path::new("/"));
    std::fs::metadata(parent).is_ok_and(|pm| pm.dev() != m.dev())
}

/// A mounted volume; dropping it unmounts.
struct FuseMount {
    session: Mutex<Option<BackgroundSession>>,
    point: PathBuf,
    /// The folder was made for the mount, and goes with it.
    created: bool,
    conn: Option<u64>,
}

impl Mount for FuseMount {
    fn mount_point(&self) -> &Path {
        &self.point
    }

    fn is_alive(&self) -> bool {
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|s| !s.guard.is_finished())
    }
}

impl FuseMount {
    /// Detaches the volume and waits for its session to end, aborting the connection when files
    /// still open on it keep the session going.
    fn unmount(&mut self) {
        let Some(session) = self
            .session
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(session.umount_and_join());
        });
        let mut result = rx.recv_timeout(Duration::from_secs(3));
        if result.is_err()
            && let Some(conn) = self.conn
        {
            tracing::warn!(point = %self.point.display(), "files still open: aborting the volume");
            let _ = std::fs::write(format!("/sys/fs/fuse/connections/{conn}/abort"), "1");
            result = rx.recv_timeout(Duration::from_secs(5));
        }
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(point = %self.point.display(), "unmounting: {e}"),
            Err(_) => tracing::warn!(point = %self.point.display(), "the volume did not stop"),
        }
    }
}

impl Drop for FuseMount {
    fn drop(&mut self) {
        self.unmount();
        if self.created {
            let _ = std::fs::remove_dir(&self.point);
        }
        tracing::info!(point = %self.point.display(), "unmounted");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_points_are_empty_folders() {
        let d = tempfile::tempdir().unwrap();
        let fresh = d.path().join("a").join("b");
        assert_eq!(prepare_dir(&fresh).unwrap(), (fresh.clone(), true));
        assert_eq!(prepare_dir(&fresh).unwrap(), (fresh.clone(), false));
        std::fs::write(fresh.join("x"), "x").unwrap();
        assert_eq!(prepare_dir(&fresh), Err(MountError::InUse(fresh.clone())));
        assert_eq!(
            prepare_dir(&fresh.join("x")),
            Err(MountError::InUse(fresh.join("x")))
        );
    }

    #[test]
    fn default_folders_count_up() {
        let d = tempfile::tempdir().unwrap();
        // SAFETY: the only test of this binary that reads the variable.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", d.path()) };
        let a = new_dir("photos").unwrap();
        assert_eq!(a, d.path().join("omnizip/mnt/photos"));
        // Empty and not mounted on: taken again.
        assert_eq!(new_dir("photos").unwrap(), a);
        std::fs::write(a.join("x"), "x").unwrap();
        assert_eq!(
            new_dir("photos").unwrap(),
            d.path().join("omnizip/mnt/photos (2)")
        );
        assert_eq!(new_dir("").unwrap(), d.path().join("omnizip/mnt/archive"));
    }
}
