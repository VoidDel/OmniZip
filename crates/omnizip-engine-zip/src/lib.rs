//! ZIP engine: read, write, and in-place update (add / delete / rename without recompressing
//! untouched entries).

use std::{
    collections::HashSet,
    fs::File,
    io::{self, Read},
    path::Path,
};

use omnizip_core::{
    ArchiveInfo, ArchiveReader, Capabilities, Engine, Entry, EntryData, EntryKind, Error,
    ErrorCode, ExtractSink, FormatId, JobCtx, Level, OpenOptions, Phase, Report, Result, Source,
    SourceKind, UpdateOp, WriteOptions,
    encoding::{self, NameEncoding},
    job::{MAX_PASSWORD_ATTEMPTS, ProgressReader},
    source, timeutil,
    volume::{self, Output, ReadSeek},
};
use zip::{
    AesMode, CompressionMethod, DateTime, HasZipMetadata, System, ZipArchive, ZipReadOptions,
    ZipWriter, result::ZipError, write::SimpleFileOptions,
};

const UTF8_FLAG: u16 = 1 << 11;

/// The ZIP engine.
#[derive(Debug, Default, Clone, Copy)]
pub struct ZipEngine;

impl Engine for ZipEngine {
    fn name(&self) -> &'static str {
        "zip"
    }

    fn formats(&self) -> &'static [FormatId] {
        &[FormatId::Zip]
    }

    fn capabilities(&self, _format: FormatId) -> Capabilities {
        Capabilities::READ
            | Capabilities::WRITE
            | Capabilities::UPDATE
            | Capabilities::ENCRYPT
            | Capabilities::DECRYPT
            | Capabilities::COMMENT
            | Capabilities::MULTI_VOLUME
            | Capabilities::MOUNT
    }

    fn open(
        &self,
        path: &Path,
        _format: FormatId,
        opts: &OpenOptions,
        ctx: &JobCtx,
    ) -> Result<Box<dyn ArchiveReader>> {
        Ok(Box::new(ZipReader::open(path, opts, ctx)?))
    }

    fn create(
        &self,
        dest: &Path,
        sources: &[Source],
        opts: &WriteOptions,
        ctx: &JobCtx,
    ) -> Result<Report> {
        let out = Output::create(dest, opts.volume_size)?;
        let mut zw = ZipWriter::new(out);
        if let Some(c) = &opts.comment {
            zw.set_comment(c.clone()).map_err(|e| map_err(e, dest))?;
        }
        ctx.begin(
            Phase::Compressing,
            source::total_size(sources),
            source::file_count(sources),
        );
        let mut report = Report::default();
        write_sources(&mut zw, sources, opts, ctx, &mut report)?;
        let out = zw.finish().map_err(|e| map_err(e, dest))?;
        report.outputs = out.commit()?;
        ctx.finish();
        Ok(report)
    }

    fn update(
        &self,
        path: &Path,
        _format: FormatId,
        ops: &[UpdateOp],
        opts: &WriteOptions,
        ctx: &JobCtx,
    ) -> Result<Report> {
        update(path, ops, opts, ctx)
    }
}

/// Maps a `zip` crate error.
fn map_err(e: ZipError, path: &Path) -> Error {
    let err = match e {
        ZipError::Io(io) => Error::from(io),
        ZipError::InvalidArchive(msg) => Error::new(ErrorCode::Corrupt).detail(msg),
        ZipError::UnsupportedArchive(msg) => Error::new(ErrorCode::UnsupportedMethod).detail(msg),
        ZipError::CompressionMethodNotSupported(m) => {
            Error::new(ErrorCode::UnsupportedMethod).with("method", m)
        }
        ZipError::InvalidPassword => Error::new(ErrorCode::WrongPassword),
        ZipError::FileNotFound => Error::internal("zip entry vanished"),
        other => Error::new(ErrorCode::Corrupt).detail(other),
    };
    if err.param("path").is_none() {
        err.with("path", path.display())
    } else {
        err
    }
}

/// Turns the crate's "Invalid checksum" I/O error into a typed CRC error.
struct CrcMapReader<R>(R);

impl<R: Read> Read for CrcMapReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf).map_err(|e| {
            if e.kind() == io::ErrorKind::InvalidData && e.to_string().contains("checksum") {
                Error::new(ErrorCode::CrcMismatch).into()
            } else {
                e
            }
        })
    }
}

fn method_name(m: CompressionMethod) -> String {
    match m {
        CompressionMethod::Stored => "Store".into(),
        CompressionMethod::Deflated => "Deflate".into(),
        CompressionMethod::Deflate64 => "Deflate64".into(),
        CompressionMethod::Bzip2 => "BZip2".into(),
        CompressionMethod::Zstd => "Zstandard".into(),
        CompressionMethod::Lzma => "LZMA".into(),
        CompressionMethod::Xz => "XZ".into(),
        CompressionMethod::Ppmd => "PPMd".into(),
        other => format!("{other:?}"),
    }
}

/// Decodes every entry name with one consistent policy:
/// UTF-8 flag or valid UTF-8 → UTF-8; everything else → one detected (or forced) legacy encoding.
fn decide_names(
    raw: &[(Vec<u8>, bool)],
    opts: &OpenOptions,
) -> (Vec<String>, Option<NameEncoding>) {
    let legacy: Vec<&[u8]> = raw
        .iter()
        .filter(|(n, utf8)| !*utf8 && (opts.encoding.is_some() || std::str::from_utf8(n).is_err()))
        .map(|(n, _)| n.as_slice())
        .collect();
    let enc = match opts.encoding {
        Some(e) => Some(e),
        None if legacy.is_empty() => None,
        None => Some(encoding::detect(
            legacy.iter().copied(),
            opts.locale.as_deref(),
        )),
    };
    let names = raw
        .iter()
        .map(|(n, utf8)| match (enc, *utf8) {
            (Some(e), false) if opts.encoding.is_some() || std::str::from_utf8(n).is_err() => {
                e.decode(n)
            }
            _ => String::from_utf8_lossy(n).into_owned(),
        })
        .collect();
    (names, enc)
}

/// An opened ZIP archive.
pub struct ZipReader {
    archive: ZipArchive<Box<dyn ReadSeek>>,
    info: ArchiveInfo,
    entries: Vec<Entry>,
    password: Option<String>,
    display_name: String,
}

impl ZipReader {
    fn open(path: &Path, opts: &OpenOptions, ctx: &JobCtx) -> Result<Self> {
        let input = volume::open_input(path)?;
        let archive_size = volume::input_size(path)?;
        let archive = ZipArchive::new(input).map_err(|e| map_err(e, path))?;
        ctx.policy.check_entries(archive.len() as u64)?;

        let mut raw = Vec::with_capacity(archive.len());
        for i in 0..archive.len() {
            let e = archive.by_index_data(i).map_err(|e| map_err(e, path))?;
            raw.push((e.name_raw().to_vec(), e.flags().as_u16() & UTF8_FLAG != 0));
        }
        let (names, enc) = decide_names(&raw, opts);

        let mut info = ArchiveInfo::new(path, FormatId::Zip, archive_size);
        info.encoding = enc;
        info.sfx = archive.offset() > 0;
        info.volumes = volume::split_volumes(path).map_or(1, |v| v.len() as u32);
        let comment = archive.comment();
        if !comment.is_empty() {
            info.comment = Some(
                String::from_utf8(comment.to_vec())
                    .unwrap_or_else(|_| enc.unwrap_or(NameEncoding::Cp437).decode(comment)),
            );
        }

        let mut entries = Vec::with_capacity(archive.len());
        for (i, ((raw_name, _), name)) in raw.into_iter().zip(names).enumerate() {
            ctx.check()?;
            let z = archive.by_index_data(i).map_err(|e| map_err(e, path))?;
            let kind = if z.is_dir() {
                EntryKind::Dir
            } else if z.is_symlink() {
                EntryKind::Symlink
            } else {
                EntryKind::File
            };
            let mut e = Entry::new(i as u32, name, kind);
            e.raw_name = raw_name;
            e.size = z.size();
            e.packed_size = Some(z.compressed_size());
            e.mtime = z.last_modified().and_then(|d| {
                timeutil::local_to_system(
                    i32::from(d.year()),
                    d.month(),
                    d.day(),
                    d.hour(),
                    d.minute(),
                    d.second(),
                )
            });
            e.unix_mode = z.unix_mode();
            if z.system() == System::Dos {
                e.win_attrs = Some(z.external_attributes() & 0xFFFF);
            }
            e.crc32 = Some(z.crc32());
            e.encrypted = z.encrypted();
            if !e.is_dir() {
                let m = method_name(z.compression());
                info.add_method(m.clone());
                e.method = Some(m);
            }
            if e.encrypted {
                info.encrypted = true;
                let scheme = match z.get_metadata().aes_settings() {
                    Some((AesMode::Aes256, _)) => "AES-256",
                    Some((AesMode::Aes192, _)) => "AES-192",
                    Some((AesMode::Aes128, _)) => "AES-128",
                    None => "ZipCrypto",
                };
                info.encryption.get_or_insert_with(|| scheme.to_owned());
            }
            let c = z.comment();
            if !c.is_empty() {
                e.comment = Some(c.to_owned());
            }
            entries.push(e);
        }
        Ok(Self {
            archive,
            info,
            entries,
            password: opts.password.clone(),
            display_name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        })
    }

    fn password_for(&mut self, entry: &str, attempt: u32, ctx: &JobCtx) -> Result<String> {
        if attempt == 0
            && let Some(p) = &self.password
        {
            return Ok(p.clone());
        }
        let p = ctx.ask_password(&self.display_name, Some(entry), attempt)?;
        self.password = Some(p.clone());
        Ok(p)
    }
}

impl ArchiveReader for ZipReader {
    fn info(&self) -> &ArchiveInfo {
        &self.info
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn extract_to(&mut self, sink: &mut dyn ExtractSink, ctx: &JobCtx) -> Result<()> {
        for i in 0..self.entries.len() {
            ctx.check()?;
            if !sink.wants(&self.entries[i]) {
                continue;
            }
            let entry = self.entries[i].clone();
            if entry.is_dir() {
                sink.entry(&entry, EntryData::None, ctx)?;
            } else {
                let mut attempt = 0;
                loop {
                    let pw = if entry.encrypted {
                        Some(self.password_for(&entry.path, attempt, ctx)?)
                    } else {
                        None
                    };
                    let ro = ZipReadOptions::new().password(pw.as_deref().map(str::as_bytes));
                    match self.archive.by_index_with_options(i, ro) {
                        Ok(f) => {
                            let mut r = CrcMapReader(f);
                            let res = sink.entry(&entry, EntryData::Stream(&mut r), ctx);
                            // A wrong ZipCrypto password can pass the header check and only fail
                            // the checksum; nothing more can be done about that here.
                            res?;
                            break;
                        }
                        Err(ZipError::InvalidPassword) => {
                            self.password = None;
                            attempt += 1;
                            if attempt >= MAX_PASSWORD_ATTEMPTS {
                                return Err(Error::new(ErrorCode::WrongPassword)
                                    .with("path", &self.display_name));
                            }
                        }
                        Err(e) => return Err(map_err(e, &self.info.path)),
                    }
                }
            }
            if sink.done() {
                break;
            }
        }
        Ok(())
    }
}

/// Methods ZIP can write. LZMA and PPMd are read-only in the underlying crate.
pub const WRITE_METHODS: &[&str] = &["Deflate", "BZip2", "Zstandard", "XZ", "Store"];

/// `(compression method, level)` for the options.
fn method_and_level(opts: &WriteOptions) -> Result<(CompressionMethod, Option<i64>)> {
    if opts.level == Level::Store {
        return Ok((CompressionMethod::Stored, None));
    }
    let i = opts.level.index();
    let method = match opts
        .method
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None | Some("deflate") => CompressionMethod::Deflated,
        Some("store" | "copy") => return Ok((CompressionMethod::Stored, None)),
        Some("bzip2") => CompressionMethod::Bzip2,
        Some("zstd" | "zstandard") => CompressionMethod::Zstd,
        Some("xz") => CompressionMethod::Xz,
        Some(other) => return Err(Error::new(ErrorCode::UnsupportedMethod).with("method", other)),
    };
    let level = match method {
        CompressionMethod::Deflated | CompressionMethod::Bzip2 => [0, 1, 3, 6, 7, 9, 9][i],
        CompressionMethod::Zstd => [0, 1, 3, 6, 12, 19, 22][i],
        _ => [0, 0, 2, 6, 7, 9, 9][i],
    };
    Ok((method, Some(level)))
}

fn zip_time(t: Option<std::time::SystemTime>) -> DateTime {
    t.map(timeutil::system_to_local_fields)
        .and_then(|(y, mo, d, h, mi, s)| {
            DateTime::from_date_and_time(y.clamp(1980, 2107) as u16, mo, d, h, mi, s).ok()
        })
        .unwrap_or_default()
}

fn write_sources<W: io::Write + io::Seek>(
    zw: &mut ZipWriter<W>,
    sources: &[Source],
    opts: &WriteOptions,
    ctx: &JobCtx,
    report: &mut Report,
) -> Result<()> {
    let (method, level) = method_and_level(opts)?;
    for s in sources {
        ctx.check()?;
        ctx.begin_file(&s.archive_path);
        let mut base = SimpleFileOptions::default()
            .compression_method(method)
            .compression_level(level)
            .last_modified_time(zip_time(s.mtime))
            .large_file(s.size >= 0xFFFF_FFFF);
        if let Some(mode) = s.mode {
            base = base.unix_permissions(mode);
        }
        let name = s.archive_path.trim_end_matches('/');
        match &s.kind {
            SourceKind::Dir => {
                zw.add_directory(format!("{name}/"), base)
                    .map_err(|e| map_err(e, &s.fs_path))?;
                report.dirs += 1;
            }
            SourceKind::Symlink { target } => {
                zw.add_symlink(name, target, base)
                    .map_err(|e| map_err(e, &s.fs_path))?;
                report.links += 1;
            }
            SourceKind::File => {
                let fo = match &opts.password {
                    Some(pw) => base.with_aes_encryption(AesMode::Aes256, pw),
                    None => base,
                };
                zw.start_file(name, fo)
                    .map_err(|e| map_err(e, &s.fs_path))?;
                let f = File::open(&s.fs_path).map_err(|e| Error::io(e, &s.fs_path))?;
                let mut r = ProgressReader::new(f, ctx);
                let n = io::copy(&mut r, zw).map_err(|e| Error::io(e, &s.fs_path))?;
                report.files += 1;
                report.bytes += n;
                ctx.file_done();
            }
        }
    }
    Ok(())
}

fn trim(p: &str) -> &str {
    p.trim_matches('/')
}

fn under(name: &str, prefix: &str) -> bool {
    let (n, p) = (trim(name), trim(prefix));
    !p.is_empty() && (n == p || n.starts_with(p) && n.as_bytes().get(p.len()) == Some(&b'/'))
}

/// In-place update: untouched entries are copied raw (no recompression), then additions are
/// written. The result replaces the original atomically.
fn update(path: &Path, ops: &[UpdateOp], opts: &WriteOptions, ctx: &JobCtx) -> Result<Report> {
    if volume::split_volumes(path).is_some_and(|v| v.len() > 1) {
        return Err(Error::new(ErrorCode::UnsupportedOperation).with("operation", "update-split"));
    }
    let open_opts = OpenOptions::default();
    let reader = ZipReader::open(path, &open_opts, ctx)?;
    let names: Vec<String> = reader.entries.iter().map(|e| e.path.clone()).collect();
    let mut src = reader.archive;

    let mut deletes: Vec<&str> = Vec::new();
    let mut renames: Vec<(&str, &str)> = Vec::new();
    let mut adds: Vec<&Source> = Vec::new();
    for op in ops {
        match op {
            UpdateOp::Delete { paths } => deletes.extend(paths.iter().map(String::as_str)),
            UpdateOp::Rename { from, to } => renames.push((from, to)),
            UpdateOp::Add { sources } => adds.extend(sources.iter()),
        }
    }
    let added: HashSet<&str> = adds.iter().map(|s| trim(&s.archive_path)).collect();

    let out = Output::create(path, None)?;
    let mut zw = ZipWriter::new(out);
    if !src.comment().is_empty() {
        zw.set_raw_comment(src.comment().into())
            .map_err(|e| map_err(e, path))?;
    }
    let total = adds
        .iter()
        .filter(|s| s.kind == SourceKind::File)
        .map(|s| s.size)
        .sum();
    ctx.begin(Phase::Updating, total, adds.len() as u64);
    let mut report = Report::default();
    for (i, name) in names.iter().enumerate() {
        ctx.check()?;
        if deletes.iter().any(|d| under(name, d)) {
            report.skipped += 1;
            continue;
        }
        if added.contains(trim(name)) {
            continue; // replaced by a new file
        }
        let renamed = renames
            .iter()
            .find(|(from, _)| under(name, from))
            .map(|(from, to)| {
                let rest = &trim(name)[trim(from).len()..];
                let dir_suffix = if name.ends_with('/') { "/" } else { "" };
                format!("{}{}{}", trim(to), rest, dir_suffix)
            });
        let f = src.by_index_raw(i).map_err(|e| map_err(e, path))?;
        match renamed {
            Some(new) => zw.raw_copy_file_rename(f, new),
            None => zw.raw_copy_file(f),
        }
        .map_err(|e| map_err(e, path))?;
    }
    let owned: Vec<Source> = adds.into_iter().cloned().collect();
    write_sources(&mut zw, &owned, opts, ctx, &mut report)?;
    let out = zw.finish().map_err(|e| map_err(e, path))?;
    drop(src);
    report.outputs = out.commit()?;
    ctx.finish();
    Ok(report)
}

#[cfg(test)]
mod tests;
