//! Reads and writes at a position of a file, from several threads at once (no shared cursor).

use std::{fs::File, io};

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
