//! Unix `ar` archives (`.a`, `.lib`) and Debian packages built on them (`docs/03-formats.md`
//! §3). GNU (`name/`, `//` long-name table, `/123` references) and BSD (`#1/len`, the name
//! before the data) member names are understood; symbol tables are skipped.

use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};

use omnizip_core::{
    ArchiveInfo, Codec, Entry, EntryKind, Error, ErrorCode, FormatId, JobCtx, OpenOptions, Result,
    timeutil, volume::SharedInput,
};

use crate::common::{Stored, Streamed, Walk, corrupt, decode_names, entry_path, read_at};

const MAGIC: &[u8] = b"!<arch>\n";

/// One member: its name and where its data lies.
pub(crate) struct Member {
    pub name: Vec<u8>,
    pub mtime: u64,
    pub mode: u32,
    pub offset: u64,
    pub size: u64,
}

fn field(h: &[u8]) -> &str {
    std::str::from_utf8(h).unwrap_or("").trim()
}

/// Lists the members of the archive in `input`.
pub(crate) fn members(input: &mut SharedInput, path: &Path, ctx: &JobCtx) -> Result<Vec<Member>> {
    let len = input.len();
    if read_at(input, 0, MAGIC.len(), path)? != MAGIC {
        return Err(corrupt("not an ar archive", path));
    }
    let mut out = Vec::new();
    let mut long_names: Vec<u8> = Vec::new();
    let mut pos = MAGIC.len() as u64;
    while pos + 60 <= len {
        ctx.check()?;
        let h = read_at(input, pos, 60, path)?;
        if &h[58..60] != b"`\n" {
            return Err(corrupt("bad member header", path));
        }
        let raw_name = field(&h[..16]).to_owned();
        let size: u64 = field(&h[48..58])
            .parse()
            .map_err(|_| corrupt("bad member size", path))?;
        let mut offset = pos + 60;
        let next = offset
            .checked_add(size)
            .map(|e| e + (size & 1))
            .ok_or_else(|| corrupt("bad member size", path))?;
        if offset + size > len {
            return Err(corrupt("truncated", path));
        }
        let mtime = field(&h[16..28]).parse().unwrap_or(0);
        let mode = u32::from_str_radix(field(&h[40..48]), 8).unwrap_or(0o644);
        pos = next;

        let mut data_size = size;
        let name: Vec<u8> = match raw_name.as_str() {
            // Symbol tables (GNU, 64-bit GNU, BSD).
            "/" | "/SYM64/" | "__.SYMDEF" | "__.SYMDEF SORTED" => continue,
            // GNU long-name table.
            "//" => {
                long_names = read_at(input, offset, usize::try_from(size).unwrap_or(0), path)?;
                continue;
            }
            n if n.starts_with('/') && n[1..].bytes().all(|b| b.is_ascii_digit()) => {
                let at: usize = n[1..].parse().unwrap_or(usize::MAX);
                let rest = long_names
                    .get(at..)
                    .ok_or_else(|| corrupt("bad long name reference", path))?;
                let end = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
                let mut n = rest[..end].to_vec();
                if n.last() == Some(&b'/') {
                    n.pop();
                }
                n
            }
            n if n.starts_with("#1/") => {
                let name_len: u64 = n[3..].parse().map_err(|_| corrupt("bad BSD name", path))?;
                if name_len > size {
                    return Err(corrupt("bad BSD name", path));
                }
                let mut n = read_at(input, offset, name_len as usize, path)?;
                while n.last() == Some(&0) {
                    n.pop();
                }
                offset += name_len;
                data_size -= name_len;
                n
            }
            n => n.strip_suffix('/').unwrap_or(n).as_bytes().to_vec(),
        };
        out.push(Member {
            name,
            mtime,
            mode,
            offset,
            size: data_size,
        });
        ctx.policy.check_entries(out.len() as u64)?;
    }
    Ok(out)
}

/// An `ar` archive: every member is a file stored as it is.
pub(crate) fn open_ar(path: &Path, opts: &OpenOptions, ctx: &JobCtx) -> Result<Stored> {
    let mut input = SharedInput::open(path)?;
    let size = input.len();
    let members = members(&mut input, path, ctx)?;
    let raw: Vec<Vec<u8>> = members.iter().map(|m| m.name.clone()).collect();
    let (names, enc) = decode_names(&raw, opts);
    let mut info = ArchiveInfo::new(path, FormatId::Ar, size);
    info.encoding = enc;
    info.add_method("Store");
    let mut entries = Vec::with_capacity(members.len());
    let mut ranges = Vec::with_capacity(members.len());
    for (m, name) in members.iter().zip(names) {
        let Some(p) = entry_path(&name, EntryKind::File) else {
            continue;
        };
        let mut e = Entry::new(entries.len() as u32, p, EntryKind::File);
        e.raw_name = m.name.clone();
        e.size = m.size;
        e.packed_size = Some(m.size);
        e.mtime = (m.mtime > 0).then(|| timeutil::from_unix_secs(m.mtime));
        e.unix_mode = Some(m.mode & 0o7777);
        entries.push(e);
        ranges.push(Some((m.offset, m.size)));
    }
    Ok(Stored {
        info,
        entries: entries.into(),
        ranges: ranges.into(),
        chains: Default::default(),
        codings: Arc::from([]),
        checks: Arc::from([]),
        input,
        in_place: true,
    })
}

/// The codec of a Debian package member (`data.tar.xz`).
fn member_codec(name: &[u8]) -> Option<Option<Codec>> {
    let n = std::str::from_utf8(name).ok()?;
    let ext = n
        .strip_prefix("control.tar")
        .or_else(|| n.strip_prefix("data.tar"))?;
    Some(match ext {
        "" => None,
        ".gz" => Some(Codec::Gzip),
        ".xz" => Some(Codec::Xz),
        ".zst" => Some(Codec::Zstd),
        ".bz2" => Some(Codec::Bzip2),
        ".lzma" => Some(Codec::Lzma),
        _ => return None,
    })
}

/// One tar inside a Debian package, and the folder its entries go into (`DEBIAN/` for the
/// control files, as `dpkg-deb -R` lays a package out).
#[derive(Clone)]
struct Part {
    prefix: &'static str,
    codec: Option<Codec>,
    offset: u64,
    size: u64,
}

struct DebWalk {
    input: SharedInput,
    parts: Vec<Part>,
    path: std::path::PathBuf,
}

/// What a tar header says, for listing.
struct TarHead {
    name: Vec<u8>,
    kind: EntryKind,
    size: u64,
    mtime: u64,
    mode: u32,
    link: Option<Vec<u8>>,
}

fn decode_err(e: std::io::Error, path: &Path) -> Error {
    let err = Error::from(e);
    if err.code() == ErrorCode::Io {
        corrupt(err, path)
    } else {
        err
    }
}

/// Called with a part, an entry header (`None` for records that are no entry) and its data.
type PartVisitor<'a> = dyn FnMut(&Part, Option<TarHead>, &mut dyn Read) -> Result<bool> + 'a;

impl DebWalk {
    /// Runs `f` over every tar entry of every part: the part, the entry's header and data.
    fn each(&self, ctx: &JobCtx, f: &mut PartVisitor<'_>) -> Result<()> {
        for part in &self.parts {
            let mut input = self.input.clone();
            input
                .seek(SeekFrom::Start(part.offset))
                .map_err(|e| Error::io(e, &self.path))?;
            let raw = input.take(part.size);
            let stream: Box<dyn Read + Send> = match part.codec {
                Some(c) => omnizip_engine_tar::codec::decoder(c, raw)
                    .map_err(|e| decode_err(e, &self.path))?,
                None => Box::new(raw),
            };
            let mut ar = tar::Archive::new(stream);
            for item in ar.entries().map_err(|e| decode_err(e, &self.path))? {
                ctx.check()?;
                let mut ent = item.map_err(|e| decode_err(e, &self.path))?;
                let h = ent.header();
                let kind = match h.entry_type() {
                    tar::EntryType::Regular | tar::EntryType::Continuous => Some(EntryKind::File),
                    tar::EntryType::Directory => Some(EntryKind::Dir),
                    tar::EntryType::Symlink => Some(EntryKind::Symlink),
                    tar::EntryType::Link => Some(EntryKind::Hardlink),
                    tar::EntryType::XGlobalHeader
                    | tar::EntryType::XHeader
                    | tar::EntryType::GNULongName
                    | tar::EntryType::GNULongLink => None,
                    _ => Some(EntryKind::Other),
                };
                let head = kind.map(|kind| TarHead {
                    name: ent.path_bytes().into_owned(),
                    kind,
                    size: if kind == EntryKind::File {
                        ent.size()
                    } else {
                        0
                    },
                    mtime: h.mtime().unwrap_or(0),
                    mode: h.mode().unwrap_or(0),
                    link: ent.link_name_bytes().map(|l| l.into_owned()),
                });
                if !f(part, head, &mut ent)? {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

impl Walk for DebWalk {
    fn walk(
        &self,
        ctx: &JobCtx,
        f: &mut dyn FnMut(usize, &mut dyn Read) -> Result<bool>,
    ) -> Result<()> {
        let mut ord = 0;
        self.each(ctx, &mut |_, _, data| {
            let go = f(ord, data)?;
            ord += 1;
            Ok(go)
        })
    }
}

/// A Debian package: the files it installs, and its control files under `DEBIAN/`.
pub(crate) fn open_deb(path: &Path, opts: &OpenOptions, ctx: &JobCtx) -> Result<Streamed> {
    let mut input = SharedInput::open(path)?;
    let size = input.len();
    let members = members(&mut input, path, ctx)?;
    let mut parts = Vec::new();
    for (prefix, want) in [("DEBIAN/", &b"control.tar"[..]), ("", &b"data.tar"[..])] {
        let m = members
            .iter()
            .find(|m| m.name.starts_with(want) && member_codec(&m.name).is_some())
            .ok_or_else(|| corrupt(format!("no {} member", String::from_utf8_lossy(want)), path))?;
        parts.push(Part {
            prefix,
            codec: member_codec(&m.name).flatten(),
            offset: m.offset,
            size: m.size,
        });
    }
    let mut info = ArchiveInfo::new(path, FormatId::Deb, size);
    if let Some(c) = parts[1].codec {
        info.add_method(c.method_name());
    }
    let walker = DebWalk {
        input,
        parts,
        path: path.to_owned(),
    };

    // List both tars once.
    let mut heads: Vec<(&'static str, TarHead)> = Vec::new();
    let mut ordinals: Vec<Option<usize>> = Vec::new();
    walker.each(ctx, &mut |part, head, _| {
        match head {
            Some(h) => {
                ordinals.push(Some(heads.len()));
                heads.push((part.prefix, h));
                ctx.policy.check_entries(heads.len() as u64)?;
            }
            None => ordinals.push(None),
        }
        Ok(true)
    })?;
    let raw: Vec<Vec<u8>> = heads.iter().map(|(_, h)| h.name.clone()).collect();
    let (names, enc) = decode_names(&raw, opts);
    info.encoding = enc;
    let decode = |b: &[u8]| match enc {
        Some(e) if std::str::from_utf8(b).is_err() => e.decode(b),
        _ => String::from_utf8_lossy(b).into_owned(),
    };

    let mut entries: Vec<Entry> = Vec::new();
    // Head index → entry id.
    let mut ids: Vec<Option<u32>> = Vec::with_capacity(heads.len());
    // The control files' folder, when the control tar has no entry for its root.
    let mut have_control_dir = false;
    for ((prefix, h), name) in heads.into_iter().zip(names) {
        let path = match entry_path(&name, h.kind) {
            Some(p) => format!("{prefix}{p}"),
            None if !prefix.is_empty() && !have_control_dir => prefix.to_owned(),
            None => {
                ids.push(None);
                continue;
            }
        };
        if path == "DEBIAN/" {
            have_control_dir = true;
        }
        let mut e = Entry::new(entries.len() as u32, path, h.kind);
        e.raw_name = h.name;
        e.size = h.size;
        e.mtime = (h.mtime > 0).then(|| timeutil::from_unix_secs(h.mtime));
        e.unix_mode = Some(h.mode & 0o7777);
        e.link_target = h.link.map(|l| {
            let t = decode(&l);
            // Hard links name another entry of the same tar.
            if h.kind == EntryKind::Hardlink {
                format!("{prefix}{}", entry_path(&t, EntryKind::File).unwrap_or(t))
            } else {
                t
            }
        });
        ids.push(Some(e.id));
        entries.push(e);
    }
    let ordinals: Vec<Option<u32>> = ordinals
        .into_iter()
        .map(|o| o.and_then(|i| ids[i]))
        .collect();
    Ok(Streamed {
        info,
        entries: entries.into(),
        ordinals: ordinals.into(),
        walker: Arc::new(walker),
    })
}
