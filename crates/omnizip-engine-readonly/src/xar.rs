//! XAR archives and the macOS flat packages built on them (`.xar`, `.pkg`;
//! `docs/03-formats.md` §3): a header, a zlib-compressed XML table of contents, and a heap with
//! each file's data, compressed on its own (zlib, bzip2, xz, lzma) or stored.

use std::{collections::HashMap, io::Read, path::Path, sync::Arc, time::SystemTime};

use omnizip_core::{
    ArchiveInfo, Codec, Entry, EntryKind, Error, ErrorCode, FormatId, JobCtx, OpenOptions, Result,
    volume::SharedInput,
};

use crate::common::{Coding, Stored, corrupt, decode_names, read_at, utc_time};

/// Largest table of contents read, compressed and decompressed.
const MAX_TOC: u64 = 64 << 20;
/// Deepest folder nesting followed.
const MAX_DEPTH: usize = 256;

/// `2026-11-11T03:43:57Z` (fractions of a second ignored).
fn iso8601(s: &str) -> Option<SystemTime> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<u32>().ok();
    utc_time(
        i64::from(n(0..4)?),
        n(5..7)?,
        n(8..10)?,
        n(11..13)?,
        n(14..16)?,
        n(17..19)?,
        0,
    )
}

fn child<'a>(n: roxmltree::Node<'a, 'a>, tag: &str) -> Option<roxmltree::Node<'a, 'a>> {
    n.children().find(|c| c.has_tag_name(tag))
}

fn text<'a>(n: roxmltree::Node<'a, 'a>, tag: &str) -> Option<&'a str> {
    child(n, tag).and_then(|c| c.text()).map(str::trim)
}

/// A `<name>`, decoded from base64 when the archive says so (names that are not UTF-8).
fn name_bytes(n: roxmltree::Node<'_, '_>) -> Option<Vec<u8>> {
    let node = child(n, "name")?;
    let t = node.text().unwrap_or("");
    if node.attribute("enctype") == Some("base64") {
        return base64(t);
    }
    Some(t.as_bytes().to_vec())
}

fn base64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let bytes: Vec<u8> = s
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            acc |= val(c)? << (18 - 6 * i);
        }
        let n = chunk.len() * 6 / 8;
        out.extend_from_slice(&acc.to_be_bytes()[1..1 + n]);
    }
    Some(out)
}

fn coding(style: &str) -> Option<Option<Coding>> {
    Some(match style {
        "application/octet-stream" | "" => None,
        "application/x-gzip" | "application/zlib" => Some(Coding::Zlib),
        "application/x-bzip2" => Some(Coding::Codec(Codec::Bzip2)),
        "application/x-xz" => Some(Coding::Codec(Codec::Xz)),
        "application/x-lzma" => Some(Coding::Codec(Codec::Lzma)),
        _ => return None,
    })
}

/// One listed file before names are decoded.
struct Item {
    raw: Vec<u8>,
    kind: EntryKind,
    size: u64,
    range: Option<(u64, u64)>,
    coding: Option<Coding>,
    mtime: Option<SystemTime>,
    mode: Option<u32>,
    link: Option<Vec<u8>>,
    /// A hard link: the `id` of the file holding the data.
    link_id: Option<String>,
    id: Option<String>,
}

pub(crate) fn open(path: &Path, opts: &OpenOptions, ctx: &JobCtx) -> Result<Stored> {
    let mut input = SharedInput::open(path)?;
    let size = input.len();
    let h = read_at(&mut input, 0, 28, path)?;
    if &h[..4] != b"xar!" {
        return Err(corrupt("not a xar archive", path));
    }
    let header_len = u64::from(u16::from_be_bytes([h[4], h[5]]));
    let toc_packed = u64::from_be_bytes(h[8..16].try_into().expect("8 bytes"));
    let toc_size = u64::from_be_bytes(h[16..24].try_into().expect("8 bytes"));
    if header_len < 28 || toc_packed > MAX_TOC || toc_size > MAX_TOC {
        return Err(corrupt("bad xar header", path));
    }
    let packed = read_at(&mut input, header_len, toc_packed as usize, path)?;
    let mut xml = Vec::with_capacity(toc_size as usize);
    flate2::read::ZlibDecoder::new(packed.as_slice())
        .take(MAX_TOC)
        .read_to_end(&mut xml)
        .map_err(|e| corrupt(e, path))?;
    let heap = header_len + toc_packed;
    let xml =
        String::from_utf8(xml).map_err(|_| corrupt("table of contents is not UTF-8", path))?;
    let doc = roxmltree::Document::parse(&xml).map_err(|e| corrupt(e, path))?;
    let toc = doc
        .root_element()
        .children()
        .find(|c| c.has_tag_name("toc"))
        .ok_or_else(|| corrupt("no table of contents", path))?;

    let mut items: Vec<Item> = Vec::new();
    let mut stack: Vec<(roxmltree::Node<'_, '_>, Vec<u8>, usize)> = toc
        .children()
        .filter(|c| c.has_tag_name("file"))
        .map(|f| (f, Vec::new(), 0))
        .collect();
    stack.reverse();
    while let Some((f, prefix, depth)) = stack.pop() {
        ctx.check()?;
        if depth > MAX_DEPTH {
            continue;
        }
        let Some(name) = name_bytes(f).filter(|n| !n.is_empty()) else {
            continue;
        };
        let mut full = prefix;
        full.extend_from_slice(&name);
        let ty = child(f, "type");
        let ty_text = ty.and_then(|t| t.text()).unwrap_or("file").trim();
        let mut kind = match ty_text {
            "directory" => EntryKind::Dir,
            "symlink" => EntryKind::Symlink,
            "hardlink" => EntryKind::Hardlink,
            "file" => EntryKind::File,
            _ => EntryKind::Other,
        };
        let mut link_id = None;
        if kind == EntryKind::Hardlink {
            match ty.and_then(|t| t.attribute("link")) {
                // The first of a group of hard links holds the data.
                Some("original") | None => kind = EntryKind::File,
                Some(id) => link_id = Some(id.to_owned()),
            }
        }
        let data = child(f, "data");
        let (mut range, mut coding_of, mut unpacked) = (None, None, 0);
        if kind == EntryKind::File
            && let Some(d) = data
        {
            let num = |tag| text(d, tag).and_then(|t| t.parse::<u64>().ok());
            let offset = num("offset").ok_or_else(|| corrupt("file without offset", path))?;
            let length = num("length").ok_or_else(|| corrupt("file without length", path))?;
            unpacked = num("size").unwrap_or(length);
            let style = child(d, "encoding")
                .and_then(|e| e.attribute("style"))
                .unwrap_or("");
            coding_of = coding(style).ok_or_else(|| {
                Error::new(ErrorCode::UnsupportedMethod)
                    .with("method", style)
                    .with("path", path.display())
            })?;
            let start = heap
                .checked_add(offset)
                .filter(|s| s.checked_add(length).is_some())
                .ok_or_else(|| corrupt("bad file offset", path))?;
            range = Some((start, length));
        }
        let link = (kind == EntryKind::Symlink)
            .then(|| {
                child(f, "link")
                    .and_then(|l| l.text())
                    .map(|t| t.as_bytes().to_vec())
            })
            .flatten();
        if kind == EntryKind::Dir {
            full.push(b'/');
            let mut subs: Vec<_> = f
                .children()
                .filter(|c| c.has_tag_name("file"))
                .map(|c| (c, full.clone(), depth + 1))
                .collect();
            subs.reverse();
            stack.extend(subs);
        }
        items.push(Item {
            raw: full,
            kind,
            size: unpacked,
            range,
            coding: coding_of,
            mtime: text(f, "mtime").and_then(iso8601),
            mode: text(f, "mode").and_then(|m| u32::from_str_radix(m, 8).ok()),
            link,
            link_id,
            id: f.attribute("id").map(str::to_owned),
        });
        ctx.policy.check_entries(items.len() as u64)?;
    }
    for it in &items {
        if let Some((start, len)) = it.range
            && start + len > size
        {
            return Err(corrupt("truncated", path));
        }
    }

    let raw: Vec<Vec<u8>> = items.iter().map(|i| i.raw.clone()).collect();
    let (names, enc) = decode_names(&raw, opts);
    let by_id: HashMap<&str, &str> = items
        .iter()
        .zip(&names)
        .filter_map(|(i, n)| Some((i.id.as_deref()?, n.as_str())))
        .collect();
    let mut entries = Vec::with_capacity(items.len());
    let mut ranges = Vec::with_capacity(items.len());
    let mut codings = Vec::with_capacity(items.len());
    let mut info = ArchiveInfo::new(path, FormatId::Xar, size);
    for (it, name) in items.iter().zip(&names) {
        let mut e = Entry::new(entries.len() as u32, name.clone(), it.kind);
        e.raw_name = it.raw.clone();
        e.size = it.size;
        e.packed_size = it.range.map(|r| r.1);
        e.mtime = it.mtime;
        e.unix_mode = it.mode.map(|m| m & 0o7777);
        e.link_target = match (&it.link, &it.link_id) {
            (Some(l), _) => Some(String::from_utf8_lossy(l).into_owned()),
            (None, Some(id)) => by_id.get(id.as_str()).map(|s| (*s).to_owned()),
            _ => None,
        };
        if it.kind == EntryKind::File {
            info.add_method(match it.coding {
                None => "Store",
                Some(Coding::Zlib) => "Deflate",
                Some(Coding::Codec(c)) => c.method_name(),
                // XAR uses none of the others.
                Some(_) => "?",
            });
        }
        entries.push(e);
        ranges.push(it.range);
        codings.push(it.coding);
    }
    info.encoding = enc;
    Ok(Stored {
        info,
        entries: entries.into(),
        ranges: ranges.into(),
        chains: Default::default(),
        codings: codings.into(),
        checks: Arc::from([]),
        input,
        in_place: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_names() {
        assert_eq!(base64("5Lit5paH").unwrap(), "中文".as_bytes());
        assert_eq!(base64("YQ==").unwrap(), b"a");
        assert_eq!(base64("YWI=").unwrap(), b"ab");
        assert!(base64("!!").is_none());
    }
}
