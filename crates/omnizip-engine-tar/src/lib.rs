//! TAR engine (plain and compressed: gzip, bzip2, xz, zstd, lz4, brotli) and single-file
//! compressed streams (`.gz`, `.xz`, …).
//!
//! Compressed tars are one stream without an index, so listing reads the whole archive once and
//! extraction reads it again (`docs/03-formats.md` §8).

pub mod codec;

use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

use omnizip_core::{
    ArchiveInfo, ArchiveReader, Capabilities, Codec, Engine, Entry, EntryData, EntryKind, Error,
    ErrorCode, ExtractSink, FormatId, JobCtx, OpenOptions, Phase, Report, Result, Source,
    SourceKind, WriteOptions,
    detect::is_tar_header,
    encoding,
    format::archive_stem,
    job::ProgressReader,
    source, timeutil,
    volume::{self, Output},
};

const TAR_FORMATS: &[FormatId] = &[
    FormatId::Tar,
    FormatId::TarGz,
    FormatId::TarBz2,
    FormatId::TarXz,
    FormatId::TarZst,
    FormatId::TarLz4,
    FormatId::TarBr,
    FormatId::Gz,
    FormatId::Bz2,
    FormatId::Xz,
    FormatId::Lzma,
    FormatId::Zst,
    FormatId::Lz4,
    FormatId::Br,
];

/// The TAR / stream engine.
#[derive(Debug, Default, Clone, Copy)]
pub struct TarEngine;

impl Engine for TarEngine {
    fn name(&self) -> &'static str {
        "tar"
    }

    fn formats(&self) -> &'static [FormatId] {
        TAR_FORMATS
    }

    fn capabilities(&self, format: FormatId) -> Capabilities {
        let mut c = Capabilities::READ
            | Capabilities::STREAMING
            | Capabilities::MULTI_VOLUME
            | Capabilities::MOUNT;
        if format != FormatId::Lzma {
            c |= Capabilities::WRITE;
        }
        if format.is_single_file() {
            c |= Capabilities::SINGLE_FILE;
        }
        c
    }

    fn refine(&self, path: &Path, detected: FormatId) -> FormatId {
        let Some(codec) = detected.codec() else {
            return detected;
        };
        let looks_tar = volume::open_input(path)
            .ok()
            .and_then(|input| codec::decoder(codec, input).ok())
            .and_then(|mut d| {
                let mut block = [0u8; 512];
                d.read_exact(&mut block).ok()?;
                Some(is_tar_header(&block))
            })
            .unwrap_or(false);
        match (looks_tar, detected.is_tar()) {
            (true, false) => detected.as_tar().unwrap_or(detected),
            (false, true) => single_for(codec),
            _ => detected,
        }
    }

    fn open(
        &self,
        path: &Path,
        format: FormatId,
        opts: &OpenOptions,
        ctx: &JobCtx,
    ) -> Result<Box<dyn ArchiveReader>> {
        Ok(Box::new(TarReader::open(path, format, opts, ctx)?))
    }

    fn create(
        &self,
        dest: &Path,
        sources: &[Source],
        opts: &WriteOptions,
        ctx: &JobCtx,
    ) -> Result<Report> {
        if opts.format.is_single_file() {
            create_single(dest, sources, opts, ctx)
        } else {
            create_tar(dest, sources, opts, ctx)
        }
    }
}

fn single_for(codec: Codec) -> FormatId {
    match codec {
        Codec::Gzip => FormatId::Gz,
        Codec::Bzip2 => FormatId::Bz2,
        Codec::Xz => FormatId::Xz,
        Codec::Lzma => FormatId::Lzma,
        Codec::Zstd => FormatId::Zst,
        Codec::Lz4 => FormatId::Lz4,
        Codec::Brotli => FormatId::Br,
    }
}

/// Maps errors raised while decoding: damaged compressed data reads as `InvalidData` /
/// `InvalidInput` / `Other` from the codecs.
fn decode_err(e: io::Error, path: &Path) -> Error {
    let kind = e.kind();
    let err = Error::from(e);
    if err.code() == ErrorCode::Io
        && matches!(kind, io::ErrorKind::InvalidInput | io::ErrorKind::Other)
    {
        return Error::new(ErrorCode::Corrupt)
            .with("path", path.display())
            .detail(err);
    }
    if err.param("path").is_none() {
        err.with("path", path.display())
    } else {
        err
    }
}

/// An opened tar or single-stream file.
pub struct TarReader {
    info: ArchiveInfo,
    entries: Vec<Entry>,
    /// Ordinal of each raw tar header → entry id (`None` for skipped meta headers).
    ordinals: Vec<Option<u32>>,
    codec: Option<Codec>,
    single: bool,
}

impl TarReader {
    fn open(path: &Path, format: FormatId, opts: &OpenOptions, ctx: &JobCtx) -> Result<Self> {
        let archive_size = volume::input_size(path)?;
        let codec = format.codec();
        let mut info = ArchiveInfo::new(path, format, archive_size);
        info.volumes = volume::split_volumes(path).map_or(1, |v| v.len() as u32);
        if let Some(c) = codec {
            info.add_method(c.method_name());
        }
        if format.is_single_file() {
            let entry = single_entry(path, format, archive_size)?;
            return Ok(Self {
                info,
                entries: vec![entry],
                ordinals: vec![Some(0)],
                codec,
                single: true,
            });
        }

        ctx.begin(Phase::Scanning, archive_size, 0);
        let input = ProgressReader::new(volume::open_input(path)?, ctx);
        let stream: Box<dyn Read + Send + '_> = match codec {
            Some(c) => codec::decoder(c, input).map_err(|e| decode_err(e, path))?,
            None => Box::new(input),
        };
        let mut ar = tar::Archive::new(stream);
        let mut raw: Vec<RawHeader> = Vec::new();
        let mut ordinals = Vec::new();
        for item in ar.entries().map_err(|e| decode_err(e, path))? {
            ctx.check()?;
            let ent = item.map_err(|e| decode_err(e, path))?;
            let h = ent.header();
            let kind = match h.entry_type() {
                tar::EntryType::Regular
                | tar::EntryType::Continuous
                | tar::EntryType::GNUSparse => EntryKind::File,
                tar::EntryType::Directory => EntryKind::Dir,
                tar::EntryType::Symlink => EntryKind::Symlink,
                tar::EntryType::Link => EntryKind::Hardlink,
                tar::EntryType::XGlobalHeader
                | tar::EntryType::XHeader
                | tar::EntryType::GNULongName
                | tar::EntryType::GNULongLink => {
                    ordinals.push(None);
                    continue;
                }
                _ => EntryKind::Other,
            };
            let name = ent.path_bytes().into_owned();
            let link = ent.link_name_bytes().map(|l| l.into_owned());
            let size = if kind == EntryKind::File {
                ent.size()
            } else {
                0
            };
            ordinals.push(Some(raw.len() as u32));
            raw.push((
                name,
                kind,
                size,
                h.mtime().unwrap_or(0),
                h.mode().unwrap_or(0),
                link,
            ));
            ctx.policy.check_entries(raw.len() as u64)?;
        }

        // Names: tar has no encoding field; treat invalid UTF-8 as a legacy code page.
        let legacy: Vec<&[u8]> = raw
            .iter()
            .map(|r| r.0.as_slice())
            .filter(|n| opts.encoding.is_some() || std::str::from_utf8(n).is_err())
            .collect();
        let enc = opts.encoding.or_else(|| {
            (!legacy.is_empty())
                .then(|| encoding::detect(legacy.iter().copied(), opts.locale.as_deref()))
        });
        info.encoding = enc;
        let decode = |b: &[u8]| match (enc, std::str::from_utf8(b)) {
            (Some(e), Err(_)) => e.decode(b),
            (Some(e), Ok(_)) if opts.encoding.is_some() => e.decode(b),
            _ => String::from_utf8_lossy(b).into_owned(),
        };

        let entries = raw
            .into_iter()
            .enumerate()
            .map(|(i, (name, kind, size, mtime, mode, link))| {
                let mut path = decode(&name);
                if kind == EntryKind::Dir && !path.ends_with('/') {
                    path.push('/');
                }
                let mut e = Entry::new(i as u32, path, kind);
                e.raw_name = name;
                e.size = size;
                e.mtime = (mtime > 0).then(|| timeutil::from_unix_secs(mtime));
                e.unix_mode = Some(mode & 0o7777);
                e.link_target = link.map(|l| decode(&l));
                e.method = codec.map(|c| c.method_name().to_owned());
                e
            })
            .collect();
        ctx.finish();
        Ok(Self {
            info,
            entries,
            ordinals,
            codec,
            single: false,
        })
    }

    fn open_stream<'a>(&self, ctx: &'a JobCtx) -> Result<Box<dyn Read + Send + 'a>> {
        let path = &self.info.path;
        let input = volume::open_input(path)?;
        let _ = ctx;
        Ok(match self.codec {
            Some(c) => codec::decoder(c, input).map_err(|e| decode_err(e, path))?,
            None => Box::new(input),
        })
    }
}

/// `(name, kind, size, mtime, mode, link target)` read from one tar header.
type RawHeader = (Vec<u8>, EntryKind, u64, u64, u32, Option<Vec<u8>>);

/// Builds the single entry of a `.gz`-style file.
fn single_entry(path: &Path, format: FormatId, archive_size: u64) -> Result<Entry> {
    let (head, tail, _) = omnizip_core::detect::read_windows(path)?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut name = archive_stem(&file_name);
    if name == file_name {
        name.push_str(".out");
    }
    let mut mtime = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
    let mut size = 0;
    let mut size_known = false;
    match format {
        FormatId::Gz => {
            if let Some(h) = codec::parse_gzip_header(&head) {
                if let Some(n) = h.name.filter(|n| !n.is_empty()) {
                    let n =
                        String::from_utf8(n.clone()).unwrap_or_else(|_| encoding::decode_cp437(&n));
                    // Only the final component: a header name must not smuggle in a path.
                    if let Some(last) = n.rsplit(['/', '\\']).next().filter(|s| !s.is_empty()) {
                        name = last.to_owned();
                    }
                }
                if h.mtime > 0 {
                    mtime = Some(timeutil::from_unix_secs(u64::from(h.mtime)));
                }
            }
            // ISIZE: uncompressed size mod 2^32 of the last member. Exact for one member < 4 GiB.
            if tail.len() >= 4 && archive_size < u64::from(u32::MAX) {
                let t = &tail[tail.len() - 4..];
                size = u64::from(u32::from_le_bytes([t[0], t[1], t[2], t[3]]));
            }
        }
        FormatId::Zst => {
            if let Ok(Some(n)) = zstd::zstd_safe::get_frame_content_size(&head) {
                size = n;
                size_known = false; // the stream may contain more frames
            }
        }
        _ => {}
    }
    let mut e = Entry::new(0, name, EntryKind::File);
    e.size = size;
    e.size_known = size_known;
    e.mtime = mtime;
    e.method = format.codec().map(|c| c.method_name().to_owned());
    Ok(e)
}

impl ArchiveReader for TarReader {
    fn info(&self) -> &ArchiveInfo {
        &self.info
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn extract_to(&mut self, sink: &mut dyn ExtractSink, ctx: &JobCtx) -> Result<()> {
        let path = self.info.path.clone();
        let mut remaining = self.entries.iter().filter(|e| sink.wants(e)).count();
        if remaining == 0 {
            return Ok(());
        }
        let mut stream = self.open_stream(ctx)?;
        if self.single {
            let e = &self.entries[0];
            let mut r = DecodeErrReader {
                inner: &mut stream,
                path: &path,
            };
            return sink.entry(e, EntryData::Stream(&mut r), ctx);
        }
        let mut ar = tar::Archive::new(stream);
        for (ord, item) in ar.entries().map_err(|e| decode_err(e, &path))?.enumerate() {
            ctx.check()?;
            let mut ent = item.map_err(|e| decode_err(e, &path))?;
            let Some(Some(id)) = self.ordinals.get(ord).copied() else {
                continue;
            };
            let e = &self.entries[id as usize];
            if !sink.wants(e) {
                continue;
            }
            let mut r = DecodeErrReader {
                inner: &mut ent,
                path: &path,
            };
            let data = match e.kind {
                EntryKind::File => EntryData::Stream(&mut r),
                _ => EntryData::None,
            };
            sink.entry(e, data, ctx)?;
            remaining -= 1;
            if remaining == 0 || sink.done() {
                break;
            }
        }
        Ok(())
    }
}

/// Converts codec errors met while reading entry data into typed errors.
struct DecodeErrReader<'a, R: ?Sized> {
    inner: &'a mut R,
    path: &'a Path,
}

impl<R: Read + ?Sized> Read for DecodeErrReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf).map_err(|e| {
            let kind = e.kind();
            match kind {
                io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData | io::ErrorKind::Other
                    if !e.get_ref().is_some_and(|i| i.is::<Error>()) =>
                {
                    Error::new(ErrorCode::Corrupt)
                        .with("path", self.path.display())
                        .detail(e)
                        .into()
                }
                _ => e,
            }
        })
    }
}

fn header_for(s: &Source, kind: tar::EntryType) -> tar::Header {
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(kind);
    h.set_size(if kind == tar::EntryType::Regular {
        s.size
    } else {
        0
    });
    h.set_mtime(s.mtime.map(timeutil::unix_secs).unwrap_or(0));
    let default_mode = match kind {
        tar::EntryType::Directory => 0o755,
        tar::EntryType::Symlink => 0o777,
        _ if s.readonly => 0o444,
        _ => 0o644,
    };
    h.set_mode(s.mode.unwrap_or(default_mode));
    h
}

fn create_tar(
    dest: &Path,
    sources: &[Source],
    opts: &WriteOptions,
    ctx: &JobCtx,
) -> Result<Report> {
    let out = Output::create(dest, opts.volume_size)?;
    let enc = codec::Encoder::new(
        opts.format.codec(),
        out,
        opts.level,
        opts.effective_threads(),
        None,
        0,
    )
    .map_err(|e| Error::io(e, dest))?;
    let mut b = tar::Builder::new(enc);
    b.follow_symlinks(false);
    ctx.begin(
        Phase::Compressing,
        source::total_size(sources),
        source::file_count(sources),
    );
    let mut report = Report::default();
    for s in sources {
        ctx.check()?;
        ctx.begin_file(&s.archive_path);
        let name = s.archive_path.trim_end_matches('/');
        let io_err = |e: io::Error| Error::io(e, &s.fs_path);
        match &s.kind {
            SourceKind::Dir => {
                let mut h = header_for(s, tar::EntryType::Directory);
                b.append_data(&mut h, format!("{name}/"), io::empty())
                    .map_err(io_err)?;
                report.dirs += 1;
            }
            SourceKind::Symlink { target } => {
                let mut h = header_for(s, tar::EntryType::Symlink);
                b.append_link(&mut h, name, target).map_err(io_err)?;
                report.links += 1;
            }
            SourceKind::File => {
                let f = File::open(&s.fs_path).map_err(io_err)?;
                // The size in the header must match the bytes written, even if the file changes.
                let actual = f.metadata().map(|m| m.len()).unwrap_or(s.size);
                let mut h = header_for(
                    &Source {
                        size: actual,
                        ..s.clone()
                    },
                    tar::EntryType::Regular,
                );
                let r = ProgressReader::new(f.take(actual), ctx);
                b.append_data(&mut h, name, r).map_err(io_err)?;
                report.files += 1;
                report.bytes += actual;
                ctx.file_done();
            }
        }
    }
    let enc = b.into_inner().map_err(|e| Error::io(e, dest))?;
    let out = enc.finish().map_err(|e| Error::io(e, dest))?;
    report.outputs = out.commit()?;
    ctx.finish();
    Ok(report)
}

fn create_single(
    dest: &Path,
    sources: &[Source],
    opts: &WriteOptions,
    ctx: &JobCtx,
) -> Result<Report> {
    let files: Vec<&Source> = sources
        .iter()
        .filter(|s| s.kind == SourceKind::File)
        .collect();
    let [s] = files.as_slice() else {
        return Err(Error::new(ErrorCode::UnsupportedOperation)
            .with("format", opts.format)
            .with("operation", "multiple-files")
            .detail(format!(
                "{} holds exactly one file; use tar.{} for several",
                opts.format, opts.format
            )));
    };
    let out = Output::create(dest, opts.volume_size)?;
    let name = s.archive_path.rsplit('/').next().unwrap_or(&s.archive_path);
    let mtime = s
        .mtime
        .map(timeutil::unix_secs)
        .unwrap_or(0)
        .min(u64::from(u32::MAX)) as u32;
    let mut enc = codec::Encoder::new(
        opts.format.codec(),
        out,
        opts.level,
        opts.effective_threads(),
        Some(name),
        mtime,
    )
    .map_err(|e| Error::io(e, dest))?;
    ctx.begin(Phase::Compressing, s.size, 1);
    ctx.begin_file(name);
    let f = File::open(&s.fs_path).map_err(|e| Error::io(e, &s.fs_path))?;
    let mut r = ProgressReader::new(f, ctx);
    let n = io::copy(&mut r, &mut enc).map_err(|e| Error::io(e, &s.fs_path))?;
    drop(r);
    let out = enc.finish().map_err(|e| Error::io(e, dest))?;
    let mut report = Report {
        files: 1,
        bytes: n,
        ..Default::default()
    };
    report.outputs = out.commit()?;
    ctx.finish();
    Ok(report)
}

#[cfg(test)]
mod tests;
