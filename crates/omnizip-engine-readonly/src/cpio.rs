//! CPIO archives (`docs/03-formats.md` §3): the portable ASCII formats (`newc` 070701, `crc`
//! 070702, `odc` 070707) and the old binary format in either byte order, as initramfs images
//! and RPM payloads use them.
//!
//! The parser reads forward only, so it serves both a plain `.cpio` file (whose file data is
//! then read in place) and the decompressed payload of an RPM package.

use std::{
    collections::HashMap,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};

use omnizip_core::{
    ArchiveInfo, Entry, EntryKind, FormatId, JobCtx, OpenOptions, Result, timeutil,
    volume::{self, SharedInput},
};

use crate::common::{Stored, corrupt, decode_names, entry_path};

/// Longest name accepted (with its terminating NUL): more is damage, not a path.
const MAX_NAME: u64 = 64 << 10;
/// Longest symbolic-link target read while listing.
const MAX_LINK: u64 = 64 << 10;

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;

/// An input the parser can skip forward in: seeking where it can, reading otherwise.
pub(crate) trait Skip: Read + Sized {
    fn skip(&mut self, n: u64) -> io::Result<()> {
        let got = io::copy(&mut self.by_ref().take(n), &mut io::sink())?;
        if got < n {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(())
    }
}

impl Skip for SharedInput {
    fn skip(&mut self, n: u64) -> io::Result<()> {
        let i = i64::try_from(n).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        self.seek(SeekFrom::Current(i)).map(|_| ())
    }
}

impl Skip for Box<dyn Read + Send + '_> {}

/// One record's header.
pub(crate) struct Header {
    pub name: Vec<u8>,
    pub mode: u32,
    pub mtime: u64,
    pub size: u64,
    pub dev: u64,
    pub ino: u64,
    pub nlink: u32,
    /// Where the data starts in the input.
    pub data_pos: u64,
}

impl Header {
    pub fn kind(&self) -> EntryKind {
        match self.mode & S_IFMT {
            S_IFDIR => EntryKind::Dir,
            S_IFREG => EntryKind::File,
            S_IFLNK => EntryKind::Symlink,
            // Very old archives leave the type out of regular files.
            0 => EntryKind::File,
            _ => EntryKind::Other,
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    /// `newc` and `crc`: hex fields, 4-byte alignment.
    Newc,
    /// `odc`: octal fields, no alignment.
    Odc,
    /// Old binary, 2-byte alignment, in the given byte order.
    Bin { big_endian: bool },
}

/// Reads records one after another.
pub(crate) struct Cpio<R> {
    r: R,
    pos: u64,
    /// Data of the current record not read yet, and the padding after it.
    left: u64,
    pad: u64,
    done: bool,
}

fn hex(b: &[u8]) -> io::Result<u64> {
    u64::from_str_radix(std::str::from_utf8(b).unwrap_or("x"), 16)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad cpio header"))
}

fn oct(b: &[u8]) -> io::Result<u64> {
    u64::from_str_radix(std::str::from_utf8(b).unwrap_or("x"), 8)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad cpio header"))
}

impl<R: Skip> Cpio<R> {
    pub fn new(r: R) -> Self {
        Self {
            r,
            pos: 0,
            left: 0,
            pad: 0,
            done: false,
        }
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.r.read_exact(buf)?;
        self.pos += buf.len() as u64;
        Ok(())
    }

    fn skip(&mut self, n: u64) -> io::Result<()> {
        if n > 0 {
            self.r.skip(n)?;
            self.pos += n;
        }
        Ok(())
    }

    /// The next record, after skipping what is left of the previous one. `None` at the
    /// trailer, or where the input ends between records.
    pub fn next(&mut self) -> io::Result<Option<Header>> {
        if self.done {
            return Ok(None);
        }
        self.skip(self.left + self.pad)?;
        self.left = 0;
        self.pad = 0;
        let mut magic = [0u8; 6];
        match self.r.read(&mut magic[..1])? {
            0 => {
                self.done = true;
                return Ok(None);
            }
            _ => self.pos += 1,
        }
        self.read_exact(&mut magic[1..2])?;
        let kind = match magic[..2] {
            [0xC7, 0x71] => Kind::Bin { big_endian: false },
            [0x71, 0xC7] => Kind::Bin { big_endian: true },
            _ => {
                self.read_exact(&mut magic[2..])?;
                match &magic {
                    b"070701" | b"070702" => Kind::Newc,
                    b"070707" => Kind::Odc,
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "bad cpio header",
                        ));
                    }
                }
            }
        };
        let (mode, mtime, size, namesize, dev, ino, nlink) = match kind {
            Kind::Newc => {
                let mut h = [0u8; 104];
                self.read_exact(&mut h)?;
                let f = |i: usize| hex(&h[i * 8..i * 8 + 8]);
                (
                    f(1)? as u32,
                    f(5)?,
                    f(6)?,
                    f(11)?,
                    (f(7)? << 32) | f(8)?,
                    f(0)?,
                    f(4)? as u32,
                )
            }
            Kind::Odc => {
                let mut h = [0u8; 70];
                self.read_exact(&mut h)?;
                (
                    oct(&h[12..18])? as u32,
                    oct(&h[42..53])?,
                    oct(&h[59..70])?,
                    oct(&h[53..59])?,
                    oct(&h[0..6])?,
                    oct(&h[6..12])?,
                    oct(&h[30..36])? as u32,
                )
            }
            Kind::Bin { big_endian } => {
                let mut h = [0u8; 24];
                self.read_exact(&mut h)?;
                let w = |i: usize| {
                    let b = [h[i * 2], h[i * 2 + 1]];
                    u64::from(if big_endian {
                        u16::from_be_bytes(b)
                    } else {
                        u16::from_le_bytes(b)
                    })
                };
                // dev ino mode uid gid nlink rdev mtime(2) namesize filesize(2)
                (
                    w(2) as u32,
                    (w(7) << 16) | w(8),
                    (w(10) << 16) | w(11),
                    w(9),
                    w(0),
                    w(1),
                    w(5) as u32,
                )
            }
        };
        if namesize == 0 || namesize > MAX_NAME {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad cpio name"));
        }
        let mut name = vec![0u8; namesize as usize];
        self.read_exact(&mut name)?;
        while name.last() == Some(&0) {
            name.pop();
        }
        let align = match kind {
            Kind::Newc => 4,
            Kind::Odc => 1,
            Kind::Bin { .. } => 2,
        };
        let pad_to = |p: u64| (align - p % align) % align;
        self.skip(pad_to(self.pos))?;
        if name == b"TRAILER!!!" {
            self.done = true;
            return Ok(None);
        }
        self.left = size;
        self.pad = pad_to(self.pos + size);
        Ok(Some(Header {
            name,
            mode,
            mtime,
            size,
            dev,
            ino,
            nlink,
            data_pos: self.pos,
        }))
    }

    /// The data of the current record.
    pub fn data(&mut self) -> Data<'_, R> {
        Data { c: self }
    }
}

/// Reads the data of the current record.
pub(crate) struct Data<'a, R> {
    c: &'a mut Cpio<R>,
}

impl<R: Skip> Read for Data<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let want = buf
            .len()
            .min(usize::try_from(self.c.left).unwrap_or(usize::MAX));
        if want == 0 {
            return Ok(0);
        }
        let n = self.c.r.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.c.left -= n as u64;
        self.c.pos += n as u64;
        Ok(n)
    }
}

/// A listed record: the header, and a symbolic link's target.
pub(crate) struct Listed {
    pub head: Header,
    pub link: Option<Vec<u8>>,
}

/// Lists every record of `cpio`. Each record's ordinal is its position in the result.
pub(crate) fn list<R: Skip>(cpio: &mut Cpio<R>, path: &Path, ctx: &JobCtx) -> Result<Vec<Listed>> {
    let mut out = Vec::new();
    while let Some(head) = cpio.next().map_err(|e| corrupt(e, path))? {
        ctx.check()?;
        let link = if head.kind() == EntryKind::Symlink && head.size <= MAX_LINK {
            let mut t = Vec::new();
            cpio.data()
                .read_to_end(&mut t)
                .map_err(|e| corrupt(e, path))?;
            Some(t)
        } else {
            None
        };
        out.push(Listed { head, link });
        ctx.policy.check_entries(out.len() as u64)?;
    }
    Ok(out)
}

/// Entries for listed records, with each record's entry id. Hard-linked files keep their data
/// once, with one of the names (the last, in the `newc` format); the others become links to it.
pub(crate) fn entries(
    listed: &[Listed],
    opts: &OpenOptions,
) -> (
    Vec<Entry>,
    Vec<Option<u32>>,
    Option<omnizip_core::encoding::NameEncoding>,
) {
    let raw: Vec<Vec<u8>> = listed.iter().map(|l| l.head.name.clone()).collect();
    let (names, enc) = decode_names(&raw, opts);
    let paths: Vec<Option<String>> = listed
        .iter()
        .zip(&names)
        .map(|(l, n)| entry_path(n, l.head.kind()))
        .collect();
    // Which record of each hard-link group carries the data.
    let mut holder: HashMap<(u64, u64), usize> = HashMap::new();
    for (i, l) in listed.iter().enumerate() {
        let h = &l.head;
        if h.kind() == EntryKind::File && h.nlink > 1 && h.size > 0 && paths[i].is_some() {
            holder.entry((h.dev, h.ino)).or_insert(i);
        }
    }
    let mut entries = Vec::new();
    let mut ids = Vec::with_capacity(listed.len());
    for (i, l) in listed.iter().enumerate() {
        let h = &l.head;
        let Some(path) = paths[i].clone() else {
            ids.push(None);
            continue;
        };
        let mut kind = h.kind();
        let mut target = None;
        if kind == EntryKind::File
            && h.nlink > 1
            && h.size == 0
            && let Some(&j) = holder.get(&(h.dev, h.ino)).filter(|&&j| j != i)
        {
            kind = EntryKind::Hardlink;
            target = paths[j].clone();
        }
        let mut e = Entry::new(entries.len() as u32, path, kind);
        e.raw_name = h.name.clone();
        e.size = if kind == EntryKind::File { h.size } else { 0 };
        e.mtime = (h.mtime > 0).then(|| timeutil::from_unix_secs(h.mtime));
        e.unix_mode = Some(h.mode & 0o7777);
        e.link_target = target.or_else(|| {
            l.link.as_ref().map(|t| match enc {
                Some(enc) if std::str::from_utf8(t).is_err() => enc.decode(t),
                _ => String::from_utf8_lossy(t).into_owned(),
            })
        });
        ids.push(Some(e.id));
        entries.push(e);
    }
    (entries, ids, enc)
}

/// A plain `.cpio` file: file data is read where it lies.
pub(crate) fn open(path: &Path, opts: &OpenOptions, ctx: &JobCtx) -> Result<Stored> {
    let input = SharedInput::open(path)?;
    let size = input.len();
    let mut cpio = Cpio::new(input.clone());
    let listed = list(&mut cpio, path, ctx)?;
    for l in &listed {
        if l.head.data_pos + l.head.size > size {
            return Err(corrupt("truncated", path));
        }
    }
    let (entries, ids, enc) = entries(&listed, opts);
    let mut ranges = vec![None; entries.len()];
    for (l, id) in listed.iter().zip(&ids) {
        if let Some(id) = id
            && entries[*id as usize].kind == EntryKind::File
        {
            ranges[*id as usize] = Some((l.head.data_pos, l.head.size));
        }
    }
    let mut info = ArchiveInfo::new(path, FormatId::Cpio, size);
    info.encoding = enc;
    info.volumes = volume::split_volumes(path).map_or(1, |v| v.len() as u32);
    info.add_method("Store");
    Ok(Stored {
        in_place: info.volumes == 1,
        info,
        entries: entries.into(),
        ranges: ranges.into(),
        input,
    })
}
