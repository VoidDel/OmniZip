//! Job bodies shared by UI commands and file-manager launches.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use omnizip_core::{
    CollectOptions, Error, ErrorCode, ExtractOptions, FormatId, JobCtx, OpenOptions, OverwriteMode,
    PathMode, Report, Selection, SymlinkPolicy, UpdateOp, WriteOptions, encoding::NameEncoding,
    format::archive_stem, source,
};
use omnizip_formats::shared;
use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::{
    jobs::{Finish, Work},
    state::{AppState, OpenedArchive},
};

/// Extraction request from the UI (`docs/04-ui-design.md` §4.3).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractRequest {
    /// An archive open in the browser…
    pub archive_id: Option<u32>,
    /// …or archive files on disk.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Selected nodes (empty = everything).
    #[serde(default)]
    pub nodes: Vec<u32>,
    /// Directory the selection is relative to (its path prefix is stripped).
    pub relative_to: Option<u32>,
    pub destination: String,
    /// `smart`, `full` or `flat`.
    pub path_mode: String,
    pub overwrite: OverwriteMode,
    pub password: Option<String>,
    pub encoding: Option<String>,
    #[serde(default)]
    pub open_after: bool,
    #[serde(default)]
    pub delete_after: bool,
    #[serde(default)]
    pub keep_broken: bool,
    /// One folder per archive (batch extraction).
    #[serde(default)]
    pub each_to_folder: bool,
}

/// Compression request (`docs/04-ui-design.md` §4.2).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressRequest {
    pub paths: Vec<String>,
    pub destination: String,
    pub format: FormatId,
    pub level: omnizip_core::Level,
    pub method: Option<String>,
    pub password: Option<String>,
    #[serde(default)]
    pub encrypt_names: bool,
    #[serde(default = "yes")]
    pub solid: bool,
    pub volume_size: Option<u64>,
    pub comment: Option<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub delete_source: bool,
    #[serde(default)]
    pub verify: bool,
    #[serde(default)]
    pub open_after: bool,
    /// Save this new archive (`crate::draft`) instead of collecting `paths`.
    #[serde(default)]
    pub draft: Option<u32>,
    /// Store files whose content is compressed already (photos, video, archives).
    #[serde(default = "yes")]
    pub skip_compressed: bool,
}

/// Conversion request: archives on disk written again in another format
/// (`docs/04-ui-design.md` §4.11).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertRequest {
    pub items: Vec<ConvertItem>,
    pub format: FormatId,
    pub level: omnizip_core::Level,
    #[serde(default)]
    pub verify: bool,
    #[serde(default)]
    pub delete_source: bool,
    #[serde(default)]
    pub open_after: bool,
}

/// One archive to convert and the path of its new archive (the dialog shows that path).
#[derive(Debug, Clone, Deserialize)]
pub struct ConvertItem {
    pub source: String,
    pub destination: String,
}

fn yes() -> bool {
    true
}

fn symlink_policy(s: &str) -> SymlinkPolicy {
    match s {
        "inside" => SymlinkPolicy::CreateIfInside,
        "skip" => SymlinkPolicy::Skip,
        _ => SymlinkPolicy::AsFile,
    }
}

fn reveal(path: &Path) {
    if path.is_dir() {
        let _ = tauri_plugin_opener::open_path(path, None::<&str>);
    } else {
        let _ = tauri_plugin_opener::reveal_item_in_dir(path);
    }
}

fn merge(into: &mut Report, r: Report) {
    into.files += r.files;
    into.dirs += r.dirs;
    into.links += r.links;
    into.bytes += r.bytes;
    into.skipped += r.skipped;
    into.warnings.extend(r.warnings);
    into.outputs.extend(r.outputs);
}

/// Builds the extraction work.
pub fn extract_work(app: &AppHandle, req: ExtractRequest) -> Result<(String, Work), Error> {
    let state = app.state::<AppState>();
    let settings = state.settings();
    let opened = match req.archive_id {
        Some(id) => Some(
            state
                .archive(id)
                .ok_or_else(|| Error::invalid_argument("archive closed"))?,
        ),
        None => None,
    };
    let title = match (&opened, req.paths.as_slice()) {
        (Some(a), _) => file_name(&a.path),
        (None, [one]) => file_name(Path::new(one)),
        (None, many) => format!(
            "{} × {}",
            many.len(),
            file_name(Path::new(many.first().map_or("", String::as_str)))
        ),
    };
    let encoding = match req.encoding.as_deref() {
        None | Some("auto") | Some("") => None,
        Some(l) => NameEncoding::from_label(l),
    };
    let locale = omnizip_i18n::detect_language(Some(&settings.language.ui)).to_owned();
    let base_opts = ExtractOptions {
        destination: PathBuf::from(&req.destination),
        path_mode: if req.path_mode == "flat" {
            PathMode::Flat
        } else {
            PathMode::Full
        },
        smart_folder: req.path_mode == "smart",
        overwrite: req.overwrite,
        keep_broken: req.keep_broken,
        symlinks: symlink_policy(&settings.security.symlinks),
        propagate_origin: settings.security.propagate_origin,
        ..ExtractOptions::default()
    };
    let work: Work = Box::new(move |ctx: &JobCtx| {
        let mut total = Report::default();
        let mut outputs: Vec<PathBuf> = Vec::new();
        if let Some(a) = opened {
            let selection = selection_for(&a, &req.nodes, req.relative_to);
            let mut reader = a.reader.lock().unwrap_or_else(|e| e.into_inner());
            let r = omnizip_core::extract(reader.as_mut(), &base_opts, selection, ctx)?;
            outputs.extend(r.outputs.clone());
            merge(&mut total, r);
        } else {
            for p in &req.paths {
                ctx.check()?;
                let path = PathBuf::from(p);
                let oo = OpenOptions {
                    password: req.password.clone(),
                    encoding,
                    locale: Some(locale.clone()),
                };
                let mut reader = shared().open(&path, &oo, ctx)?;
                let mut opts = base_opts.clone();
                if req.each_to_folder {
                    opts.destination = opts.destination.join(archive_stem(&file_name(&path)));
                    opts.smart_folder = false;
                }
                let r = omnizip_core::extract(reader.as_mut(), &opts, Selection::all(), ctx)?;
                drop(reader);
                outputs.extend(r.outputs.clone());
                let clean = r.warnings.is_empty();
                merge(&mut total, r);
                if req.delete_after && clean {
                    let _ = trash::delete(&path);
                }
            }
        }
        let out = outputs.first().cloned();
        if req.open_after
            && let Some(o) = &out
        {
            reveal(o);
        }
        Ok((total, out.map(|p| p.to_string_lossy().into_owned())))
    });
    Ok((title, work))
}

/// Entry ids for selected nodes, relative to a directory node.
fn selection_for(a: &OpenedArchive, nodes: &[u32], relative_to: Option<u32>) -> Selection {
    if nodes.is_empty() {
        return Selection::all();
    }
    let ids = a.tree.collect_entries(nodes);
    let rel = relative_to.map(|n| a.tree.path(n)).unwrap_or_default();
    Selection::of(ids, &rel)
}

/// Builds the compression work.
/// `sources` are the items of a new archive being saved; otherwise `req.paths` are collected.
pub fn compress_work(
    req: CompressRequest,
    sources: Option<Vec<omnizip_core::Source>>,
    threads: u32,
) -> (String, Work) {
    let dest = PathBuf::from(&req.destination);
    let title = file_name(&dest);
    let work: Work = Box::new(move |ctx: &JobCtx| {
        let sources = match &sources {
            Some(items) => items.clone(),
            None => {
                let paths: Vec<PathBuf> = req.paths.iter().map(PathBuf::from).collect();
                source::collect(
                    &paths,
                    &CollectOptions {
                        exclude: req.exclude.clone(),
                        never: vec![dest.clone()],
                        ..Default::default()
                    },
                    ctx,
                )?
            }
        };
        let opts = WriteOptions {
            format: req.format,
            level: req.level,
            method: req.method.clone(),
            threads,
            solid: req.solid,
            password: req.password.clone().filter(|p| !p.is_empty()),
            encrypt_names: req.encrypt_names,
            volume_size: req.volume_size,
            comment: req.comment.clone().filter(|c| !c.is_empty()),
            skip_compressed: req.skip_compressed,
        };
        let mut report = shared().create(&dest, &sources, &opts, ctx)?;
        if req.verify {
            let first = report
                .outputs
                .first()
                .cloned()
                .unwrap_or_else(|| dest.clone());
            let oo = OpenOptions {
                password: opts.password.clone(),
                ..Default::default()
            };
            let mut r = shared().open(&first, &oo, ctx)?;
            let t = omnizip_core::test(r.as_mut(), Selection::all(), ctx)?;
            if let Some(w) = t.warnings.first() {
                return Err(Error::new(w.code).with("path", &w.path));
            }
            report.warnings.extend(t.warnings);
        }
        if req.delete_source {
            for p in &req.paths {
                let _ = trash::delete(p);
            }
        }
        let out = report.outputs.first().cloned().unwrap_or(dest.clone());
        if req.open_after {
            reveal(&out);
        }
        Ok((report, Some(out.to_string_lossy().into_owned())))
    });
    (title, work)
}

/// Builds the test work for an open archive or files on disk.
pub fn test_work(
    opened: Option<Arc<OpenedArchive>>,
    paths: Vec<String>,
    password: Option<String>,
) -> (String, Work) {
    let title = match (&opened, paths.first()) {
        (Some(a), _) => file_name(&a.path),
        (None, Some(p)) => file_name(Path::new(p)),
        _ => String::new(),
    };
    let work: Work = Box::new(move |ctx: &JobCtx| {
        let mut total = Report::default();
        if let Some(a) = opened {
            let mut r = a.reader.lock().unwrap_or_else(|e| e.into_inner());
            merge(
                &mut total,
                omnizip_core::test(r.as_mut(), Selection::all(), ctx)?,
            );
        } else {
            for p in &paths {
                let oo = OpenOptions {
                    password: password.clone(),
                    ..Default::default()
                };
                let mut r = shared().open(Path::new(p), &oo, ctx)?;
                merge(
                    &mut total,
                    omnizip_core::test(r.as_mut(), Selection::all(), ctx)?,
                );
            }
        }
        Ok((total, None))
    });
    (title, work)
}

/// New archives must not replace an archive being converted, nor each other.
fn check_destinations(items: &[ConvertItem]) -> Result<(), Error> {
    let key = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
    let sources: Vec<PathBuf> = items.iter().map(|i| key(&i.source)).collect();
    let mut taken = Vec::new();
    for i in items {
        let dest = key(&i.destination);
        if sources.contains(&dest) || taken.contains(&dest) {
            return Err(Error::new(ErrorCode::AlreadyExists).with("path", &i.destination));
        }
        taken.push(dest);
    }
    Ok(())
}

/// Builds the conversion work: each archive is extracted to a staging folder next to its new
/// archive, which is then written from it (`omnizip_formats::convert`). A password the source
/// needs is asked for by the job.
pub fn convert_work(
    req: ConvertRequest,
    threads: u32,
    skip_compressed: bool,
    locale: String,
) -> Result<(String, Work), Error> {
    check_destinations(&req.items)?;
    let title = match req.items.as_slice() {
        [one] => file_name(Path::new(&one.source)),
        many => format!(
            "{} × {}",
            many.len(),
            file_name(Path::new(many.first().map_or("", |i| i.source.as_str())))
        ),
    };
    let work: Work = Box::new(move |ctx: &JobCtx| {
        let mut total = Report::default();
        for item in &req.items {
            ctx.check()?;
            let src = PathBuf::from(&item.source);
            let dest = PathBuf::from(&item.destination);
            let oo = OpenOptions {
                locale: Some(locale.clone()),
                ..Default::default()
            };
            let wo = WriteOptions {
                format: req.format,
                level: req.level,
                threads,
                skip_compressed,
                ..Default::default()
            };
            let report = omnizip_formats::convert(shared(), &src, &dest, &oo, &wo, ctx)?;
            if req.verify {
                let mut r = shared().open(&dest, &OpenOptions::default(), ctx)?;
                let t = omnizip_core::test(r.as_mut(), Selection::all(), ctx)?;
                if let Some(w) = t.warnings.first() {
                    return Err(Error::new(w.code).with("path", &w.path));
                }
            }
            // A source with entries that were not carried over (warnings) is kept.
            if req.delete_source && report.warnings.is_empty() {
                let _ = trash::delete(&src);
            }
            merge(&mut total, report);
        }
        let out = total.outputs.first().cloned();
        if req.open_after
            && let Some(o) = &out
        {
            reveal(o);
        }
        Ok((total, out.map(|p| p.to_string_lossy().into_owned())))
    });
    Ok((title, work))
}

/// In-place update of an open archive; on success the archive is reopened and the UI is told
/// through `archive://changed`.
pub fn update_work(
    app: &AppHandle,
    a: Arc<OpenedArchive>,
    ops: Vec<UpdateOp>,
) -> (String, Work, Finish) {
    let title = file_name(&a.path);
    let path = a.path.clone();
    let password = a.password.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let settings = app.state::<AppState>().settings();
    let (threads, skip_compressed) = (settings.advanced.threads, settings.formats.skip_compressed);
    let work: Work = Box::new(move |ctx: &JobCtx| {
        // Release our own read handle so the file can be replaced (Windows locks open files).
        {
            let mut r = a.reader.lock().unwrap_or_else(|e| e.into_inner());
            *r = Box::new(Closed(a.info.clone()));
        }
        let wo = WriteOptions {
            password: password.clone(),
            threads,
            skip_compressed,
            ..Default::default()
        };
        let report = shared().update(&path, &ops, &wo, ctx)?;
        Ok((report, Some(path.to_string_lossy().into_owned())))
    });
    let finish: Finish = Box::new(|app: &AppHandle, info| {
        if let Some(id) = info.archive_id {
            let state = app.state::<AppState>();
            if let Some(old) = state.archive(id)
                && let Ok(fresh) = reopen(&old)
            {
                state
                    .archives
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id, Arc::new(fresh));
                let _ = app.emit("archive://changed", id);
            }
        }
    });
    (title, work, finish)
}

/// Saves the changes of a writable mount, kept at its unmount, into the archive
/// (`omnizip_mountd::save`). Archives open in the browser with that path are closed meanwhile
/// and opened again afterwards.
pub fn save_mount_work(
    app: &AppHandle,
    saved: omnizip_mountd::SavedLayer,
) -> (String, Work, Finish) {
    let title = file_name(&saved.archive);
    let state = app.state::<AppState>();
    let threads = state.settings().advanced.threads;
    let opened: Vec<Arc<OpenedArchive>> = state
        .archives
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter(|a| !a.draft && crate::mounts::same_path(&a.path, &saved.archive))
        .cloned()
        .collect();
    let ids: Vec<u32> = opened.iter().map(|a| a.id).collect();
    let work: Work = Box::new(move |ctx: &JobCtx| {
        for a in &opened {
            *a.reader.lock().unwrap_or_else(|e| e.into_inner()) = Box::new(Closed(a.info.clone()));
        }
        let report = omnizip_mountd::save::save(&saved, threads, ctx)?;
        Ok((report, Some(saved.archive.to_string_lossy().into_owned())))
    });
    let finish: Finish = Box::new(move |app: &AppHandle, _| {
        let state = app.state::<AppState>();
        for id in ids {
            if let Some(old) = state.archive(id)
                && let Ok(fresh) = reopen(&old)
            {
                state
                    .archives
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id, Arc::new(fresh));
                let _ = app.emit("archive://changed", id);
            }
        }
    });
    (title, work, finish)
}

/// A placeholder reader used while an archive file is being rewritten.
struct Closed(omnizip_core::ArchiveInfo);

impl omnizip_core::ArchiveReader for Closed {
    fn info(&self) -> &omnizip_core::ArchiveInfo {
        &self.0
    }
    fn entries(&self) -> &[omnizip_core::Entry] {
        &[]
    }
    fn extract_to(
        &mut self,
        _: &mut dyn omnizip_core::ExtractSink,
        _: &JobCtx,
    ) -> Result<(), Error> {
        Err(Error::new(ErrorCode::Io).detail("archive is being updated"))
    }
}

/// Opens an archive again with the same password and encoding.
pub fn reopen(a: &OpenedArchive) -> Result<OpenedArchive, Error> {
    let password = a.password.lock().unwrap_or_else(|e| e.into_inner()).clone();
    open_archive(a.id, &a.path, password, a.encoding)
}

/// Opens an archive for browsing.
pub fn open_archive(
    id: u32,
    path: &Path,
    password: Option<String>,
    encoding: Option<NameEncoding>,
) -> Result<OpenedArchive, Error> {
    let ctx = match &password {
        Some(p) => JobCtx::new(
            Arc::new(omnizip_core::NullProgress),
            Arc::new(omnizip_core::FixedPassword(p.clone())),
        ),
        None => JobCtx::default(),
    };
    let locale = omnizip_i18n::detect_language(None).to_owned();
    let oo = OpenOptions {
        password: password.clone(),
        encoding,
        locale: Some(locale),
    };
    let reader = shared().open(path, &oo, &ctx)?;
    let tree = omnizip_core::TreeIndex::build(reader.entries());
    let kinds = reader.entries().iter().map(|e| e.kind).collect();
    Ok(OpenedArchive {
        id,
        path: path.to_owned(),
        info: reader.info().clone(),
        tree,
        kinds,
        reader: std::sync::Mutex::new(reader),
        password: std::sync::Mutex::new(password),
        encoding,
        draft: false,
    })
}

pub fn file_name(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Renames (`from`, `to`) that move `nodes` into the folder `dir`. Items already in `dir` are
/// left out, and so is an item inside another moved folder (it moves with that folder).
pub fn move_plan(
    tree: &omnizip_core::TreeIndex,
    nodes: &[omnizip_core::NodeId],
    dir: omnizip_core::NodeId,
) -> Result<Vec<(String, String)>, Error> {
    let target = tree.path(dir);
    let below = |path: &str, folder: &str| {
        path.strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
    };
    let froms: Vec<String> = nodes
        .iter()
        .map(|&n| tree.path(n))
        .filter(|p| !p.is_empty())
        .collect();
    let mut moves = Vec::new();
    for from in froms.iter().filter(|p| !froms.iter().any(|q| below(p, q))) {
        if target == *from || below(&target, from) {
            return Err(Error::invalid_argument("a folder cannot move into itself"));
        }
        let name = from.rsplit('/').next().unwrap_or(from);
        let to = if target.is_empty() {
            name.to_owned()
        } else {
            format!("{target}/{name}")
        };
        if to == *from {
            continue;
        }
        if tree.find(&to).is_some() {
            return Err(Error::new(ErrorCode::AlreadyExists).with("path", &to));
        }
        moves.push((from.clone(), to));
    }
    Ok(moves)
}

/// Whether the content of an open archive can change: new archives, and formats updated in
/// place (single volume).
pub fn writable(a: &OpenedArchive) -> bool {
    a.draft
        || shared()
            .capabilities(a.info.format)
            .contains(omnizip_core::Capabilities::UPDATE)
            && a.info.volumes <= 1
}

/// Paths of the top items among `nodes`: an item inside another selected folder comes along
/// with that folder.
pub fn top_paths(tree: &omnizip_core::TreeIndex, nodes: &[omnizip_core::NodeId]) -> Vec<String> {
    let paths: Vec<String> = nodes
        .iter()
        .map(|&n| tree.path(n))
        .filter(|p| !p.is_empty())
        .collect();
    let below = |p: &str, q: &str| p.strip_prefix(q).is_some_and(|r| r.starts_with('/'));
    paths
        .iter()
        .filter(|p| !paths.iter().any(|q| below(p, q)))
        .cloned()
        .collect()
}

/// A scratch folder, removed when dropped unless a new archive took it over.
struct Scratch {
    dir: PathBuf,
    keep: bool,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// Copies the items at `paths` of the open archive `src` into the folder `dir` of `dst` (another
/// archive, or the same one), or moves them: they are extracted to `scratch`, added to `dst`,
/// and for a move deleted from `src`. A new archive (`crate::draft`) takes the extracted files
/// as they are and keeps `scratch` until it is closed.
pub fn transfer_work(
    app: &AppHandle,
    src: Arc<OpenedArchive>,
    paths: Vec<String>,
    dst: Arc<OpenedArchive>,
    dir: String,
    moving: bool,
    scratch: PathBuf,
) -> (String, Work, Finish) {
    let title = file_name(&dst.path);
    let threads = app.state::<AppState>().settings().advanced.threads;
    // Archive files rewritten by the job, reopened when it ends (also after a failure: their
    // readers were closed for the rewrite).
    let mut rewritten = Vec::new();
    if !dst.draft {
        rewritten.push(dst.id);
    }
    if moving && !src.draft && src.id != dst.id {
        rewritten.push(src.id);
    }
    let app = app.clone();
    let work: Work = Box::new(move |ctx: &JobCtx| {
        let mut scratch = Scratch {
            dir: scratch,
            keep: false,
        };
        let report = transfer(
            &app,
            &src,
            &paths,
            &dst,
            &dir,
            moving,
            &mut scratch,
            threads,
            ctx,
        )?;
        Ok((report, Some(dst.path.to_string_lossy().into_owned())))
    });
    let finish: Finish = Box::new(move |app: &AppHandle, _| {
        let state = app.state::<AppState>();
        for id in rewritten {
            if let Some(old) = state.archive(id)
                && let Ok(fresh) = reopen(&old)
            {
                state
                    .archives
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id, Arc::new(fresh));
                let _ = app.emit("archive://changed", id);
            }
        }
    });
    (title, work, finish)
}

#[allow(clippy::too_many_arguments)]
fn transfer(
    app: &AppHandle,
    src: &OpenedArchive,
    paths: &[String],
    dst: &OpenedArchive,
    dir: &str,
    moving: bool,
    scratch: &mut Scratch,
    threads: u32,
    ctx: &JobCtx,
) -> Result<Report, Error> {
    std::fs::create_dir_all(&scratch.dir).map_err(|e| Error::io(e, &scratch.dir))?;
    let opts = ExtractOptions {
        destination: scratch.dir.clone(),
        path_mode: PathMode::Full,
        smart_folder: false,
        overwrite: OverwriteMode::Overwrite,
        ..ExtractOptions::default()
    };
    let report = extract_items(src, paths, &opts, ctx)?;
    let items: Vec<PathBuf> = paths
        .iter()
        .map(|p| scratch.dir.join(p.rsplit('/').next().unwrap_or(p)))
        .collect();

    let state = app.state::<AppState>();
    if dst.draft {
        edit_draft(app, &state, dst.id, |d| {
            d.add(&items, dir)?;
            d.scratch.push(scratch.dir.clone());
            Ok(())
        })?;
        scratch.keep = true;
    } else {
        let sources = source::collect(
            &items,
            &CollectOptions {
                prefix: dir.to_owned(),
                ..Default::default()
            },
            ctx,
        )?;
        let skip = state.settings().formats.skip_compressed;
        rewrite(dst, &[UpdateOp::Add { sources }], threads, skip, ctx)?;
    }
    if moving {
        if src.draft {
            edit_draft(app, &state, src.id, |d| {
                paths.iter().for_each(|p| d.delete(p));
                Ok(())
            })?;
        } else {
            let paths = paths.to_vec();
            rewrite(src, &[UpdateOp::Delete { paths }], threads, true, ctx)?;
        }
    }
    Ok(report)
}

/// Extracts the items at `paths` of `src` (with everything below them) into
/// `opts.destination`, each under its own name: the items of one parent folder are extracted
/// together, that folder's path stripped.
pub fn extract_items(
    src: &OpenedArchive,
    paths: &[String],
    opts: &ExtractOptions,
    ctx: &JobCtx,
) -> Result<Report, Error> {
    let mut groups: std::collections::BTreeMap<&str, Vec<omnizip_core::NodeId>> =
        Default::default();
    for p in paths {
        let parent = p.rsplit_once('/').map_or("", |(parent, _)| parent);
        if let Some(n) = src.tree.find(p) {
            groups.entry(parent).or_default().push(n);
        }
    }
    let mut report = Report::default();
    let mut reader = src.reader.lock().unwrap_or_else(|e| e.into_inner());
    for (parent, nodes) in &groups {
        let selection = Selection::of(src.tree.collect_entries(nodes), parent);
        let r = omnizip_core::extract(reader.as_mut(), opts, selection, ctx)?;
        merge(&mut report, r);
    }
    Ok(report)
}

/// Options for extracting items leaving the app as files (dragged out, copied to the clipboard)
/// into the scratch folder `destination`: the security settings of any extraction; the folder is
/// new, so nothing is in the way.
pub fn export_options(state: &AppState, destination: PathBuf) -> ExtractOptions {
    let settings = state.settings();
    ExtractOptions {
        destination,
        path_mode: PathMode::Full,
        smart_folder: false,
        overwrite: OverwriteMode::Overwrite,
        symlinks: symlink_policy(&settings.security.symlinks),
        propagate_origin: settings.security.propagate_origin,
        ..ExtractOptions::default()
    }
}

/// Applies `ops` to the archive file of `a` (its reader is closed so that the file can be
/// replaced; the caller reopens it). `skip_compressed` applies to the files added.
fn rewrite(
    a: &OpenedArchive,
    ops: &[UpdateOp],
    threads: u32,
    skip_compressed: bool,
    ctx: &JobCtx,
) -> Result<(), Error> {
    {
        let mut r = a.reader.lock().unwrap_or_else(|e| e.into_inner());
        *r = Box::new(Closed(a.info.clone()));
    }
    let wo = WriteOptions {
        password: a.password.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        threads,
        skip_compressed,
        ..Default::default()
    };
    shared().update(&a.path, ops, &wo, ctx)?;
    Ok(())
}

/// Edits a new archive from a job and refreshes its listing.
fn edit_draft(
    app: &AppHandle,
    state: &AppState,
    id: u32,
    edit: impl FnOnce(&mut crate::draft::Draft) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut drafts = state.drafts.lock().unwrap_or_else(|e| e.into_inner());
    let d = drafts
        .get_mut(&id)
        .ok_or_else(|| Error::invalid_argument("archive closed"))?;
    edit(d)?;
    let opened = d.open(id);
    drop(drafts);
    state
        .archives
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, Arc::new(opened));
    let _ = app.emit("archive://changed", id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use omnizip_core::{Entry, EntryKind, TreeIndex};

    use super::*;

    fn tree(paths: &[&str]) -> TreeIndex {
        let entries: Vec<Entry> = paths
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let kind = if p.ends_with('/') {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                };
                Entry::new(i as u32, (*p).to_owned(), kind)
            })
            .collect();
        TreeIndex::build(&entries)
    }

    fn node(t: &TreeIndex, path: &str) -> omnizip_core::NodeId {
        t.find(path).unwrap()
    }

    #[test]
    fn extracts_items_under_their_own_names() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        std::fs::create_dir_all(src.join("docs/notes")).unwrap();
        std::fs::write(src.join("docs/notes/a.txt"), "a").unwrap();
        std::fs::write(src.join("docs/b.txt"), "b").unwrap();
        std::fs::write(src.join("top.txt"), "t").unwrap();
        let ctx = JobCtx::default();
        let items = [src.join("docs"), src.join("top.txt")];
        let sources = source::collect(&items, &CollectOptions::default(), &ctx).unwrap();
        let zip = root.path().join("t.zip");
        let wo = WriteOptions {
            format: FormatId::Zip,
            ..Default::default()
        };
        shared().create(&zip, &sources, &wo, &ctx).unwrap();

        let a = open_archive(1, &zip, None, None).unwrap();
        let out = root.path().join("out");
        let opts = ExtractOptions {
            destination: out.clone(),
            smart_folder: false,
            overwrite: OverwriteMode::Overwrite,
            ..Default::default()
        };
        let paths = ["docs/notes".to_owned(), "top.txt".to_owned()];
        extract_items(&a, &paths, &opts, &ctx).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("notes/a.txt")).unwrap(),
            "a"
        );
        assert_eq!(std::fs::read_to_string(out.join("top.txt")).unwrap(), "t");
        assert!(!out.join("docs").exists());
    }

    #[test]
    fn moves_items_into_a_folder() {
        let t = tree(&["a.txt", "docs/", "docs/b.txt", "out/", "out/c.txt"]);
        let plan = move_plan(&t, &[node(&t, "a.txt"), node(&t, "docs")], node(&t, "out")).unwrap();
        assert_eq!(
            plan,
            [
                ("a.txt".to_owned(), "out/a.txt".to_owned()),
                ("docs".to_owned(), "out/docs".to_owned())
            ]
        );
        // Back to the root.
        let plan = move_plan(&t, &[node(&t, "out/c.txt")], TreeIndex::ROOT).unwrap();
        assert_eq!(plan, [("out/c.txt".to_owned(), "c.txt".to_owned())]);
    }

    #[test]
    fn skips_what_needs_no_move_and_refuses_the_impossible() {
        let t = tree(&[
            "a.txt",
            "docs/",
            "docs/b.txt",
            "docs/sub/",
            "out/",
            "out/a.txt",
        ]);
        // Already in the folder; a file inside a moved folder moves with it.
        let plan = move_plan(&t, &[node(&t, "docs/b.txt")], node(&t, "docs")).unwrap();
        assert!(plan.is_empty());
        let plan = move_plan(
            &t,
            &[node(&t, "docs"), node(&t, "docs/b.txt")],
            node(&t, "out"),
        )
        .unwrap();
        assert_eq!(plan, [("docs".to_owned(), "out/docs".to_owned())]);
        // Into itself or below itself; onto an existing name.
        assert!(move_plan(&t, &[node(&t, "docs")], node(&t, "docs")).is_err());
        assert!(move_plan(&t, &[node(&t, "docs")], node(&t, "docs/sub")).is_err());
        assert!(move_plan(&t, &[node(&t, "a.txt")], node(&t, "out")).is_err());
    }

    #[test]
    fn convert_never_replaces_a_source_or_another_output() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n).to_string_lossy().into_owned();
        std::fs::write(p("photos.zip"), "").unwrap();
        std::fs::write(p("photos.7z"), "").unwrap();
        let item = |s: &str, d: &str| ConvertItem {
            source: p(s),
            destination: p(d),
        };
        // An existing archive that is not being converted is replaced (the dialog says so).
        assert!(check_destinations(&[item("photos.zip", "photos.7z")]).is_ok());
        assert!(check_destinations(&[item("photos.7z", "photos.7z")]).is_err());
        // Converting both to 7z: the first must not take the second's name, nor share one.
        assert!(
            check_destinations(&[
                item("photos.zip", "photos.7z"),
                item("photos.7z", "photos (2).7z")
            ])
            .is_err()
        );
        assert!(
            check_destinations(&[
                item("photos.zip", "photos (2).7z"),
                item("photos.7z", "photos (2).7z")
            ])
            .is_err()
        );
        assert!(
            check_destinations(&[
                item("photos.zip", "photos (2).7z"),
                item("photos.7z", "photos (3).7z")
            ])
            .is_ok()
        );
    }
}
