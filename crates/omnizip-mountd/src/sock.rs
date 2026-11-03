//! Unix sockets between OmniZip and its daemon on Linux, and who may use them.
//!
//! The socket lives in `$XDG_RUNTIME_DIR/omnizip`, which only the user may enter. Both ends
//! also check who is at the other one (`SO_PEERCRED`): the daemon serves only processes of its
//! own user, and a client talks only to a daemon of its own user. One daemon per user: whoever
//! holds the lock next to the socket. There is no elevated daemon on Linux.

use std::{
    fs::File,
    io,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crate::proto::Level;

/// A connection to or from a daemon.
pub type Conn = UnixStream;

/// The user's runtime directory for OmniZip (`$XDG_RUNTIME_DIR/omnizip`), or a folder of the
/// user's in the temporary directory without one.
fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d).join("omnizip"),
        None => std::env::temp_dir().join(format!("omnizip-{}", uid())),
    }
}

fn uid() -> u32 {
    // SAFETY: no arguments, cannot fail.
    unsafe { libc::getuid() }
}

/// `mountd.sock` in the runtime directory. `OMNIZIP_MOUNTD_INSTANCE` adds a suffix (tests run
/// their own daemons). The elevated daemon exists on Windows only: `NotFound`.
pub fn daemon_socket(level: Level) -> io::Result<PathBuf> {
    if level == Level::Admin {
        return Err(io::ErrorKind::NotFound.into());
    }
    let suffix = std::env::var("OMNIZIP_MOUNTD_INSTANCE")
        .map(|s| format!("-{s}"))
        .unwrap_or_default();
    Ok(runtime_dir().join(format!("mountd{suffix}.sock")))
}

/// The user id of the process at the other end.
fn peer_uid(s: &UnixStream) -> io::Result<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: a connected socket and a buffer of the length given.
    let r = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
}

fn same_user(s: &UnixStream) -> io::Result<()> {
    let peer = peer_uid(s)?;
    if peer != uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the other end belongs to user {peer}"),
        ));
    }
    Ok(())
}

/// Connects to a daemon's socket. A socket nobody listens on (a daemon that did not exit
/// cleanly) is `NotFound`, as is a missing one.
pub fn connect(path: &Path) -> io::Result<Conn> {
    let s = UnixStream::connect(path).map_err(|e| match e.kind() {
        io::ErrorKind::ConnectionRefused => io::ErrorKind::NotFound.into(),
        _ => e,
    })?;
    same_user(&s)?;
    Ok(s)
}

/// The listening socket of the daemon, and the lock that makes it the only one.
pub struct Listener {
    socket: UnixListener,
    _lock: File,
}

/// Listens on `path`; `None` when another daemon holds it.
pub fn listen(path: &Path) -> io::Result<Option<Listener>> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path.with_extension("lock"))?;
    // SAFETY: an open file; the lock goes with it.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let e = io::Error::last_os_error();
        return match e.raw_os_error() {
            Some(libc::EWOULDBLOCK) => Ok(None),
            _ => Err(e),
        };
    }
    // Left by a daemon that did not exit cleanly.
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let socket = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(Some(Listener {
        socket,
        _lock: lock,
    }))
}

impl Listener {
    /// The next client of the daemon's own user (others are turned away).
    pub fn accept(&self) -> io::Result<Conn> {
        loop {
            let (s, _) = self.socket.accept()?;
            match same_user(&s) {
                Ok(()) => return Ok(s),
                Err(e) => tracing::warn!("omnizip-mountd: {e}"),
            }
        }
    }
}

/// Starts `exe` on its own: in a session of its own (a terminal closing does not end it), in
/// its own directory, without standard streams. A thread waits for it, so that it does not
/// linger as a zombie once it exits while OmniZip still runs.
pub fn spawn_detached(exe: &Path) -> io::Result<()> {
    let mut cmd = Command::new(exe);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}
