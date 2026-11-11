//! What the readers share: entry lists over byte ranges of the archive file ([`Stored`]),
//! entry lists walked from a decoded stream ([`Streamed`]), and name decoding.

use std::{
    collections::HashMap,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
    time::SystemTime,
};

use omnizip_core::{
    ArchiveInfo, ArchiveReader, Entry, EntryData, EntryId, EntryKind, Error, ErrorCode,
    ExtractSink, JobCtx, OpenOptions, Result,
    encoding::{self, NameEncoding},
    volume::SharedInput,
};

/// Decodes names with one policy for the whole archive: UTF-8 stays UTF-8, everything else is
/// decoded with one detected (or chosen) legacy encoding.
pub(crate) fn decode_names(
    raw: &[Vec<u8>],
    opts: &OpenOptions,
) -> (Vec<String>, Option<NameEncoding>) {
    let legacy: Vec<&[u8]> = raw
        .iter()
        .map(Vec::as_slice)
        .filter(|n| opts.encoding.is_some() || std::str::from_utf8(n).is_err())
        .collect();
    let enc = opts.encoding.or_else(|| {
        (!legacy.is_empty())
            .then(|| encoding::detect(legacy.iter().copied(), opts.locale.as_deref()))
    });
    let names = raw
        .iter()
        .map(|b| match (enc, std::str::from_utf8(b)) {
            (Some(e), Err(_)) => e.decode(b),
            (Some(e), Ok(_)) if opts.encoding.is_some() => e.decode(b),
            _ => String::from_utf8_lossy(b).into_owned(),
        })
        .collect();
    (names, enc)
}

/// An archive path as entries carry it: no leading `./` or `/`, a trailing `/` on
/// directories. `None` for the root itself (`.`, `./`). Unsafe parts (`..`) stay: extraction
/// sanitizes every path.
pub(crate) fn entry_path(name: &str, kind: EntryKind) -> Option<String> {
    let mut p = name;
    loop {
        if let Some(rest) = p.strip_prefix("./") {
            p = rest;
        } else if let Some(rest) = p.strip_prefix('/') {
            p = rest;
        } else {
            break;
        }
    }
    let p = p.trim_end_matches('/');
    if p.is_empty() || p == "." {
        return None;
    }
    Some(if kind == EntryKind::Dir {
        format!("{p}/")
    } else {
        p.to_owned()
    })
}

/// A calendar time `offset` seconds east of UTC as a `SystemTime`. `None` for impossible
/// dates and for times before 1970.
pub(crate) fn utc_time(
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    offset: i64,
) -> Option<SystemTime> {
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days since 1970-01-01 (H. Hinnant's days_from_civil).
    let m = i64::from(month);
    let y = if m <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second)
        - offset;
    u64::try_from(secs)
        .ok()
        .map(omnizip_core::timeutil::from_unix_secs)
}

pub(crate) fn corrupt(what: impl std::fmt::Display, path: &Path) -> Error {
    Error::new(ErrorCode::Corrupt)
        .detail(what)
        .with("path", path.display())
}

/// Reads `len` bytes at `pos`; a short read is damage.
pub(crate) fn read_at(
    input: &mut SharedInput,
    pos: u64,
    len: usize,
    path: &Path,
) -> Result<Vec<u8>> {
    let mut buf = vec![0; len];
    input
        .seek(SeekFrom::Start(pos))
        .and_then(|_| input.read_exact(&mut buf))
        .map_err(|_| corrupt("truncated", path))?;
    Ok(buf)
}

/// Entry data of a known length: a truncated archive is reported as damaged instead of
/// yielding a short file.
pub(crate) struct Exact<'a, R> {
    pub inner: R,
    pub left: u64,
    pub path: &'a Path,
}

impl<R: Read> Read for Exact<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 || buf.is_empty() {
            return Ok(0);
        }
        let want = buf
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(corrupt("truncated", self.path).into());
        }
        self.left -= n as u64;
        Ok(n)
    }
}

/// How one file's data is compressed, in formats that compress each file on its own (XAR).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Coding {
    /// A zlib stream (XAR calls it `application/x-gzip`).
    Zlib,
    Codec(omnizip_core::Codec),
}

/// An archive whose files each lie in one byte range of the input (several for the rare file
/// in several extents), stored as they are or compressed on their own.
#[derive(Clone)]
pub(crate) struct Stored {
    pub info: ArchiveInfo,
    pub entries: Arc<[Entry]>,
    /// Per entry, where a file's data lies in the input: `(offset, length)`.
    pub ranges: Arc<[Option<(u64, u64)>]>,
    /// Files in several pieces (ISO multi-extent files): the pieces in order. `ranges` has
    /// none for them.
    pub chains: Arc<HashMap<EntryId, Vec<(u64, u64)>>>,
    /// Per entry, how its data is compressed (empty: nothing is).
    pub codings: Arc<[Option<Coding>]>,
    pub input: SharedInput,
    /// The input is the archive file itself (not a window or joined volumes): mounts may read
    /// the ranges from that file.
    pub in_place: bool,
}

impl Stored {
    fn coding(&self, i: usize) -> Option<Coding> {
        self.codings.get(i).copied().flatten()
    }
}

/// Reads pieces of the input one after another.
struct Chain<'a> {
    input: &'a mut SharedInput,
    pieces: std::slice::Iter<'a, (u64, u64)>,
    left: u64,
}

impl Read for Chain<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.left == 0 {
            let Some(&(offset, len)) = self.pieces.next() else {
                return Ok(0);
            };
            self.input.seek(SeekFrom::Start(offset))?;
            self.left = len;
        }
        let want = buf
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.input.read(&mut buf[..want])?;
        self.left -= n as u64;
        Ok(n)
    }
}

impl ArchiveReader for Stored {
    fn info(&self) -> &ArchiveInfo {
        &self.info
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn stored_range(&mut self, id: EntryId) -> Option<(u64, u64)> {
        let e = self.entries.get(id as usize)?;
        (self.in_place && e.kind == EntryKind::File && self.coding(id as usize).is_none())
            .then(|| self.ranges[id as usize])
            .flatten()
    }

    fn try_clone(&self) -> Option<Box<dyn ArchiveReader>> {
        Some(Box::new(self.clone()))
    }

    fn extract_to(&mut self, sink: &mut dyn ExtractSink, ctx: &JobCtx) -> Result<()> {
        let entries = self.entries.clone();
        let order: Vec<usize> = match sink.wanted() {
            Some(ids) => {
                let mut v: Vec<usize> = ids
                    .into_iter()
                    .map(|id| id as usize)
                    .filter(|&i| i < entries.len())
                    .collect();
                v.sort_unstable();
                v.dedup();
                v
            }
            None => (0..entries.len()).collect(),
        };
        let path = self.info.path.clone();
        for i in order {
            ctx.check()?;
            let e = &entries[i];
            if !sink.wants(e) {
                continue;
            }
            if e.kind != EntryKind::File {
                sink.entry(e, EntryData::None, ctx)?;
            } else {
                // A file without a range has no data: empty, or reported as cut short.
                let chains = self.chains.clone();
                let single;
                let pieces: &[(u64, u64)] = match self.ranges[i] {
                    Some(r) => {
                        single = [r];
                        &single
                    }
                    None => chains.get(&(i as EntryId)).map_or(&[], Vec::as_slice),
                };
                let packed: u64 = pieces.iter().map(|p| p.1).sum();
                let coding = self.coding(i);
                let raw = Chain {
                    input: &mut self.input,
                    pieces: pieces.iter(),
                    left: 0,
                };
                let raw = Exact {
                    inner: raw,
                    left: packed,
                    path: &path,
                };
                let mut decoded: Box<dyn Read + Send + '_> = match coding {
                    None => Box::new(raw),
                    Some(Coding::Zlib) => Box::new(flate2::read::ZlibDecoder::new(raw)),
                    Some(Coding::Codec(c)) => {
                        omnizip_engine_tar::codec::decoder(c, raw).map_err(|e| corrupt(e, &path))?
                    }
                };
                let mut r = Exact {
                    inner: &mut decoded,
                    left: e.size,
                    path: &path,
                };
                sink.entry(e, EntryData::Stream(&mut r), ctx)?;
            }
            if sink.done() {
                break;
            }
        }
        Ok(())
    }
}

/// Decodes an archive from its start and hands each record to a callback, in order.
pub(crate) trait Walk: Send + Sync {
    /// Calls `f` with each record's ordinal and data, until `f` returns `false`. Data `f` does
    /// not read is skipped.
    fn walk(
        &self,
        ctx: &JobCtx,
        f: &mut dyn FnMut(usize, &mut dyn Read) -> Result<bool>,
    ) -> Result<()>;
}

/// An archive read as one stream (compressed payloads): every extraction decodes it from the
/// start.
#[derive(Clone)]
pub(crate) struct Streamed {
    pub info: ArchiveInfo,
    pub entries: Arc<[Entry]>,
    /// Record ordinal → entry id (`None` for records that are no entry).
    pub ordinals: Arc<[Option<u32>]>,
    pub walker: Arc<dyn Walk>,
}

impl ArchiveReader for Streamed {
    fn info(&self) -> &ArchiveInfo {
        &self.info
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn try_clone(&self) -> Option<Box<dyn ArchiveReader>> {
        Some(Box::new(self.clone()))
    }

    fn extract_to(&mut self, sink: &mut dyn ExtractSink, ctx: &JobCtx) -> Result<()> {
        let mut remaining = self.entries.iter().filter(|e| sink.wants(e)).count();
        if remaining == 0 {
            return Ok(());
        }
        let entries = self.entries.clone();
        let ordinals = self.ordinals.clone();
        self.walker.walk(ctx, &mut |ord, data| {
            ctx.check()?;
            let Some(Some(id)) = ordinals.get(ord).copied() else {
                return Ok(true);
            };
            let e = &entries[id as usize];
            if !sink.wants(e) {
                return Ok(true);
            }
            if e.kind == EntryKind::File {
                sink.entry(e, EntryData::Stream(data), ctx)?;
            } else {
                sink.entry(e, EntryData::None, ctx)?;
            }
            remaining -= 1;
            Ok(remaining > 0 && !sink.done())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_times() {
        let secs = |t: SystemTime| t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(secs(utc_time(1970, 1, 1, 0, 0, 0, 0).unwrap()), 0);
        assert_eq!(
            secs(utc_time(2026, 1, 1, 0, 0, 0, 0).unwrap()),
            1_767_225_600
        );
        // 08:00 at UTC+8 is midnight UTC.
        assert_eq!(
            secs(utc_time(2026, 1, 1, 8, 0, 0, 8 * 3600).unwrap()),
            1_767_225_600
        );
        assert_eq!(
            secs(utc_time(2024, 2, 29, 12, 0, 0, 0).unwrap()),
            1_709_208_000
        );
        assert!(utc_time(1900, 0, 0, 0, 0, 0, 0).is_none());
        assert!(utc_time(1969, 12, 31, 0, 0, 0, 0).is_none());
    }

    #[test]
    fn entry_paths() {
        assert_eq!(
            entry_path("./usr/bin/x", EntryKind::File).as_deref(),
            Some("usr/bin/x")
        );
        assert_eq!(entry_path("/etc", EntryKind::Dir).as_deref(), Some("etc/"));
        assert_eq!(entry_path("./", EntryKind::Dir), None);
        assert_eq!(entry_path(".", EntryKind::Dir), None);
        assert_eq!(entry_path("a/b/", EntryKind::Dir).as_deref(), Some("a/b/"));
    }
}
