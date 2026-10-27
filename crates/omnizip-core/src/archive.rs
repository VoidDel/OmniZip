//! Engine and reader traits, plus the high-level extract / test / preview operations that every
//! front end (GUI, CLI, shell, mount daemon) shares.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    Capabilities, Error, ErrorCode, FormatId, JobCtx, Result,
    encoding::NameEncoding,
    entry::{Entry, EntryId},
    format::archive_stem,
    job::{Confirm, Phase},
    options::{ExtractOptions, OpenOptions, UpdateOp, WriteOptions},
    sink::{CaptureSink, ExtractSink, FsSink, Report, Selection, TestSink},
    source::Source,
    tree::TreeIndex,
};

/// Archive-level facts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveInfo {
    pub path: PathBuf,
    pub format: FormatId,
    /// Compression methods in use (`LZMA2`, `Deflate`, …), without encryption.
    pub methods: Vec<String>,
    pub solid: bool,
    /// Any entry is encrypted.
    pub encrypted: bool,
    /// The file list itself is encrypted (7z, RAR).
    pub header_encrypted: bool,
    /// Encryption scheme name (`AES-256`, `ZipCrypto`).
    pub encryption: Option<String>,
    pub comment: Option<String>,
    /// Number of volume files.
    pub volumes: u32,
    /// Name encoding used to decode entry names (`None` when the format stores Unicode).
    pub encoding: Option<NameEncoding>,
    /// Size on disk (all volumes).
    pub archive_size: u64,
    /// Embedded in an executable.
    pub sfx: bool,
}

impl ArchiveInfo {
    /// Fresh info with defaults.
    pub fn new(path: &Path, format: FormatId, archive_size: u64) -> Self {
        Self {
            path: path.to_owned(),
            format,
            methods: Vec::new(),
            solid: false,
            encrypted: false,
            header_encrypted: false,
            encryption: None,
            comment: None,
            volumes: 1,
            encoding: None,
            archive_size,
            sfx: false,
        }
    }

    /// Adds a method name once.
    pub fn add_method(&mut self, m: impl Into<String>) {
        let m = m.into();
        if !self.methods.contains(&m) {
            self.methods.push(m);
        }
    }
}

/// An opened archive.
pub trait ArchiveReader: Send {
    /// Archive-level facts.
    fn info(&self) -> &ArchiveInfo;

    /// All entries, in archive order. `entries()[i].id == i`.
    fn entries(&self) -> &[Entry];

    /// Delivers every entry the sink wants, in archive order.
    fn extract_to(&mut self, sink: &mut dyn ExtractSink, ctx: &JobCtx) -> Result<()>;

    /// Verifies every entry. The default decodes everything into a [`TestSink`].
    fn test(&mut self, selection: Selection, ctx: &JobCtx) -> Result<Report> {
        let mut sink = TestSink::new(selection);
        self.extract_to(&mut sink, ctx)?;
        Ok(sink.report)
    }
}

/// A format implementation.
pub trait Engine: Send + Sync {
    /// Engine name for diagnostics (`zip`, `sevenz-rust2`, `unrar`).
    fn name(&self) -> &'static str;

    /// Formats this engine handles.
    fn formats(&self) -> &'static [FormatId];

    /// What the engine can do with one of its formats.
    fn capabilities(&self, format: FormatId) -> Capabilities;

    /// Refines a detection (for example `gz` → `tar.gz` after peeking into the stream).
    fn refine(&self, _path: &Path, detected: FormatId) -> FormatId {
        detected
    }

    /// Opens an archive and reads its entry list.
    fn open(
        &self,
        path: &Path,
        format: FormatId,
        opts: &OpenOptions,
        ctx: &JobCtx,
    ) -> Result<Box<dyn ArchiveReader>>;

    /// Creates a new archive from sources.
    fn create(
        &self,
        _dest: &Path,
        _sources: &[Source],
        opts: &WriteOptions,
        _ctx: &JobCtx,
    ) -> Result<Report> {
        Err(Error::new(ErrorCode::UnsupportedOperation).with("format", opts.format))
    }

    /// Modifies an existing archive in place.
    fn update(
        &self,
        _path: &Path,
        format: FormatId,
        _ops: &[UpdateOp],
        _opts: &WriteOptions,
        _ctx: &JobCtx,
    ) -> Result<Report> {
        Err(Error::new(ErrorCode::UnsupportedOperation).with("format", format))
    }
}

/// Where a smart extraction will write: `dest/<archive stem>` when the archive has several
/// top-level items, otherwise `dest` itself.
pub fn extraction_root(
    reader: &dyn ArchiveReader,
    tree: &TreeIndex,
    opts: &ExtractOptions,
    selection: &Selection,
) -> PathBuf {
    if !opts.smart_folder || selection.ids.is_some() {
        return opts.destination.clone();
    }
    if tree.needs_wrapper_folder() {
        let name = reader
            .info()
            .path
            .file_name()
            .map(|n| archive_stem(&n.to_string_lossy()))
            .unwrap_or_else(|| "archive".to_owned());
        let name = crate::path::sanitize(
            &name,
            crate::path::TargetOs::current(),
            crate::path::UnsafePathMode::Strip,
        )
        .map(|s| s.to_slash_path())
        .unwrap_or_else(|_| "archive".to_owned());
        opts.destination.join(name)
    } else {
        opts.destination.clone()
    }
}

/// Declared bytes and files of the selection.
pub fn selection_totals(entries: &[Entry], selection: &Selection) -> (u64, u64) {
    entries
        .iter()
        .filter(|e| selection.contains(e.id) && !e.is_dir())
        .fold((0, 0), |(b, f), e| (b + e.size, f + 1))
}

/// Extracts entries to disk: smart folder, bomb pre-check, then the engine pass.
pub fn extract(
    reader: &mut dyn ArchiveReader,
    opts: &ExtractOptions,
    selection: Selection,
    ctx: &JobCtx,
) -> Result<Report> {
    let tree = TreeIndex::build(reader.entries());
    let root = extraction_root(reader, &tree, opts, &selection);
    let (bytes, files) = selection_totals(reader.entries(), &selection);
    let archive_size = reader.info().archive_size;
    ctx.policy.check_entries(reader.entries().len() as u64)?;
    if ctx.policy.is_high_ratio(bytes, archive_size) {
        let ratio = bytes / archive_size.max(1);
        if !ctx.interaction.confirm(&Confirm::HighRatio {
            ratio,
            unpacked: bytes,
        }) {
            return Err(ctx.policy.bomb_error(bytes, archive_size));
        }
    }
    let archive_path = reader.info().path.clone();
    let mut sink = FsSink::new(
        root,
        opts.clone(),
        selection,
        Some(&archive_path),
        archive_size,
    )?;
    ctx.begin(Phase::Extracting, bytes, files);
    reader.extract_to(&mut sink, ctx)?;
    let report = sink.finish(ctx)?;
    ctx.finish();
    Ok(report)
}

/// Tests entries.
pub fn test(reader: &mut dyn ArchiveReader, selection: Selection, ctx: &JobCtx) -> Result<Report> {
    let (bytes, files) = selection_totals(reader.entries(), &selection);
    ctx.begin(Phase::Testing, bytes, files);
    let report = reader.test(selection, ctx)?;
    ctx.finish();
    Ok(report)
}

/// Reads up to `max` bytes of one entry (previews). Returns the data and whether it was cut.
pub fn read_entry(
    reader: &mut dyn ArchiveReader,
    id: EntryId,
    max: usize,
    ctx: &JobCtx,
) -> Result<(Vec<u8>, bool)> {
    if reader.entries().get(id as usize).is_none() {
        return Err(Error::invalid_argument(format!("no entry {id}")));
    }
    let mut sink = CaptureSink::new(id, max);
    reader.extract_to(&mut sink, ctx)?;
    let truncated = sink.truncated;
    sink.data
        .map(|d| (d, truncated))
        .ok_or_else(|| Error::internal("entry not delivered"))
}
