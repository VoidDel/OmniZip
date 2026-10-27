//! Format detection by magic bytes, with the file name as a fallback (`docs/02-architecture.md` §4.2).
//!
//! Compound formats (`tar.gz` versus a plain `.gz`) need the codec to look inside the stream, so
//! the final decision is made by the engine through `Engine::refine`.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::{Error, ErrorCode, FormatId, Result};

/// Bytes read from the start of the file for sniffing (large enough for the ISO 9660 descriptor).
pub const HEAD_LEN: usize = 0x8800;
/// Bytes read from the end of the file (ZIP end-of-central-directory search window).
pub const TAIL_LEN: usize = 66 * 1024;

/// Result of format detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detection {
    /// The detected format.
    pub format: FormatId,
    /// Whether the decision came from content rather than the file name.
    pub by_magic: bool,
    /// The file name suggests a different format than the content.
    pub extension_mismatch: bool,
    /// The archive is embedded in an executable (self-extracting archive).
    pub sfx: bool,
}

/// Checks the content of `head` (start of file) for a known signature.
pub fn sniff(head: &[u8]) -> Option<FormatId> {
    let starts = |sig: &[u8]| head.starts_with(sig);
    if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") || starts(b"PK\x07\x08") {
        return Some(FormatId::Zip);
    }
    if starts(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
        return Some(FormatId::SevenZ);
    }
    if starts(b"Rar!\x1a\x07\x00") || starts(b"Rar!\x1a\x07\x01\x00") {
        return Some(FormatId::Rar);
    }
    if starts(&[0x1F, 0x8B]) {
        return Some(FormatId::Gz);
    }
    if starts(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        return Some(FormatId::Xz);
    }
    if starts(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return Some(FormatId::Zst);
    }
    if head.len() >= 4 && starts(b"BZh") && (b'1'..=b'9').contains(&head[3]) {
        return Some(FormatId::Bz2);
    }
    if starts(&[0x04, 0x22, 0x4D, 0x18]) {
        return Some(FormatId::Lz4);
    }
    if starts(b"MSCF") {
        return Some(FormatId::Cab);
    }
    if starts(b"MSWIM\0\0\0") {
        return Some(FormatId::Wim);
    }
    if starts(b"xar!") {
        return Some(FormatId::Xar);
    }
    if starts(&[0xED, 0xAB, 0xEE, 0xDB]) {
        return Some(FormatId::Rpm);
    }
    if starts(b"!<arch>\n") {
        let is_deb = head.len() > 8 + 13 && &head[8..8 + 13] == b"debian-binary";
        return Some(if is_deb { FormatId::Deb } else { FormatId::Ar });
    }
    if starts(b"070701") || starts(b"070702") || starts(b"070707") {
        return Some(FormatId::Cpio);
    }
    if head.len() >= 2 && (head[..2] == [0xC7, 0x71] || head[..2] == [0x71, 0xC7]) {
        return Some(FormatId::Cpio);
    }
    if starts(&[0x60, 0xEA]) {
        return Some(FormatId::Arj);
    }
    if head.len() >= 7 && &head[2..5] == b"-lh" && head[6] == b'-' {
        return Some(FormatId::Lzh);
    }
    if is_tar_header(head) {
        return Some(FormatId::Tar);
    }
    if head.len() >= 0x8006 && &head[0x8001..0x8006] == b"CD001" {
        return Some(FormatId::Iso);
    }
    None
}

/// Whether `block` starts with a valid tar header (ustar magic or a matching checksum).
pub fn is_tar_header(block: &[u8]) -> bool {
    if block.len() < 512 {
        return false;
    }
    let h = &block[..512];
    if &h[257..262] == b"ustar" {
        return true;
    }
    // Pre-POSIX (v7) headers have no magic: verify the checksum instead.
    if h.iter().all(|&b| b == 0) {
        return false;
    }
    let stored = parse_octal(&h[148..156]);
    let Some(stored) = stored else { return false };
    let mut sum: u64 = 0;
    for (i, &b) in h.iter().enumerate() {
        sum += if (148..156).contains(&i) {
            u64::from(b' ')
        } else {
            u64::from(b)
        };
    }
    sum == stored && h[0] != 0
}

fn parse_octal(field: &[u8]) -> Option<u64> {
    let s: Vec<u8> = field
        .iter()
        .copied()
        .filter(|b| *b != 0 && *b != b' ')
        .collect();
    if s.is_empty() || !s.iter().all(|b| (b'0'..=b'7').contains(b)) {
        return None;
    }
    u64::from_str_radix(std::str::from_utf8(&s).ok()?, 8).ok()
}

/// Looks for an archive embedded in a Windows executable (self-extracting archive).
fn sniff_sfx(head: &[u8], tail: &[u8]) -> Option<FormatId> {
    if !head.starts_with(b"MZ") {
        return None;
    }
    // ZIP end-of-central-directory near the end of the file.
    if find(tail, b"PK\x05\x06").is_some() {
        return Some(FormatId::Zip);
    }
    if find(head, &[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]).is_some() {
        return Some(FormatId::SevenZ);
    }
    if find(head, b"Rar!\x1a\x07").is_some() {
        return Some(FormatId::Rar);
    }
    None
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Reads the head and tail windows of a file.
pub fn read_windows(path: &Path) -> Result<(Vec<u8>, Vec<u8>, u64)> {
    let mut f = File::open(path).map_err(|e| Error::io(e, path))?;
    let len = f.metadata().map_err(|e| Error::io(e, path))?.len();
    let mut head = vec![0u8; HEAD_LEN.min(len as usize)];
    f.read_exact(&mut head).map_err(|e| Error::io(e, path))?;
    let tail_len = TAIL_LEN.min(len as usize);
    let mut tail = vec![0u8; tail_len];
    f.seek(SeekFrom::End(-(tail_len as i64)))
        .map_err(|e| Error::io(e, path))?;
    f.read_exact(&mut tail).map_err(|e| Error::io(e, path))?;
    Ok((head, tail, len))
}

/// Detects the format of a file on disk without consulting engines.
pub fn detect_path(path: &Path) -> Result<Detection> {
    let meta = std::fs::metadata(path).map_err(|e| Error::io(e, path))?;
    if meta.is_dir() {
        return Err(Error::new(ErrorCode::InvalidArgument).with("path", path.display()));
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let by_name = FormatId::from_file_name(&name);
    let (head, tail, _len) = read_windows(path)?;

    if let Some(found) = sniff(&head) {
        // A ".tar.gz" file sniffs as gzip; that is consistent, not a mismatch.
        let consistent = by_name.is_none_or(|n| {
            n == found || n.codec().is_some() && n.codec() == found.codec() || n == FormatId::Deb
        });
        let format = match by_name {
            // Keep the more specific compound name when the codec agrees.
            Some(n) if n.is_tar() && n.codec() == found.codec() && n.codec().is_some() => n,
            _ => found,
        };
        return Ok(Detection {
            format,
            by_magic: true,
            extension_mismatch: !consistent,
            sfx: false,
        });
    }
    if let Some(found) = sniff_sfx(&head, &tail) {
        return Ok(Detection {
            format: found,
            by_magic: true,
            extension_mismatch: false,
            sfx: true,
        });
    }
    // Brotli and raw LZMA have no reliable signature; trust the name.
    if let Some(n) = by_name
        && matches!(n, FormatId::Br | FormatId::TarBr | FormatId::Lzma)
    {
        return Ok(Detection {
            format: n,
            by_magic: false,
            extension_mismatch: false,
            sfx: false,
        });
    }
    Err(Error::new(ErrorCode::UnknownFormat).with("path", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures() {
        assert_eq!(sniff(b"PK\x03\x04rest"), Some(FormatId::Zip));
        assert_eq!(
            sniff(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C, 0, 4]),
            Some(FormatId::SevenZ)
        );
        assert_eq!(sniff(b"Rar!\x1a\x07\x01\x00"), Some(FormatId::Rar));
        assert_eq!(sniff(&[0x1F, 0x8B, 8, 0]), Some(FormatId::Gz));
        assert_eq!(sniff(b"BZh91AY"), Some(FormatId::Bz2));
        assert_eq!(sniff(b"BZhx"), None);
        assert_eq!(sniff(b"hello world"), None);
        assert_eq!(sniff(b"!<arch>\ndebian-binary   "), Some(FormatId::Deb));
    }

    #[test]
    fn tar_header_checksum() {
        let mut h = [0u8; 512];
        h[..5].copy_from_slice(b"a.txt");
        h[100..107].copy_from_slice(b"0000644");
        h[124..135].copy_from_slice(b"00000000005");
        let mut sum: u32 = 0;
        for (i, b) in h.iter().enumerate() {
            sum += if (148..156).contains(&i) {
                32
            } else {
                u32::from(*b)
            };
        }
        let chk = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(chk.as_bytes());
        assert!(is_tar_header(&h));
        h[0] = b'b';
        assert!(!is_tar_header(&h));
        let mut u = [0u8; 512];
        u[257..262].copy_from_slice(b"ustar");
        assert!(is_tar_header(&u));
    }

    #[test]
    fn detect_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.tar.gz");
        std::fs::write(&p, [0x1F, 0x8B, 8, 0, 0, 0]).unwrap();
        let d = detect_path(&p).unwrap();
        assert_eq!(d.format, FormatId::TarGz);
        assert!(!d.extension_mismatch);

        let p = dir.path().join("misnamed.rar");
        std::fs::write(&p, b"PK\x03\x04....").unwrap();
        let d = detect_path(&p).unwrap();
        assert_eq!(d.format, FormatId::Zip);
        assert!(d.extension_mismatch);

        let p = dir.path().join("notes.txt");
        std::fs::write(&p, b"just text").unwrap();
        assert_eq!(
            detect_path(&p).unwrap_err().code(),
            ErrorCode::UnknownFormat
        );
    }
}
