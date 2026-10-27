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
pub fn compress_work(req: CompressRequest, threads: u32) -> (String, Work) {
    let dest = PathBuf::from(&req.destination);
    let title = file_name(&dest);
    let work: Work = Box::new(move |ctx: &JobCtx| {
        let paths: Vec<PathBuf> = req.paths.iter().map(PathBuf::from).collect();
        let sources = source::collect(
            &paths,
            &CollectOptions {
                exclude: req.exclude.clone(),
                never: vec![dest.clone()],
                ..Default::default()
            },
            ctx,
        )?;
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
            for p in &paths {
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
    let threads = app.state::<AppState>().settings().advanced.threads;
    let work: Work = Box::new(move |ctx: &JobCtx| {
        // Release our own read handle so the file can be replaced (Windows locks open files).
        {
            let mut r = a.reader.lock().unwrap_or_else(|e| e.into_inner());
            *r = Box::new(Closed(a.info.clone()));
        }
        let wo = WriteOptions {
            password: password.clone(),
            threads,
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
    })
}

pub fn file_name(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}
