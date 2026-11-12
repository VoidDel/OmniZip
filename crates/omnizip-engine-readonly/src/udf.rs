//! UDF (ECMA-167 with the OSTA UDF profile, revisions 1.02 to 2.60), the file system of DVD and
//! Blu-ray images and of Windows installation images: their ISO 9660 side often holds only a
//! `README.TXT` saying the files are in UDF. Read only, files in place, like ISO 9660 (`iso`).
//!
//! The path from the image to the files: the anchor (sector 256) → the volume descriptors
//! (partitions, the logical volume and its partition maps) → the file set → the root directory's
//! file entry → file identifiers → file entries, whose allocation descriptors say where data lies.
//! UDF 2.50's metadata partition (file entries and directories kept in a file of their own) is
//! followed through that file's extents.

use std::{collections::HashSet, path::Path, time::SystemTime};

use omnizip_core::{EntryKind, JobCtx, Result, volume::SharedInput};

use crate::{
    common::{corrupt, read_at, utc_time},
    iso::Item,
};

/// Volume descriptors read at most in a sequence.
const MAX_VDS: u32 = 256;
/// Deepest directory nesting followed.
const MAX_DEPTH: usize = 128;
/// Largest directory read.
const MAX_DIR: u64 = 64 << 20;
/// Allocation extents (continuations) followed per file.
const MAX_AED: usize = 4096;

// Descriptor tags.
const TAG_AVDP: u16 = 2;
const TAG_PD: u16 = 5;
const TAG_LVD: u16 = 6;
const TAG_TD: u16 = 8;
const TAG_FSD: u16 = 256;
const TAG_FID: u16 = 257;
const TAG_AED: u16 = 258;
const TAG_FE: u16 = 261;
const TAG_EFE: u16 = 266;

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u64le(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap_or_default())
}

/// Whether `b` starts with a descriptor tag `id`: its checksum right (the tag's location is not
/// checked: images are often written for another place).
fn tag_is(b: &[u8], id: u16) -> bool {
    if b.len() < 16 || u16le(b, 0) != id {
        return false;
    }
    let sum = b[..16]
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 4)
        .fold(0u8, |s, (_, x)| s.wrapping_add(*x));
    sum == b[4]
}

/// A name in OSTA compressed Unicode: a first byte of 8 (one byte a character) or 16 (two,
/// big-endian), as UTF-8.
fn cs0(b: &[u8]) -> String {
    match b.first() {
        Some(8 | 254) => b[1..].iter().map(|&c| char::from(c)).collect(),
        Some(16 | 255) => {
            let units: Vec<u16> = b[1..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes(*c))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => String::new(),
    }
}

/// A `dstring`: compressed Unicode in a fixed field whose last byte is the length used.
fn dstring(b: &[u8]) -> String {
    let Some(&len) = b.last() else {
        return String::new();
    };
    let len = usize::from(len).min(b.len() - 1);
    cs0(&b[..len])
}

/// A 12-byte timestamp: type and time zone (minutes from UTC), year, month, day, hour,
/// minute, second.
fn timestamp(b: &[u8]) -> Option<SystemTime> {
    let tz = u16le(b, 0);
    // Type 1: local time; the zone is 12 bits, signed, -2047 meaning none.
    let zone = i64::from(((tz & 0x0fff) as i16) << 4 >> 4);
    let offset = if tz >> 12 == 1 && zone != -2047 {
        zone * 60
    } else {
        0
    };
    let year = i64::from(u16le(b, 2) as i16);
    if year == 0 {
        return None;
    }
    utc_time(
        year,
        u32::from(b[4]),
        u32::from(b[5]),
        u32::from(b[6]),
        u32::from(b[7]),
        u32::from(b[8]),
        offset,
    )
}

/// A partition as the logical volume sees it.
#[derive(Clone)]
enum Map {
    /// Blocks of a partition descriptor: its first block.
    Physical { start: u64 },
    /// UDF 2.50: blocks of the metadata file, whose extents lie in a physical partition.
    Metadata { extents: Vec<(u64, u64)> },
}

struct Volume<'a> {
    input: &'a mut SharedInput,
    path: &'a Path,
    block: u64,
    maps: Vec<Map>,
}

/// Where data lies: in the image, or (an extent recorded as unwritten) nowhere.
#[derive(Clone, Copy)]
enum Piece {
    At(u64, u64),
}

impl Volume<'_> {
    /// The image offset of block `lbn` of partition `part` (partition-map index).
    fn offset(&self, part: u16, lbn: u64) -> Result<u64> {
        match self.maps.get(usize::from(part)) {
            Some(Map::Physical { start }) => Ok((start + lbn) * self.block),
            Some(Map::Metadata { extents }) => {
                // The metadata file's bytes, extent by extent.
                let mut at = lbn * self.block;
                for &(off, len) in extents {
                    if at < len {
                        return Ok(off + at);
                    }
                    at -= len;
                }
                Err(corrupt("block outside the metadata file", self.path))
            }
            None => Err(corrupt("unknown partition", self.path)),
        }
    }

    fn read(&mut self, part: u16, lbn: u64, len: usize) -> Result<Vec<u8>> {
        let at = self.offset(part, lbn)?;
        read_at(self.input, at, len, self.path)
    }

    /// The bytes of `pieces` one after another, at most `max`.
    fn gather(&mut self, pieces: &[Piece], max: u64) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for &Piece::At(off, len) in pieces {
            let take = len.min(max.saturating_sub(out.len() as u64));
            if take == 0 {
                break;
            }
            out.extend(read_at(self.input, off, take as usize, self.path)?);
        }
        Ok(out)
    }
}

/// A file entry, read.
struct Node {
    kind: u8,
    size: u64,
    mtime: Option<SystemTime>,
    pieces: Vec<Piece>,
}

/// File type in the ICB tag: a directory.
const FT_DIR: u8 = 4;
/// A symbolic link (its target is a path in UDF's own form; listed as a link without one).
const FT_SYMLINK: u8 = 12;

/// Reads the file entry at `part`/`lbn` and where its data lies.
fn node(v: &mut Volume, part: u16, lbn: u64) -> Result<Node> {
    let block = v.block as usize;
    let fe = v.read(part, lbn, block)?;
    let extended = tag_is(&fe, TAG_EFE);
    if !extended && !tag_is(&fe, TAG_FE) {
        return Err(corrupt("bad file entry", v.path));
    }
    let kind = fe[27];
    let ad_type = u16le(&fe, 34) & 7;
    let size = u64le(&fe, 56);
    let (mtime_at, ea_at, ad_len_at, base) = if extended {
        (92, 208, 212, 216)
    } else {
        (84, 168, 172, 176)
    };
    let mtime = timestamp(&fe[mtime_at..mtime_at + 12]);
    let ea = u32le(&fe, ea_at) as usize;
    let ad_len = u32le(&fe, ad_len_at) as usize;
    let start = base + ea;
    if start + ad_len > fe.len() {
        return Err(corrupt("bad file entry", v.path));
    }
    let mut pieces = Vec::new();
    if ad_type == 3 {
        // The data is in the entry itself.
        let at = v.offset(part, lbn)? + start as u64;
        pieces.push(Piece::At(at, (ad_len as u64).min(size)));
    } else {
        allocation(v, part, &fe[start..start + ad_len], ad_type, &mut pieces)?;
    }
    Ok(Node {
        kind,
        size,
        mtime,
        pieces,
    })
}

/// The pieces allocation descriptors of type `ad_type` (0 short, 1 long, 2 extended) name,
/// following continuations. `part` is the partition of the file entry (short descriptors' own).
fn allocation(
    v: &mut Volume,
    part: u16,
    ads: &[u8],
    ad_type: u16,
    out: &mut Vec<Piece>,
) -> Result<()> {
    let mut ads = ads.to_vec();
    let mut part_now = part;
    for _ in 0..MAX_AED {
        let step = match ad_type {
            0 => 8,
            1 => 16,
            2 => 20,
            _ => return Err(corrupt("bad allocation descriptor type", v.path)),
        };
        let mut next = None;
        for d in ads.chunks_exact(step) {
            let raw = u32le(d, 0);
            let (len, kind) = (u64::from(raw & 0x3fff_ffff), raw >> 30);
            if len == 0 {
                break;
            }
            let (lbn, p) = match ad_type {
                0 => (u64::from(u32le(d, 4)), part_now),
                1 => (u64::from(u32le(d, 4)), u16le(d, 8)),
                // Extended: extent length, recorded length, information length, then a long
                // address.
                _ => (u64::from(u32le(d, 12)), u16le(d, 16)),
            };
            match kind {
                0 => out.push(Piece::At(v.offset(p, lbn)?, len)),
                3 => next = Some((p, lbn, len)),
                // Allocated but not written, or not allocated: no data here (an image written
                // in full has none).
                _ => {}
            }
        }
        let Some((p, lbn, len)) = next else {
            return Ok(());
        };
        // An allocation extent: a tag, the previous extent's place, its length, descriptors.
        let aed = v.read(p, lbn, (len.min(v.block) as usize).max(24))?;
        if !tag_is(&aed, TAG_AED) {
            return Err(corrupt("bad allocation extent", v.path));
        }
        let n = (u32le(&aed, 20) as usize).min(aed.len() - 24);
        ads = aed[24..24 + n].to_vec();
        part_now = p;
    }
    Err(corrupt("too many allocation extents", v.path))
}

/// Joins pieces that follow each other.
fn joined(pieces: &[Piece], size: u64) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    let mut left = size;
    for &Piece::At(off, len) in pieces {
        let len = len.min(left);
        if len == 0 {
            break;
        }
        left -= len;
        match out.last_mut() {
            Some(last) if last.0 + last.1 == off => last.1 += len,
            _ => out.push((off, len)),
        }
    }
    out
}

/// Whether the image has a UDF volume: `NSR02` or `NSR03` among its volume structure
/// descriptors (after ISO 9660's, from sector 16).
pub(crate) fn present(input: &mut SharedInput, path: &Path) -> bool {
    let size = input.len();
    (16..64u64).any(|s| {
        let pos = s * 2048;
        pos + 6 <= size
            && read_at(input, pos, 6, path)
                .is_ok_and(|d| &d[1..6] == b"NSR02" || &d[1..6] == b"NSR03")
    })
}

/// The volume's label and its files and folders.
pub(crate) fn read(
    input: &mut SharedInput,
    path: &Path,
    ctx: &JobCtx,
) -> Result<(String, Vec<Item>)> {
    // The anchor: sector 256 (also the last, or 256 before it), at the block sizes images use.
    let size = input.len();
    let mut found = None;
    'search: for block in [2048u64, 512, 4096, 1024] {
        let last = size / block;
        for at in [256, last.saturating_sub(1), last.saturating_sub(257)] {
            if at == 0 || (at + 1) * block > size {
                continue;
            }
            let d = read_at(input, at * block, 512, path)?;
            if tag_is(&d, TAG_AVDP) {
                found = Some((block, d));
                break 'search;
            }
        }
    }
    let (block, avdp) = found.ok_or_else(|| corrupt("no UDF anchor", path))?;
    let (vds_len, vds_at) = (u64::from(u32le(&avdp, 16)), u64::from(u32le(&avdp, 20)));

    // The main volume descriptor sequence: partitions and the logical volume.
    let mut partitions: Vec<(u16, u64)> = Vec::new();
    let mut lvd = None;
    for i in 0..(vds_len / block).min(u64::from(MAX_VDS)) {
        let d = read_at(input, (vds_at + i) * block, block as usize, path)?;
        if tag_is(&d, TAG_PD) {
            partitions.push((u16le(&d, 22), u64::from(u32le(&d, 188))));
        } else if tag_is(&d, TAG_LVD) {
            lvd.get_or_insert(d);
        } else if tag_is(&d, TAG_TD) {
            break;
        }
    }
    let lvd = lvd.ok_or_else(|| corrupt("no logical volume descriptor", path))?;
    let label = dstring(&lvd[84..212]);
    let lv_block = u64::from(u32le(&lvd, 212));
    let block = if lv_block.is_power_of_two() && (512..=65536).contains(&lv_block) {
        lv_block
    } else {
        block
    };
    let physical = |n: u16| partitions.iter().find(|p| p.0 == n).map(|p| p.1);

    // Partition maps: type 1 a partition descriptor's blocks; type 2 by its identifier (only
    // the metadata partition is read: virtual and sparable ones are for discs being written).
    let count = u32le(&lvd, 268) as usize;
    let mut maps: Vec<Map> = Vec::with_capacity(count);
    let mut metadata: Vec<(usize, u16, u64)> = Vec::new();
    let mut at = 440;
    for _ in 0..count {
        let (kind, len) = (lvd.get(at).copied(), lvd.get(at + 1).copied());
        let (Some(kind), Some(len)) = (kind, len) else {
            break;
        };
        let len = usize::from(len);
        if len < 6 || at + len > lvd.len() {
            break;
        }
        let m = &lvd[at..at + len];
        match kind {
            1 => maps.push(Map::Physical {
                start: physical(u16le(m, 4)).unwrap_or(0),
            }),
            2 if len >= 44 && m.get(5..28) == Some(b"*UDF Metadata Partition".as_slice()) => {
                metadata.push((maps.len(), u16le(m, 38), u64::from(u32le(m, 40))));
                maps.push(Map::Physical { start: 0 });
            }
            _ if len >= 40 => maps.push(Map::Physical {
                start: physical(u16le(m, 38)).unwrap_or(0),
            }),
            _ => maps.push(Map::Physical { start: 0 }),
        }
        at += len;
    }
    let mut v = Volume {
        input,
        path,
        block,
        maps,
    };
    // A metadata partition: its file's extents, from the physical partition it lies in.
    for (index, part_num, file_lbn) in metadata {
        let phys = v
            .maps
            .iter()
            .position(
                |m| matches!(m, Map::Physical { start } if Some(*start) == physical(part_num)),
            )
            .unwrap_or(0) as u16;
        let file = node(&mut v, phys, file_lbn)?;
        let extents = file.pieces.iter().map(|&Piece::At(o, l)| (o, l)).collect();
        v.maps[index] = Map::Metadata { extents };
    }

    // The file set: where the root directory is.
    let (fsd_lbn, fsd_part) = (u64::from(u32le(&lvd, 252)), u16le(&lvd, 256));
    let fsd = v.read(fsd_part, fsd_lbn, block as usize)?;
    if !tag_is(&fsd, TAG_FSD) {
        return Err(corrupt("no file set descriptor", path));
    }
    let (root_lbn, root_part) = (u64::from(u32le(&fsd, 404)), u16le(&fsd, 408));

    let mut items = Vec::new();
    let mut visited: HashSet<(u16, u64)> = HashSet::new();
    let mut stack: Vec<(u16, u64, Vec<u8>, usize)> = vec![(root_part, root_lbn, Vec::new(), 0)];
    while let Some((part, lbn, prefix, depth)) = stack.pop() {
        ctx.check()?;
        if depth > MAX_DEPTH || !visited.insert((part, lbn)) {
            continue;
        }
        let dir = node(&mut v, part, lbn)?;
        let data = v.gather(&dir.pieces, dir.size.min(MAX_DIR))?;
        let mut p = 0usize;
        while p + 38 <= data.len() {
            let fid = &data[p..];
            if !tag_is(fid, TAG_FID) {
                break;
            }
            let traits = fid[18];
            let name_len = usize::from(fid[19]);
            let (child_lbn, child_part) = (u64::from(u32le(fid, 24)), u16le(fid, 28));
            let iu = usize::from(u16le(fid, 36));
            let total = (38 + iu + name_len).div_ceil(4) * 4;
            let name_at = 38 + iu;
            let name = fid
                .get(name_at..name_at + name_len)
                .map(cs0)
                .unwrap_or_default();
            p += total;
            // Deleted, or the parent ("..").
            if traits & 0x04 != 0 || traits & 0x08 != 0 || name.is_empty() {
                continue;
            }
            let mut full = prefix.clone();
            full.extend_from_slice(name.replace('/', "_").as_bytes());
            let child = node(&mut v, child_part, child_lbn)?;
            if traits & 0x02 != 0 || child.kind == FT_DIR {
                let mut sub = full.clone();
                sub.push(b'/');
                stack.push((child_part, child_lbn, sub, depth + 1));
                full.push(b'/');
                items.push(Item {
                    raw: full,
                    kind: EntryKind::Dir,
                    size: 0,
                    mtime: child.mtime,
                    mode: None,
                    link: None,
                    pieces: Vec::new(),
                });
            } else {
                let kind = if child.kind == FT_SYMLINK {
                    EntryKind::Symlink
                } else {
                    EntryKind::File
                };
                let size = if kind == EntryKind::File {
                    child.size
                } else {
                    0
                };
                items.push(Item {
                    raw: full,
                    kind,
                    size,
                    mtime: child.mtime,
                    mode: None,
                    link: None,
                    pieces: if kind == EntryKind::File {
                        joined(&child.pieces, size)
                    } else {
                        Vec::new()
                    },
                });
            }
            ctx.policy.check_entries(items.len() as u64)?;
        }
    }
    Ok((label, items))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_times() {
        assert_eq!(cs0(b"\x08README.TXT"), "README.TXT");
        assert_eq!(cs0(b"\x10\x4e\x2d\x65\x87"), "中文");
        assert_eq!(dstring(b"\x08ESD-ISO\0\0\0\0\0\0\0\0\x08"), "ESD-ISO");
        // 2025-07-13 08:35:00 at UTC+8 (480 minutes, type 1).
        let t = [0xe0, 0x11, 0xe9, 0x07, 7, 13, 8, 35, 0, 0, 0, 0];
        let want = utc_time(2025, 7, 13, 8, 35, 0, 480 * 60);
        assert_eq!(timestamp(&t), want);
    }

    #[test]
    fn tags_by_checksum() {
        let mut t = [0u8; 16];
        t[0] = 2;
        t[4] = 2;
        assert!(tag_is(&t, TAG_AVDP));
        t[4] = 3;
        assert!(!tag_is(&t, TAG_AVDP));
    }

    #[test]
    fn pieces_join_and_stop_at_the_size() {
        let p = [Piece::At(100, 50), Piece::At(150, 50), Piece::At(400, 50)];
        assert_eq!(joined(&p, 120), vec![(100, 100), (400, 20)]);
        assert_eq!(joined(&p, 150), vec![(100, 100), (400, 50)]);
    }
}
