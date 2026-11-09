//! Destinations for extracted data.
//!
//! Engines walk the archive in its natural order (mandatory for solid and stream formats) and
//! hand each wanted entry to an [`ExtractSink`]. The sink owns every decision about the file
//! system; engines never build output paths themselves.
//!
//! Engine contract: after [`ExtractSink::entry`] returns, the engine drains whatever the sink
//! left unread, so the next entry of a solid block starts at the right position.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::SystemTime,
};

use serde::{Deserialize, Serialize};

use crate::{
    Error, ErrorCode, JobCtx, Result,
    dirguard::{DirGuard, EnterError, Kind},
    entry::{Entry, EntryId, EntryKind},
    job::{
        Confirm, Conflict, ConflictChoice, ConflictDecision, copy_with_buffer, copy_with_progress,
    },
    options::{ExtractOptions, OverwriteMode, PathMode, SymlinkPolicy},
    origin::{self, Origin},
    path::{TargetOs, UnsafePathMode, numbered_name, resolve_link, sanitize},
};

/// How an engine delivers an entry's content.
pub enum EntryData<'a> {
    /// Decompressed content as a stream.
    Stream(&'a mut dyn Read),
    /// The engine can only write files itself (RAR): the sink supplies a path.
    WriteTo(&'a mut dyn FnMut(&Path) -> Result<()>),
    /// No content: directories, and links whose target is in [`Entry::link_target`].
    None,
}

/// Receives entries from an engine.
pub trait ExtractSink {
    /// Whether the engine should deliver this entry (selection filter).
    fn wants(&self, entry: &Entry) -> bool;
    /// Every entry wanted, when the sink knows them up front. An engine that can reach any
    /// entry directly (ZIP) then goes straight to them instead of asking [`wants`](Self::wants)
    /// about each entry before: a mount decodes one entry at a time, and asking about all
    /// entries up to it every time adds up to the square of their number. `None` (the default):
    /// ask about each.
    fn wanted(&self) -> Option<Vec<EntryId>> {
        None
    }
    /// Handles one entry.
    fn entry(&mut self, entry: &Entry, data: EntryData<'_>, ctx: &JobCtx) -> Result<()>;
    /// Whether the engine may stop early (everything wanted has been handled).
    fn done(&self) -> bool {
        false
    }
}

/// A non-fatal problem recorded during a job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Warning {
    pub code: ErrorCode,
    pub path: String,
    pub detail: Option<String>,
}

/// Summary of a finished job.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub files: u64,
    pub dirs: u64,
    pub links: u64,
    pub bytes: u64,
    pub skipped: u64,
    pub warnings: Vec<Warning>,
    /// Where things were written: the folder extracted (the single folder written below the
    /// destination, otherwise the destination), or archive volumes.
    pub outputs: Vec<PathBuf>,
}

impl Report {
    /// Records a warning.
    pub fn warn(&mut self, code: ErrorCode, path: impl Into<String>, detail: Option<String>) {
        self.warnings.push(Warning {
            code,
            path: path.into(),
            detail,
        });
    }

    /// CLI exit code for a job that finished: 0 clean, 1 warnings, 8 security refusals.
    pub fn exit_code(&self) -> i32 {
        if self.warnings.iter().any(|w| w.code.is_security()) {
            8
        } else if self.warnings.is_empty() {
            0
        } else {
            1
        }
    }
}

/// Which entries to extract.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// `None` = every entry.
    pub ids: Option<HashSet<EntryId>>,
    /// Leading path components removed from selected entries, so extracting `src/app/x` from
    /// inside `src/app` yields `x` rather than `src/app/x`.
    pub strip_prefix: Vec<String>,
}

impl Selection {
    /// Everything.
    pub fn all() -> Self {
        Self::default()
    }

    /// Selected ids, optionally relative to a directory.
    pub fn of(ids: impl IntoIterator<Item = EntryId>, relative_to: &str) -> Self {
        Self {
            ids: Some(ids.into_iter().collect()),
            strip_prefix: crate::path::logical_components(relative_to)
                .into_iter()
                .map(str::to_owned)
                .collect(),
        }
    }

    /// Whether an entry is selected.
    pub fn contains(&self, id: EntryId) -> bool {
        self.ids.as_ref().is_none_or(|s| s.contains(&id))
    }
}

struct DeferredLink {
    path: PathBuf,
    target: String,
    kind: EntryKind,
    entry_path: String,
}

/// What every writer of one extraction shares ([`FsSink::fork`]).
struct Shared {
    root: PathBuf,
    opts: ExtractOptions,
    selection: Selection,
    os: TargetOs,
    origin: Option<Origin>,
    archive_size: u64,
    /// Bytes written so far, for the streaming bomb check.
    total_written: AtomicU64,
    bomb_confirmed: AtomicBool,
    /// Questions to the user go one at a time (conflicts, a high ratio); holds the conflict
    /// choice the user applied to all.
    asking: Mutex<Option<ConflictChoice>>,
    state: Mutex<State>,
}

/// What the writers of one extraction record.
#[derive(Default)]
struct State {
    report: Report,
    dir_times: Vec<(PathBuf, SystemTime)>,
    /// Directories this extraction created: only these take a mode from the archive, never a
    /// directory that was already there (the destination itself included).
    created_dirs: HashSet<PathBuf>,
    /// Modes of directory entries, applied at the end so a read-only directory can still be
    /// filled.
    dir_modes: Vec<(PathBuf, u32)>,
    links: Vec<DeferredLink>,
    /// Sanitized archive path → written file, for hard links.
    written: HashMap<String, PathBuf>,
    /// The folder written directly below the root, while it is the only item there; `top_mixed`
    /// once anything else is.
    top_dir: Option<PathBuf>,
    top_mixed: bool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Writes entries below a destination directory, enforcing the security policy.
///
/// Several writers can fill one destination from several threads ([`fork`](Self::fork)): they
/// share the report, the questions to the user and what is done at the end, and each holds its
/// own current directory and is handed its own entries ([`set_unit`](Self::set_unit)).
pub struct FsSink {
    shared: Arc<Shared>,
    /// The directory being written into, held open from the root down (`crate::dirguard`):
    /// nothing below the root is looked up by its path again, so a directory swapped for a link
    /// meanwhile cannot redirect a write.
    guard: DirGuard,
    /// Copy buffer shared by all files (allocated on first use).
    buf: Vec<u8>,
    /// The entries this writer still has to write, when it was handed some; otherwise it takes
    /// the whole selection.
    unit: Option<HashSet<EntryId>>,
}

impl FsSink {
    /// Creates a sink writing below `root` (created when missing).
    pub fn new(
        root: PathBuf,
        opts: ExtractOptions,
        selection: Selection,
        archive_path: Option<&Path>,
        archive_size: u64,
    ) -> Result<Self> {
        fs::create_dir_all(&root).map_err(|e| Error::io(e, &root))?;
        let guard = DirGuard::open(&root).map_err(|e| Error::io(e, &root))?;
        let origin = if opts.propagate_origin {
            archive_path.and_then(origin::read)
        } else {
            None
        };
        Ok(Self {
            shared: Arc::new(Shared {
                root,
                opts,
                selection,
                os: TargetOs::current(),
                origin,
                archive_size,
                total_written: AtomicU64::new(0),
                bomb_confirmed: AtomicBool::new(false),
                asking: Mutex::new(None),
                state: Mutex::default(),
            }),
            guard,
            buf: Vec::new(),
            unit: None,
        })
    }

    /// Another writer of the same extraction, for another thread: it shares everything but the
    /// directory it is in and its buffer. Only the first writer is finished.
    pub fn fork(&self) -> Result<Self> {
        let root = &self.shared.root;
        Ok(Self {
            shared: self.shared.clone(),
            guard: DirGuard::open(root).map_err(|e| Error::io(e, root))?,
            buf: Vec::new(),
            unit: None,
        })
    }

    /// Hands this writer `ids` (of the selection): it wants only these until they are written.
    pub fn set_unit(&mut self, ids: impl IntoIterator<Item = EntryId>) {
        self.unit = Some(ids.into_iter().collect());
    }

    /// The user already accepted a high ratio (the check on declared sizes before extracting):
    /// do not ask again while writing.
    pub fn high_ratio_confirmed(self) -> Self {
        self.shared.bomb_confirmed.store(true, Ordering::Relaxed);
        self
    }

    /// Overrides the file-system rules (tests; before any fork).
    pub fn with_target_os(mut self, os: TargetOs) -> Self {
        if let Some(s) = Arc::get_mut(&mut self.shared) {
            s.os = os;
        }
        self
    }

    /// The destination root.
    pub fn root(&self) -> &Path {
        &self.shared.root
    }

    /// Whether the selection holds `id`.
    pub fn selects(&self, id: EntryId) -> bool {
        self.shared.selection.contains(id)
    }

    fn st(&self) -> MutexGuard<'_, State> {
        lock(&self.shared.state)
    }

    /// Where `entry` goes below the root, as a key that is the same for two entries exactly when
    /// they would land on the same file (names differing only in case are on Windows and macOS);
    /// `None` when it is skipped. Records nothing.
    pub fn target_key(&self, entry: &Entry) -> Option<String> {
        let s = sanitize(&entry.path, self.shared.os, self.path_mode()).ok()?;
        self.arrange(s.components).map(|c| link_key(&c))
    }

    /// Creates deferred links, applies directory modes and times and returns the report.
    pub fn finish(mut self, ctx: &JobCtx) -> Result<Report> {
        let (links, dir_modes, mut times, written) = {
            let mut st = self.st();
            (
                std::mem::take(&mut st.links),
                std::mem::take(&mut st.dir_modes),
                std::mem::take(&mut st.dir_times),
                std::mem::take(&mut st.written),
            )
        };
        let root = self.shared.root.clone();
        // Every symbolic link of this extraction, whether created before or after the one being
        // checked: the order they are created in must not decide where a link leads.
        let pending: HashMap<String, String> = links
            .iter()
            .filter(|l| l.kind == EntryKind::Symlink)
            .filter_map(|l| Some((link_key(&below(&root, &l.path)?), l.target.clone())))
            .collect();
        for l in links {
            ctx.check()?;
            self.create_link(l, &pending, &written);
        }
        for (p, mode) in dir_modes {
            if self.enter_existing(&p) {
                apply_dir_mode(self.guard.dir(), mode);
            }
        }
        // Deepest directories first so parents are not touched afterwards.
        times.sort_by_key(|(p, _)| std::cmp::Reverse(p.components().count()));
        for (p, t) in times {
            if self.enter_existing(&p) {
                let _ = filetime::set_file_handle_times(
                    self.guard.dir(),
                    None,
                    Some(filetime::FileTime::from_system_time(t)),
                );
            }
        }
        let mut st = std::mem::take(&mut *self.st());
        // A single folder (an archive holding one folder, without a wrapper folder) is what was
        // extracted, not the destination around it.
        let out = match st.top_dir.take() {
            Some(d) if !st.top_mixed => d,
            _ => root,
        };
        st.report.outputs = vec![out];
        Ok(st.report)
    }

    /// Records an item written directly below the root: a folder (`Some`) or anything else.
    fn note_top(&self, dir: Option<PathBuf>) {
        let mut st = self.st();
        match (dir, &st.top_dir) {
            (Some(d), Some(t)) if d == *t => {}
            (Some(d), None) if !st.top_mixed => st.top_dir = Some(d),
            _ => st.top_mixed = true,
        }
    }

    fn path_mode(&self) -> UnsafePathMode {
        if self.shared.opts.strip_unsafe_paths {
            UnsafePathMode::Strip
        } else {
            UnsafePathMode::Reject
        }
    }

    /// Sanitized components with the selection's prefix stripped, flattened in flat mode.
    fn arrange(&self, mut comps: Vec<String>) -> Option<Vec<String>> {
        let strip = &self.shared.selection.strip_prefix;
        if !strip.is_empty() && comps.len() > strip.len() && comps[..strip.len()] == strip[..] {
            comps.drain(..strip.len());
        }
        if self.shared.opts.path_mode == PathMode::Flat {
            let last = comps.pop()?;
            comps = vec![last];
        }
        Some(comps)
    }

    /// Relative components for an entry, or `None` (with a warning) when it must be skipped.
    fn relative(&self, entry: &Entry) -> Option<Vec<String>> {
        let comps = match sanitize(&entry.path, self.shared.os, self.path_mode()) {
            Ok(s) => {
                if !s.repaired.is_empty() {
                    let detail = format!("{:?}", s.repaired);
                    self.st()
                        .report
                        .warn(ErrorCode::PathEscape, &entry.path, Some(detail));
                }
                s.components
            }
            Err(e) => {
                let mut st = self.st();
                st.report
                    .warn(e.code(), &entry.path, e.param("issue").map(str::to_owned));
                st.report.skipped += 1;
                return None;
            }
        };
        self.arrange(comps)
    }

    /// Enters the directory leading to `comps` (its last component excluded), creating what is
    /// missing and refusing links on the way; the path of `comps`.
    fn ensure_parents(&mut self, comps: &[String]) -> Result<PathBuf> {
        let parents = &comps[..comps.len().saturating_sub(1)];
        self.enter(parents, true)?;
        Ok(self.shared.root.join(comps.iter().collect::<PathBuf>()))
    }

    /// Makes `dirs` (below the root) the current directory of the guard, level by level without
    /// following links (`UnsafeLink` when one is in the way, `AlreadyExists` for a file).
    fn enter(&mut self, dirs: &[String], create: bool) -> Result<()> {
        let mut made = Vec::new();
        let entered = self
            .guard
            .enter(dirs, create, &mut |p| made.push(p.to_owned()));
        if !made.is_empty() {
            self.st().created_dirs.extend(made);
        }
        entered.map_err(|e| match e {
            EnterError::Link(p) => Error::new(ErrorCode::UnsafeLink).with("path", p.display()),
            EnterError::NotDir(p) => Error::new(ErrorCode::AlreadyExists).with("path", p.display()),
            EnterError::Io(e, p) => Error::io(e, &p),
        })
    }

    /// Enters the existing directory `dir` (a path below the root); whether that worked.
    fn enter_existing(&mut self, dir: &Path) -> bool {
        below(&self.shared.root, dir).is_some_and(|comps| self.enter(&comps, false).is_ok())
    }

    fn decide_conflict(
        &mut self,
        target: &Path,
        entry: &Entry,
        ctx: &JobCtx,
    ) -> Result<Option<PathBuf>> {
        let Ok(meta) = self.guard.stat(&file_name(target)) else {
            return Ok(Some(target.to_owned()));
        };
        if meta.kind == Kind::Dir {
            let mut st = self.st();
            st.report.warn(ErrorCode::AlreadyExists, &entry.path, None);
            st.report.skipped += 1;
            return Ok(None);
        }
        let existing_mtime = meta.modified;
        let choice = {
            // One question at a time; an answer applied to all holds for every writer.
            let mut apply_all = lock(&self.shared.asking);
            match (self.shared.opts.overwrite, *apply_all) {
                (_, Some(c)) => c,
                (OverwriteMode::Skip, _) => ConflictChoice::Skip,
                (OverwriteMode::Overwrite, _) => ConflictChoice::Overwrite,
                (OverwriteMode::OverwriteOlder, _) => ConflictChoice::OverwriteOlder,
                (OverwriteMode::Rename, _) => ConflictChoice::Rename,
                (OverwriteMode::Ask, None) => {
                    let _one = ctx.one_question();
                    let ConflictDecision {
                        choice,
                        apply_to_all,
                    } = ctx.interaction.conflict(&Conflict {
                        path: target.to_owned(),
                        existing_size: meta.len,
                        existing_mtime,
                        incoming_size: entry.size,
                        incoming_mtime: entry.mtime,
                    });
                    if apply_to_all {
                        *apply_all = Some(choice);
                    }
                    choice
                }
            }
        };
        match choice {
            ConflictChoice::Overwrite => Ok(Some(target.to_owned())),
            ConflictChoice::OverwriteOlder => {
                let newer = matches!((entry.mtime, existing_mtime), (Some(a), Some(b)) if a > b);
                if newer {
                    Ok(Some(target.to_owned()))
                } else {
                    self.st().report.skipped += 1;
                    Ok(None)
                }
            }
            ConflictChoice::Skip => {
                self.st().report.skipped += 1;
                Ok(None)
            }
            ConflictChoice::Rename => {
                let name = target
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let parent = target.parent().unwrap_or(&self.shared.root);
                for n in 2..10_000 {
                    let candidate = parent.join(numbered_name(&name, n));
                    if self.guard.stat(&file_name(&candidate)).is_err() {
                        return Ok(Some(candidate));
                    }
                }
                Err(Error::new(ErrorCode::AlreadyExists).with("path", target.display()))
            }
            ConflictChoice::Abort => Err(Error::cancelled()),
        }
    }

    fn check_streaming_ratio(&self, ctx: &JobCtx) -> Result<()> {
        let s = &*self.shared;
        let p = &ctx.policy;
        let total = s.total_written.load(Ordering::Relaxed);
        if s.bomb_confirmed.load(Ordering::Relaxed) || !p.is_high_ratio(total, s.archive_size) {
            return Ok(());
        }
        let _one_question = lock(&s.asking);
        // Another writer may have asked meanwhile.
        if s.bomb_confirmed.load(Ordering::Relaxed) {
            return Ok(());
        }
        let ratio = total / s.archive_size.max(1);
        let _one = ctx.one_question();
        if ctx.interaction.confirm(&Confirm::HighRatio {
            ratio,
            unpacked: total,
        }) {
            s.bomb_confirmed.store(true, Ordering::Relaxed);
            Ok(())
        } else {
            Err(p.bomb_error(total, s.archive_size))
        }
    }

    /// Writes a new file in place (`file` was just created at `target`). Whatever goes wrong,
    /// the partial file is removed; a broken entry is skipped with a warning.
    fn write_new(
        &mut self,
        entry: &Entry,
        data: EntryData<'_>,
        target: &Path,
        file: File,
        ctx: &JobCtx,
    ) -> Result<Option<PathBuf>> {
        let name = file_name(target);
        let path = self.guard.path_for_engine(&name);
        match self.fill(entry, data, file, &path, target, ctx) {
            Ok(n) => {
                self.placed(target, entry, n, ctx)?;
                Ok(Some(target.to_owned()))
            }
            Err(e) => {
                let _ = self.guard.remove_file(&name);
                self.skip_broken(entry, e).map(|()| None)
            }
        }
    }

    /// Replaces an existing file (or writes a renamed one): the content goes to a temporary file
    /// renamed into place, so the old file stays whole until the new one is.
    fn write_replacing(
        &mut self,
        entry: &Entry,
        data: EntryData<'_>,
        target: PathBuf,
        ctx: &JobCtx,
    ) -> Result<Option<PathBuf>> {
        let name = file_name(&target);
        let (file, tmp) = self
            .guard
            .create_temp()
            .map_err(|e| Error::io(e, self.guard.path()))?;
        let tmp_path = self.guard.path_for_engine(&tmp);
        let n = match self.fill(entry, data, file, &tmp_path, &target, ctx) {
            Ok(n) => n,
            Err(e) => {
                let _ = self.guard.remove_file(&tmp);
                return self.skip_broken(entry, e).map(|()| None);
            }
        };
        // Replace a read-only file we are allowed to overwrite (only Windows refuses that; on
        // Unix the directory's permissions decide, and changing the mode by path would follow
        // a link put there meanwhile).
        #[cfg(windows)]
        if self
            .guard
            .stat(&name)
            .is_ok_and(|s| s.readonly && s.kind == Kind::File)
            && let Ok(m) = fs::metadata(&target)
        {
            let mut p = m.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            p.set_readonly(false);
            let _ = fs::set_permissions(&target, p);
        }
        if let Err(e) = self.guard.rename(&tmp, &name) {
            let _ = self.guard.remove_file(&tmp);
            return Err(Error::io(e, &target));
        }
        self.placed(&target, entry, n, ctx)?;
        Ok(Some(target))
    }

    /// Writes an entry's content into `file` (open at `path`) and returns its size. Engines
    /// that write files themselves (`WriteTo`) get `path`; errors name `target`.
    fn fill(
        &mut self,
        entry: &Entry,
        data: EntryData<'_>,
        mut file: File,
        path: &Path,
        target: &Path,
        ctx: &JobCtx,
    ) -> Result<u64> {
        // Declared sizes are exact for ZIP / 7z / tar / RAR; anything beyond is a lie.
        let limit = entry
            .size_known
            .then(|| entry.size.saturating_add(64 * 1024));
        match data {
            EntryData::Stream(reader) => {
                if self.buf.is_empty() {
                    self.buf = vec![0; 1 << 20];
                }
                let mut guard = RatioGuard {
                    file: &mut file,
                    unpacked: self.shared.total_written.load(Ordering::Relaxed),
                    shared: &self.shared,
                    ctx,
                };
                let n = copy_with_buffer(reader, &mut guard, &mut self.buf, ctx, limit)?;
                finish_file(&file, entry, &self.shared.opts);
                Ok(n)
            }
            EntryData::WriteTo(write) => {
                drop(file);
                write(path)?;
                let n = fs::metadata(path).map_err(|e| Error::io(e, target))?.len();
                if let Some(limit) = limit
                    && n > limit
                {
                    return Err(Error::new(ErrorCode::CompressionBomb).with("size", n));
                }
                ctx.add_bytes(n);
                if let Ok(f) = File::options().write(true).open(path) {
                    finish_file(&f, entry, &self.shared.opts);
                }
                Ok(n)
            }
            EntryData::None => {
                finish_file(&file, entry, &self.shared.opts);
                Ok(0)
            }
        }
    }

    /// Checksum and data errors skip the entry with a warning; anything else stops the job.
    fn skip_broken(&self, entry: &Entry, e: Error) -> Result<()> {
        if matches!(
            e.code(),
            ErrorCode::CrcMismatch | ErrorCode::Corrupt | ErrorCode::Truncated
        ) {
            let mut st = self.st();
            st.report
                .warn(e.code(), &entry.path, e.param("detail").map(str::to_owned));
            st.report.skipped += 1;
            Ok(())
        } else {
            Err(e)
        }
    }

    /// A file now in place: permissions, origin, totals and the streaming bomb check.
    fn placed(&self, target: &Path, entry: &Entry, n: u64, ctx: &JobCtx) -> Result<()> {
        // On Unix set through the handle when the file was written (`finish_file`).
        #[cfg(not(unix))]
        apply_permissions(target, entry, self.shared.opts.restore_permissions);
        #[cfg(unix)]
        let _ = entry;
        if let Some(origin) = &self.shared.origin {
            let _ = origin::apply(target, origin);
        }
        self.shared.total_written.fetch_add(n, Ordering::Relaxed);
        {
            let mut st = self.st();
            st.report.files += 1;
            st.report.bytes += n;
        }
        self.check_streaming_ratio(ctx)
    }

    /// Whether the symbolic link `l` leads somewhere inside the destination, following the
    /// links of this extraction (`pending`) and links already on disk through every component.
    fn link_inside(&self, l: &DeferredLink, pending: &HashMap<String, String>) -> bool {
        let root = &self.shared.root;
        let Some(base) = l.path.parent().and_then(|p| below(root, p)) else {
            return false;
        };
        resolve_link(&base, &l.target, |at| {
            if let Some(t) = pending.get(&link_key(at)) {
                return Some(t.clone());
            }
            let p = root.join(at.iter().collect::<PathBuf>());
            match fs::symlink_metadata(&p) {
                // An unreadable link counts as leading out.
                Ok(m) if m.file_type().is_symlink() => Some(
                    fs::read_link(&p)
                        .map(|t| t.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| "/".into()),
                ),
                _ => None,
            }
        })
        .is_some()
    }

    fn create_link(
        &mut self,
        l: DeferredLink,
        pending: &HashMap<String, String>,
        written: &HashMap<String, PathBuf>,
    ) {
        let policy = self.shared.opts.symlinks;
        let name = file_name(&l.path);
        // Enters the link's directory.
        let in_place = |s: &mut Self| l.path.parent().is_some_and(|dir| s.enter_existing(dir));
        match l.kind {
            EntryKind::Hardlink => {
                let source = sanitize(&l.target, self.shared.os, self.path_mode())
                    .ok()
                    .and_then(|s| written.get(&s.to_slash_path()).cloned());
                // The source is opened through the guard: a link put in its place meanwhile is
                // neither linked nor copied.
                let opened = source.as_deref().and_then(|src| {
                    let file = (self.enter_existing(src.parent()?))
                        .then(|| self.guard.open_file(&file_name(src)).ok())??;
                    Some((src.to_owned(), file))
                });
                match opened {
                    Some((src, mut file)) if in_place(self) => {
                        let linked = self.guard.hard_link(&file, &src, &name).is_ok()
                            || self
                                .guard
                                .create_new(&name)
                                .is_ok_and(|mut out| io::copy(&mut file, &mut out).is_ok());
                        if linked {
                            self.st().report.links += 1;
                        }
                    }
                    _ => {
                        self.st()
                            .report
                            .warn(ErrorCode::UnsafeLink, &l.entry_path, Some(l.target))
                    }
                }
            }
            _ => {
                if policy == SymlinkPolicy::Skip {
                    self.st().report.skipped += 1;
                    return;
                }
                let refuse = |s: &Self, l: &DeferredLink| {
                    let mut st = s.st();
                    st.report
                        .warn(ErrorCode::UnsafeLink, &l.entry_path, Some(l.target.clone()));
                    st.report.skipped += 1;
                };
                if !in_place(self) {
                    refuse(self, &l);
                    return;
                }
                let as_file = |s: &Self| {
                    s.guard
                        .create_new(&name)
                        .is_ok_and(|mut f| f.write_all(l.target.as_bytes()).is_ok())
                };
                if policy == SymlinkPolicy::AsFile {
                    if as_file(self) {
                        self.st().report.files += 1;
                    }
                    return;
                }
                if !self.link_inside(&l, pending) {
                    refuse(self, &l);
                    return;
                }
                match self.guard.symlink(&l.target, &name) {
                    Ok(()) => self.st().report.links += 1,
                    // No privilege for symlinks (Windows): fall back to a file with the target.
                    Err(_) => {
                        if as_file(self) {
                            self.st().report.files += 1;
                        }
                    }
                }
            }
        }
    }
}

impl ExtractSink for FsSink {
    fn wants(&self, entry: &Entry) -> bool {
        self.unit.as_ref().is_none_or(|u| u.contains(&entry.id))
            && self.shared.selection.contains(entry.id)
    }

    fn wanted(&self) -> Option<Vec<EntryId>> {
        self.unit.as_ref().map(|u| u.iter().copied().collect())
    }

    fn done(&self) -> bool {
        self.unit.as_ref().is_some_and(HashSet::is_empty)
    }

    fn entry(&mut self, entry: &Entry, data: EntryData<'_>, ctx: &JobCtx) -> Result<()> {
        ctx.check()?;
        if let Some(u) = &mut self.unit {
            u.remove(&entry.id);
        }
        ctx.begin_file(&entry.path);
        let Some(comps) = self.relative(entry) else {
            return Ok(());
        };
        if entry.kind == EntryKind::Other {
            self.st().report.skipped += 1;
            return Ok(());
        }
        let target = match self.ensure_parents(&comps) {
            Ok(t) => t,
            Err(e) if matches!(e.code(), ErrorCode::UnsafeLink | ErrorCode::AlreadyExists) => {
                let mut st = self.st();
                st.report
                    .warn(e.code(), &entry.path, e.param("path").map(str::to_owned));
                st.report.skipped += 1;
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        match (comps.as_slice(), entry.kind) {
            ([top, _, ..], _) => self.note_top(Some(self.shared.root.join(top))),
            // Noted once it is a directory.
            (_, EntryKind::Dir) => {}
            _ => self.note_top(None),
        }
        match entry.kind {
            EntryKind::Dir => {
                if self.shared.opts.path_mode == PathMode::Flat {
                    return Ok(());
                }
                match self.enter(&comps, true) {
                    Ok(()) => {}
                    Err(e)
                        if matches!(e.code(), ErrorCode::UnsafeLink | ErrorCode::AlreadyExists) =>
                    {
                        self.st().report.warn(e.code(), &entry.path, None);
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                }
                if comps.len() == 1 {
                    self.note_top(Some(target.clone()));
                }
                let opts = &self.shared.opts;
                let mut st = self.st();
                if opts.restore_permissions
                    && let Some(mode) = entry.unix_mode
                    && st.created_dirs.contains(&target)
                {
                    st.dir_modes.push((target.clone(), mode));
                }
                if opts.restore_mtime
                    && let Some(t) = entry.mtime
                {
                    st.dir_times.push((target, t));
                }
                st.report.dirs += 1;
            }
            EntryKind::Symlink | EntryKind::Hardlink => {
                let link_target = match (entry.link_target.clone(), data) {
                    (Some(t), _) => t,
                    (None, EntryData::Stream(r)) => {
                        let mut s = String::new();
                        r.take(32 * 1024)
                            .read_to_string(&mut s)
                            .map_err(Error::from)?;
                        s
                    }
                    (None, _) => String::new(),
                };
                if let Some(path) = self.decide_conflict(&target, entry, ctx)? {
                    let _ = self.guard.remove_file(&file_name(&path));
                    self.st().links.push(DeferredLink {
                        path,
                        target: link_target,
                        kind: entry.kind,
                        entry_path: entry.path.clone(),
                    });
                }
            }
            EntryKind::File => {
                // A new file is written in place; only an existing one goes through the
                // conflict decision (and is then replaced through a temporary file).
                let placed = match self.guard.create_new(&file_name(&target)) {
                    Ok(file) => self.write_new(entry, data, &target, file, ctx)?,
                    // Something is there (Windows may report a directory as access denied).
                    Err(_) if self.guard.stat(&file_name(&target)).is_ok() => {
                        match self.decide_conflict(&target, entry, ctx)? {
                            Some(t) => self.write_replacing(entry, data, t, ctx)?,
                            None => return Ok(()),
                        }
                    }
                    Err(e) => return Err(Error::io(e, &target)),
                };
                if let Some(t) = placed {
                    self.st().written.insert(comps.join("/"), t);
                }
                ctx.file_done();
            }
            EntryKind::Other => unreachable!(),
        }
        Ok(())
    }
}

/// Times and (Unix) permissions of a file just written, through its handle.
fn finish_file(file: &File, entry: &Entry, opts: &ExtractOptions) {
    #[cfg(unix)]
    apply_permissions(file, entry, opts.restore_permissions);
    if opts.restore_mtime
        && let Some(t) = entry.mtime
    {
        let _ = filetime::set_file_handle_times(
            file,
            None,
            Some(filetime::FileTime::from_system_time(t)),
        );
    }
}

/// Final permissions of an extracted file, which was written private (`0o600`): the entry's own
/// less the umask with `restore` (as `tar` does for ordinary users), otherwise those of a new
/// file.
#[cfg(unix)]
fn apply_permissions(file: &File, entry: &Entry, restore: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = match entry.unix_mode.filter(|_| restore) {
        // Never restore setuid / setgid / sticky; always keep the owner able to read and write.
        Some(mode) => (mode & 0o777 & !crate::volume::umask()) | 0o600,
        None => crate::volume::new_file_mode(),
    };
    let _ = file.set_permissions(fs::Permissions::from_mode(mode));
}

/// Mode of an extracted directory entry (open as `dir`), less the umask; the owner always keeps
/// `rwx`.
#[cfg(unix)]
fn apply_dir_mode(dir: &File, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let mode = (mode & 0o777 & !crate::volume::umask()) | 0o700;
    let _ = dir.set_permissions(fs::Permissions::from_mode(mode));
}

/// Directory modes are Unix-only.
#[cfg(not(unix))]
fn apply_dir_mode(_dir: &File, _mode: u32) {}

#[cfg(not(unix))]
fn apply_permissions(path: &Path, entry: &Entry, restore: bool) {
    const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
    let readonly = entry
        .win_attrs
        .is_some_and(|a| a & FILE_ATTRIBUTE_READONLY != 0)
        || entry
            .unix_mode
            .is_some_and(|m| m & 0o222 == 0 && m & 0o400 != 0);
    if restore
        && readonly
        && let Ok(m) = fs::metadata(path)
    {
        let mut p = m.permissions();
        p.set_readonly(true);
        let _ = fs::set_permissions(path, p);
    }
}

/// The last component of a path below the root (a name in the guard's current directory).
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The components of `path` below `root`; `None` when it is not below it.
fn below(root: &Path, path: &Path) -> Option<Vec<String>> {
    path.strip_prefix(root)
        .ok()?
        .components()
        .map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// How a path below the destination is looked up among the links being created: names differing
/// only in case are the same file on Windows and (by default) macOS.
fn link_key(components: &[String]) -> String {
    let key = components.join("/");
    if cfg!(any(windows, target_os = "macos")) {
        key.to_lowercase()
    } else {
        key
    }
}

/// Reads entries without writing anything (integrity test). Checksum failures are recorded as
/// warnings and the test continues.
pub struct TestSink {
    selection: Selection,
    pub report: Report,
}

impl TestSink {
    pub fn new(selection: Selection) -> Self {
        Self {
            selection,
            report: Report::default(),
        }
    }
}

impl ExtractSink for TestSink {
    fn wants(&self, entry: &Entry) -> bool {
        self.selection.contains(entry.id)
    }

    fn entry(&mut self, entry: &Entry, data: EntryData<'_>, ctx: &JobCtx) -> Result<()> {
        ctx.check()?;
        ctx.begin_file(&entry.path);
        let res = match data {
            EntryData::Stream(r) => copy_with_progress(r, &mut io::sink(), ctx, None),
            EntryData::WriteTo(w) => {
                let tmp = tempfile::NamedTempFile::new()?.into_temp_path();
                w(&tmp).and_then(|()| Ok(fs::metadata(&tmp)?.len()))
            }
            EntryData::None => Ok(0),
        };
        match res {
            Ok(n) => {
                match entry.kind {
                    EntryKind::Dir => self.report.dirs += 1,
                    EntryKind::File => self.report.files += 1,
                    _ => self.report.links += 1,
                }
                self.report.bytes += n;
                ctx.file_done();
                Ok(())
            }
            Err(e)
                if matches!(
                    e.code(),
                    ErrorCode::CrcMismatch | ErrorCode::Corrupt | ErrorCode::Truncated
                ) =>
            {
                self.report
                    .warn(e.code(), &entry.path, e.param("detail").map(str::to_owned));
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

/// Captures the content of one entry in memory (previews), up to a byte limit.
pub struct CaptureSink {
    id: EntryId,
    max: usize,
    pub data: Option<Vec<u8>>,
    /// The entry was longer than `max`.
    pub truncated: bool,
}

impl CaptureSink {
    pub fn new(id: EntryId, max: usize) -> Self {
        Self {
            id,
            max,
            data: None,
            truncated: false,
        }
    }
}

impl ExtractSink for CaptureSink {
    fn wants(&self, entry: &Entry) -> bool {
        entry.id == self.id && self.data.is_none()
    }

    fn wanted(&self) -> Option<Vec<EntryId>> {
        Some(vec![self.id])
    }

    fn entry(&mut self, entry: &Entry, data: EntryData<'_>, _ctx: &JobCtx) -> Result<()> {
        let mut buf = Vec::new();
        match data {
            EntryData::Stream(r) => {
                r.take(self.max as u64 + 1).read_to_end(&mut buf)?;
            }
            EntryData::WriteTo(w) => {
                let tmp = tempfile::NamedTempFile::new()?.into_temp_path();
                w(&tmp)?;
                File::open(&tmp)?
                    .take(self.max as u64 + 1)
                    .read_to_end(&mut buf)?;
            }
            EntryData::None => {
                if let Some(t) = &entry.link_target {
                    buf.extend_from_slice(t.as_bytes());
                }
            }
        }
        if buf.len() > self.max {
            buf.truncate(self.max);
            self.truncated = true;
        }
        self.data = Some(buf);
        Ok(())
    }

    fn done(&self) -> bool {
        self.data.is_some()
    }
}

/// The file being written, behind the streaming bomb check. An entry of unknown size (a lone
/// .gz / .zst / .xz) has no declared size to cap it, and a check once the file is whole comes
/// after the disk is full; so the check runs before each write, with the bytes of earlier files.
struct RatioGuard<'a> {
    file: &'a mut File,
    /// Bytes this extraction has written: earlier files (as far as other writers had finished
    /// theirs when this file began) and this one so far.
    unpacked: u64,
    shared: &'a Shared,
    ctx: &'a JobCtx,
}

impl Write for RatioGuard<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let after = self.unpacked.saturating_add(buf.len() as u64);
        let (policy, s) = (&self.ctx.policy, self.shared);
        if !s.bomb_confirmed.load(Ordering::Relaxed) && policy.is_high_ratio(after, s.archive_size)
        {
            let _one_question = lock(&s.asking);
            if !s.bomb_confirmed.load(Ordering::Relaxed) {
                let ratio = after / s.archive_size.max(1);
                let _one = self.ctx.one_question();
                if !self.ctx.interaction.confirm(&Confirm::HighRatio {
                    ratio,
                    unpacked: after,
                }) {
                    return Err(policy.bomb_error(after, s.archive_size).into());
                }
                s.bomb_confirmed.store(true, Ordering::Relaxed);
            }
        }
        let n = self.file.write(buf)?;
        self.unpacked += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Reads `reader` to its end, discarding the data (engines call this for skipped entries).
pub fn drain(reader: &mut dyn Read) -> io::Result<u64> {
    io::copy(reader, &mut io::sink())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{FixedPassword, NullProgress};
    use std::sync::Arc;

    fn file_entry(id: u32, path: &str, data: &[u8]) -> Entry {
        let mut e = Entry::new(id, path.into(), EntryKind::File);
        e.size = data.len() as u64;
        e
    }

    fn sink(root: &Path, opts: ExtractOptions) -> FsSink {
        FsSink::new(root.to_owned(), opts, Selection::all(), None, 1 << 20).unwrap()
    }

    fn put(s: &mut FsSink, e: &Entry, data: &[u8], ctx: &JobCtx) {
        let mut r = data;
        s.entry(e, EntryData::Stream(&mut r), ctx).unwrap();
    }

    #[test]
    fn writes_files_and_dirs() {
        let d = tempfile::tempdir().unwrap();
        let ctx = JobCtx::default();
        let mut s = sink(d.path(), ExtractOptions::default());
        s.entry(
            &Entry::new(0, "a/".into(), EntryKind::Dir),
            EntryData::None,
            &ctx,
        )
        .unwrap();
        put(
            &mut s,
            &file_entry(1, "a/b/c.txt", b"hello"),
            b"hello",
            &ctx,
        );
        let r = s.finish(&ctx).unwrap();
        assert_eq!(fs::read(d.path().join("a/b/c.txt")).unwrap(), b"hello");
        assert_eq!((r.files, r.dirs, r.bytes), (1, 1, 5));
        let leftovers: Vec<_> = fs::read_dir(d.path().join("a/b")).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "no temp files left behind");
    }

    #[test]
    fn reports_the_folder_extracted() {
        let ctx = JobCtx::default();
        // One folder at the top: that folder, also when only its files are in the archive.
        let d = tempfile::tempdir().unwrap();
        let mut s = sink(d.path(), ExtractOptions::default());
        s.entry(
            &Entry::new(0, "photos/".into(), EntryKind::Dir),
            EntryData::None,
            &ctx,
        )
        .unwrap();
        put(&mut s, &file_entry(1, "photos/a.jpg", b"a"), b"a", &ctx);
        put(
            &mut s,
            &file_entry(2, "photos/2026/b.jpg", b"b"),
            b"b",
            &ctx,
        );
        assert_eq!(s.finish(&ctx).unwrap().outputs, [d.path().join("photos")]);
        let d = tempfile::tempdir().unwrap();
        let mut s = sink(d.path(), ExtractOptions::default());
        put(&mut s, &file_entry(0, "docs/a.txt", b"a"), b"a", &ctx);
        assert_eq!(s.finish(&ctx).unwrap().outputs, [d.path().join("docs")]);

        // Anything else at the top: the destination.
        for paths in [
            &["docs/a.txt", "b.txt"][..],
            &["b.txt", "docs/a.txt"],
            &["docs/a.txt", "more/c.txt"],
            &["b.txt"],
        ] {
            let d = tempfile::tempdir().unwrap();
            let mut s = sink(d.path(), ExtractOptions::default());
            for (i, p) in paths.iter().enumerate() {
                put(&mut s, &file_entry(i as u32, p, b"x"), b"x", &ctx);
            }
            let r = s.finish(&ctx).unwrap();
            assert_eq!(r.outputs, [d.path().to_owned()], "{paths:?}");
        }
    }

    #[test]
    fn a_new_file_that_fails_leaves_nothing() {
        // Half the content, then the job is cancelled.
        struct Half<'a>(bool, &'a JobCtx);
        impl Read for Half<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if std::mem::replace(&mut self.0, true) {
                    self.1.cancel.cancel();
                    return Err(Error::cancelled().into());
                }
                buf[..4].copy_from_slice(b"half");
                Ok(4)
            }
        }
        let d = tempfile::tempdir().unwrap();
        let ctx = JobCtx::default();
        let mut s = sink(d.path(), ExtractOptions::default());
        let e = file_entry(0, "a/part.bin", &[0; 100]);
        let err = s
            .entry(&e, EntryData::Stream(&mut Half(false, &ctx)), &ctx)
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::Cancelled);
        assert_eq!(fs::read_dir(d.path().join("a")).unwrap().count(), 0);
    }

    #[test]
    fn a_directory_in_the_way_of_a_file_is_reported() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("taken")).unwrap();
        let ctx = JobCtx::default();
        let mut s = sink(d.path(), ExtractOptions::default());
        put(&mut s, &file_entry(0, "taken", b"x"), b"x", &ctx);
        let r = s.finish(&ctx).unwrap();
        assert!(d.path().join("taken").is_dir());
        assert_eq!(r.skipped, 1);
        assert_eq!(r.warnings[0].code, ErrorCode::AlreadyExists);
    }

    #[cfg(unix)]
    #[test]
    fn files_get_the_entry_mode_or_that_of_a_new_file() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: PathBuf| fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        let d = tempfile::tempdir().unwrap();
        let ctx = JobCtx::default();
        let mut s = sink(d.path(), ExtractOptions::default());
        // No mode in the archive (made on Windows): not left at the temporary file's 0600.
        put(&mut s, &file_entry(0, "plain.txt", b"a"), b"a", &ctx);
        let mut script = file_entry(1, "run.sh", b"b");
        script.unix_mode = Some(0o4755);
        put(&mut s, &script, b"b", &ctx);
        let mut key = file_entry(2, "key", b"c");
        key.unix_mode = Some(0o400);
        put(&mut s, &key, b"c", &ctx);
        let mut open = file_entry(3, "open", b"d");
        open.unix_mode = Some(0o777);
        put(&mut s, &open, b"d", &ctx);
        s.finish(&ctx).unwrap();
        let umask = crate::volume::umask();
        assert_eq!(
            mode(d.path().join("plain.txt")),
            crate::volume::new_file_mode()
        );
        assert_eq!(
            mode(d.path().join("run.sh")),
            (0o755 & !umask) | 0o600,
            "no setuid"
        );
        assert_eq!(
            mode(d.path().join("key")),
            0o600,
            "owner keeps read and write"
        );
        assert_eq!(
            mode(d.path().join("open")),
            0o777 & !umask,
            "less the umask"
        );

        let d = tempfile::tempdir().unwrap();
        let opts = ExtractOptions {
            restore_permissions: false,
            ..ExtractOptions::default()
        };
        let mut s = sink(d.path(), opts);
        put(&mut s, &script, b"b", &ctx);
        s.finish(&ctx).unwrap();
        assert_eq!(
            mode(d.path().join("run.sh")),
            crate::volume::new_file_mode()
        );
    }

    #[cfg(unix)]
    #[test]
    fn created_dirs_get_the_entry_mode_at_the_end() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: PathBuf| fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        let dir = |id, path: &str, m| {
            let mut e = Entry::new(id, path.into(), EntryKind::Dir);
            e.unix_mode = Some(m);
            e
        };
        let umask = crate::volume::umask();
        let d = tempfile::tempdir().unwrap();
        let pre = d.path().join("pre");
        fs::create_dir(&pre).unwrap();
        fs::set_permissions(&pre, fs::Permissions::from_mode(0o700)).unwrap();
        let ctx = JobCtx::default();
        let mut s = sink(d.path(), ExtractOptions::default());
        // Read-only, and listed before its contents: still filled.
        s.entry(&dir(0, "ro/", 0o555), EntryData::None, &ctx)
            .unwrap();
        put(&mut s, &file_entry(1, "ro/a.txt", b"a"), b"a", &ctx);
        // Listed after its contents (as in 7z): created for the file, then given its mode.
        put(&mut s, &file_entry(2, "late/b.txt", b"b"), b"b", &ctx);
        s.entry(&dir(3, "late/", 0o2750), EntryData::None, &ctx)
            .unwrap();
        // Already there before extraction: left alone.
        s.entry(&dir(4, "pre/", 0o777), EntryData::None, &ctx)
            .unwrap();
        s.finish(&ctx).unwrap();
        assert_eq!(fs::read(d.path().join("ro/a.txt")).unwrap(), b"a");
        assert_eq!(mode(d.path().join("ro")), (0o555 & !umask) | 0o700);
        assert_eq!(
            mode(d.path().join("late")),
            (0o750 & !umask) | 0o700,
            "no setgid"
        );
        assert_eq!(mode(pre), 0o700);

        let d = tempfile::tempdir().unwrap();
        let opts = ExtractOptions {
            restore_permissions: false,
            ..ExtractOptions::default()
        };
        let mut s = sink(d.path(), opts);
        s.entry(&dir(0, "x/", 0o711), EntryData::None, &ctx)
            .unwrap();
        s.finish(&ctx).unwrap();
        assert_eq!(mode(d.path().join("x")), 0o777 & !umask);
    }

    #[test]
    fn zip_slip_is_skipped_with_warning() {
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("out");
        let ctx = JobCtx::default();
        let mut s = sink(&dest, ExtractOptions::default());
        put(&mut s, &file_entry(0, "../evil.txt", b"x"), b"x", &ctx);
        put(&mut s, &file_entry(1, "/abs.txt", b"x"), b"x", &ctx);
        let r = s.finish(&ctx).unwrap();
        assert!(!d.path().join("evil.txt").exists());
        assert_eq!(r.skipped, 2);
        assert_eq!(r.exit_code(), 8);
    }

    #[test]
    fn conflict_modes() {
        let d = tempfile::tempdir().unwrap();
        let ctx = JobCtx::default();
        fs::write(d.path().join("f.txt"), b"old").unwrap();
        let e = file_entry(0, "f.txt", b"new");

        let mut s = sink(
            d.path(),
            ExtractOptions {
                overwrite: OverwriteMode::Skip,
                ..Default::default()
            },
        );
        put(&mut s, &e, b"new", &ctx);
        assert_eq!(s.finish(&ctx).unwrap().skipped, 1);
        assert_eq!(fs::read(d.path().join("f.txt")).unwrap(), b"old");

        let mut s = sink(
            d.path(),
            ExtractOptions {
                overwrite: OverwriteMode::Rename,
                ..Default::default()
            },
        );
        put(&mut s, &e, b"new", &ctx);
        s.finish(&ctx).unwrap();
        assert_eq!(fs::read(d.path().join("f (2).txt")).unwrap(), b"new");

        let mut s = sink(
            d.path(),
            ExtractOptions {
                overwrite: OverwriteMode::Overwrite,
                ..Default::default()
            },
        );
        put(&mut s, &e, b"new", &ctx);
        s.finish(&ctx).unwrap();
        assert_eq!(fs::read(d.path().join("f.txt")).unwrap(), b"new");
    }

    #[test]
    fn declared_size_is_enforced() {
        let d = tempfile::tempdir().unwrap();
        let ctx = JobCtx::default();
        let mut s = sink(d.path(), ExtractOptions::default());
        let e = file_entry(0, "liar.bin", b"x"); // declares 1 byte
        let data = vec![0u8; 200_000];
        let mut r = data.as_slice();
        let err = s.entry(&e, EntryData::Stream(&mut r), &ctx).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CompressionBomb);
        assert!(!d.path().join("liar.bin").exists());
    }

    #[test]
    fn streaming_ratio_check_asks_for_confirmation() {
        let d = tempfile::tempdir().unwrap();
        let ctx = JobCtx::new(Arc::new(NullProgress), Arc::new(FixedPassword("x".into())))
            .with_policy(crate::SecurityPolicy {
                ratio_min_bytes: 1000,
                max_ratio: 10,
                ..Default::default()
            });
        let mut s = FsSink::new(
            d.path().to_owned(),
            ExtractOptions::default(),
            Selection::all(),
            None,
            10,
        )
        .unwrap();
        let mut e = file_entry(0, "big.bin", b"");
        e.size_known = false;
        let data = vec![0u8; 5000];
        let mut r = data.as_slice();
        let err = s.entry(&e, EntryData::Stream(&mut r), &ctx).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CompressionBomb);
    }

    /// Counts the prompts and answers them.
    struct Answer {
        yes: bool,
        asked: std::sync::atomic::AtomicU32,
    }

    impl crate::job::Interaction for Answer {
        fn confirm(&self, _c: &Confirm) -> bool {
            self.asked
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.yes
        }
    }

    /// An entry of unknown size is stopped at the threshold, before the rest is decoded and
    /// written, not once it is whole.
    #[test]
    fn streaming_ratio_check_stops_unknown_sizes_early() {
        let run = |yes: bool| {
            let d = tempfile::tempdir().unwrap();
            let answer = Arc::new(Answer {
                yes,
                asked: 0.into(),
            });
            let ctx = JobCtx::new(Arc::new(NullProgress), answer.clone()).with_policy(
                crate::SecurityPolicy {
                    ratio_min_bytes: 4 << 20,
                    max_ratio: 10,
                    ..Default::default()
                },
            );
            let mut s = FsSink::new(
                d.path().to_owned(),
                ExtractOptions::default(),
                Selection::all(),
                None,
                1000,
            )
            .unwrap();
            let mut e = file_entry(0, "bomb.bin", b"");
            e.size_known = false;
            let mut r = io::repeat(0).take(256 << 20);
            let result = s.entry(&e, EntryData::Stream(&mut r), &ctx);
            let decoded = (256 << 20) - r.limit();
            let asked = answer.asked.load(std::sync::atomic::Ordering::Relaxed);
            (result, decoded, asked, d.path().join("bomb.bin").exists())
        };

        let (result, decoded, asked, exists) = run(false);
        assert_eq!(result.unwrap_err().code(), ErrorCode::CompressionBomb);
        assert!(decoded <= 6 << 20, "stopped after {decoded} bytes");
        assert_eq!(asked, 1);
        assert!(!exists, "the partial file is removed");

        let (result, decoded, asked, exists) = run(true);
        result.unwrap();
        assert_eq!(decoded, 256 << 20);
        assert_eq!(asked, 1, "asked once");
        assert!(exists);
    }

    #[test]
    fn flat_mode_and_prefix_strip() {
        let d = tempfile::tempdir().unwrap();
        let ctx = JobCtx::default();
        let mut s = sink(
            d.path(),
            ExtractOptions {
                path_mode: PathMode::Flat,
                ..Default::default()
            },
        );
        put(&mut s, &file_entry(0, "x/y/z.txt", b"z"), b"z", &ctx);
        s.finish(&ctx).unwrap();
        assert!(d.path().join("z.txt").exists());

        let d = tempfile::tempdir().unwrap();
        let mut s = FsSink::new(
            d.path().to_owned(),
            ExtractOptions::default(),
            Selection::of([0], "src/app"),
            None,
            1,
        )
        .unwrap();
        put(&mut s, &file_entry(0, "src/app/ui/a.tsx", b"a"), b"a", &ctx);
        s.finish(&ctx).unwrap();
        assert!(d.path().join("ui/a.tsx").exists());
    }

    /// Another program swaps a directory of the destination for a link between two files written
    /// into it: the second still lands in the directory that was entered (Unix), or the swap is
    /// refused while the directory is held (Windows). Nothing is written where the link points.
    #[test]
    fn a_directory_swapped_during_extraction_does_not_redirect_writes() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dest = d.path().join("dest");
        let ctx = JobCtx::default();
        let mut s = sink(&dest, ExtractOptions::default());
        put(&mut s, &file_entry(0, "sub/a.txt", b"a"), b"a", &ctx);
        let moved = std::fs::rename(dest.join("sub"), d.path().join("moved"));
        #[cfg(windows)]
        assert!(moved.is_err(), "a held directory cannot be moved");
        #[cfg(unix)]
        {
            moved.unwrap();
            std::os::unix::fs::symlink(outside.path(), dest.join("sub")).unwrap();
        }
        put(&mut s, &file_entry(1, "sub/b.txt", b"b"), b"b", &ctx);
        s.finish(&ctx).unwrap();
        assert!(!outside.path().join("b.txt").exists());
        #[cfg(unix)]
        assert_eq!(fs::read(d.path().join("moved/b.txt")).unwrap(), b"b");
        #[cfg(windows)]
        assert_eq!(fs::read(dest.join("sub/b.txt")).unwrap(), b"b");
    }

    /// A link whose target text stays inside but which leads out through another link of the
    /// same archive is not created, whichever of the two comes first.
    #[test]
    fn links_leading_out_through_other_links_are_refused() {
        for order in [["sub/up", "sub/x/esc"], ["sub/x/esc", "sub/up"]] {
            let d = tempfile::tempdir().unwrap();
            let ctx = JobCtx::default();
            let mut s = sink(
                d.path(),
                ExtractOptions {
                    symlinks: SymlinkPolicy::CreateIfInside,
                    ..Default::default()
                },
            );
            for (i, path) in order.iter().enumerate() {
                let mut e = Entry::new(i as u32, (*path).into(), EntryKind::Symlink);
                e.link_target = Some(if *path == "sub/up" { ".." } else { "../up/.." }.into());
                s.entry(&e, EntryData::None, &ctx).unwrap();
            }
            let report = s.finish(&ctx).unwrap();
            let refused: Vec<&str> = report
                .warnings
                .iter()
                .filter(|w| w.code == ErrorCode::UnsafeLink)
                .map(|w| w.path.as_str())
                .collect();
            assert_eq!(refused, ["sub/x/esc"], "{order:?}");
            assert!(fs::symlink_metadata(d.path().join("sub/x/esc")).is_err());
        }
    }

    /// Links already in the destination are followed too.
    #[test]
    #[cfg(unix)]
    fn links_leading_out_through_existing_links_are_refused() {
        let d = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/etc", d.path().join("etc")).unwrap();
        let ctx = JobCtx::default();
        let mut s = sink(d.path(), ExtractOptions::default());
        let mut e = Entry::new(0, "passwd".into(), EntryKind::Symlink);
        e.link_target = Some("etc/passwd".into());
        s.entry(&e, EntryData::None, &ctx).unwrap();
        let report = s.finish(&ctx).unwrap();
        assert_eq!(report.warnings[0].code, ErrorCode::UnsafeLink);
        assert!(fs::symlink_metadata(d.path().join("passwd")).is_err());
    }

    #[test]
    fn symlink_as_file_policy() {
        let d = tempfile::tempdir().unwrap();
        let ctx = JobCtx::default();
        let mut s = sink(
            d.path(),
            ExtractOptions {
                symlinks: SymlinkPolicy::AsFile,
                ..Default::default()
            },
        );
        let mut e = Entry::new(0, "link".into(), EntryKind::Symlink);
        e.link_target = Some("target.txt".into());
        s.entry(&e, EntryData::None, &ctx).unwrap();
        s.finish(&ctx).unwrap();
        assert_eq!(
            fs::read_to_string(d.path().join("link")).unwrap(),
            "target.txt"
        );
    }

    #[test]
    fn capture_and_test_sinks() {
        let ctx = JobCtx::default();
        let e = file_entry(7, "a.txt", b"0123456789");
        let mut c = CaptureSink::new(7, 4);
        assert!(c.wants(&e));
        let mut r: &[u8] = b"0123456789";
        c.entry(&e, EntryData::Stream(&mut r), &ctx).unwrap();
        assert_eq!(c.data.as_deref(), Some(&b"0123"[..]));
        assert!(c.truncated && c.done());

        let mut t = TestSink::new(Selection::all());
        let mut r: &[u8] = b"0123456789";
        t.entry(&e, EntryData::Stream(&mut r), &ctx).unwrap();
        assert_eq!(t.report.bytes, 10);
    }
}
