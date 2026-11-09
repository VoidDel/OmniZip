//! Reads and writes at a position of a file, from several threads at once (no shared cursor).

use std::{fs::File, io, path::Path};

#[cfg(windows)]
pub fn read_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(f, buf, offset)
}

#[cfg(unix)]
pub fn read_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(f, buf, offset)
}

#[cfg(windows)]
pub fn write_at(f: &File, buf: &[u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_write(f, buf, offset)
}

#[cfg(unix)]
pub fn write_at(f: &File, buf: &[u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::write_at(f, buf, offset)
}

/// Fills `buf` from `offset`; fewer bytes only at the end of the file.
pub fn read_full_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match read_at(f, &mut buf[n..], offset + n as u64)? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Writes all of `buf` at `offset`.
pub fn write_all_at(f: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    let mut n = 0;
    while n < buf.len() {
        match write_at(f, &buf[n..], offset + n as u64)? {
            0 => return Err(io::ErrorKind::WriteZero.into()),
            k => n += k,
        }
    }
    Ok(())
}

/// Bytes this user may still write on the disk holding `dir`; `None` when unknown.
#[cfg(windows)]
pub fn free_space(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    // SAFETY: a NUL-terminated path; the outputs not wanted may be null.
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

/// Bytes this user may still write on the disk holding `dir`; `None` when unknown.
#[cfg(unix)]
pub fn free_space(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: all-zero is a valid statvfs; `path` is NUL-terminated.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut st) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)] // the field types differ between platforms
    Some(st.f_bavail as u64 * st.f_frsize as u64)
}
