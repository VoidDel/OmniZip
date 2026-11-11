//! LZH / LHA archives (`docs/03-formats.md` §3), header levels 0–3, every method `delharc`
//! decodes (`-lh0-` … `-lh7-`, `-lhx-`, `-lzs-`, `-lz5-`, `-pm1-`, `-pm2-`). Each file is
//! compressed on its own right after its header, so files are read directly; stored files
//! (`-lh0-`) in place when mounted. Names keep their raw bytes for code-page detection: LZH
//! archives from Japan are mostly Shift_JIS.

use std::{
    io::{Seek, SeekFrom},
    path::Path,
};

use delharc::{CompressionMethod, LhaHeader};
use omnizip_core::{
    ArchiveInfo, Entry, EntryKind, FormatId, JobCtx, OpenOptions, Result, timeutil,
    volume::SharedInput,
};

use crate::{
    common::{Check, Coding, Stored, corrupt, decode_names},
    lha::dos_time,
};

const EXT_FILENAME: u8 = 0x01;
const EXT_PATH: u8 = 0x02;
const EXT_UNIX_MODE: u8 = 0x50;
const EXT_UNIX_TIME: u8 = 0x54;

/// The path as stored: directory and file name, `0xFF` separators made `/`. Backslashes stay
/// until the name is decoded: in Shift_JIS a `0x5C` byte can be half of a character.
fn raw_path(h: &LhaHeader) -> Vec<u8> {
    let mut dir = Vec::new();
    let mut name = None;
    for x in h.iter_extra() {
        match x {
            [EXT_FILENAME, data @ ..] => name = Some(data.to_vec()),
            [EXT_PATH, data @ ..] => {
                dir = data.to_vec();
                if !dir.is_empty() && !dir.ends_with(&[0xFF]) && !dir.ends_with(b"/") {
                    dir.push(0xFF);
                }
            }
            _ => {}
        }
    }
    let mut name = name.unwrap_or_else(|| h.filename.to_vec());
    // Amiga and OS-9 put a comment after a NUL.
    if let Some(nul) = name.iter().position(|&b| b == 0) {
        name.truncate(nul);
    }
    dir.extend_from_slice(&name);
    for b in &mut dir {
        if *b == 0xFF {
            *b = b'/';
        }
    }
    dir
}

fn extra(h: &LhaHeader, id: u8) -> Option<&[u8]> {
    h.iter_extra().find_map(|x| match x {
        [first, data @ ..] if *first == id => Some(data),
        _ => None,
    })
}

/// One file before names are decoded.
struct Item {
    raw: Vec<u8>,
    link: Option<Vec<u8>>,
    kind: EntryKind,
    header: LhaHeader,
    data: u64,
}

pub(crate) fn open(path: &Path, opts: &OpenOptions, ctx: &JobCtx) -> Result<Stored> {
    let mut input = SharedInput::open(path)?;
    let size = input.len();
    let mut items = Vec::new();
    loop {
        ctx.check()?;
        let header = match LhaHeader::read(&mut input) {
            Ok(Some(h)) => h,
            Ok(None) => break,
            Err(e) => return Err(corrupt(e, path)),
        };
        let data = input
            .stream_position()
            .map_err(|e| omnizip_core::Error::io(e, path))?;
        let end = data
            .checked_add(header.compressed_size)
            .filter(|&e| e <= size)
            .ok_or_else(|| corrupt("truncated", path))?;
        input
            .seek(SeekFrom::Start(end))
            .map_err(|e| omnizip_core::Error::io(e, path))?;
        let mut raw = raw_path(&header);
        let method = header.compression_method().ok();
        // Unix LHA writes a symbolic link as a directory entry named "name|target".
        let mut link = None;
        let kind = if method == Some(CompressionMethod::Lhd)
            && let Some(bar) = raw.iter().position(|&b| b == b'|')
        {
            link = Some(raw[bar + 1..].to_vec());
            raw.truncate(bar);
            EntryKind::Symlink
        } else if header.is_directory() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        if kind == EntryKind::Dir && !raw.ends_with(b"/") {
            raw.push(b'/');
        }
        items.push(Item {
            raw,
            link,
            kind,
            header,
            data,
        });
        ctx.policy.check_entries(items.len() as u64)?;
    }

    let raw: Vec<Vec<u8>> = items.iter().map(|i| i.raw.clone()).collect();
    let (names, enc) = decode_names(&raw, opts);
    let mut info = ArchiveInfo::new(path, FormatId::Lzh, size);
    info.encoding = enc;
    let mut entries = Vec::with_capacity(items.len());
    let mut ranges = Vec::with_capacity(items.len());
    let mut codings = Vec::with_capacity(items.len());
    let mut checks = Vec::with_capacity(items.len());
    for (it, name) in items.into_iter().zip(names) {
        let h = &it.header;
        let path_str = name.replace('\\', "/");
        let path_str = path_str.trim_start_matches('/').to_owned();
        if path_str.is_empty() || path_str == "/" {
            continue;
        }
        let mut e = Entry::new(entries.len() as u32, path_str, it.kind);
        e.raw_name = it.raw;
        let method_name = String::from_utf8_lossy(&h.compression)
            .trim_matches('-')
            .to_ascii_uppercase();
        e.mtime = match extra(h, EXT_UNIX_TIME) {
            Some(t) if t.len() >= 4 => {
                Some(timeutil::from_unix_secs(u64::from(u32::from_le_bytes([
                    t[0], t[1], t[2], t[3],
                ]))))
            }
            _ if h.level >= 2 => Some(timeutil::from_unix_secs(u64::from(h.last_modified))),
            _ => dos_time(h.last_modified),
        };
        e.unix_mode = extra(h, EXT_UNIX_MODE)
            .filter(|m| m.len() >= 2)
            .map(|m| u32::from(u16::from_le_bytes([m[0], m[1]])) & 0o7777);
        if e.unix_mode.is_none() {
            e.win_attrs = Some(u32::from(h.msdos_attrs.bits()));
        }
        e.link_target = it.link.map(|l| match enc {
            Some(enc) if std::str::from_utf8(&l).is_err() => enc.decode(&l),
            _ => String::from_utf8_lossy(&l).into_owned(),
        });
        let (range, coding, check) = if it.kind == EntryKind::File {
            e.size = h.original_size;
            e.packed_size = Some(h.compressed_size);
            e.method = Some(method_name.clone());
            info.add_method(method_name);
            let coding = match h.compression_method() {
                Ok(CompressionMethod::Lh0 | CompressionMethod::Lz4 | CompressionMethod::Pm0) => {
                    None
                }
                Ok(m) => Some(Coding::Lha(m)),
                Err(_) => Some(Coding::Unsupported),
            };
            (
                Some((it.data, h.compressed_size)),
                coding,
                Some(Check::Crc16(h.file_crc)),
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
