//! How expensive the archive's password is to try, read before unrar sees the archive.
//!
//! RAR 5 stores the number of PBKDF2 rounds that turn the password into a key, as a power of
//! two that unrar accepts up to 2^24. WinRAR writes 2^15. A crafted archive asking for 2^24 makes
//! every open with a password run 16 million rounds: seconds of CPU that cannot be cancelled,
//! repeated by every operation since the native handle is reopened each time (the nightly
//! fuzzer's RAR timeouts). Such archives are refused.

use std::{
    fs::File,
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

/// The most rounds accepted: 2^20, 32 times what WinRAR uses.
pub const MAX_LG2: u8 = 20;

const SIGNATURE: &[u8] = b"Rar!\x1a\x07\x01\x00";
/// How far into the file a RAR 5 archive may start (after a self-extracting module).
const SFX_MAX: usize = 1 << 20;
/// Headers walked at most, and the largest header read.
const MAX_HEADERS: usize = 1 << 20;
const MAX_HEADER: u64 = 2 << 20;

const HEAD_FILE: u64 = 2;
const HEAD_SERVICE: u64 = 3;
const HEAD_CRYPT: u64 = 4;
const HEAD_END: u64 = 5;
const HFL_EXTRA: u64 = 0x1;
const HFL_DATA: u64 = 0x2;
const FHEXTRA_CRYPT: u64 = 0x1;

/// The largest round count (log2) the RAR 5 archive at `path` declares: in the archive
/// encryption header when the headers are encrypted, else in the files' encryption records.
/// `None` for RAR 4, archives without encryption, and anything unreadable (unrar reports that).
pub fn max_lg2(path: &Path) -> Option<u8> {
    let mut r = BufReader::new(File::open(path).ok()?);
    let mut head = vec![0; SFX_MAX + SIGNATURE.len()];
    let n = read_up_to(&mut r, &mut head).ok()?;
    let start = head[..n]
        .windows(SIGNATURE.len())
        .position(|w| w == SIGNATURE)?;
    r.seek(SeekFrom::Start((start + SIGNATURE.len()) as u64))
        .ok()?;
    walk(&mut r)
}

fn walk(r: &mut (impl Read + Seek)) -> Option<u8> {
    let mut max = None;
    for _ in 0..MAX_HEADERS {
        let mut crc = [0; 4];
        if r.read_exact(&mut crc).is_err() {
            break;
        }
        let size = read_vint(r)?;
        if size == 0 || size > MAX_HEADER {
            break;
        }
        let mut header = vec![0; size as usize];
        r.read_exact(&mut header).ok()?;
        let mut h = header.as_slice();
        let kind = vint(&mut h)?;
        let flags = vint(&mut h)?;
        let extra = if flags & HFL_EXTRA != 0 {
            vint(&mut h)?
        } else {
            0
        };
        let data = if flags & HFL_DATA != 0 {
            vint(&mut h)?
        } else {
            0
        };
        match kind {
            HEAD_CRYPT => {
                // Everything after it is encrypted with this key.
                let _version = vint(&mut h)?;
                let _flags = vint(&mut h)?;
                return h.first().map(|&n| max.map_or(n, |m: u8| m.max(n)));
            }
            HEAD_FILE | HEAD_SERVICE if extra > 0 && extra <= size => {
                let records = &header[(size - extra) as usize..];
                for n in crypt_records(records) {
                    max = Some(max.map_or(n, |m: u8| m.max(n)));
                }
            }
            HEAD_END => break,
            _ => {}
        }
        if data > 0 {
            r.seek(SeekFrom::Current(i64::try_from(data).ok()?)).ok()?;
        }
    }
    max
}

/// Round counts of the file encryption records in a header's extra area.
fn crypt_records(mut area: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while !area.is_empty() {
        let Some(size) = vint(&mut area) else { break };
        let Some(record) = usize::try_from(size).ok().and_then(|s| area.get(..s)) else {
            break;
        };
        area = &area[record.len()..];
        let mut rec = record;
        if vint(&mut rec) == Some(FHEXTRA_CRYPT)
            && vint(&mut rec).is_some()
            && vint(&mut rec).is_some()
            && let Some(&n) = rec.first()
        {
            out.push(n);
        }
    }
    out
}

/// A RAR 5 variable-length integer from a byte slice, advancing it.
fn vint(b: &mut &[u8]) -> Option<u64> {
    let mut v = 0u64;
    for i in 0..10 {
        let (&x, rest) = b.split_first()?;
        *b = rest;
        v |= u64::from(x & 0x7f) << (7 * i);
        if x & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

fn read_vint(r: &mut impl Read) -> Option<u64> {
    let mut v = 0u64;
    for i in 0..10 {
        let mut x = [0];
        r.read_exact(&mut x).ok()?;
        v |= u64::from(x[0] & 0x7f) << (7 * i);
        if x[0] & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(kind: u8, flags: u8, fields: &[u8], extra: &[u8]) -> Vec<u8> {
        let mut h = vec![kind, flags];
        if flags & 1 != 0 {
            h.push(extra.len() as u8);
        }
        h.extend(fields);
        h.extend(extra);
        let mut b = vec![0, 0, 0, 0, h.len() as u8];
        b.extend(h);
        b
    }

    fn archive(blocks: &[Vec<u8>]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, SIGNATURE).unwrap();
        for b in blocks {
            std::io::Write::write_all(&mut f, b).unwrap();
        }
        f
    }

    #[test]
    fn round_counts_are_found() {
        // Encrypted headers: version 0, flags 1, 2^24 rounds, salt and check value.
        let mut crypt = vec![0, 1, 24];
        crypt.extend([0; 16 + 12]);
        let a = archive(&[block(4, 0, &crypt, &[])]);
        assert_eq!(max_lg2(a.path()), Some(24));

        // Encrypted files: a main header, then file headers with encryption records.
        let record = |lg2: u8| {
            let mut r = vec![1, 0, 0, lg2];
            r.extend([0; 16 + 16]);
            let mut sized = vec![r.len() as u8];
            sized.extend(r);
            sized
        };
        let main = block(1, 0, &[0], &[]);
        let file = |lg2| block(2, 1, &[0, 0, 0, 0], &record(lg2));
        let a = archive(&[main.clone(), file(15), file(22), block(5, 0, &[0], &[])]);
        assert_eq!(max_lg2(a.path()), Some(22));

        // Nothing encrypted.
        let a = archive(&[main, block(5, 0, &[0], &[])]);
        assert_eq!(max_lg2(a.path()), None);
    }

    #[test]
    fn the_fixtures_are_ordinary() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rar");
        assert_eq!(max_lg2(&dir.join("comment-hpw-password.rar")), Some(15));
        assert_eq!(max_lg2(&dir.join("solid.rar")), None);
        // RAR 4.
        assert_eq!(max_lg2(&dir.join("crypted.rar")), None);
    }
}
