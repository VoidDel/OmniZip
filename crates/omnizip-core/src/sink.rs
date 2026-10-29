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
    io::{self, Read},
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

use serde::{Deserialize, Serialize};

use crate::{
    Error, ErrorCode, JobCtx, Result,
    entry::{Entry, EntryId, EntryKind},
    job::{Confirm, Conflict, ConflictChoice, ConflictDecision, copy_with_progress},
    options::{ExtractOptions, OverwriteMode, PathMode, SymlinkPolicy},
    origin::{self, Origin},
    path::{TargetOs, UnsafePathMode, numbered_name, sanitize},
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
    /// Where things were written (extraction root, or archive volumes).
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

/// Writes entries below a destination directory, enforcing the security policy.
pub struct FsSink {
    root: PathBuf,
    opts: ExtractOptions,
    selection: Selection,
    os: TargetOs,
    report: Report,
    dir_times: Vec<(PathBuf, SystemTime)>,
    /// Directories this extraction created: only these take a mode from the archive, never a
    /// directory that was already there (the destination itself included).
    created_dirs: HashSet<PathBuf>,
    /// Modes of directory entries, applied at the end so a read-only directory can still be
    /// filled.
    dir_modes: Vec<(PathBuf, u32)>,
    links: Vec<DeferredLink>,
    apply_all: Option<ConflictChoice>,
    origin: Option<Origin>,
    /// Sanitized archive path → written file, for hard links.
    written: HashMap<String, PathBuf>,
    /// Bytes written so far, and the archive size, for the streaming bomb check.
    total_written: u64,
    archive_size: u64,
    bomb_confirmed: bool,
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
        let origin = if opts.propagate_origin {
            archive_path.and_then(origin::read)
        } else {
            None
        };
        Ok(Self {
            root,
            opts,
            selection,
            os: TargetOs::current(),
            report: Report::default(),
            dir_times: Vec::new(),
            created_dirs: HashSet::new(),
            dir_modes: Vec::new(),
            links: Vec::new(),
            apply_all: None,
            origin,
            written: HashMap::new(),
            total_written: 0,
            archive_size,
            bomb_confirmed: false,
        })
    }

    /// Overrides the file-system rules (tests).
    pub fn with_target_os(mut self, os: TargetOs) -> Self {
        self.os = os;
        self
    }

    /// The destination root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Creates deferred links, applies directory modes and times and returns the report.
    pub fn finish(mut self, ctx: &JobCtx) -> Result<Report> {
        let links = std::mem::take(&mut self.links);
        for l in links {
            ctx.check()?;
            self.create_link(l);
        }
        for (p, mode) in &self.dir_modes {
            apply_dir_mode(p, *mode);
        }
        // Deepest directories first so parents are not touched afterwards.
        self.dir_times
            .sort_by_key(|(p, _)| std::cmp::Reverse(p.components().count()));
        for (p, t) in &self.dir_times {
            let _ = filetime::set_file_mtime(p, filetime::FileTime::from_system_time(*t));
        }
        self.report.outputs = vec![self.root.clone()];
        Ok(self.report)
    }

    /// Relative components for an entry, or `None` (with a warning) when it must be skipped.
    fn relative(&mut self, entry: &Entry) -> Option<Vec<String>> {
        let mode = if self.opts.strip_unsafe_paths {
            UnsafePathMode::Strip
        } else {
            UnsafePathMode::Reject
        };
        let mut comps = match sanitize(&entry.path, self.os, mode) {
            Ok(s) => {
                if !s.repaired.is_empty() {
                    let detail = format!("{:?}", s.repaired);
                    self.report
                        .warn(ErrorCode::PathEscape, &entry.path, Some(detail));
                }
                s.components
            }
            Err(e) => {
                self.report
                    .warn(e.code(), &entry.path, e.param("issue").map(str::to_owned));
                self.report.skipped += 1;
                return None;
            }
        };
        let strip = &self.selection.strip_prefix;
        if !strip.is_empty() && comps.len() > strip.len() && comps[..strip.len()] == strip[..] {
            comps.drain(..strip.len());
        }
        if self.opts.path_mode == PathMode::Flat {
            let last = comps.pop()?;
            comps = vec![last];
        }
        Some(comps)
    }

    /// Creates the directories leading to `comps` (excluding the last), refusing to follow
    /// links that could lead outside the root.
    fn ensure_parents(&mut self, comps: &[String]) -> Result<PathBuf> {
        let mut cur = self.root.clone();
        for c in &comps[..comps.len().saturating_sub(1)] {
            cur.push(c);
            match fs::symlink_metadata(&cur) {
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(Error::new(ErrorCode::UnsafeLink).with("path", cur.display()));
                }
                Ok(m) if m.is_dir() => {}
                Ok(_) => {
                    // A file is in the way of a directory.
                    return Err(Error::new(ErrorCode::AlreadyExists).with("path", cur.display()));
                }
                Err(_) => match fs::create_dir(&cur) {
                    Ok(()) => {
                        self.created_dirs.insert(cur.clone());
                    }
                    Err(_) if cur.is_dir() => {}
                    Err(e) => return Err(Error::io(e, &cur)),
                },
            }
        }
        Ok(self.root.join(comps.iter().collect::<PathBuf>()))
    }

    fn decide_conflict(
        &mut self,
        target: &Path,
        entry: &Entry,
        ctx: &JobCtx,
    ) -> Result<Option<PathBuf>> {
        let Ok(meta) = fs::symlink_metadata(target) else {
            return Ok(Some(target.to_owned()));
        };
        if meta.is_dir() {
            self.report
                .warn(ErrorCode::AlreadyExists, &entry.path, None);
            self.report.skipped += 1;
            return Ok(None);
        }
        let existing_mtime = meta.modified().ok();
        let choice = match (self.opts.overwrite, self.apply_all) {
            (_, Some(c)) => c,
            (OverwriteMode::Skip, _) => ConflictChoice::Skip,
            (OverwriteMode::Overwrite, _) => ConflictChoice::Overwrite,
            (OverwriteMode::OverwriteOlder, _) => ConflictChoice::OverwriteOlder,
            (OverwriteMode::Rename, _) => ConflictChoice::Rename,
            (OverwriteMode::Ask, None) => {
                let ConflictDecision {
                    choice,
                    apply_to_all,
                } = ctx.interaction.conflict(&Conflict {
                    path: target.to_owned(),
                    existing_size: meta.len(),
                    existing_mtime,
                    incoming_size: entry.size,
                    incoming_mtime: entry.mtime,
                });
                if apply_to_all {
                    self.apply_all = Some(choice);
                }
                choice
            }
        };
        match choice {
            ConflictChoice::Overwrite => Ok(Some(target.to_owned())),
            ConflictChoice::OverwriteOlder => {
                let newer = matches!((entry.mtime, existing_mtime), (Some(a), Some(b)) if a > b);
                if newer {
                    Ok(Some(target.to_owned()))
                } else {
                    self.report.skipped += 1;
                    Ok(None)
                }
            }
            ConflictChoice::Skip => {
                self.report.skipped += 1;
                Ok(None)
            }
            ConflictChoice::Rename => {
                let name = target
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let parent = target.parent().unwrap_or(&self.root);
                for n in 2..10_000 {
                    let candidate = parent.join(numbered_name(&name, n));
                    if fs::symlink_metadata(&candidate).is_err() {
                        return Ok(Some(candidate));
                    }
                }
                Err(Error::new(ErrorCode::AlreadyExists).with("path", target.display()))
            }
            ConflictChoice::Abort => Err(Error::cancelled()),
        }
    }

    fn check_streaming_ratio(&mut self, ctx: &JobCtx) -> Result<()> {
        let p = &ctx.policy;
        if self.bomb_confirmed || !p.is_high_ratio(self.total_written, self.archive_size) {
            return Ok(());
        }
        let ratio = self.total_written / self.archive_size.max(1);
        if ctx.interaction.confirm(&Confirm::HighRatio {
            ratio,
            unpacked: self.total_written,
        }) {
            self.bomb_confirmed = true;
            Ok(())
        } else {
            Err(p.bomb_error(self.total_written, self.archive_size))
        }
    }

    fn write_file(
        &mut self,
        entry: &Entry,
        data: EntryData<'_>,
        target: &Path,
        ctx: &JobCtx,
    ) -> Result<()> {
        let parent = target.parent().unwrap_or(&self.root);
        let tmp = tempfile::Builder::new()
            .prefix(".omnizip-")
            .suffix(".part")
            .tempfile_in(parent)
            .map_err(|e| Error::io(e, parent))?;
        // Declared sizes are exact for ZIP / 7z / tar / RAR; anything beyond is a lie.
        let limit = entry
            .size_known
            .then(|| entry.size.saturating_add(64 * 1024));
        let result: Result<(tempfile::TempPath, u64)> = match data {
            EntryData::Stream(reader) => {
                let (file, path) = tmp.into_parts();
                let mut w = io::BufWriter::with_capacity(1 << 20, file);
                let n = copy_with_progress(reader, &mut w, ctx, limit);
                let n = n.and_then(|n| {
                    let f = w
                        .into_inner()
                        .map_err(|e| Error::io(e.into_error(), target))?;
                    finish_file(&f, entry, &self.opts);
                    Ok(n)
                });
                n.map(|n| (path, n))
            }
            EntryData::WriteTo(write) => {
                let path = tmp.into_temp_path();
                write(&path).and_then(|()| {
                    let n = fs::metadata(&path).map_err(|e| Error::io(e, target))?.len();
                    if let Some(limit) = limit
                        && n > limit
                    {
                        return Err(Error::new(ErrorCode::CompressionBomb).with("size", n));
                    }
                    ctx.add_bytes(n);
                    if let Ok(f) = File::options().write(true).open(&path) {
                        finish_file(&f, entry, &self.opts);
                    }
                    Ok((path, n))
                })
            }
            EntryData::None => {
                let (file, path) = tmp.into_parts();
                finish_file(&file, entry, &self.opts);
                Ok((path, 0))
            }
        };
        let (tmp_path, n) = match result {
            Ok(v) => v,
            Err(e)
                if matches!(
                    e.code(),
                    ErrorCode::CrcMismatch | ErrorCode::Corrupt | ErrorCode::Truncated
                ) =>
            {
                self.report
                    .warn(e.code(), &entry.path, e.param("detail").map(str::to_owned));
                self.report.skipped += 1;
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        // Replace a read-only file we are allowed to overwrite.
        if let Ok(m) = fs::metadata(target)
            && m.permissions().readonly()
        {
            let mut p = m.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            p.set_readonly(false);
            let _ = fs::set_permissions(target, p);
        }
        tmp_path
            .persist(target)
            .map_err(|e| Error::io(e.error, target))?;
        apply_permissions(target, entry, self.opts.restore_permissions);
        if let Some(origin) = &self.origin {
            let _ = origin::apply(target, origin);
        }
        self.total_written += n;
        self.report.files += 1;
        self.report.bytes += n;
        self.check_streaming_ratio(ctx)
    }

    fn create_link(&mut self, l: DeferredLink) {
        let policy = self.opts.symlinks;
        match l.kind {
            EntryKind::Hardlink => {
                let mode = if self.opts.strip_unsafe_paths {
                    UnsafePathMode::Strip
                } else {
                    UnsafePathMode::Reject
                };
                let source = sanitize(&l.target, self.os, mode)
                    .ok()
                    .and_then(|s| self.written.get(&s.to_slash_path()).cloned());
                match source {
                    Some(src) => {
                        if fs::hard_link(&src, &l.path)
                            .or_else(|_| fs::copy(&src, &l.path).map(|_| ()))
                            .is_ok()
                        {
                            self.report.links += 1;
                        }
                    }
                    None => self
                        .report
                        .warn(ErrorCode::UnsafeLink, &l.entry_path, Some(l.target)),
                }
            }
            _ => {
                if policy == SymlinkPolicy::Skip {
                    self.report.skipped += 1;
                    return;
                }
                if policy == SymlinkPolicy::AsFile {
                    if fs::write(&l.path, l.target.as_bytes()).is_ok() {
                        self.report.files += 1;
                    }
                    return;
                }
                if !link_stays_inside(&self.root, &l.path, &l.target) {
                    self.report
                        .warn(ErrorCode::UnsafeLink, &l.entry_path, Some(l.target));
                    self.report.skipped += 1;
                    return;
                }
                match make_symlink(&l.target, &l.path) {
                    Ok(()) => self.report.links += 1,
                    // No privilege for symlinks (Windows): fall back to a file with the target.
                    Err(_) => {
                        if fs::write(&l.path, l.target.as_bytes()).is_ok() {
                            self.report.files += 1;
                        }
                    }
                }
            }
        }
    }
}

impl ExtractSink for FsSink {
    fn wants(&self, entry: &Entry) -> bool {
        self.selection.contains(entry.id)
    }

    fn entry(&mut self, entry: &Entry, data: EntryData<'_>, ctx: &JobCtx) -> Result<()> {
        ctx.check()?;
        ctx.begin_file(&entry.path);
        let Some(comps) = self.relative(entry) else {
            return Ok(());
        };
        if entry.kind == EntryKind::Other {
            self.report.skipped += 1;
            return Ok(());
        }
        let target = match self.ensure_parents(&comps) {
            Ok(t) => t,
            Err(e) if matches!(e.code(), ErrorCode::UnsafeLink | ErrorCode::AlreadyExists) => {
                self.report
                    .warn(e.code(), &entry.path, e.param("path").map(str::to_owned));
                self.report.skipped += 1;
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        match entry.kind {
            EntryKind::Dir => {
                if self.opts.path_mode == PathMode::Flat {
                    return Ok(());
                }
                match fs::symlink_metadata(&target) {
                    Ok(m) if m.is_dir() => {}
                    Ok(_) => {
                        self.report
                            .warn(ErrorCode::AlreadyExists, &entry.path, None);
                        return Ok(());
                    }
                    Err(_) => {
                        fs::create_dir(&target).map_err(|e| Error::io(e, &target))?;
                        self.created_dirs.insert(target.clone());
                    }
                }
                if self.opts.restore_permissions
                    && let Some(mode) = entry.unix_mode
                    && self.created_dirs.contains(&target)
                {
                    self.dir_modes.push((target.clone(), mode));
                }
                if self.opts.restore_mtime
                    && let Some(t) = entry.mtime
                {
                    self.dir_times.push((target, t));
                }
                self.report.dirs += 1;
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
                    let _ = fs::remove_file(&path);
                    self.links.push(DeferredLink {
                        path,
                        target: link_target,
                        kind: entry.kind,
                        entry_path: entry.path.clone(),
                    });
                }
            }
            EntryKind::File => {
                let Some(target) = self.decide_conflict(&target, entry, ctx)? else {
                    return Ok(());
                };
                self.write_file(entry, data, &target, ctx)?;
                self.written.insert(comps.join("/"), target);
                ctx.file_done();
            }
            EntryKind::Other => unreachable!(),
        }
        Ok(())
    }
}

fn finish_file(file: &File, entry: &Entry, opts: &ExtractOptions) {
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

/// Final permissions of an extracted file, which was written through a private temporary file
/// (`0o600`): the entry's own less the umask with `restore` (as `tar` does for ordinary users),
/// otherwise those of a new file.
#[cfg(unix)]
fn apply_permissions(path: &Path, entry: &Entry, restore: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = match entry.unix_mode.filter(|_| restore) {
        // Never restore setuid / setgid / sticky; always keep the owner able to read and write.
        Some(mode) => (mode & 0o777 & !crate::volume::umask()) | 0o600,
        None => crate::volume::new_file_mode(),
    };
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

/// Mode of an extracted directory entry, less the umask; the owner always keeps `rwx`.
#[cfg(unix)]
fn apply_dir_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let mode = (mode & 0o777 & !crate::volume::umask()) | 0o700;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

/// Directory modes are Unix-only.
#[cfg(not(unix))]
fn apply_dir_mode(_path: &Path, _mode: u32) {}

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

#[cfg(unix)]
fn make_symlink(target: &str, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn make_symlink(target: &str, link: &Path) -> io::Result<()> {
    let t = target.replace('/', "\\");
    let resolved = link.parent().map(|p| p.join(&t));
    if resolved.is_some_and(|r| r.is_dir()) {
        std::os::windows::fs::symlink_dir(&t, link)
    } else {
        std::os::windows::fs::symlink_file(&t, link)
    }
}

/// Lexically resolves `target` relative to the link's directory and checks it stays in `root`.
pub fn link_stays_inside(root: &Path, link: &Path, target: &str) -> bool {
    let t = target.replace('\\', "/");
    if t.starts_with('/') || t.len() >= 2 && t.as_bytes()[1] == b':' {
        return false;
    }
    let Ok(rel_dir) = link.parent().unwrap_or(root).strip_prefix(root) else {
        return false;
    };
    let mut stack: Vec<String> = rel_dir
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    for part in t.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if stack.pop().is_none() {
                    return false;
                }
            }
            p => stack.push(p.to_owned()),
        }
    }
    true
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

    #[test]
    fn symlink_escape_detection() {
        let root = Path::new("/out");
        assert!(link_stays_inside(
            root,
            Path::new("/out/a/link"),
            "../b.txt"
        ));
        assert!(link_stays_inside(root, Path::new("/out/a/link"), "c/d"));
        assert!(!link_stays_inside(
            root,
            Path::new("/out/a/link"),
            "../../etc/passwd"
        ));
        assert!(!link_stays_inside(
            root,
            Path::new("/out/link"),
            "/etc/passwd"
        ));
        assert!(!link_stays_inside(
            root,
            Path::new("/out/link"),
            "C:\\Windows"
        ));
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
