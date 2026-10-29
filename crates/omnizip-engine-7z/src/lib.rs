//! 7z engine: read (including encrypted headers and solid blocks) and write (LZMA2 with
//! multi-threading, AES-256 with optional file-list encryption, solid blocks, split volumes).

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use omnizip_core::{
    ArchiveInfo, ArchiveReader, Capabilities, Engine, Entry, EntryData, EntryKind, Error,
    ErrorCode, ExtractSink, FormatId, JobCtx, Level, OpenOptions, Phase, Report, Result, Source,
    SourceKind, WriteOptions,
    job::MAX_PASSWORD_ATTEMPTS,
    sink::drain,
    source,
    volume::{self, Output, ReadSeek},
};
use sevenz_rust2::{
    Archive, ArchiveEntry, ArchiveWriter, BlockDecoder, EncoderConfiguration, EncoderMethod,
    NtTime, Password, SourceReader,
    encoder_options::{
        AesEncoderOptions, Bzip2Options, Lzma2Options, LzmaOptions, ZstandardOptions,
    },
};

/// Windows attribute flag that marks Unix permission bits in the high 16 bits.
const FILE_ATTRIBUTE_UNIX_EXTENSION: u32 = 0x8000;
const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;
const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;

/// The 7z engine.
#[derive(Debug, Default, Clone, Copy)]
pub struct SevenZEngine;

impl Engine for SevenZEngine {
    fn name(&self) -> &'static str {
        "sevenz-rust2"
    }

    fn formats(&self) -> &'static [FormatId] {
        &[FormatId::SevenZ]
    }

    fn capabilities(&self, _format: FormatId) -> Capabilities {
        Capabilities::READ
            | Capabilities::WRITE
            | Capabilities::ENCRYPT
            | Capabilities::ENCRYPT_NAMES
            | Capabilities::DECRYPT
            | Capabilities::SOLID
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
        Ok(Box::new(SevenZReader::open(path, opts, ctx)?))
    }

    fn create(
        &self,
        dest: &Path,
        sources: &[Source],
        opts: &WriteOptions,
        ctx: &JobCtx,
    ) -> Result<Report> {
        create(dest, sources, opts, ctx)
    }
}

fn map_err(e: sevenz_rust2::Error, path: &Path) -> Error {
    use sevenz_rust2::Error as E;
    let err = match e {
        E::BadSignature(_) => Error::new(ErrorCode::UnknownFormat),
        E::UnsupportedVersion { .. } => Error::new(ErrorCode::UnsupportedFormat),
        E::ChecksumVerificationFailed => Error::new(ErrorCode::CrcMismatch),
        E::NextHeaderCrcMismatch => Error::new(ErrorCode::Corrupt).detail("header checksum"),
        E::Io(io, msg) => {
            let err = Error::from(io);
            if msg.is_empty() { err } else { err.detail(msg) }
        }
        E::FileOpen(io, p) => Error::io(io, p),
        E::Other(msg) => Error::new(ErrorCode::Corrupt).detail(msg),
        E::ExternalUnsupported => Error::new(ErrorCode::UnsupportedMethod),
        E::UnsupportedCompressionMethod(m) => {
            Error::new(ErrorCode::UnsupportedMethod).with("method", m)
        }
        E::Unsupported(m) => Error::new(ErrorCode::UnsupportedMethod).detail(m),
        E::MaxMemLimited { .. } => {
            Error::new(ErrorCode::UnsupportedMethod).detail("dictionary exceeds memory limit")
        }
        E::PasswordRequired => Error::new(ErrorCode::PasswordRequired),
        E::MaybeBadPassword(_) => Error::new(ErrorCode::WrongPassword),
        E::FileNotFound => Error::internal("7z entry vanished"),
        other => Error::new(ErrorCode::Corrupt).detail(other),
    };
    if err.param("path").is_none() {
        err.with("path", path.display())
    } else {
        err
    }
}

/// An opened 7z archive.
pub struct SevenZReader {
    source: Box<dyn ReadSeek>,
    archive: Archive,
    info: ArchiveInfo,
    entries: Vec<Entry>,
    /// Entry indices of each block, in decode order.
    block_files: Vec<Vec<usize>>,
    /// Blocks that need the password.
    block_encrypted: Vec<bool>,
    /// Indices of entries without data (directories, empty files).
    streamless: Vec<usize>,
    password: Option<String>,
    password_verified: bool,
    display_name: String,
}

impl SevenZReader {
    fn open(path: &Path, opts: &OpenOptions, ctx: &JobCtx) -> Result<Self> {
        let mut source = volume::open_input(path)?;
        let archive_size = volume::input_size(path)?;
        let display_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut password = opts.password.clone();
        let mut header_encrypted = false;
        let mut attempt = 0;
        let archive = loop {
            ctx.check()?;
            source
                .seek(SeekFrom::Start(0))
                .map_err(|e| Error::io(e, path))?;
            let pw = password
                .as_deref()
                .map_or_else(Password::empty, Password::new);
            match Archive::read(&mut source, &pw) {
                Ok(a) => break a,
                Err(sevenz_rust2::Error::PasswordRequired) => {
                    header_encrypted = true;
                    password = Some(ctx.ask_password(&display_name, None, attempt)?);
                    attempt += 1;
                }
                // A wrong key for an encrypted header shows up as a broken header. Tell that
                // apart from real damage by checking whether the header needs a key at all.
                Err(e) if password.is_some() && is_header_damage(&e) => {
                    if !header_encrypted {
                        source
                            .seek(SeekFrom::Start(0))
                            .map_err(|e| Error::io(e, path))?;
                        match Archive::read(&mut source, &Password::empty()) {
                            Err(sevenz_rust2::Error::PasswordRequired) => header_encrypted = true,
                            _ => return Err(map_err(e, path)),
                        }
                    }
                    if attempt >= MAX_PASSWORD_ATTEMPTS {
                        return Err(
                            Error::new(ErrorCode::WrongPassword).with("path", path.display())
                        );
                    }
                    password = Some(ctx.ask_password(&display_name, None, attempt.max(1))?);
                    attempt += 1;
                }
                Err(e) => return Err(map_err(e, path)),
            }
        };
        ctx.policy.check_entries(archive.files.len() as u64)?;

        let mut info = ArchiveInfo::new(path, FormatId::SevenZ, archive_size);
        info.solid = archive.is_solid;
        info.header_encrypted = header_encrypted;
        info.volumes = volume::split_volumes(path).map_or(1, |v| v.len() as u32);

        // Per-block methods, encryption and packed size.
        let pack_sizes = archive.pack_sizes();
        let first_pack = archive.stream_map.block_first_pack_stream_index();
        let mut block_methods = Vec::with_capacity(archive.blocks.len());
        let mut block_encrypted = Vec::with_capacity(archive.blocks.len());
        let mut block_packed = Vec::with_capacity(archive.blocks.len());
        for (b, block) in archive.blocks.iter().enumerate() {
            let mut names = Vec::new();
            let mut enc = false;
            for coder in &block.coders {
                match EncoderMethod::by_id(coder.encoder_method_id()) {
                    Some(m) if m.id() == EncoderMethod::ID_AES256_SHA256 => enc = true,
                    Some(m) => names.push(m.name().to_owned()),
                    None => names.push("?".to_owned()),
                }
            }
            for n in &names {
                info.add_method(n.clone());
            }
            let start = first_pack.get(b).copied().unwrap_or(0);
            let end = first_pack.get(b + 1).copied().unwrap_or(pack_sizes.len());
            block_packed.push(
                pack_sizes
                    .get(start..end)
                    .map_or(0, |s| s.iter().sum::<u64>()),
            );
            block_methods.push(names.join(" "));
            block_encrypted.push(enc);
        }
        if block_encrypted.iter().any(|e| *e) || header_encrypted {
            info.encrypted = true;
            info.encryption = Some("AES-256".to_owned());
        }

        let mut block_files = vec![Vec::new(); archive.blocks.len()];
        let mut streamless = Vec::new();
        let mut entries = Vec::with_capacity(archive.files.len());
        for (i, f) in archive.files.iter().enumerate() {
            let block = archive
                .stream_map
                .file_block_index
                .get(i)
                .copied()
                .flatten();
            let unix_mode = (f.has_windows_attributes
                && f.windows_attributes & FILE_ATTRIBUTE_UNIX_EXTENSION != 0)
                .then_some(f.windows_attributes >> 16);
            let kind = if f.is_anti_item {
                EntryKind::Other
            } else if f.is_directory {
                EntryKind::Dir
            } else if unix_mode.is_some_and(|m| m & S_IFMT == S_IFLNK) {
                EntryKind::Symlink
            } else {
                EntryKind::File
            };
            let mut name = f.name.replace('\\', "/");
            if kind == EntryKind::Dir && !name.ends_with('/') {
                name.push('/');
            }
            let mut e = Entry::new(i as u32, name, kind);
            e.size = f.size;
            e.crc32 = f.has_crc.then_some(f.crc as u32);
            e.mtime = f
                .has_last_modified_date
                .then(|| f.last_modified_date.into());
            e.unix_mode = unix_mode.map(|m| m & 0o7777);
            e.win_attrs = f
                .has_windows_attributes
                .then_some(f.windows_attributes & 0xFFFF);
            match block {
                Some(b) => {
                    if block_files[b].is_empty() {
                        e.packed_size = Some(block_packed[b]);
                    }
                    block_files[b].push(i);
                    e.encrypted = block_encrypted[b];
                    e.method = Some(block_methods[b].clone());
                }
                None => streamless.push(i),
            }
            entries.push(e);
        }

        Ok(Self {
            source,
            archive,
            info,
            entries,
            block_files,
            block_encrypted,
            streamless,
            password,
            // A key that decrypted the file list is proven; others are probed on first use.
            password_verified: header_encrypted,
            display_name,
        })
    }

    fn threads() -> u32 {
        std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
    }

    /// Decodes a little of an encrypted block to check the password (7z has no check value; a
    /// wrong key makes the LZMA decoder fail almost immediately).
    fn probe_block(
        &mut self,
        b: usize,
        pw: &Password,
    ) -> std::result::Result<(), sevenz_rust2::Error> {
        let dec = BlockDecoder::new(1, b, &self.archive, pw, &mut self.source);
        let mut budget: u64 = 256 * 1024;
        dec.for_each_entries(&mut |_e, r| {
            let n = io::copy(&mut r.take(budget), &mut io::sink())
                .map_err(|e| sevenz_rust2::Error::Io(e, "".into()))?;
            budget -= n.min(budget);
            Ok(budget > 0)
        })?;
        Ok(())
    }

    fn ensure_password(&mut self, b: usize, ctx: &JobCtx) -> Result<Password> {
        if !self.block_encrypted[b] {
            return Ok(self
                .password
                .as_deref()
                .map_or_else(Password::empty, Password::new));
        }
        let mut attempt = 0;
        loop {
            if self.password.is_none() {
                self.password = Some(ctx.ask_password(&self.display_name, None, attempt)?);
            }
            let pw = Password::new(self.password.as_deref().unwrap_or_default());
            if self.password_verified {
                return Ok(pw);
            }
            match self.probe_block(b, &pw) {
                Ok(()) => {
                    self.password_verified = true;
                    return Ok(pw);
                }
                Err(_) => {
                    self.password = None;
                    attempt += 1;
                    if attempt >= MAX_PASSWORD_ATTEMPTS {
                        return Err(
                            Error::new(ErrorCode::WrongPassword).with("path", &self.display_name)
                        );
                    }
                }
            }
        }
    }
}

fn is_header_damage(e: &sevenz_rust2::Error) -> bool {
    use sevenz_rust2::Error as E;
    matches!(
        e,
        E::MaybeBadPassword(_)
            | E::NextHeaderCrcMismatch
            | E::ChecksumVerificationFailed
            | E::Other(_)
            | E::Io(..)
            | E::BadTerminatedHeader(_)
            | E::BadTerminatedStreamsInfo(_)
            | E::BadTerminatedUnpackInfo
            | E::BadTerminatedPackInfo(_)
            | E::BadTerminatedSubStreamsInfo
    )
}

impl ArchiveReader for SevenZReader {
    fn info(&self) -> &ArchiveInfo {
        &self.info
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn extract_to(&mut self, sink: &mut dyn ExtractSink, ctx: &JobCtx) -> Result<()> {
        let path = self.info.path.clone();
        for b in 0..self.block_files.len() {
            ctx.check()?;
            // Blocks without wanted entries are skipped entirely.
            if !self.block_files[b]
                .iter()
                .any(|&i| sink.wants(&self.entries[i]))
            {
                continue;
            }
            let pw = self.ensure_password(b, ctx)?;
            let order = self.block_files[b].clone();
            let entries = &self.entries;
            let mut k = 0usize;
            let mut sink_error: Option<Error> = None;
            let mut stop = false;
            let dec = BlockDecoder::new(Self::threads(), b, &self.archive, &pw, &mut self.source);
            let res = dec.for_each_entries(&mut |_e, reader| {
                let Some(&idx) = order.get(k) else {
                    return Ok(false);
                };
                k += 1;
                let entry = &entries[idx];
                if sink.wants(entry)
                    && let Err(e) = sink.entry(entry, EntryData::Stream(reader), ctx)
                {
                    sink_error = Some(e);
                    return Err(sevenz_rust2::Error::Other("aborted by sink".into()));
                }
                // Solid blocks: the next entry starts where this one ends.
                drain(reader).map_err(|e| sevenz_rust2::Error::Io(e, "drain".into()))?;
                if sink.done() {
                    stop = true;
                    return Ok(false);
                }
                Ok(true)
            });
            if let Some(e) = sink_error {
                return Err(e);
            }
            res.map_err(|e| map_err(e, &path))?;
            if stop {
                return Ok(());
            }
        }
        for &i in &self.streamless.clone() {
            ctx.check()?;
            let entry = &self.entries[i];
            if sink.wants(entry) {
                sink.entry(entry, EntryData::None, ctx)?;
                if sink.done() {
                    break;
                }
            }
        }
        Ok(())
    }
}

/// Content method chain for the options.
fn content_methods(opts: &WriteOptions) -> Result<Vec<EncoderConfiguration>> {
    let i = opts.level.index();
    let mut methods: Vec<EncoderConfiguration> = Vec::new();
    if let Some(pw) = &opts.password {
        methods.push(AesEncoderOptions::new(Password::new(pw)).into());
    }
    if opts.level == Level::Store {
        methods.push(EncoderMethod::COPY.into());
        return Ok(methods);
    }
    let level = [0u32, 1, 3, 5, 7, 9, 9][i];
    let m = opts.method.as_deref().map(str::to_ascii_lowercase);
    match m.as_deref() {
        None | Some("lzma2") => {
            let threads = opts.effective_threads();
            let mut o = if threads > 1 {
                Lzma2Options::from_level_mt(level, threads, 0)
            } else {
                Lzma2Options::from_level(level)
            };
            if opts.level == Level::Ultra {
                o.set_dictionary_size(128 << 20);
            }
            methods.push(o.into());
        }
        Some("lzma") => methods.push(LzmaOptions::from_level(level).into()),
        Some("bzip2") => methods.push(Bzip2Options::from_level(level.max(1)).into()),
        Some("zstd" | "zstandard") => {
            methods.push(ZstandardOptions::from_level([0, 1, 3, 6, 12, 19, 22][i]).into())
        }
        Some("copy" | "store") => methods.push(EncoderMethod::COPY.into()),
        Some(other) => return Err(Error::new(ErrorCode::UnsupportedMethod).with("method", other)),
    }
    Ok(methods)
}

/// Methods 7z can write.
pub const WRITE_METHODS: &[&str] = &["LZMA2", "LZMA", "BZip2", "Zstandard", "Copy"];

/// Opens a source file lazily and reports progress while it is read.
struct LazySource<'a> {
    path: PathBuf,
    file: Option<File>,
    ctx: &'a JobCtx,
    finished: bool,
}

impl Read for LazySource<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.ctx.cancel.is_cancelled() {
            return Err(Error::cancelled().into());
        }
        if self.file.is_none() {
            self.ctx.begin_file(&self.path.to_string_lossy());
            self.file = Some(File::open(&self.path)?);
        }
        let n = self.file.as_mut().expect("opened above").read(buf)?;
        if n == 0 && !self.finished {
            self.finished = true;
            self.file = None;
            self.ctx.file_done();
        }
        self.ctx.add_bytes(n as u64);
        Ok(n)
    }
}

fn entry_for(s: &Source) -> ArchiveEntry {
    let name = s.archive_path.trim_end_matches('/');
    let mut e = match s.kind {
        SourceKind::Dir => ArchiveEntry::new_directory(name),
        _ => ArchiveEntry::new_file(name),
    };
    if let Some(t) = s.mtime.and_then(|t| NtTime::try_from(t).ok()) {
        e.has_last_modified_date = true;
        e.last_modified_date = t;
    }
    let mut attrs = match s.kind {
        SourceKind::Dir => FILE_ATTRIBUTE_DIRECTORY,
        _ => FILE_ATTRIBUTE_ARCHIVE,
    };
    if s.readonly {
        attrs |= FILE_ATTRIBUTE_READONLY;
    }
    let mode = match (&s.kind, s.mode) {
        (SourceKind::Symlink { .. }, m) => Some(S_IFLNK | m.unwrap_or(0o777)),
        (SourceKind::Dir, Some(m)) => Some(0o040000 | m),
        (SourceKind::File, Some(m)) => Some(0o100000 | m),
        _ => None,
    };
    if let Some(m) = mode {
        attrs |= FILE_ATTRIBUTE_UNIX_EXTENSION | (m << 16);
    }
    e.has_windows_attributes = true;
    e.windows_attributes = attrs;
    e
}

/// Solid blocks are capped so one damaged block cannot take the whole archive with it and memory
/// stays bounded.
const SOLID_BLOCK_BYTES: u64 = 2 << 30;
const SOLID_BLOCK_FILES: usize = 100_000;

fn create(dest: &Path, sources: &[Source], opts: &WriteOptions, ctx: &JobCtx) -> Result<Report> {
    let out = Output::create(dest, opts.volume_size)?;
    let mut w = ArchiveWriter::new(out).map_err(|e| map_err(e, dest))?;
    w.set_content_methods(content_methods(opts)?);
    w.set_encrypt_header(opts.password.is_some() && opts.encrypt_names);
    ctx.begin(
        Phase::Compressing,
        source::total_size(sources),
        source::file_count(sources),
    );

    let mut report = Report::default();
    let mut pending: Vec<&Source> = Vec::new();
    let mut pending_bytes = 0u64;

    let flush = |w: &mut ArchiveWriter<Output>,
                 pending: &mut Vec<&Source>,
                 report: &mut Report|
     -> Result<()> {
        if pending.is_empty() {
            return Ok(());
        }
        let entries: Vec<ArchiveEntry> = pending.iter().map(|s| entry_for(s)).collect();
        let readers: Vec<SourceReader<LazySource<'_>>> = pending
            .iter()
            .map(|s| {
                SourceReader::new(LazySource {
                    path: s.fs_path.clone(),
                    file: None,
                    ctx,
                    finished: false,
                })
            })
            .collect();
        w.push_archive_entries(entries, readers)
            .map_err(|e| map_err(e, dest))?;
        report.files += pending.len() as u64;
        report.bytes += pending.iter().map(|s| s.size).sum::<u64>();
        pending.clear();
        Ok(())
    };

    // Directories go first. sevenz-rust2 writes the kEmptyFile bit vector only up to the last
    // empty file, while readers (Bandizip too) take one bit per stream-less entry: a directory after
    // the last empty file would leave the header unreadable. With the directories first, the
    // last stream-less entry is always an empty file (hasenbanck/sevenz-rust2#155).
    for s in sources.iter().filter(|s| s.kind == SourceKind::Dir) {
        ctx.check()?;
        w.push_archive_entry(entry_for(s), None::<&[u8]>)
            .map_err(|e| map_err(e, dest))?;
        report.dirs += 1;
    }
    for s in sources {
        ctx.check()?;
        match &s.kind {
            SourceKind::Dir => {}
            SourceKind::Symlink { target } => {
                w.push_archive_entry(entry_for(s), Some(target.as_bytes()))
                    .map_err(|e| map_err(e, dest))?;
                report.links += 1;
            }
            SourceKind::File if s.size == 0 => {
                // 7z stores empty files as stream-less entries.
                w.push_archive_entry(entry_for(s), None::<&[u8]>)
                    .map_err(|e| map_err(e, dest))?;
                report.files += 1;
                ctx.file_done();
            }
            SourceKind::File if opts.solid => {
                pending.push(s);
                pending_bytes += s.size;
                if pending_bytes >= SOLID_BLOCK_BYTES || pending.len() >= SOLID_BLOCK_FILES {
                    flush(&mut w, &mut pending, &mut report)?;
                    pending_bytes = 0;
                }
            }
            SourceKind::File => {
                let reader = LazySource {
                    path: s.fs_path.clone(),
                    file: None,
                    ctx,
                    finished: false,
                };
                w.push_archive_entry(entry_for(s), Some(reader))
                    .map_err(|e| map_err(e, dest))?;
                report.files += 1;
                report.bytes += s.size;
            }
        }
    }
    flush(&mut w, &mut pending, &mut report)?;
    ctx.check()?;
    let out = w.finish().map_err(|e| Error::io(e, dest))?;
    report.outputs = out.commit()?;
    ctx.finish();
    Ok(report)
}

#[cfg(test)]
mod tests;
