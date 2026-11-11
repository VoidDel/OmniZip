//! Engine and reader traits, plus the high-level extract / test / preview operations that every
//! front end (GUI, CLI, shell, mount daemon) shares.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    Capabilities, Error, ErrorCode, FormatId, JobCtx, MountClass, Result,
    encoding::NameEncoding,
    entry::{Entry, EntryId},
    format::archive_stem,
    job::{Confirm, Phase},
    options::{ExtractOptions, OpenOptions, UpdateOp, WriteOptions},
    sink::{CaptureSink, ExtractSink, FileSink, FsSink, Report, Selection, TestSink},
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

    /// Where a file's data lies byte for byte in the archive file, as `(offset, length)`:
    /// stored, unencrypted, in an archive of one file. Mounts read such files in place instead
    /// of decoding them. `None` when that is not so, or not known.
    fn stored_range(&mut self, _id: EntryId) -> Option<(u64, u64)> {
        None
    }

    /// Another reader of the same archive, which decodes on another thread while this one does:
    /// a mount decodes several entries or solid blocks at once this way (`docs/12-mount.md`
    /// §5). It shares what was parsed and the open files where the engine can. `None` when the
    /// engine cannot.
    fn try_clone(&self) -> Option<Box<dyn ArchiveReader>> {
        None
    }

    /// The solid block holding a file's data, in archives with several: the files of one block
    /// decode only together, from the block's start, while different blocks decode
    /// independently. `None` for entries decoded on their own, and when not known.
    fn solid_block(&self, _id: EntryId) -> Option<u32> {
        None
    }

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

/// Declared bytes and files of the selection. Declared sizes come from the archive, so the sum
/// saturates: a wrapped total would pass the bomb check.
pub fn selection_totals(entries: &[Entry], selection: &Selection) -> (u64, u64) {
    entries
        .iter()
        .filter(|e| selection.contains(e.id) && !e.is_dir())
        .fold((0, 0), |(b, f), e| (b.saturating_add(e.size), f + 1))
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
    // Entries sharing their data (an overlap bomb) are extracted one at a time at most.
    let overlapping = reader
        .entries()
        .iter()
        .filter(|e| e.overlaps && !e.is_dir() && selection.contains(e.id))
        .count();
    if overlapping > 1 && !ctx.policy.allow_high_ratio {
        return Err(ctx
            .policy
            .bomb_error(bytes, archive_size)
            .with("issue", "overlapping-entries")
            .with("count", overlapping));
    }
    let confirmed = ctx.policy.is_high_ratio(bytes, archive_size);
    if confirmed {
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
    if confirmed {
        sink = sink.high_ratio_confirmed();
    }
    ctx.begin(Phase::Extracting, bytes, files);
    match parallel_plan(reader, &sink, opts.effective_threads()) {
        Some(plan) => extract_parallel(reader, &mut sink, plan, opts.effective_threads(), ctx)?,
        None => reader.extract_to(&mut sink, ctx)?,
    }
    let report = sink.finish(ctx)?;
    ctx.finish();
    Ok(report)
}

/// Files a parallel extraction hands out together: up to this many neighbours in the archive,
/// and this many declared bytes (a larger file goes alone).
const UNIT_ENTRIES: usize = 32;
const UNIT_BYTES: u64 = 8 << 20;

/// How an extraction is shared out among threads.
struct ParallelPlan {
    /// Directories, extracted first on the calling thread.
    dirs: Vec<EntryId>,
    /// Units of files, each decoded and written by one thread, the largest first so that a big
    /// file does not end the job alone.
    units: Vec<Vec<EntryId>>,
}

/// The plan for extracting on `threads` threads, or `None` to extract on one. Entries must
/// decode on their own (ZIP, non-solid 7z: units of neighbouring files) or in solid blocks the
/// engine knows (7z: a unit per block), and no two selected entries may land on the same file:
/// which of them were written last would then depend on timing.
fn parallel_plan(
    reader: &dyn ArchiveReader,
    sink: &FsSink,
    threads: usize,
) -> Option<ParallelPlan> {
    let info = reader.info();
    if threads < 2 || !matches!(info.format, FormatId::Zip | FormatId::SevenZ) {
        return None;
    }
    let selected: Vec<&Entry> = reader
        .entries()
        .iter()
        .filter(|e| sink.selects(e.id))
        .collect();
    let mut targets = std::collections::HashSet::new();
    for e in &selected {
        if let Some(key) = sink.target_key(e)
            && !targets.insert(key)
        {
            return None;
        }
    }
    let (dirs, files): (Vec<&Entry>, Vec<&Entry>) = selected.into_iter().partition(|e| e.is_dir());
    let mut units: Vec<(u64, Vec<EntryId>)> = Vec::new();
    match info.format.mount_class(info.solid) {
        MountClass::B => {
            let (mut unit, mut bytes) = (Vec::new(), 0u64);
            for e in files {
                if !unit.is_empty()
                    && (unit.len() >= UNIT_ENTRIES || bytes.saturating_add(e.size) > UNIT_BYTES)
                {
                    units.push((bytes, std::mem::take(&mut unit)));
                    bytes = 0;
                }
                unit.push(e.id);
                bytes = bytes.saturating_add(e.size);
            }
            if !unit.is_empty() {
                units.push((bytes, unit));
            }
        }
        MountClass::C => {
            let mut blocks: std::collections::BTreeMap<Option<u32>, (u64, Vec<EntryId>)> =
                std::collections::BTreeMap::new();
            for e in files {
                let b = blocks.entry(reader.solid_block(e.id)).or_default();
                b.0 = b.0.saturating_add(e.size);
                b.1.push(e.id);
            }
            if blocks.keys().filter(|b| b.is_some()).count() < 2 {
                return None;
            }
            units = blocks.into_values().collect();
        }
        MountClass::A | MountClass::D => return None,
    }
    if units.len() < 2 {
        return None;
    }
    units.sort_by_key(|(bytes, _)| std::cmp::Reverse(*bytes));
    Some(ParallelPlan {
        dirs: dirs.iter().map(|e| e.id).collect(),
        units: units.into_iter().map(|(_, u)| u).collect(),
    })
}

/// Runs a plan: directories on this thread, then the units on up to `threads` threads, each with
/// its own clone of the reader and its own writer of the sink. The first error stops the other
/// threads before their next unit and is returned.
fn extract_parallel(
    reader: &mut dyn ArchiveReader,
    sink: &mut FsSink,
    plan: ParallelPlan,
    threads: usize,
    ctx: &JobCtx,
) -> Result<()> {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    let ParallelPlan { dirs, mut units } = plan;
    if !dirs.is_empty() {
        sink.set_unit(dirs);
        reader.extract_to(sink, ctx)?;
    }
    // The password is asked for once: a unit holding an encrypted entry is decoded here first,
    // so that the clones made after it know the password.
    let encrypted = |u: &Vec<EntryId>| {
        u.iter().any(|&id| {
            reader
                .entries()
                .get(id as usize)
                .is_some_and(|e| e.encrypted)
        })
    };
    if let Some(i) = units.iter().position(encrypted) {
        sink.set_unit(units.remove(i));
        reader.extract_to(sink, ctx)?;
    }
    let mut clones: Vec<Box<dyn ArchiveReader>> = Vec::new();
    while clones.len() + 1 < threads.min(units.len()) {
        match reader.try_clone() {
            Some(r) => clones.push(r),
            None => break,
        }
    }
    let writers = (0..=clones.len())
        .map(|_| sink.fork())
        .collect::<Result<Vec<_>>>()?;
    let units = &units;
    let (next, stop) = (AtomicUsize::new(0), AtomicBool::new(false));
    let failed: Mutex<Option<Error>> = Mutex::new(None);
    let readers = std::iter::once(reader).chain(
        clones
            .iter_mut()
            .map(|r| -> &mut dyn ArchiveReader { r.as_mut() }),
    );
    std::thread::scope(|s| {
        for (r, mut w) in readers.zip(writers) {
            let (next, stop, failed) = (&next, &stop, &failed);
            s.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let Some(unit) = units.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    w.set_unit(unit.iter().copied());
                    if let Err(e) = r.extract_to(&mut w, ctx) {
                        stop.store(true, Ordering::Relaxed);
                        failed
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .get_or_insert(e);
                        break;
                    }
                }
            });
        }
    });
    match failed.into_inner().unwrap_or_else(|e| e.into_inner()) {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Tests entries.
pub fn test(reader: &mut dyn ArchiveReader, selection: Selection, ctx: &JobCtx) -> Result<Report> {
    let (bytes, files) = selection_totals(reader.entries(), &selection);
    ctx.begin(Phase::Testing, bytes, files);
    let report = reader.test(selection, ctx)?;
    ctx.finish();
    Ok(report)
}

/// Writes one entry to `path` as it is decoded (opening it with another program, previewing it in
/// another app), never holding it whole in memory. `archive_size` sets the bomb cap. Returns the
/// bytes written.
pub fn extract_entry(
    reader: &mut dyn ArchiveReader,
    id: EntryId,
    path: &std::path::Path,
    archive_size: u64,
    ctx: &JobCtx,
) -> Result<u64> {
    if reader.entries().get(id as usize).is_none() {
        return Err(Error::invalid_argument(format!("no entry {id}")));
    }
    let limit = ctx.policy.max_unpacked(archive_size);
    let mut sink = FileSink::new(id, path.to_owned(), limit);
    reader.extract_to(&mut sink, ctx)?;
    sink.written
        .ok_or_else(|| Error::internal("entry not delivered"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::EntryKind;

    /// Entries with sizes, optionally in solid blocks; extraction is never run.
    struct Planned {
        info: ArchiveInfo,
        entries: Vec<Entry>,
        blocks: Option<Vec<u32>>,
    }

    impl Planned {
        fn new(format: FormatId, files: &[(&str, u64)]) -> Self {
            let entries = files
                .iter()
                .enumerate()
                .map(|(i, (path, size))| {
                    let kind = if path.ends_with('/') {
                        EntryKind::Dir
                    } else {
                        EntryKind::File
                    };
                    let mut e = Entry::new(i as u32, (*path).into(), kind);
                    e.size = *size;
                    e
                })
                .collect();
            Self {
                info: ArchiveInfo::new(Path::new("x"), format, 1),
                entries,
                blocks: None,
            }
        }
    }

    impl ArchiveReader for Planned {
        fn info(&self) -> &ArchiveInfo {
            &self.info
        }

        fn entries(&self) -> &[Entry] {
            &self.entries
        }

        fn solid_block(&self, id: EntryId) -> Option<u32> {
            self.blocks.as_ref()?.get(id as usize).copied()
        }

        fn extract_to(&mut self, _sink: &mut dyn ExtractSink, _ctx: &JobCtx) -> Result<()> {
            unreachable!()
        }
    }

    fn plan(r: &Planned, threads: usize) -> Option<ParallelPlan> {
        let d = tempfile::tempdir().unwrap();
        let sink = FsSink::new(
            d.path().to_owned(),
            ExtractOptions::default(),
            Selection::all(),
            None,
            1,
        )
        .unwrap();
        parallel_plan(r, &sink, threads)
    }

    #[test]
    fn files_are_shared_out_in_units_the_largest_first() {
        let mut files: Vec<(String, u64)> = vec![("dir/".into(), 0)];
        files.extend((0..70).map(|i| (format!("dir/f{i:02}"), 1000)));
        files.push(("dir/big".into(), 20 << 20));
        let named: Vec<(&str, u64)> = files.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        let r = Planned::new(FormatId::Zip, &named);
        let p = plan(&r, 4).expect("a plan");
        assert_eq!(p.dirs, vec![0]);
        assert_eq!(p.units[0], vec![71], "the big file alone, first");
        assert_eq!(p.units[1].len(), UNIT_ENTRIES);
        let all: usize = p.units.iter().map(Vec::len).sum();
        assert_eq!(all, 71);
        // One thread, or a format that is one stream: no plan.
        assert!(plan(&r, 1).is_none());
        assert!(plan(&Planned::new(FormatId::TarGz, &named), 4).is_none());
    }

    #[test]
    fn entries_landing_on_one_file_extract_on_one_thread() {
        let mut files: Vec<(String, u64)> = (0..70).map(|i| (format!("f{i:02}"), 10)).collect();
        // The same file, differently written: case and a `./` prefix.
        files.push(("./F00".into(), 10));
        let named: Vec<(&str, u64)> = files.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        let r = Planned::new(FormatId::Zip, &named);
        assert_eq!(
            plan(&r, 4).is_none(),
            cfg!(any(windows, target_os = "macos"))
        );
        files.push(("f01".into(), 10));
        let named: Vec<(&str, u64)> = files.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        assert!(plan(&Planned::new(FormatId::Zip, &named), 4).is_none());
    }

    #[test]
    fn solid_blocks_are_units_of_their_own() {
        // Block 1 holds the most bytes.
        let named: Vec<(String, u64)> = (0..10)
            .map(|i| (format!("f{i}"), if (3..7).contains(&i) { 500 } else { 100 }))
            .collect();
        let named: Vec<(&str, u64)> = named.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        let mut r = Planned::new(FormatId::SevenZ, &named);
        r.info.solid = true;
        // One block: nothing to share out.
        r.blocks = Some(vec![0; 10]);
        assert!(plan(&r, 4).is_none());
        r.blocks = Some(vec![0, 0, 0, 1, 1, 1, 1, 2, 2, 0]);
        let p = plan(&r, 4).expect("a plan");
        assert_eq!(
            p.units,
            vec![vec![3, 4, 5, 6], vec![0, 1, 2, 9], vec![7, 8]]
        );
    }
}
