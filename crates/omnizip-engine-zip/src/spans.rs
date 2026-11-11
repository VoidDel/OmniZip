//! Split and spanned ZIP archives as PKZIP, WinZip and Info-ZIP write them: `x.z01`, `x.z02`,
//! …, `x.zip`, the central directory at the end of the last file (`docs/03-formats.md` §7).
//!
//! Every offset in such an archive counts from the start of the volume it lies on, with the
//! volume's number beside it. The `zip` crate reads one stream with offsets from its start, so
//! the volumes are joined into one stream and a rewritten central directory is appended after
//! them, with each offset made absolute (volume start + offset) and every volume number 0. The
//! data itself is read where it is; the original central directory is never looked at again.

use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use omnizip_core::{Error, ErrorCode, Result, volume::SharedInput};

const EOCD: &[u8; 4] = b"PK\x05\x06";
const EOCD64_LOCATOR: &[u8; 4] = b"PK\x06\x07";
const EOCD64: &[u8; 4] = b"PK\x06\x06";
const CENTRAL: &[u8; 4] = b"PK\x01\x02";
/// End record (22 bytes) plus the longest comment.
const TAIL: u64 = 22 + 0xFFFF;

/// The files of the spanned set `path` belongs to, in order (`x.z01` … `x.zip`), when it is
/// one: `path` is `x.zip` or `x.zNN` and `x.z01` exists. The last entry is `x.zip` even when
/// that file is missing, which [`open`] reports.
pub fn volumes(path: &Path) -> Option<Vec<PathBuf>> {
    let name = path.file_name()?.to_str()?;
    let (stem, ext) = name.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    let span = ext.len() == 3
        && ext.starts_with('z')
        && ext[1..].bytes().all(|b| b.is_ascii_digit())
        && ext != "z00";
    if ext != "zip" && !span {
        return None;
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let find = |ext: &str| {
        [ext.to_owned(), ext.to_ascii_uppercase()]
            .into_iter()
            .map(|e| dir.join(format!("{stem}.{e}")))
            .find(|p| p.is_file())
    };
    let mut out: Vec<PathBuf> = (1..=99u32)
        .map_while(|n| find(&format!("z{n:02}")))
        .collect();
    if out.is_empty() {
        return None;
    }
    out.push(find("zip").unwrap_or_else(|| dir.join(format!("{stem}.zip"))));
    Some(out)
}

fn corrupt(what: &str, path: &Path) -> Error {
    Error::new(ErrorCode::Corrupt)
        .detail(what)
        .with("path", path.display())
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().expect("4 bytes"))
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().expect("8 bytes"))
}

fn read_exact_at(input: &mut SharedInput, pos: u64, len: u64, path: &Path) -> Result<Vec<u8>> {
    let mut buf = vec![0; usize::try_from(len).map_err(|_| corrupt("size", path))?];
    input
        .seek(SeekFrom::Start(pos))
        .and_then(|_| input.read_exact(&mut buf))
        .map_err(|_| corrupt("truncated", path))?;
    Ok(buf)
}

/// What the end records say.
struct End {
    /// Number of the last volume (0-based).
    last_disk: u32,
    /// Volume the central directory starts on, and its offset there.
    cd_disk: u32,
    cd_offset: u64,
    cd_size: u64,
    comment: Vec<u8>,
}

/// Reads the end records from the last volume (`last_start`, `last_len` within `joined`).
fn read_end(
    joined: &mut SharedInput,
    starts: &[u64],
    last_start: u64,
    last_len: u64,
    path: &Path,
) -> Result<End> {
    let tail_len = last_len.min(TAIL);
    let tail = read_exact_at(joined, last_start + last_len - tail_len, tail_len, path)?;
    let at = (0..=tail.len().saturating_sub(22))
        .rev()
        .find(|&i| &tail[i..i + 4] == EOCD)
        .ok_or_else(|| corrupt("no end of central directory", path))?;
    let e = &tail[at..];
    let comment_len = usize::from(u16_at(e, 20));
    let comment = e[22..].get(..comment_len).unwrap_or(&e[22..]).to_vec();
    let mut end = End {
        last_disk: u32::from(u16_at(e, 4)),
        cd_disk: u32::from(u16_at(e, 6)),
        cd_offset: u64::from(u32_at(e, 16)),
        cd_size: u64::from(u32_at(e, 12)),
        comment,
    };
    let saturated = end.last_disk == 0xFFFF
        || end.cd_disk == 0xFFFF
        || u16_at(e, 10) == 0xFFFF
        || end.cd_size == 0xFFFF_FFFF
        || end.cd_offset == 0xFFFF_FFFF;
    if saturated && at >= 20 && &tail[at - 20..at - 16] == EOCD64_LOCATOR {
        let loc = &tail[at - 20..at];
        let disk = usize::try_from(u32_at(loc, 4)).unwrap_or(usize::MAX);
        let start = *starts
            .get(disk)
            .ok_or_else(|| corrupt("zip64 end record", path))?;
        let rec = read_exact_at(joined, start + u64_at(loc, 8), 56, path)?;
        if &rec[..4] != EOCD64 {
            return Err(corrupt("zip64 end record", path));
        }
        end.last_disk = u32_at(&rec, 16);
        end.cd_disk = u32_at(&rec, 20);
        end.cd_size = u64_at(&rec, 40);
        end.cd_offset = u64_at(&rec, 48);
    }
    Ok(end)
}

/// One central-directory record with its offset made absolute.
fn rewrite_record(
    h: &[u8],
    name: &[u8],
    extra: &[u8],
    comment: &[u8],
    starts: &[u64],
    out: &mut Vec<u8>,
    path: &Path,
) -> Result<()> {
    let mut packed = u64::from(u32_at(h, 20));
    let mut size = u64::from(u32_at(h, 24));
    let mut disk = u32::from(u16_at(h, 34));
    let mut offset = u64::from(u32_at(h, 42));
    // The zip64 field holds, in this order, only the values that did not fit.
    let mut other = Vec::new();
    let mut q = 0;
    while q + 4 <= extra.len() {
        let id = u16_at(extra, q);
        let len = usize::from(u16_at(extra, q + 2));
        let Some(data) = extra.get(q + 4..q + 4 + len) else {
            break;
        };
        if id == 1 {
            let mut r = 0;
            let mut next8 = |v: &mut u64| {
                if *v == 0xFFFF_FFFF && r + 8 <= data.len() {
                    *v = u64_at(data, r);
                    r += 8;
                }
            };
            next8(&mut size);
            next8(&mut packed);
            next8(&mut offset);
            if disk == 0xFFFF && r + 4 <= data.len() {
                disk = u32_at(data, r);
            }
        } else {
            other.extend_from_slice(&extra[q..q + 4 + len]);
        }
        q += 4 + len;
    }
    let start = *starts
        .get(usize::try_from(disk).unwrap_or(usize::MAX))
        .ok_or_else(|| corrupt("entry on a volume that does not exist", path))?;
    let offset = start + offset;

    let mut zip64 = Vec::new();
    for v in [size, packed, offset] {
        if v >= 0xFFFF_FFFF {
            zip64.extend_from_slice(&v.to_le_bytes());
        }
    }
    let mut new_extra = Vec::new();
    if !zip64.is_empty() {
        new_extra.extend_from_slice(&1u16.to_le_bytes());
        new_extra.extend_from_slice(&(zip64.len() as u16).to_le_bytes());
        new_extra.extend_from_slice(&zip64);
    }
    new_extra.extend_from_slice(&other);
    let extra_len =
        u16::try_from(new_extra.len()).map_err(|_| corrupt("extra field too long", path))?;
    let fit = |v: u64| u32::try_from(v).ok().filter(|&v| v != 0xFFFF_FFFF);

    let mut nh = h.to_vec();
    nh[20..24].copy_from_slice(&fit(packed).unwrap_or(0xFFFF_FFFF).to_le_bytes());
    nh[24..28].copy_from_slice(&fit(size).unwrap_or(0xFFFF_FFFF).to_le_bytes());
    nh[30..32].copy_from_slice(&extra_len.to_le_bytes());
    nh[34..36].copy_from_slice(&0u16.to_le_bytes());
    nh[42..46].copy_from_slice(&fit(offset).unwrap_or(0xFFFF_FFFF).to_le_bytes());
    out.extend_from_slice(&nh);
    out.extend_from_slice(name);
    out.extend_from_slice(&new_extra);
    out.extend_from_slice(comment);
    Ok(())
}

/// The spanned set `paths` ([`volumes`]) as one input the `zip` crate reads like any archive.
pub fn open(paths: &[PathBuf]) -> Result<SharedInput> {
    let last = paths.last().expect("a spanned set has volumes");
    if !last.is_file() {
        return Err(Error::new(ErrorCode::MissingVolume).with("path", last.display()));
    }
    let mut starts = Vec::with_capacity(paths.len());
    let mut total = 0u64;
    for p in paths {
        starts.push(total);
        total += fs::metadata(p).map_err(|e| Error::io(e, p))?.len();
    }
    let mut joined = SharedInput::join(paths, Vec::new())?;
    let last_start = starts[starts.len() - 1];
    let end = read_end(&mut joined, &starts, last_start, total - last_start, last)?;
    if end.last_disk as usize + 1 != paths.len() {
        // Volumes run out before the one the end record is on: name the first one missing.
        let missing = if (end.last_disk as usize) + 1 > paths.len() {
            last.with_extension(format!("z{:02}", paths.len()))
        } else {
            last.clone()
        };
        return Err(Error::new(ErrorCode::MissingVolume).with("path", missing.display()));
    }
    let cd_start = starts
        .get(end.cd_disk as usize)
        .map(|s| s + end.cd_offset)
        .filter(|s| s.checked_add(end.cd_size).is_some_and(|e| e <= total))
        .ok_or_else(|| corrupt("central directory out of range", last))?;
    let cd = read_exact_at(&mut joined, cd_start, end.cd_size, last)?;

    let mut out = Vec::with_capacity(cd.len() + 128);
    let mut count = 0u64;
    let mut p = 0;
    while p + 46 <= cd.len() && &cd[p..p + 4] == CENTRAL {
        let h = &cd[p..p + 46];
        let name_len = usize::from(u16_at(h, 28));
        let extra_len = usize::from(u16_at(h, 30));
        let comment_len = usize::from(u16_at(h, 32));
        let name_at = p + 46;
        let extra_at = name_at + name_len;
        let comment_at = extra_at + extra_len;
        let next = comment_at + comment_len;
        if next > cd.len() {
            return Err(corrupt("central directory record", last));
        }
        rewrite_record(
            h,
            &cd[name_at..extra_at],
            &cd[extra_at..comment_at],
            &cd[comment_at..next],
            &starts,
            &mut out,
            last,
        )?;
        count += 1;
        p = next;
    }

    // New end records: the central directory starts right after the last volume.
    let cd_size = out.len() as u64;
    let cd_offset = total;
    if count >= 0xFFFF || cd_size >= 0xFFFF_FFFF || cd_offset >= 0xFFFF_FFFF {
        let record = total + cd_size;
        out.extend_from_slice(EOCD64);
        out.extend_from_slice(&44u64.to_le_bytes());
        out.extend_from_slice(&45u16.to_le_bytes());
        out.extend_from_slice(&45u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(EOCD64_LOCATOR);
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&record.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
    }
    let n16 = u16::try_from(count).unwrap_or(0xFFFF);
    out.extend_from_slice(EOCD);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&n16.to_le_bytes());
    out.extend_from_slice(&n16.to_le_bytes());
    out.extend_from_slice(&u32::try_from(cd_size).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&u32::try_from(cd_offset).unwrap_or(u32::MAX).to_le_bytes());
    let comment = &end.comment[..end.comment.len().min(0xFFFF)];
    out.extend_from_slice(&(comment.len() as u16).to_le_bytes());
    out.extend_from_slice(comment);
    SharedInput::join(paths, out)
}

/// Total size of the volumes on disk.
pub fn size(paths: &[PathBuf]) -> Result<u64> {
    paths.iter().try_fold(0u64, |t, p| {
        Ok(t + fs::metadata(p).map_err(|e| Error::io(e, p))?.len())
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Turns a one-file ZIP into a spanned set the way PKZIP writes one: the split marker,
    /// data volumes of `volume` bytes, the central directory on a last volume of its own, and
    /// every offset relative to its volume. Returns the volume contents in order.
    pub(crate) fn span(zip: &[u8], volume: usize) -> Vec<Vec<u8>> {
        let at = (0..=zip.len() - 22)
            .rev()
            .find(|&i| &zip[i..i + 4] == EOCD)
            .unwrap();
        let cd_size = u32_at(zip, at + 12) as usize;
        let cd_offset = u32_at(zip, at + 16) as usize;
        // Marker + data, cut into volumes; offsets shift by the 4-byte marker.
        let mut data = b"PK\x07\x08".to_vec();
        data.extend_from_slice(&zip[..cd_offset]);
        let mut vols: Vec<Vec<u8>> = data.chunks(volume).map(<[u8]>::to_vec).collect();
        let mut cd = zip[cd_offset..cd_offset + cd_size].to_vec();
        let mut p = 0;
        while p < cd.len() {
            let abs = u32_at(&cd, p + 42) as usize + 4;
            cd[p + 34..p + 36].copy_from_slice(&((abs / volume) as u16).to_le_bytes());
            cd[p + 42..p + 46].copy_from_slice(&((abs % volume) as u32).to_le_bytes());
            p += 46
                + u16_at(&cd, p + 28) as usize
                + u16_at(&cd, p + 30) as usize
                + u16_at(&cd, p + 32) as usize;
        }
        let last = vols.len() as u16;
        let mut tail = cd;
        let mut e = zip[at..].to_vec();
        e[4..6].copy_from_slice(&last.to_le_bytes());
        e[6..8].copy_from_slice(&last.to_le_bytes());
        e[16..20].copy_from_slice(&0u32.to_le_bytes());
        tail.extend_from_slice(&e);
        vols.push(tail);
        vols
    }

    #[test]
    fn finds_the_set_from_any_volume() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        for n in ["x.z01", "x.z02", "x.zip", "plain.zip"] {
            fs::write(p(n), b"").unwrap();
        }
        let want = vec![p("x.z01"), p("x.z02"), p("x.zip")];
        assert_eq!(volumes(&p("x.zip")), Some(want.clone()));
        assert_eq!(volumes(&p("x.z02")), Some(want));
        assert_eq!(volumes(&p("plain.zip")), None);
        assert_eq!(volumes(&p("x.7z")), None);
        // Without the last file the set is still recognized, and opening it says so.
        fs::remove_file(p("x.zip")).unwrap();
        let v = volumes(&p("x.z01")).unwrap();
        assert_eq!(v.last().unwrap(), &p("x.zip"));
        assert_eq!(open(&v).unwrap_err().code(), ErrorCode::MissingVolume);
    }
}
