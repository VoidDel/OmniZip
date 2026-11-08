//! Talking to the daemons from OmniZip (or any client of the same user).

#[cfg(windows)]
use std::{fs::File, io, os::windows::io::AsRawHandle, sync::mpsc};
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    time::{Duration, Instant},
};

use omnizip_core::{Error, ErrorCode};
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{HANDLE, WAIT_TIMEOUT},
    System::Threading::{GetProcessId, WaitForSingleObject},
};

#[cfg(windows)]
use crate::pipe::{self, Access, Conn};
use crate::proto::{Level, MountSpec, MountStatus, Request, Response};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::sock::{self, Conn};

/// Sends one request on a connection and reads its response; the daemon's error as `Err`.
pub fn exchange(conn: &Conn, req: &Request) -> Result<Response, Error> {
    let mut line = serde_json::to_string(req).map_err(Error::internal)?;
    line.push('\n');
    (&*conn).write_all(line.as_bytes())?;
    let mut reply = String::new();
    BufReader::new(conn).read_line(&mut reply)?;
    if reply.is_empty() {
        return Err(Error::internal("omnizip-mountd closed the connection"));
    }
    serde_json::from_str::<Response>(&reply)
        .map_err(Error::internal)?
        .into_result()
}

/// A connection to a daemon. Fails with `NotFound` when that daemon does not run.
fn open(level: Level) -> Result<Conn, Error> {
    #[cfg(windows)]
    let conn = {
        let server = match level {
            Level::User => pipe::Server::User,
            Level::Admin => pipe::Server::Elevated,
        };
        pipe::connect(&pipe::daemon_pipe(level)?, server)?
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let conn = sock::connect(&sock::daemon_socket(level)?)?;
    Ok(conn)
}

/// One request to a daemon. Fails with `NotFound` when that daemon does not run.
pub fn call(level: Level, req: &Request) -> Result<Response, Error> {
    exchange(&open(level)?, req)
}

/// Whether a daemon runs (answers on its pipe or socket).
pub fn running(level: Level) -> bool {
    open(level).is_ok()
}

/// Starts the user's daemon (`exe`) unless it runs, and waits until it answers. It runs on its
/// own, outside OmniZip's process tree, so that mounts outlive OmniZip.
pub fn ensure_user_daemon(exe: &Path) -> Result<(), Error> {
    if running(Level::User) {
        return Ok(());
    }
    #[cfg(windows)]
    pipe::spawn_detached(exe).map_err(|e| Error::io(e, exe))?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    sock::spawn_detached(exe).map_err(|e| Error::io(e, exe))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if running(Level::User) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(Error::internal("omnizip-mountd did not start"))
}

/// Calls `on_change` for every change of a daemon's mounts, until the daemon exits (or does
/// not run).
pub fn watch(level: Level, mut on_change: impl FnMut()) -> Result<(), Error> {
    let conn = open(level)?;
    let mut line = serde_json::to_string(&Request::Watch).map_err(Error::internal)?;
    line.push('\n');
    (&conn).write_all(line.as_bytes())?;
    for l in BufReader::new(&conn).lines() {
        let l = l?;
        if matches!(serde_json::from_str(&l), Ok(Response::Changed)) {
            on_change();
        }
    }
    Ok(())
}

/// Mounts through the elevated daemon, which exists on Windows only.
#[cfg(not(windows))]
pub fn mount_elevated(_exe: &Path, _spec: &MountSpec) -> Result<MountStatus, Error> {
    Err(Error::new(ErrorCode::UnsupportedOperation).with("operation", "mount"))
}

/// Mounts through the elevated daemon. Asks for administrator rights (the UAC prompt) to start
/// `exe --elevated --handoff <pipe>`, which hosts the mount itself or hands it to the elevated
/// daemon running already. The spec, password included, travels over a pipe that only the
/// started process can be on the other end of.
#[cfg(windows)]
pub fn mount_elevated(exe: &Path, spec: &MountSpec) -> Result<MountStatus, Error> {
    let name = pipe::handoff_pipe(std::process::id(), random());
    let server = pipe::server(&name, true, Access::Handoff)?;
    let process = pipe::launch_elevated(exe, &format!("--elevated --handoff \"{name}\"")).map_err(
        |e| match e.kind() {
            io::ErrorKind::Interrupted => Error::cancelled(),
            _ => Error::io(e, exe),
        },
    )?;
    // SAFETY: a process handle this function owns.
    let pid = unsafe { GetProcessId(process.as_raw_handle() as HANDLE) };

    // Wait for the started process to connect; if it exits first, connect ourselves to end
    // the wait (and then refuse that connection).
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = pipe::accept(&server);
        let _ = tx.send((server, r));
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    let (server, accepted) = loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(done) => break done,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Error::internal("pipe wait failed"));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // SAFETY: a process handle this function owns.
                let alive = unsafe { WaitForSingleObject(process.as_raw_handle() as HANDLE, 0) }
                    == WAIT_TIMEOUT;
                if !alive || Instant::now() > deadline {
                    let _unblock = pipe::connect(&name, pipe::Server::Process(std::process::id()));
                    let _ = rx.recv();
                    return Err(Error::internal("omnizip-mountd --elevated did not connect"));
                }
            }
        }
    };
    accepted?;
    let conn = File::from(server);
    if pipe::client_process(&conn)? != pid {
        return Err(Error::new(ErrorCode::PermissionDenied).with("path", name));
    }
    match exchange(&conn, &Request::Mount { spec: spec.clone() })? {
        Response::Mounted { mount } => Ok(mount),
        r => Err(Error::internal(format!("unexpected answer {r:?}"))),
    }
}

/// Bits that differ between calls (not cryptographic: the pipe is checked anyway).
#[cfg(windows)]
fn random() -> u64 {
    use std::hash::{BuildHasher, RandomState};
    RandomState::new().hash_one(Instant::now())
}
