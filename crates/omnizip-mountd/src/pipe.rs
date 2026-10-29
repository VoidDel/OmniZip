//! Named pipes between OmniZip and its daemons, and who may use them.
//!
//! Every pipe is open to the current user (and SYSTEM) only. The elevated daemon's pipe also
//! carries a medium integrity label, so that the app, which is not elevated, may write to it.
//! A client checks who owns the pipe it reached (the user, or the Administrators group for an
//! elevated server) before sending anything: another user cannot stand in for a daemon by
//! creating its pipe first.

use std::{
    ffi::{OsStr, c_void},
    fs::File,
    io,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    },
    path::Path,
    ptr,
    sync::OnceLock,
};

use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_CANCELLED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SE_KERNEL_OBJECT,
        },
        GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, RevertToSelf,
        SECURITY_ATTRIBUTES, TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenUser,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
        SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
    },
    System::{
        Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize},
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId,
            ImpersonateNamedPipeClient, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
        },
        Threading::{
            CreateProcessW, GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken,
            PROCESS_INFORMATION, STARTUPINFOW,
        },
    },
    UI::{
        Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
        WindowsAndMessaging::SW_HIDE,
    },
};

use crate::proto::Level;

const ADMINISTRATORS: &str = "S-1-5-32-544";

fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain([0]).collect()
}

/// The SID of the user this process runs as (the same for its elevated twin), as a string.
pub fn user_sid() -> io::Result<&'static str> {
    static SID: OnceLock<String> = OnceLock::new();
    if let Some(s) = SID.get() {
        return Ok(s);
    }
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: the pseudo-handle of this process; the token is closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);
    let mut buf = vec![0u8; 256];
    let mut len = 0u32;
    // SAFETY: a buffer of the length given.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            &mut len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetTokenInformation filled a TOKEN_USER.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let s = sid_string(sid)?;
    Ok(SID.get_or_init(|| s))
}

fn sid_string(sid: PSID) -> io::Result<String> {
    let mut p: *mut u16 = ptr::null_mut();
    // SAFETY: a valid SID; the string is freed with LocalFree.
    if unsafe { ConvertSidToStringSidW(sid, &mut p) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a NUL-terminated string allocated by the call above.
    let s = unsafe {
        let mut n = 0;
        while *p.add(n) != 0 {
            n += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
        LocalFree(p.cast());
        s
    };
    Ok(s)
}

/// `\\.\pipe\omnizip-mountd-<user SID>`, `-admin` for the elevated daemon.
/// `OMNIZIP_MOUNTD_INSTANCE` adds a suffix (tests run their own daemons).
pub fn daemon_pipe(level: Level) -> io::Result<String> {
    let suffix = std::env::var("OMNIZIP_MOUNTD_INSTANCE")
        .map(|s| format!("-{s}"))
        .unwrap_or_default();
    let admin = if level == Level::Admin { "-admin" } else { "" };
    Ok(format!(
        r"\\.\pipe\omnizip-mountd-{}{admin}{suffix}",
        user_sid()?
    ))
}

/// Who may connect to a pipe this process creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// The current user, at its own integrity level or above.
    User,
    /// The current user from a lower integrity level too (the elevated daemon's pipe).
    UserFromBelow,
}

/// Creates one instance of a pipe server; `first` fails when the pipe exists already (another
/// daemon owns it).
pub fn server(name: &str, first: bool, access: Access) -> io::Result<OwnedHandle> {
    let mut sddl = format!("D:P(A;;GA;;;{})(A;;GA;;;SY)", user_sid()?);
    if access == Access::UserFromBelow {
        sddl.push_str("S:(ML;;NW;;;ME)");
    }
    let sddl = wide(&sddl);
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: valid SDDL; the descriptor is freed below.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut sd,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let mode = PIPE_ACCESS_DUPLEX
        | if first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
    let name_w = wide(name);
    // SAFETY: a NUL-terminated name and valid security attributes.
    let h = unsafe {
        CreateNamedPipeW(
            name_w.as_ptr(),
            mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            &sa,
        )
    };
    let err = io::Error::last_os_error();
    // SAFETY: allocated by the conversion above.
    unsafe { LocalFree(sd) };
    if h == INVALID_HANDLE_VALUE {
        return Err(err);
    }
    // SAFETY: a handle this process owns from now on.
    Ok(unsafe { OwnedHandle::from_raw_handle(h as RawHandle) })
}

/// Waits for a client on a server instance.
pub fn accept(server: &OwnedHandle) -> io::Result<()> {
    // SAFETY: a pipe server handle; synchronous.
    if unsafe { ConnectNamedPipe(server.as_raw_handle() as HANDLE, ptr::null_mut()) } != 0 {
        return Ok(());
    }
    // A client that connected between creation and this call.
    match unsafe { GetLastError() } {
        ERROR_PIPE_CONNECTED => Ok(()),
        e => Err(io::Error::from_raw_os_error(e as i32)),
    }
}

/// Connects to a pipe and checks who owns it (the user, or Administrators for an elevated
/// server). The server may identify, not impersonate, this process.
pub fn connect(name: &str) -> io::Result<File> {
    let name_w = wide(name);
    let file = loop {
        // SAFETY: a NUL-terminated name; the handle is owned below.
        let h = unsafe {
            CreateFileW(
                name_w.as_ptr(),
                0x8000_0000 | 0x4000_0000, // GENERIC_READ | GENERIC_WRITE
                0,
                ptr::null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                ptr::null_mut(),
            )
        };
        if h != INVALID_HANDLE_VALUE {
            // SAFETY: a handle this process owns from now on.
            break unsafe { File::from_raw_handle(h as RawHandle) };
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) {
            return Err(err);
        }
        // Every instance is busy: wait for one.
        // SAFETY: a NUL-terminated name.
        if unsafe { WaitNamedPipeW(name_w.as_ptr(), 5_000) } == 0 {
            return Err(io::Error::last_os_error());
        }
    };
    let owner = owner_of(&file)?;
    if owner != user_sid()? && owner != ADMINISTRATORS {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{name} belongs to {owner}"),
        ));
    }
    Ok(file)
}

fn owner_of(h: &impl AsRawHandle) -> io::Result<String> {
    let mut owner: PSID = ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: a handle opened with read access (READ_CONTROL); `sd` is freed below and
    // `owner` points into it.
    let e = unsafe {
        GetSecurityInfo(
            h.as_raw_handle() as HANDLE,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut sd,
        )
    };
    if e != 0 {
        return Err(io::Error::from_raw_os_error(e as i32));
    }
    let s = sid_string(owner);
    // SAFETY: allocated by GetSecurityInfo.
    unsafe { LocalFree(sd) };
    s
}

/// The process at the other end of a server instance.
pub fn client_process(pipe: &File) -> io::Result<u32> {
    let mut pid = 0;
    // SAFETY: a connected pipe server handle.
    if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle() as HANDLE, &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pid)
}

/// Whether the client of a server instance runs elevated (identified through its token).
pub fn client_elevated(pipe: &File) -> bool {
    // SAFETY: a connected pipe server handle; the impersonation ends below.
    if unsafe { ImpersonateNamedPipeClient(pipe.as_raw_handle() as HANDLE) } == 0 {
        return false;
    }
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: this thread's impersonation token, opened as the process.
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } != 0;
    // SAFETY: undoes the impersonation above.
    unsafe { RevertToSelf() };
    opened && token_elevated(Handle(token).0)
}

/// Whether this process runs elevated.
pub fn elevated() -> bool {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: the pseudo-handle of this process.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    token_elevated(Handle(token).0)
}

fn token_elevated(token: HANDLE) -> bool {
    let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len = 0;
    // SAFETY: a buffer the size of TOKEN_ELEVATION.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut e).cast::<c_void>(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    };
    ok != 0 && e.TokenIsElevated != 0
}

/// Starts `exe` on its own: no console, a process group of its own, out of the caller's job
/// when allowed (a job closing would otherwise end it), in its own directory, and without
/// inheriting the caller's handles (pipes of whoever started the caller would stay open).
pub fn spawn_detached(exe: &Path) -> io::Result<()> {
    const DETACHED_PROCESS: u32 = 0x8;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
    const CREATE_UNICODE_ENVIRONMENT: u32 = 0x400;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let app = wide(exe);
    let dir = exe.parent().map(wide);
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT;
    let mut last = io::Error::other("CreateProcessW");
    for flags in [base | CREATE_BREAKAWAY_FROM_JOB, base] {
        // The command line must be writable.
        let mut cmdline = wide(format!("\"{}\"", exe.display()));
        // SAFETY: all-zero is valid for both (sizes and null pointers).
        let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
        si.cb = size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: NUL-terminated strings that outlive the call; no handles are inherited.
        let ok = unsafe {
            CreateProcessW(
                app.as_ptr(),
                cmdline.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                flags,
                ptr::null(),
                dir.as_ref().map_or(ptr::null(), |d| d.as_ptr()),
                &si,
                &mut pi,
            )
        };
        if ok != 0 {
            // SAFETY: handles CreateProcessW returned.
            unsafe {
                CloseHandle(pi.hThread);
                CloseHandle(pi.hProcess);
            }
            return Ok(());
        }
        last = io::Error::last_os_error();
    }
    Err(last)
}

/// Starts `exe args` elevated, which shows the UAC prompt; `Cancelled` when the user declines.
pub fn launch_elevated(exe: &Path, args: &str) -> io::Result<OwnedHandle> {
    let verb = wide("runas");
    let file = wide(exe);
    let params = wide(args);
    let dir = exe.parent().map(wide);
    // SAFETY: all-zero is a valid SHELLEXECUTEINFOW (plain fields and null pointers).
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.lpDirectory = dir.as_ref().map_or(ptr::null(), |d| d.as_ptr());
    info.nShow = SW_HIDE;
    // ShellExecuteEx wants COM on the calling thread.
    // SAFETY: balanced by CoUninitialize below.
    let com = unsafe {
        CoInitializeEx(
            ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    } >= 0;
    // SAFETY: a filled-in SHELLEXECUTEINFOW whose strings outlive the call.
    let ok = unsafe { ShellExecuteExW(&mut info) } != 0;
    let err = io::Error::last_os_error();
    if com {
        // SAFETY: initialized above.
        unsafe { CoUninitialize() };
    }
    if !ok {
        return Err(if err.raw_os_error() == Some(ERROR_CANCELLED as i32) {
            io::Error::new(io::ErrorKind::Interrupted, "the elevation was declined")
        } else {
            err
        });
    }
    if info.hProcess.is_null() {
        return Err(io::Error::other("no process"));
    }
    // SAFETY: the process handle ShellExecuteEx returned (SEE_MASK_NOCLOSEPROCESS).
    Ok(unsafe { OwnedHandle::from_raw_handle(info.hProcess as RawHandle) })
}

/// A handle closed when dropped.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a handle this process opened.
            unsafe { CloseHandle(self.0) };
        }
    }
}
