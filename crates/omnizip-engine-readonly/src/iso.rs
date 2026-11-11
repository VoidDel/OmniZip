//! ISO 9660 images (`docs/03-formats.md` §3) with the Rock Ridge (POSIX names, modes, links,
//! times) and Joliet (Unicode names) extensions, preferred in that order, as libarchive does.
//! Files lie as they are in the image, so mounts read them in place. UDF-only images are not
//! read (they have no ISO 9660 descriptor and are not detected as ISO).

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
    time::SystemTime,
};

use omnizip_core::{
    ArchiveInfo, Entry, EntryId, EntryKind, FormatId, JobCtx, OpenOptions, Result,
    volume::{self, SharedInput},
};

use crate::common::{Stored, corrupt, decode_names, read_at, utc_time};

const SECTOR: u64 = 2048;
/// Volume descriptors looked at before giving up on a terminator.
const MAX_DESCRIPTORS: u64 = 64;
/// Deepest directory nesting followed.
const MAX_DEPTH: usize = 128;
/// Largest directory read (a directory of 64 MiB holds about a million entries).
const MAX_DIR: u64 = 64 << 20;
/// Rock Ridge continuation areas followed per record.
const MAX_CONTINUATIONS: usize = 16;

/// A directory record.
struct Record {
    extent: u64,
    size: u64,
    flags: u8,
    name: Vec<u8>,
    system_use: Vec<u8>,
    mtime: Option<SystemTime>,
}

const FLAG_DIR: u8 = 0x02;
const FLAG_MORE: u8 = 0x80;

fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// The 7-byte recording time: years since 1900, month, day, hour, minute, second, and the
/// offset from UTC in 15-minute steps.
fn record_time(t: &[u8]) -> Option<SystemTime> {
    let offset = i64::from(t[6] as i8) * 15 * 60;
    utc_time(
        1900 + i64::from(t[0]),
        u32::from(t[1]),
        u32::from(t[2]),
        u32::from(t[3]),
        u32::from(t[4]),
        u32::from(t[5]),
        offset,
    )
}

/// The 17-byte long form (`YYYYMMDDHHMMSScc` digits and the UTC offset).
fn long_time(t: &[u8]) -> Option<SystemTime> {
    let n = |r: std::ops::Range<usize>| std::str::from_utf8(&t[r]).ok()?.parse::<u32>().ok();
    utc_time(
        i64::from(n(0..4)?),
        n(4..6)?,
        n(6..8)?,
        n(8..10)?,
        n(10..12)?,
        n(12..14)?,
        i64::from(t[16] as i8) * 15 * 60,
    )
}

fn parse_record(r: &[u8]) -> Option<Record> {
    let len = usize::from(*r.first()?);
    if len < 34 || len > r.len() {
        return None;
    }
    let r = &r[..len];
    let name_len = usize::from(r[32]);
    let name = r.get(33..33 + name_len)?.to_vec();
    // A padding byte keeps the system-use area at an even offset.
    let su_start = 33 + name_len + usize::from(name_len % 2 == 0);
    let ext_attr = u64::from(r[1]);
    Some(Record {
        extent: u64::from(u32le(&r[2..6])) + ext_attr,
        size: u64::from(u32le(&r[10..14])),
        flags: r[25],
        name,
        system_use: r.get(su_start..).unwrap_or_default().to_vec(),
        mtime: record_time(&r[18..25]),
    })
}

/// What the Rock Ridge entries of a record say.
#[derive(Default)]
struct RockRidge {
    name: Option<Vec<u8>>,
    mode: Option<u32>,
    link: Option<String>,
    mtime: Option<SystemTime>,
    /// A relocated directory's real place (`CL`), and whether this is the relocated copy
    /// itself (`RE`), which is skipped.
    child: Option<u64>,
    relocated: bool,
}

/// Reads the System Use Sharing Protocol entries of `su` (and of their continuation areas).
fn rock_ridge(su: &[u8], input: &mut SharedInput, path: &Path) -> Result<RockRidge> {
    let mut rr = RockRidge::default();
    let mut area = su.to_vec();
    let mut link_parts: Vec<String> = Vec::new();
    let mut link_open = false;
    for _ in 0..=MAX_CONTINUATIONS {
        let mut next = None;
        let mut p = 0;
        while p + 4 <= area.len() {
            let len = usize::from(area[p + 2]);
            if len < 4 || p + len > area.len() {
                break;
            }
            let d = &area[p + 4..p + len];
            match &area[p..p + 2] {
                b"NM" if !d.is_empty() && d[0] & 0b110 == 0 => {
                    rr.name
                        .get_or_insert_with(Vec::new)
                        .extend_from_slice(&d[1..]);
                }
                b"PX" if d.len() >= 4 => rr.mode = Some(u32le(d)),
                b"SL" if !d.is_empty() => {
                    let mut q = 1;
                    while q + 2 <= d.len() {
                        let (flags, n) = (d[q], usize::from(d[q + 1]));
                        let text = d.get(q + 2..q + 2 + n).unwrap_or_default();
                        let part = match flags & 0b1110 {
                            0b0010 => ".".to_owned(),
                            0b0100 => "..".to_owned(),
                            0b1000 => String::new(),
                            _ => String::from_utf8_lossy(text).into_owned(),
                        };
                        match link_parts.last_mut() {
                            Some(last) if link_open => last.push_str(&part),
                            _ => link_parts.push(part),
                        }
                        link_open = flags & 1 != 0;
                        q += 2 + n;
                    }
                }
                b"TF" if !d.is_empty() => {
                    let long = d[0] & 0x80 != 0;
                    let w = if long { 17 } else { 7 };
                    // Creation comes first when present; then modification.
                    let at = 1 + if d[0] & 1 != 0 { w } else { 0 };
                    if d[0] & 2 != 0
                        && let Some(t) = d.get(at..at + w)
                    {
                        rr.mtime = if long { long_time(t) } else { record_time(t) };
                    }
                }
                b"CL" if d.len() >= 4 => rr.child = Some(u64::from(u32le(d))),
                b"RE" => rr.relocated = true,
                b"CE" if d.len() >= 20 => {
                    next = Some((
                        u64::from(u32le(&d[0..4])),
                        u64::from(u32le(&d[8..12])),
                        u32le(&d[16..20]),
                    ));
                }
                b"ST" => break,
                _ => {}
            }
            p += len;
        }
        let Some((block, offset, len)) = next else {
            break;
        };
        area = read_at(
            input,
            block * SECTOR + offset,
            len.min(SECTOR as u32 * 32) as usize,
            path,
        )?;
    }
    if !link_parts.is_empty() {
        // A root component reads as an empty first part: "/" + the rest.
        let joined = link_parts.join("/");
        rr.link = Some(if joined.is_empty() {
            "/".to_owned()
        } else {
            joined
        });
    }
    Ok(rr)
}

/// Whether an SVD's escape sequences name a Joliet (UCS-2) level.
fn is_joliet(escapes: &[u8]) -> bool {
    escapes
        .windows(3)
        .any(|w| matches!(w, b"%/@" | b"%/C" | b"%/E"))
}

/// Which names are used.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Names {
    RockRidge,
    Joliet,
    Iso,
}

/// A name of the chosen kind as UTF-8 (or raw) bytes, without a `;1` version.
fn file_name(rec: &Record, names: Names, rr: &RockRidge) -> Vec<u8> {
    if names == Names::RockRidge
        && let Some(n) = &rr.name
    {
        return n.clone();
    }
    let mut n = if names == Names::Joliet {
        let units: Vec<u16> = rec
            .name
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        String::from_utf16_lossy(&units).into_bytes()
    } else {
        rec.name.clone()
    };
    if let Some(semi) = n.iter().rposition(|&b| b == b';') {
        n.truncate(semi);
    }
    // "README." is "README" with an empty extension.
    if names == Names::Iso && n.last() == Some(&b'.') && rec.flags & FLAG_DIR == 0 {
        n.pop();
    }
    n
}

/// Records of a directory, without `.` and `..`.
fn directory(input: &mut SharedInput, extent: u64, size: u64, path: &Path) -> Result<Vec<Record>> {
    let size = size.min(MAX_DIR);
    let data = read_at(input, extent * SECTOR, size as usize, path)?;
    let mut out = Vec::new();
    let mut p = 0usize;
    while p < data.len() {
        if data[p] == 0 {
            // Records never cross sectors: the rest of this one is padding.
            p = (p / SECTOR as usize + 1) * SECTOR as usize;
            continue;
        }
        let Some(rec) = parse_record(&data[p..]) else {
            return Err(corrupt("bad directory record", path));
        };
        p += usize::from(data[p]);
        if rec.name == [0] || rec.name == [1] {
            continue;
        }
        out.push(rec);
    }
    Ok(out)
}

/// One listed item before names are decoded.
struct Item {
    raw: Vec<u8>,
    kind: EntryKind,
    size: u64,
    mtime: Option<SystemTime>,
    mode: Option<u32>,
    link: Option<String>,
    pieces: Vec<(u64, u64)>,
}

pub(crate) fn open(path: &Path, opts: &OpenOptions, ctx: &JobCtx) -> Result<Stored> {
    let mut input = SharedInput::open(path)?;
    let size = input.len();
    let mut primary = None;
    let mut joliet = None;
    for i in 0..MAX_DESCRIPTORS {
        let pos = (16 + i) * SECTOR;
        if pos + SECTOR > size {
            break;
        }
        let d = read_at(&mut input, pos, SECTOR as usize, path)?;
        if &d[1..6] != b"CD001" {
            break;
        }
        match d[0] {
            1 => {
                primary.get_or_insert(d);
            }
            2 if is_joliet(&d[88..120]) => {
                joliet.get_or_insert(d);
            }
            255 => break,
            _ => {}
        }
    }
    let primary = primary.ok_or_else(|| corrupt("no primary volume descriptor", path))?;
    let root = parse_record(&primary[156..190]).ok_or_else(|| corrupt("bad root record", path))?;
    // Rock Ridge announces itself with an SP entry in the root's "." record.
    let first = read_at(&mut input, root.extent * SECTOR, 256, path)?;
    let rock = parse_record(&first).is_some_and(|dot| dot.system_use.starts_with(b"SP"));
    let (names, descriptor) = match (&joliet, rock) {
        (_, true) => (Names::RockRidge, &primary),
        (Some(j), false) => (Names::Joliet, j),
        (None, false) => (Names::Iso, &primary),
    };
    let root =
        parse_record(&descriptor[156..190]).ok_or_else(|| corrupt("bad root record", path))?;

    let mut items: Vec<Item> = Vec::new();
    let mut visited: HashSet<u64> = HashSet::new();
    let mut stack: Vec<(u64, u64, Vec<u8>, usize)> = vec![(root.extent, root.size, Vec::new(), 0)];
    while let Some((extent, dir_size, prefix, depth)) = stack.pop() {
        ctx.check()?;
        if depth > MAX_DEPTH || !visited.insert(extent) {
            continue;
        }
        let records = directory(&mut input, extent, dir_size, path)?;
        // Pieces of a multi-extent file gathered so far.
        let mut pieces: Vec<(u64, u64)> = Vec::new();
        for rec in records {
            let rr = if names == Names::RockRidge {
                rock_ridge(&rec.system_use, &mut input, path)?
            } else {
                RockRidge::default()
            };
            if rr.relocated {
                continue;
            }
            let name = file_name(&rec, names, &rr);
            if name.is_empty() || name == b"." || name == b".." {
                continue;
            }
            if names == Names::RockRidge && depth == 0 && name == b"rr_moved" {
                continue;
            }
            let mut full = prefix.clone();
            full.extend_from_slice(&name);
            let mtime = rr.mtime.or(rec.mtime);
            let mode = rr.mode.map(|m| m & 0o7777);
            if rec.flags & FLAG_DIR != 0 || rr.child.is_some() {
                // A relocated directory: its records are where CL points, its size in its own
                // "." record there.
                let (dir_extent, dir_len) = match rr.child {
                    Some(c) => {
                        let dot = read_at(&mut input, c * SECTOR, 256, path)?;
                        let dot = parse_record(&dot)
                            .ok_or_else(|| corrupt("bad relocated directory", path))?;
                        (c, dot.size)
                    }
                    None => (rec.extent, rec.size),
                };
                let mut child = full.clone();
                child.push(b'/');
                stack.push((dir_extent, dir_len, child, depth + 1));
                full.push(b'/');
                items.push(Item {
                    raw: full,
                    kind: EntryKind::Dir,
                    size: 0,
                    mtime,
                    mode,
                    link: None,
                    pieces: Vec::new(),
                });
            } else {
                pieces.push((rec.extent * SECTOR, rec.size));
                if rec.flags & FLAG_MORE != 0 {
                    continue;
                }
                let parts = std::mem::take(&mut pieces);
                let kind = if rr.link.is_some() {
                    EntryKind::Symlink
                } else if rr
                    .mode
                    .is_some_and(|m| m & 0o170000 != 0 && m & 0o170000 != 0o100000)
                {
                    EntryKind::Other
                } else {
                    EntryKind::File
                };
                items.push(Item {
                    raw: full,
                    kind,
                    size: if kind == EntryKind::File {
                        parts.iter().map(|p| p.1).sum()
                    } else {
                        0
                    },
                    mtime,
                    mode,
                    link: rr.link,
                    pieces: parts,
                });
            }
            ctx.policy.check_entries(items.len() as u64)?;
        }
    }
    // Directories first come out in stack order: list in path order instead.
    items.sort_by(|a, b| a.raw.cmp(&b.raw));

    let raw: Vec<Vec<u8>> = items.iter().map(|i| i.raw.clone()).collect();
    let (decoded, enc) = decode_names(&raw, opts);
    let mut entries = Vec::with_capacity(items.len());
    let mut ranges = Vec::with_capacity(items.len());
    let mut chains: HashMap<EntryId, Vec<(u64, u64)>> = HashMap::new();
    for (item, name) in items.into_iter().zip(decoded) {
        let id = entries.len() as EntryId;
        let mut e = Entry::new(id, name, item.kind);
        e.raw_name = item.raw;
        e.size = item.size;
        e.packed_size = (item.kind == EntryKind::File).then_some(item.size);
        e.mtime = item.mtime;
        e.unix_mode = item.mode;
        e.link_target = item.link;
        let mut pieces = item.pieces;
        // Pieces that follow each other are one range.
        pieces.dedup_by(|b, a| {
            let joined = a.0 + a.1 == b.0;
            if joined {
                a.1 += b.1;
            }
            joined
        });
        match (item.kind, pieces.len()) {
            (EntryKind::File, 1) => ranges.push(Some(pieces[0])),
            (EntryKind::File, n) if n > 1 => {
                ranges.push(None);
                chains.insert(id, pieces);
            }
            _ => ranges.push(None),
        }
        entries.push(e);
    }

    let mut info = ArchiveInfo::new(path, FormatId::Iso, size);
    info.encoding = enc;
    info.volumes = volume::split_volumes(path).map_or(1, |v| v.len() as u32);
    info.add_method("Store");
    let label = String::from_utf8_lossy(&primary[40..72]).trim().to_owned();
    if !label.is_empty() {
        info.comment = Some(label);
    }
    Ok(Stored {
        in_place: info.volumes == 1,
        info,
        entries: entries.into(),
        ranges: ranges.into(),
        chains: Arc::new(chains),
        codings: Arc::from([]),
        checks: Arc::from([]),
        input,
    })
}
