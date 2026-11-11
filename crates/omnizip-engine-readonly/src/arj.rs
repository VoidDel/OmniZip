//! ARJ archives (`docs/03-formats.md` §3): methods 0 (stored), 1–3 (LZH with a 26 KiB
//! window, decoded as `-lh6-`) and 4 ("fastest"). Each file follows its header and is decoded
//! on its own; its CRC-32 is checked. Garbled (password-protected) archives and archives
//! continued in further volumes are not read.

use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use omnizip_core::{
    ArchiveInfo, Entry, EntryKind, Error, ErrorCode, FormatId, JobCtx, OpenOptions, Result,
    volume::SharedInput,
};

use crate::{
    common::{Check, Coding, Stored, corrupt, decode_names},
    lha::dos_time,
};

const MAGIC: [u8; 2] = [0x60, 0xEA];
/// Largest basic header ARJ writes.
const MAX_HEADER: u16 = 2600;

const FLAG_GARBLED: u8 = 0x01;
const FLAG_VOLUME: u8 = 0x04;
const FLAG_EXTFILE: u8 = 0x08;

const HOST_UNIX: u8 = 2;

/// Reads a basic header and skips its extended headers. `None` at the end-of-archive header.
fn header(input: &mut SharedInput, path: &Path) -> Result<Option<Vec<u8>>> {
    let mut head = [0u8; 4];
    input
        .read_exact(&mut head)
        .map_err(|_| corrupt("truncated", path))?;
    if head[..2] != MAGIC {
        return Err(corrupt("bad ARJ header", path));
    }
    let size = u16::from_le_bytes([head[2], head[3]]);
    if size == 0 {
        return Ok(None);
    }
    if size > MAX_HEADER {
        return Err(corrupt("ARJ header too large", path));
    }
    let mut h = vec![0u8; usize::from(size) + 4];
    input
        .read_exact(&mut h)
        .map_err(|_| corrupt("truncated", path))?;
    let crc = u32::from_le_bytes(h[h.len() - 4..].try_into().expect("4 bytes"));
    h.truncate(usize::from(size));
    if crc32fast::hash(&h) != crc {
        return Err(corrupt("ARJ header checksum", path));
    }
    loop {
        let mut len = [0u8; 2];
        input
            .read_exact(&mut len)
            .map_err(|_| corrupt("truncated", path))?;
        let len = u16::from_le_bytes(len);
        if len == 0 {
            break;
        }
        input
            .seek(SeekFrom::Current(i64::from(len) + 4))
            .map_err(|e| Error::io(e, path))?;
    }
    Ok(Some(h))
}

/// The NUL-terminated string at `at`.
fn cstr(h: &[u8], at: usize) -> &[u8] {
    let s = h.get(at..).unwrap_or_default();
    &s[..s.iter().position(|&b| b == 0).unwrap_or(s.len())]
}

/// One listed file before names are decoded.
struct Item {
    raw: Vec<u8>,
    kind: EntryKind,
    method: u8,
    host: u8,
    mode: u16,
    mtime: u32,
    packed: u64,
    size: u64,
    crc: u32,
    data: u64,
}

pub(crate) fn open(path: &Path, opts: &OpenOptions, ctx: &JobCtx) -> Result<Stored> {
    let mut input = SharedInput::open(path)?;
    let size = input.len();
    let main = header(&mut input, path)?.ok_or_else(|| corrupt("empty ARJ archive", path))?;
    let first = usize::from(main[0]);
    let comment = cstr(&main, first + cstr(&main, first).len() + 1).to_vec();

    let mut items = Vec::new();
    while let Some(h) = header(&mut input, path)? {
        ctx.check()?;
        if h.len() < 30 {
            return Err(corrupt("bad ARJ file header", path));
        }
        let le32 = |o: usize| u32::from_le_bytes(h[o..o + 4].try_into().expect("4 bytes"));
        let flags = h[4];
        if flags & FLAG_GARBLED != 0 {
            return Err(Error::new(ErrorCode::UnsupportedMethod)
                .with("method", "ARJ garbled")
                .with("path", path.display()));
        }
        if flags & (FLAG_VOLUME | FLAG_EXTFILE) != 0 {
            return Err(Error::new(ErrorCode::UnsupportedOperation)
                .with("operation", "multi-volume ARJ")
                .with("path", path.display()));
        }
        let packed = u64::from(le32(12));
        let data = input.stream_position().map_err(|e| Error::io(e, path))?;
        let end = data
            .checked_add(packed)
            .filter(|&e| e <= size)
            .ok_or_else(|| corrupt("truncated", path))?;
        input
            .seek(SeekFrom::Start(end))
            .map_err(|e| Error::io(e, path))?;
        let kind = match h[6] {
            0 | 1 => EntryKind::File,
            3 => EntryKind::Dir,
            // Comments, volume and chapter labels.
            _ => continue,
        };
        let mut raw = cstr(&h, usize::from(h[0])).to_vec();
        if kind == EntryKind::Dir && !raw.ends_with(b"/") && !raw.ends_with(b"\\") {
            raw.push(b'/');
        }
        items.push(Item {
            raw,
            kind,
            method: h[5],
            host: h[3],
            mode: u16::from_le_bytes([h[26], h[27]]),
            mtime: le32(8),
            packed,
            size: u64::from(le32(16)),
            crc: le32(20),
            data,
        });
        ctx.policy.check_entries(items.len() as u64)?;
    }

    let raw: Vec<Vec<u8>> = items.iter().map(|i| i.raw.clone()).collect();
    let (names, enc) = decode_names(&raw, opts);
    let mut info = ArchiveInfo::new(path, FormatId::Arj, size);
    info.encoding = enc;
    if !comment.is_empty() {
        info.comment = Some(match enc {
            Some(e) => e.decode(&comment),
            None => String::from_utf8_lossy(&comment).into_owned(),
        });
    }
    let mut entries = Vec::with_capacity(items.len());
    let mut ranges = Vec::with_capacity(items.len());
    let mut codings = Vec::with_capacity(items.len());
    let mut checks = Vec::with_capacity(items.len());
    for (it, name) in items.into_iter().zip(names) {
        let p = name.replace('\\', "/").trim_start_matches('/').to_owned();
        if p.is_empty() {
            continue;
        }
        let mut e = Entry::new(entries.len() as u32, p, it.kind);
        e.raw_name = it.raw;
        e.mtime = dos_time(it.mtime);
        if it.host == HOST_UNIX {
            e.unix_mode = Some(u32::from(it.mode) & 0o7777);
        } else {
            e.win_attrs = Some(u32::from(it.mode & 0x3F));
        }
        let (range, coding, check) = if it.kind == EntryKind::File {
            e.size = it.size;
            e.packed_size = Some(it.packed);
            e.crc32 = Some(it.crc);
            let (name, coding) = match it.method {
                0 => ("Store", None),
                1..=3 => ("LZH", Some(Coding::Lha(delharc::CompressionMethod::Lh6))),
                4 => ("Fastest", Some(Coding::ArjFastest)),
                _ => ("?", Some(Coding::Unsupported)),
            };
            e.method = Some(if it.method <= 4 {
                format!("{name} (ARJ {})", it.method)
            } else {
                format!("ARJ {}", it.method)
            });
            info.add_method(e.method.clone().unwrap_or_default());
            (
                Some((it.data, it.packed)),
                coding,
                Some(Check::Crc32(it.crc)),
            )
        } else {
            (None, None, None)
        };
        entries.push(e);
        ranges.push(range);
        codings.push(coding);
        checks.push(check);
    }
    Ok(Stored {
        info,
        entries: entries.into(),
        ranges: ranges.into(),
        chains: Default::default(),
        codings: codings.into(),
        checks: checks.into(),
        input,
        in_place: true,
    })
}
