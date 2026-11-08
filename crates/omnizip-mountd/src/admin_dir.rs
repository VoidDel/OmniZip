//! The elevated daemon's own folder: spool files and the layers of writable mounts made as
//! administrator (`docs/12-mount.md` §4).
//!
//! It must not be in the user's profile (`%LOCALAPPDATA%`, or wherever `OMNIZIP_HOME` points):
//! the user's programs, which are not elevated, could replace a folder there with a link and
//! have the elevated daemon create, write or move files anywhere. It is
//! `%ProgramData%\OmniZip-mountd-admin\<user SID>` instead, found through the known folder (the
//! environment is the user's to set), and only SYSTEM and Administrators may change it. A folder
//! found there already is used only when it is a plain folder (not a link) that one of them
//! owns, and its permissions are reset.
//!
//! Kept changes are handed over to the user ([`hand_over`]): the app, which is not elevated,
//! saves them into the archive and removes them.

use std::{
    ffi::OsString,
    io,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    },
    path::{Path, PathBuf},
    ptr, slice,
};

use windows_sys::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, HANDLE, INVALID_HANDLE_VALUE, LocalFree},
    Security::{
        ACL,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SE_FILE_OBJECT, SetNamedSecurityInfoW, SetSecurityInfo,
        },
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
        UNPROTECTED_DACL_SECURITY_INFORMATION,
    },
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
        OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
    },
    System::Com::CoTaskMemFree,
    UI::Shell::{FOLDERID_ProgramData, SHGetKnownFolderPath},
};

/// SYSTEM and Administrators, inherited by everything inside.
const ADMIN_ONLY: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
const ADMINISTRATORS: &str = "S-1-5-32-544";
const SYSTEM: &str = "S-1-5-18";

/// `%ProgramData%\OmniZip-mountd-admin\<user SID>`, created or checked.
pub fn user_dir(user_sid: &str) -> io::Result<PathBuf> {
    let root = program_data()?.join("OmniZip-mountd-admin");
    private_dir(&root)?;
    let dir = root.join(user_sid);
    private_dir(&dir)?;
    Ok(dir)
}

/// Lets the user (and still SYSTEM and Administrators) read and remove `dir` and everything in
/// it: kept changes, which the app saves.
pub fn hand_over(dir: &Path, user_sid: &str) -> io::Result<()> {
    let sd = Descriptor::from_sddl(&format!("D:(A;OICI;FA;;;{user_sid})"))?;
    let mut path = wide(dir);
    // SAFETY: a NUL-terminated path and the DACL of a live descriptor. Inheritable entries
    // reach what is inside.
    let e = unsafe {
        SetNamedSecurityInfoW(
            path.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            sd.dacl()?,
            ptr::null(),
        )
    };
    if e != 0 {
        return Err(io::Error::from_raw_os_error(e as i32));
    }
    Ok(())
}

fn program_data() -> io::Result<PathBuf> {
    let mut p: *mut u16 = ptr::null_mut();
    // SAFETY: a known folder id; the string is freed with CoTaskMemFree below.
    let hr = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, 0, ptr::null_mut(), &mut p) };
    if hr < 0 {
        // SAFETY: null, or allocated by the call above.
        unsafe { CoTaskMemFree(p.cast()) };
        return Err(io::Error::from_raw_os_error(hr));
    }
    // SAFETY: a NUL-terminated string allocated by the call above, freed once copied.
    let s = unsafe {
        let mut n = 0;
        while *p.add(n) != 0 {
            n += 1;
        }
        let s = OsString::from_wide(slice::from_raw_parts(p, n));
        CoTaskMemFree(p.cast());
        s
    };
    Ok(PathBuf::from(s))
}

/// Creates `dir` for SYSTEM and Administrators only. When it exists, it must be a plain folder
/// owned by one of them; its permissions are then reset to those.
fn private_dir(dir: &Path) -> io::Result<()> {
    let sd = Descriptor::from_sddl(ADMIN_ONLY)?;
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let path = wide(dir);
    // SAFETY: a NUL-terminated path and a live descriptor.
    if unsafe { CreateDirectoryW(path.as_ptr(), &sa) } != 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
        return Err(err);
    }
    let refuse = |why: String| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{}: {why}", dir.display()),
        )
    };
    // The folder itself, not what a link there points to.
    // SAFETY: a NUL-terminated path; the handle is owned below.
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle this function owns from now on.
    let h = unsafe { OwnedHandle::from_raw_handle(h as RawHandle) };
    let raw = h.as_raw_handle() as HANDLE;
    // SAFETY: all-zero is a valid BY_HANDLE_FILE_INFORMATION (plain integers).
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: an open handle and a structure of the right size.
    if unsafe { GetFileInformationByHandle(raw, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let attrs = info.dwFileAttributes;
    if attrs & FILE_ATTRIBUTE_DIRECTORY == 0 || attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(refuse("not a plain folder".into()));
    }
    let owner = owner_of(raw)?;
    if owner != ADMINISTRATORS && owner != SYSTEM {
        return Err(refuse(format!("owned by {owner}")));
    }
    // SAFETY: an open handle with WRITE_DAC and the DACL of a live descriptor.
    let e = unsafe {
        SetSecurityInfo(
            raw,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            sd.dacl()?,
            ptr::null(),
        )
    };
    if e != 0 {
        return Err(io::Error::from_raw_os_error(e as i32));
    }
    Ok(())
}

fn owner_of(h: HANDLE) -> io::Result<String> {
    let mut owner: PSID = ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: a handle opened with READ_CONTROL; `sd` is freed below and `owner` points into
    // it.
    let e = unsafe {
        GetSecurityInfo(
            h,
            SE_FILE_OBJECT,
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
    let mut p: *mut u16 = ptr::null_mut();
    // SAFETY: the owner SID inside `sd`; the string is freed below.
    let ok = unsafe { ConvertSidToStringSidW(owner, &mut p) } != 0;
    let err = io::Error::last_os_error();
    let s = ok.then(|| {
        // SAFETY: a NUL-terminated string allocated by the call above, freed once copied.
        unsafe {
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(slice::from_raw_parts(p, n));
            LocalFree(p.cast());
            s
        }
    });
    // SAFETY: allocated by GetSecurityInfo.
    unsafe { LocalFree(sd) };
    s.ok_or(err)
}

/// A security descriptor made from SDDL, freed when dropped.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn from_sddl(sddl: &str) -> io::Result<Self> {
        let s: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: NUL-terminated SDDL; the descriptor is freed on drop.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                s.as_ptr(),
                1,
                &mut sd,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(sd))
    }

    /// Its DACL, which lives as long as the descriptor.
    fn dacl(&self) -> io::Result<*mut ACL> {
        let (mut present, mut defaulted) = (0, 0);
        let mut dacl: *mut ACL = ptr::null_mut();
        // SAFETY: a valid descriptor; the DACL points into it.
        if unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) }
            == 0
            || present == 0
        {
            return Err(io::Error::other("no DACL"));
        }
        Ok(dacl)
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

fn wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain([0]).collect()
}
