//! Microsoft Cabinet files (`docs/03-formats.md` §3): stored, MSZIP and LZX folders. Quantum is
//! listed but not decoded; files continued across the cabinets of a set are not joined.
//!
//! A folder is one compressed stream holding several files back to back, so a folder is decoded
//! once from its start for every file wanted in it, the way 7z solid blocks are
//! ([`ArchiveReader::solid_block`]). (The `cab` crate decodes a folder from its start again for
//! every file, which makes large driver cabinets quadratic.)

use std::{
    collections::BTreeMap,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
};

use omnizip_core::{
    ArchiveInfo, ArchiveReader, Entry, EntryData, EntryId, EntryKind, Error, ErrorCode,
    ExtractSink, FormatId, JobCtx, OpenOptions, Result, timeutil, volume::SharedInput,
};

use crate::common::{Exact, corrupt, decode_names, read_at};

/// Uncompressed bytes in one data block, and the most compressed bytes one may hold.
const MAX_BLOCK: usize = 0x8000;
const MAX_PACKED_BLOCK: usize = 0x8000 + 6144;
/// MSZIP history carried from block to block.
const MSZIP_WINDOW: usize = 0x8000;

const FLAG_PREV: u16 = 0x0001;
const FLAG_NEXT: u16 = 0x0002;
const FLAG_RESERVE: u16 = 0x0004;
const ATTR_UTF8: u16 = 0x80;

#[derive(Clone, Copy)]
enum Method {
    Store,
    MsZip,
    Lzx(lzxd::WindowSize),
    Quantum,
    Unknown,
}

impl Method {
    fn parse(bits: u16) -> Method {
        match bits & 0x000F {
            0 => Method::Store,
            1 => Method::MsZip,
            2 => Method::Quantum,
            3 => match (bits >> 8) & 0x1F {
                15 => Method::Lzx(lzxd::WindowSize::KB32),
                16 => Method::Lzx(lzxd::WindowSize::KB64),
                17 => Method::Lzx(lzxd::WindowSize::KB128),
                18 => Method::Lzx(lzxd::WindowSize::KB256),
                19 => Method::Lzx(lzxd::WindowSize::KB512),
                20 => Method::Lzx(lzxd::WindowSize::MB1),
                21 => Method::Lzx(lzxd::WindowSize::MB2),
                _ => Method::Unknown,
            },
            _ => Method::Unknown,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Method::Store => "Store",
            Method::MsZip => "MSZIP",
            Method::Lzx(_) => "LZX",
            Method::Quantum => "Quantum",
            Method::Unknown => "?",
        }
    }
}

#[derive(Clone, Copy)]
struct Folder {
    first_block: u64,
    blocks: u16,
    method: Method,
}

#[derive(Clone, Copy)]
struct FileRef {
    folder: usize,
    offset: u64,
    size: u64,
}

enum Decoder {
    Store,
    MsZip {
        z: flate2::Decompress,
        history: Vec<u8>,
    },
    Lzx(Box<lzxd::Lzxd>),
}

/// Decodes one folder from its start.
struct FolderReader<'a> {
    input: SharedInput,
    blocks_left: u16,
    reserve: u8,
    dec: Decoder,
    buf: Vec<u8>,
    at: usize,
    path: &'a Path,
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

impl<'a> FolderReader<'a> {
    fn new(input: &SharedInput, folder: &Folder, reserve: u8, path: &'a Path) -> Result<Self> {
        let dec = match folder.method {
            Method::Store => Decoder::Store,
            Method::MsZip => Decoder::MsZip {
                z: flate2::Decompress::new(false),
                history: Vec::new(),
            },
            Method::Lzx(w) => Decoder::Lzx(Box::new(lzxd::Lzxd::new(w))),
            m @ (Method::Quantum | Method::Unknown) => {
                return Err(Error::new(ErrorCode::UnsupportedMethod)
                    .with("method", m.name())
                    .with("path", path.display()));
            }
        };
        let mut input = input.clone();
        input
            .seek(SeekFrom::Start(folder.first_block))
            .map_err(|e| Error::io(e, path))?;
        Ok(Self {
            input,
            blocks_left: folder.blocks,
            reserve,
            dec,
            buf: Vec::new(),
            at: 0,
            path,
        })
    }

    /// Decodes the next data block into `buf`. `false` after the last one.
    fn next_block(&mut self) -> io::Result<bool> {
        if self.blocks_left == 0 {
            return Ok(false);
        }
        let mut h = [0u8; 8];
        self.input.read_exact(&mut h)?;
        let packed = usize::from(u16::from_le_bytes([h[4], h[5]]));
        let size = usize::from(u16::from_le_bytes([h[6], h[7]]));
        if packed > MAX_PACKED_BLOCK || size > MAX_BLOCK {
            return Err(bad("CAB data block too large"));
        }
        if size == 0 {
            return Err(Error::new(ErrorCode::UnsupportedOperation)
                .with("operation", "multi-cabinet")
                .with("path", self.path.display())
                .into());
        }
        if self.reserve > 0 {
            self.input
                .seek(SeekFrom::Current(i64::from(self.reserve)))?;
        }
        let mut data = vec![0u8; packed];
        self.input.read_exact(&mut data)?;
        self.buf = match &mut self.dec {
            Decoder::Store => {
                if packed != size {
                    return Err(bad("CAB stored block size"));
                }
                data
            }
            Decoder::MsZip { z, history } => mszip_block(z, history, &data, size)?,
            Decoder::Lzx(l) => l
                .decompress_next(&data, size)
                .map_err(|e| bad(&e.to_string()))?
                .to_vec(),
        };
        self.at = 0;
        self.blocks_left -= 1;
        Ok(true)
    }
}

/// One MSZIP block: "CK", then a deflate block whose back references may reach into the
/// previous block's output. The history is handed to the inflater as a stored block first.
fn mszip_block(
    z: &mut flate2::Decompress,
    history: &mut Vec<u8>,
    data: &[u8],
    size: usize,
) -> io::Result<Vec<u8>> {
    let body = data
        .strip_prefix(b"CK")
        .ok_or_else(|| bad("bad MSZIP block"))?;
    z.reset(false);
    if !history.is_empty() {
        let len = history.len() as u16;
        let mut seed = vec![0u8];
        seed.extend_from_slice(&len.to_le_bytes());
        seed.extend_from_slice(&(!len).to_le_bytes());
        seed.extend_from_slice(history);
        let mut sink = Vec::with_capacity(history.len());
        z.decompress_vec(&seed, &mut sink, flate2::FlushDecompress::Sync)
            .map_err(|e| bad(&e.to_string()))?;
    }
    let mut out = Vec::with_capacity(size);
    z.decompress_vec(body, &mut out, flate2::FlushDecompress::Finish)
        .map_err(|e| bad(&e.to_string()))?;
    if out.len() != size {
        return Err(bad("MSZIP block size"));
    }
    history.extend_from_slice(&out);
    if history.len() > MSZIP_WINDOW {
        history.drain(..history.len() - MSZIP_WINDOW);
    }
    Ok(out)
}

impl Read for FolderReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        while self.at == self.buf.len() {
            if !self.next_block()? {
                return Ok(0);
            }
        }
        let n = out.len().min(self.buf.len() - self.at);
        out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// An opened cabinet.
#[derive(Clone)]
pub(crate) struct CabReader {
    info: ArchiveInfo,
    entries: Arc<[Entry]>,
    files: Arc<[FileRef]>,
    folders: Arc<[Folder]>,
    reserve: u8,
    input: SharedInput,
    path: PathBuf,
}

/// The NUL-terminated string at `pos` (at most 256 bytes, as CAB allows).
fn cstr(input: &mut SharedInput, pos: u64, path: &Path) -> Result<Vec<u8>> {
    let len = (input.len().saturating_sub(pos)).min(257) as usize;
    let b = read_at(input, pos, len, path)?;
    let end = b
        .iter()
        .position(|&c| c == 0)
        .ok_or_else(|| corrupt("bad name", path))?;
    Ok(b[..end].to_vec())
}

pub(crate) fn open(path: &Path, opts: &OpenOptions, ctx: &JobCtx) -> Result<CabReader> {
    let mut input = SharedInput::open(path)?;
    let size = input.len();
    let h = read_at(&mut input, 0, 36, path)?;
    if &h[..4] != b"MSCF" {
        return Err(corrupt("not a cabinet", path));
    }
    let le16 = |b: &[u8], o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let le32 = |b: &[u8], o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let files_at = u64::from(le32(&h, 16));
    let (folder_count, file_count, flags) = (le16(&h, 26), le16(&h, 28), le16(&h, 30));
    let mut pos = 36u64;
    let (mut folder_reserve, mut data_reserve) = (0u64, 0u8);
    if flags & FLAG_RESERVE != 0 {
        let r = read_at(&mut input, pos, 4, path)?;
        folder_reserve = u64::from(r[2]);
        data_reserve = r[3];
        pos += 4 + u64::from(le16(&r, 0));
    }
    for flag in [FLAG_PREV, FLAG_NEXT] {
        if flags & flag != 0 {
            // Cabinet and disk names of the neighbouring cabinet.
            for _ in 0..2 {
                pos += cstr(&mut input, pos, path)?.len() as u64 + 1;
            }
        }
    }
    let mut folders = Vec::with_capacity(usize::from(folder_count));
    for _ in 0..folder_count {
        let f = read_at(&mut input, pos, 8, path)?;
        folders.push(Folder {
            first_block: u64::from(le32(&f, 0)),
            blocks: le16(&f, 4),
            method: Method::parse(le16(&f, 6)),
        });
        pos += 8 + folder_reserve;
    }
    ctx.policy.check_entries(u64::from(file_count))?;

    let mut raw = Vec::with_capacity(usize::from(file_count));
    let mut utf8 = Vec::with_capacity(usize::from(file_count));
    let mut refs = Vec::with_capacity(usize::from(file_count));
    let mut meta = Vec::with_capacity(usize::from(file_count));
    let mut pos = files_at;
    for _ in 0..file_count {
        ctx.check()?;
        let f = read_at(&mut input, pos, 16, path)?;
        let name = cstr(&mut input, pos + 16, path)?;
        pos += 16 + name.len() as u64 + 1;
        let folder = match le16(&f, 8) {
            // Continued from the previous cabinet (and maybe into the next): its data starts
            // elsewhere, so only the part in this cabinet's first folder could be read.
            0xFFFD | 0xFFFF => 0,
            // Continued into the next cabinet: it starts in the last folder.
            0xFFFE => usize::from(folder_count).saturating_sub(1),
            i => usize::from(i),
        };
        if folder >= folders.len() {
            return Err(corrupt("file in a folder that does not exist", path));
        }
        let attrs = le16(&f, 14);
        utf8.push(attrs & ATTR_UTF8 != 0);
        raw.push(name);
        refs.push(FileRef {
            folder,
            offset: u64::from(le32(&f, 4)),
            size: u64::from(le32(&f, 0)),
        });
        meta.push((le16(&f, 10), le16(&f, 12), attrs));
    }

    // Names flagged UTF-8 are; the others share one detected (or chosen) code page.
    let legacy: Vec<Vec<u8>> = raw
        .iter()
        .zip(&utf8)
        .filter(|(_, u)| !**u)
        .map(|(r, _)| r.clone())
        .collect();
    let (mut legacy_names, enc) = decode_names(&legacy, opts);
    legacy_names.reverse();
    let mut info = ArchiveInfo::new(path, FormatId::Cab, size);
    info.encoding = enc;
    for f in &folders {
        info.add_method(f.method.name());
    }
    let mut entries = Vec::with_capacity(raw.len());
    let mut files = Vec::with_capacity(raw.len());
    for ((name, is_utf8), (r, (date, time, attrs))) in
        raw.into_iter().zip(utf8).zip(refs.into_iter().zip(meta))
    {
        let decoded = if is_utf8 {
            String::from_utf8_lossy(&name).into_owned()
        } else {
            legacy_names.pop().unwrap_or_default()
        };
        let p = decoded
            .replace('\\', "/")
            .trim_start_matches('/')
            .to_owned();
        if p.is_empty() {
            continue;
        }
        let mut e = Entry::new(entries.len() as u32, p, EntryKind::File);
        e.raw_name = name;
        e.size = r.size;
        e.method = Some(folders[r.folder].method.name().to_owned());
        e.mtime = timeutil::local_to_system(
            1980 + i32::from(date >> 9),
            ((date >> 5) & 0xF) as u8,
            (date & 0x1F) as u8,
            (time >> 11) as u8,
            ((time >> 5) & 0x3F) as u8,
            ((time & 0x1F) * 2) as u8,
        );
        e.win_attrs = Some(u32::from(attrs & 0x27));
        entries.push(e);
        files.push(r);
    }
    Ok(CabReader {
        info,
        entries: entries.into(),
        files: files.into(),
        folders: folders.into(),
        reserve: data_reserve,
        input,
        path: path.to_owned(),
    })
}

impl ArchiveReader for CabReader {
    fn info(&self) -> &ArchiveInfo {
        &self.info
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn try_clone(&self) -> Option<Box<dyn ArchiveReader>> {
        Some(Box::new(self.clone()))
    }

    fn solid_block(&self, id: EntryId) -> Option<u32> {
        (self.folders.len() > 1)
            .then(|| self.files.get(id as usize).map(|f| f.folder as u32))
            .flatten()
    }

    fn extract_to(&mut self, sink: &mut dyn ExtractSink, ctx: &JobCtx) -> Result<()> {
        let entries = self.entries.clone();
        let ids: Vec<usize> = match sink.wanted() {
            Some(ids) => ids
                .into_iter()
                .map(|i| i as usize)
                .filter(|&i| i < entries.len())
                .collect(),
            None => (0..entries.len()).collect(),
        };
        // Folder → its wanted files in data order.
        let mut by_folder: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for i in ids {
            if sink.wants(&entries[i]) {
                by_folder.entry(self.files[i].folder).or_default().push(i);
            }
        }
        let path = self.path.clone();
        for (folder, mut ids) in by_folder {
            ids.sort_by_key(|&i| (self.files[i].offset, i));
            ids.dedup();
            let mut reader =
                FolderReader::new(&self.input, &self.folders[folder], self.reserve, &path)?;
            let mut pos = 0u64;
            for i in ids {
                ctx.check()?;
                let f = self.files[i];
                if f.offset < pos {
                    // Files sharing data: start the folder over.
                    reader =
                        FolderReader::new(&self.input, &self.folders[folder], self.reserve, &path)?;
                    pos = 0;
                }
                let gap = f.offset - pos;
                let skipped = io::copy(&mut (&mut reader).take(gap), &mut io::sink())
                    .map_err(|e| corrupt(e, &path))?;
                if skipped < gap {
                    return Err(corrupt("truncated", &path));
                }
                let mut data = (&mut reader).take(f.size);
                let mut r = Exact {
                    inner: &mut data,
                    left: f.size,
                    path: &path,
                };
                sink.entry(&entries[i], EntryData::Stream(&mut r), ctx)?;
                io::copy(&mut data, &mut io::sink()).map_err(|e| corrupt(e, &path))?;
                pos = f.offset + f.size;
                if sink.done() {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}
