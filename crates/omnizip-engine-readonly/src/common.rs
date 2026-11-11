//! What the readers share: entry lists over byte ranges of the archive file ([`Stored`]),
//! entry lists walked from a decoded stream ([`Streamed`]), and name decoding.

use std::{
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
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

/// An archive whose files lie as they are in the input: each has a byte range.
#[derive(Clone)]
pub(crate) struct Stored {
    pub info: ArchiveInfo,
    pub entries: Arc<[Entry]>,
    /// Per entry, where a file's data lies in the input: `(offset, length)`.
    pub ranges: Arc<[Option<(u64, u64)>]>,
    pub input: SharedInput,
    /// The input is the archive file itself (not a window or joined volumes): mounts may read
    /// the ranges from that file.
    pub in_place: bool,
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
        (self.in_place && e.kind == EntryKind::File)
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
            match (e.kind, self.ranges[i]) {
                (EntryKind::File, Some((offset, len))) => {
                    self.input
                        .seek(SeekFrom::Start(offset))
                        .map_err(|e| Error::io(e, &path))?;
                    let mut r = Exact {
                        inner: (&mut self.input).take(len),
                        left: len,
                        path: &path,
                    };
                    sink.entry(e, EntryData::Stream(&mut r), ctx)?;
                }
                _ => sink.entry(e, EntryData::None, ctx)?,
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
