//! IPC commands (`docs/02-architecture.md` §5). Every command returns `ErrorReport` on failure so
//! the UI can localize the message from its code.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
};

use base64::Engine as _;
use omnizip_core::{
    Capabilities, EntryKind, Error, ErrorCode, ErrorReport, FormatId, FormatSupport, NodeId,
    SortKey, UpdateOp,
    encoding::{self, NameEncoding},
    entry::to_millis,
};
use omnizip_formats::shared;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use crate::{
    clipboard,
    draft::Draft,
    dragout,
    integration::{self, ShellStatus},
    jobs::{Finish, JobInfo, JobSpec, JobState, PromptAnswer},
    launch::LaunchRequest,
    mounts::{self, MountInfo, MountRequest},
    ops::{self, CompressRequest, ConvertRequest, ExtractRequest},
    preview::{self, Preview},
    settings::{self, Settings},
    state::{AppState, OpenedArchive, RecentItem},
    system::{self, SystemInfo},
    theme::{self, ResolveOptions, ResolvedTheme, ThemeSummary},
    windows,
};

type CmdResult<T> = Result<T, ErrorReport>;

fn err(e: Error) -> ErrorReport {
    e.to_report()
}

fn archive(state: &AppState, id: u32) -> CmdResult<Arc<OpenedArchive>> {
    state
        .archive(id)
        .ok_or_else(|| err(Error::invalid_argument(format!("archive {id} is not open"))))
}

// ---------------------------------------------------------------------------------------------
// Bootstrap, settings, themes

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    settings: Settings,
    themes: Vec<ThemeSummary>,
    system: SystemInfo,
    launch: Option<LaunchRequest>,
    formats: Vec<FormatSupport>,
    write_methods: BTreeMap<String, Vec<&'static str>>,
    encodings: Vec<&'static str>,
    languages: Vec<(&'static str, &'static str)>,
    config_path: Option<String>,
}

#[tauri::command]
pub fn app_bootstrap(state: State<'_, AppState>) -> Bootstrap {
    let themes = theme::summaries(&state.themes.lock().unwrap_or_else(|e| e.into_inner()));
    let write_methods = [FormatId::Zip, FormatId::SevenZ]
        .iter()
        .map(|f| {
            (
                f.name().to_owned(),
                omnizip_formats::write_methods(*f).to_vec(),
            )
        })
        .collect();
    Bootstrap {
        settings: state.settings(),
        themes,
        system: system::info(),
        launch: state
            .launch
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take(),
        formats: shared().support(),
        write_methods,
        encodings: encoding::all_labels(),
        languages: omnizip_i18n::LANGUAGES.to_vec(),
        config_path: state
            .config_file
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
    }
}

/// Pending launch request (a file opened while the window was starting). The main window asks
/// once it listens for `app://launch`; later requests go there.
#[tauri::command]
pub fn app_take_launch(state: State<'_, AppState>) -> Option<LaunchRequest> {
    let mut pending = state.launch.lock().unwrap_or_else(|e| e.into_inner());
    state.launch_ready.store(true, Ordering::SeqCst);
    pending.take()
}

#[tauri::command]
pub fn settings_set(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: Settings,
) -> CmdResult<Settings> {
    let previous = state.settings();
    let material_changed = previous.appearance.material != settings.appearance.material
        || previous.appearance.material_kind != settings.appearance.material_kind;
    let menu_changed = previous.shell != settings.shell;
    if let Some(p) = &state.config_file {
        settings::save(p, &settings).map_err(|e| err(Error::io(e, p)))?;
    }
    *state.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings.clone();
    if material_changed {
        windows::apply_material(&app);
    }
    // Flat versus submenu layout lives in the registry: apply it right away.
    if menu_changed {
        integration::refresh_classic();
    }
    Ok(settings)
}

#[tauri::command]
pub fn theme_resolve(
    state: State<'_, AppState>,
    id: String,
    options: ResolveOptions,
) -> CmdResult<ResolvedTheme> {
    let themes = state.themes.lock().unwrap_or_else(|e| e.into_inner());
    let t = themes
        .iter()
        .find(|(t, _)| t.id == id)
        .or_else(|| themes.first())
        .ok_or_else(|| err(Error::internal("no themes")))?;
    let options = ResolveOptions {
        material: options.material && system::material_supported(),
        ..options
    };
    Ok(theme::resolve(&t.0, &options))
}

#[tauri::command]
pub fn themes_reload(state: State<'_, AppState>) -> Vec<ThemeSummary> {
    let all = theme::load_all(state.themes_dir.as_deref());
    let out = theme::summaries(&all);
    *state.themes.lock().unwrap_or_else(|e| e.into_inner()) = all;
    out
}

/// Validates and copies a theme file into the user theme folder.
#[tauri::command]
pub fn theme_import(state: State<'_, AppState>, path: String) -> CmdResult<Vec<ThemeSummary>> {
    let text = std::fs::read_to_string(&path).map_err(|e| err(Error::io(e, &path)))?;
    let t = theme::parse(&text).map_err(|m| err(Error::invalid_argument(m)))?;
    let dir = state
        .themes_dir
        .clone()
        .ok_or_else(|| err(Error::internal("no config dir")))?;
    std::fs::create_dir_all(&dir).map_err(|e| err(Error::io(e, &dir)))?;
    let dest = dir.join(format!("{}.json", t.id));
    std::fs::write(&dest, text).map_err(|e| err(Error::io(e, &dest)))?;
    Ok(themes_reload(state))
}

#[tauri::command]
pub fn themes_open_folder(state: State<'_, AppState>) -> CmdResult<()> {
    let dir = state
        .themes_dir
        .clone()
        .ok_or_else(|| err(Error::internal("no config dir")))?;
    std::fs::create_dir_all(&dir).map_err(|e| err(Error::io(e, &dir)))?;
    tauri_plugin_opener::open_path(&dir, None::<&str>).map_err(|e| err(Error::internal(e)))
}

// ---------------------------------------------------------------------------------------------
// Archives

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveSummary {
    id: u32,
    path: String,
    name: String,
    info: omnizip_core::ArchiveInfo,
    total_size: u64,
    packed_size: u64,
    files: u64,
    dirs: usize,
    writable: bool,
    capabilities: Vec<&'static str>,
    mount_class: omnizip_core::MountClass,
    entries: usize,
    /// A new archive not saved yet.
    draft: bool,
}

fn summary(a: &OpenedArchive) -> ArchiveSummary {
    let caps = shared().capabilities(a.info.format);
    let root = a.tree.root();
    let dirs = (1..a.tree.len() as u32)
        .filter(|&i| a.tree.node(i).is_some_and(|n| n.is_dir))
        .count();
    ArchiveSummary {
        id: a.id,
        path: a.path.to_string_lossy().into_owned(),
        name: ops::file_name(&a.path),
        info: a.info.clone(),
        total_size: root.size,
        packed_size: if root.packed > 0 {
            root.packed
        } else {
            a.info.archive_size
        },
        files: root.files,
        dirs,
        writable: a.draft || caps.contains(Capabilities::UPDATE) && a.info.volumes <= 1,
        capabilities: caps.names(),
        mount_class: a.info.format.mount_class(a.info.solid),
        entries: a.tree.len().saturating_sub(1),
        draft: a.draft,
    }
}

/// Opens an archive. `PasswordRequired` / `WrongPassword` errors let the UI ask and retry.
#[tauri::command]
pub async fn archive_open(
    state: State<'_, AppState>,
    path: String,
    password: Option<String>,
    encoding: Option<String>,
) -> CmdResult<ArchiveSummary> {
    let enc = match encoding.as_deref() {
        None | Some("") | Some("auto") => None,
        Some(l) => {
            Some(NameEncoding::from_label(l).ok_or_else(|| err(Error::invalid_argument(l)))?)
        }
    };
    let id = state.next_archive_id();
    let p = PathBuf::from(&path);
    let opened =
        tauri::async_runtime::spawn_blocking(move || ops::open_archive(id, &p, password, enc))
            .await
            .map_err(|e| err(Error::internal(e)))?
            .map_err(err)?;
    let s = summary(&opened);
    state.touch_recent(RecentItem {
        path: s.path.clone(),
        name: s.name.clone(),
        format: s.info.format.name().to_owned(),
        size: s.info.archive_size,
        opened: to_millis(std::time::SystemTime::now()),
    });
    state
        .archives
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, Arc::new(opened));
    Ok(s)
}

/// Reopens an archive with another file-name encoding (menu "File name encoding").
#[tauri::command]
pub async fn archive_reencode(
    state: State<'_, AppState>,
    id: u32,
    encoding: String,
) -> CmdResult<ArchiveSummary> {
    let a = archive(&state, id)?;
    let enc = if encoding == "auto" {
        None
    } else {
        NameEncoding::from_label(&encoding)
    };
    let password = a.password.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let path = a.path.clone();
    let opened =
        tauri::async_runtime::spawn_blocking(move || ops::open_archive(id, &path, password, enc))
            .await
            .map_err(|e| err(Error::internal(e)))?
            .map_err(err)?;
    let s = summary(&opened);
    state
        .archives
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, Arc::new(opened));
    Ok(s)
}

#[tauri::command]
pub fn archive_summary(state: State<'_, AppState>, id: u32) -> CmdResult<ArchiveSummary> {
    let a = archive(&state, id)?;
    Ok(summary(&a))
}

#[tauri::command]
pub fn archive_close(state: State<'_, AppState>, id: u32) {
    state
        .archives
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
    let draft = state
        .drafts
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
    for dir in draft.map(|d| d.scratch).unwrap_or_default() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Starts a new archive (`crate::draft`), optionally with files and folders in it, and opens
/// it for browsing.
#[tauri::command]
pub fn draft_new(
    state: State<'_, AppState>,
    name: String,
    paths: Vec<String>,
) -> CmdResult<ArchiveSummary> {
    let mut draft = Draft::new(name);
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    draft.add(&paths, "").map_err(err)?;
    let id = state.next_archive_id();
    let opened = draft.open(id);
    let s = summary(&opened);
    state
        .archives
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, Arc::new(opened));
    state
        .drafts
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, draft);
    Ok(s)
}

/// Creates an empty folder in a new archive.
#[tauri::command]
pub fn draft_mkdir(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u32,
    dir: NodeId,
    name: String,
) -> CmdResult<()> {
    if name.is_empty() || name.contains(['/', '\\']) {
        return Err(err(Error::invalid_argument("invalid name")));
    }
    let dir = archive(&state, id)?.tree.path(dir);
    edit_draft(&app, &state, id, |d| d.mkdir(&dir, &name))
        .unwrap_or_else(|| Err(err(Error::invalid_argument("not a new archive"))))
        .map(|_| ())
}

/// Applies an edit to a new archive and refreshes its listing. Drafts live in memory, so there
/// is no job (the returned job id is 0); `None` when `id` is not a draft.
fn edit_draft(
    app: &AppHandle,
    state: &AppState,
    id: u32,
    edit: impl FnOnce(&mut Draft) -> omnizip_core::Result<()>,
) -> Option<CmdResult<u32>> {
    let mut drafts = state.drafts.lock().unwrap_or_else(|e| e.into_inner());
    let draft = drafts.get_mut(&id)?;
    if let Err(e) = edit(draft) {
        return Some(Err(err(e)));
    }
    let opened = draft.open(id);
    drop(drafts);
    state
        .archives
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, Arc::new(opened));
    let _ = app.emit("archive://changed", id);
    Some(Ok(0))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    node: NodeId,
    name: String,
    is_dir: bool,
    kind: EntryKind,
    size: u64,
    packed: u64,
    mtime: Option<i64>,
    encrypted: bool,
    files: u64,
    children: usize,
    /// Parent path, for search results.
    parent: Option<String>,
}

#[derive(Serialize)]
pub struct ListPage {
    total: usize,
    offset: usize,
    rows: Vec<Row>,
}

fn row(a: &OpenedArchive, id: NodeId, with_parent: bool) -> Option<Row> {
    let n = a.tree.node(id)?;
    let kind = match n.entry.and_then(|e| a.kinds.get(e as usize).copied()) {
        Some(k) => k,
        None if n.is_dir => EntryKind::Dir,
        None => EntryKind::File,
    };
    Some(Row {
        node: id,
        name: n.name.clone(),
        is_dir: n.is_dir,
        kind,
        size: n.size,
        packed: n.packed,
        mtime: n.mtime.map(to_millis),
        encrypted: n.encrypted,
        files: n.files,
        children: n.children.len(),
        parent: with_parent.then(|| n.parent.map(|p| a.tree.path(p)).unwrap_or_default()),
    })
}

fn sort_key(s: &str) -> SortKey {
    match s {
        "size" => SortKey::Size,
        "packed" => SortKey::Packed,
        "mtime" => SortKey::Mtime,
        "type" => SortKey::Type,
        _ => SortKey::Name,
    }
}

/// One page of a directory, or of search results below it.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn archive_list(
    state: State<'_, AppState>,
    id: u32,
    dir: NodeId,
    sort: String,
    desc: bool,
    offset: usize,
    limit: usize,
    query: Option<String>,
) -> CmdResult<ListPage> {
    let a = archive(&state, id)?;
    let key = sort_key(&sort);
    let limit = limit.min(2000);
    let page = match query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        Some(q) => a.tree.search(dir, q, key, desc, offset, limit),
        None => a.tree.list(dir, key, desc, offset, limit),
    };
    let searching = query.as_deref().is_some_and(|q| !q.trim().is_empty());
    let rows = page
        .items
        .iter()
        .filter_map(|&n| row(&a, n, searching))
        .collect();
    Ok(ListPage {
        total: page.total,
        offset,
        rows,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeItem {
    node: NodeId,
    name: String,
    has_children: bool,
    nested_archive: bool,
}

/// Sub-directories of a node (sidebar tree).
#[tauri::command]
pub fn archive_dirs(state: State<'_, AppState>, id: u32, node: NodeId) -> CmdResult<Vec<TreeItem>> {
    let a = archive(&state, id)?;
    let page = a.tree.list(node, SortKey::Name, false, 0, usize::MAX);
    Ok(page
        .items
        .into_iter()
        .filter_map(|n| {
            let x = a.tree.node(n)?;
            x.is_dir.then(|| TreeItem {
                node: n,
                name: x.name.clone(),
                has_children: x
                    .children
                    .iter()
                    .any(|&c| a.tree.node(c).is_some_and(|c| c.is_dir)),
                nested_archive: FormatId::from_file_name(&x.name).is_some(),
            })
        })
        .collect())
}

#[derive(Serialize)]
pub struct Crumb {
    node: NodeId,
    name: String,
}

/// The node at a `/`-separated path. Node ids change when an archive is rewritten, so the
/// browser finds its folder again by path.
#[tauri::command]
pub fn archive_find(
    state: State<'_, AppState>,
    id: u32,
    path: String,
) -> CmdResult<Option<NodeId>> {
    Ok(archive(&state, id)?.tree.find(&path))
}

#[tauri::command]
pub fn archive_ancestors(
    state: State<'_, AppState>,
    id: u32,
    node: NodeId,
) -> CmdResult<Vec<Crumb>> {
    let a = archive(&state, id)?;
    Ok(a.tree
        .ancestors(node)
        .into_iter()
        .filter_map(|n| {
            a.tree.node(n).map(|x| Crumb {
                node: n,
                name: x.name.clone(),
            })
        })
        .collect())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeInfo {
    node: NodeId,
    name: String,
    path: String,
    is_dir: bool,
    kind: EntryKind,
    size: u64,
    packed: u64,
    files: u64,
    mtime: Option<i64>,
    crc32: Option<u32>,
    method: Option<String>,
    encrypted: bool,
    link_target: Option<String>,
    comment: Option<String>,
}

#[tauri::command]
pub fn archive_node(state: State<'_, AppState>, id: u32, node: NodeId) -> CmdResult<NodeInfo> {
    let a = archive(&state, id)?;
    let n = a
        .tree
        .node(node)
        .ok_or_else(|| err(Error::invalid_argument("no such node")))?;
    // Properties are informational: when a job holds the reader, show what the tree knows.
    let reader = a.reader.try_lock().ok();
    let e = n
        .entry
        .and_then(|e| reader.as_ref().and_then(|r| r.entries().get(e as usize)));
    Ok(NodeInfo {
        node,
        name: n.name.clone(),
        path: a.tree.path(node),
        is_dir: n.is_dir,
        kind: n
            .entry
            .and_then(|i| a.kinds.get(i as usize).copied())
            .unwrap_or(if n.is_dir {
                EntryKind::Dir
            } else {
                EntryKind::File
            }),
        size: n.size,
        packed: n.packed,
        files: n.files,
        mtime: n.mtime.map(to_millis),
        crc32: e.and_then(|e| e.crc32),
        method: e
            .and_then(|e| e.method.clone())
            .or_else(|| a.info.methods.first().cloned()),
        encrypted: n.encrypted,
        link_target: e.and_then(|e| e.link_target.clone()),
        comment: e.and_then(|e| e.comment.clone()),
    })
}

/// Declared size and file count of a selection (extract dialog summary).
#[tauri::command]
pub fn archive_selection_size(
    state: State<'_, AppState>,
    id: u32,
    nodes: Vec<NodeId>,
) -> CmdResult<(u64, u64)> {
    let a = archive(&state, id)?;
    if nodes.is_empty() {
        return Ok((a.tree.root().size, a.tree.root().files));
    }
    Ok(nodes
        .iter()
        .filter_map(|&n| a.tree.node(n))
        .fold((0, 0), |(b, f), n| (b + n.size, f + n.files)))
}

fn read_node(
    a: &OpenedArchive,
    node: NodeId,
    max: usize,
) -> Result<(String, Vec<u8>, bool), Error> {
    let n = a
        .tree
        .node(node)
        .ok_or_else(|| Error::invalid_argument("no such node"))?;
    let entry = n
        .entry
        .ok_or_else(|| Error::invalid_argument("not a file"))?;
    let ctx = match a.password.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        Some(p) => omnizip_core::JobCtx::new(
            Arc::new(omnizip_core::NullProgress),
            Arc::new(omnizip_core::FixedPassword(p)),
        ),
        None => omnizip_core::JobCtx::default(),
    };
    let (data, cut) = match a.reader.try_lock() {
        Ok(mut r) => omnizip_core::read_entry(r.as_mut(), entry, max, &ctx)?,
        // A job is using the archive: read through a second handle.
        Err(_) => {
            let fresh = ops::reopen(a)?;
            let mut r = fresh.reader.lock().unwrap_or_else(|e| e.into_inner());
            omnizip_core::read_entry(r.as_mut(), entry, max, &ctx)?
        }
    };
    Ok((n.name.clone(), data, cut))
}

#[tauri::command]
pub async fn archive_preview(
    state: State<'_, AppState>,
    id: u32,
    node: NodeId,
) -> CmdResult<Preview> {
    let a = archive(&state, id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let (name, data, cut) = read_node(&a, node, preview::MAX_PREVIEW_BYTES)?;
        Ok::<_, Error>(preview::build(&name, &data, cut))
    })
    .await
    .map_err(|e| err(Error::internal(e)))?
    .map_err(err)
}

/// Supplies the password for an encrypted archive that is already open (previews).
#[tauri::command]
pub fn archive_set_password(
    state: State<'_, AppState>,
    id: u32,
    password: String,
) -> CmdResult<()> {
    let a = archive(&state, id)?;
    *a.password.lock().unwrap_or_else(|e| e.into_inner()) = Some(password);
    Ok(())
}

/// Extracts one entry into the cache and opens it with the system application.
#[tauri::command]
pub async fn archive_open_external(
    state: State<'_, AppState>,
    id: u32,
    node: NodeId,
) -> CmdResult<String> {
    let a = archive(&state, id)?;
    let cache = state
        .cache_dir
        .clone()
        .ok_or_else(|| err(Error::internal("no cache dir")))?;
    tauri::async_runtime::spawn_blocking(move || {
        let (name, data, _) = read_node(&a, node, usize::MAX >> 1)?;
        let safe = omnizip_core::path::sanitize(
            &name,
            omnizip_core::path::TargetOs::current(),
            omnizip_core::path::UnsafePathMode::Strip,
        )?;
        let dir = cache.join("open").join(format!("{}-{node}", a.id));
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(e, &dir))?;
        let file = dir.join(safe.file_name());
        std::fs::write(&file, data).map_err(|e| Error::io(e, &file))?;
        if let Some(origin) = omnizip_core::origin::read(&a.path) {
            let _ = omnizip_core::origin::apply(&file, &origin);
        }
        tauri_plugin_opener::open_path(&file, None::<&str>).map_err(Error::internal)?;
        Ok::<_, Error>(file.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| err(Error::internal(e)))?
    .map_err(err)
}

// ---------------------------------------------------------------------------------------------
// Jobs

fn threads(state: &AppState) -> u32 {
    state.settings().advanced.threads
}

fn policy(state: &AppState) -> omnizip_core::SecurityPolicy {
    omnizip_core::SecurityPolicy {
        max_ratio: state.settings().security.max_ratio.max(10),
        ..Default::default()
    }
}

#[tauri::command]
pub fn job_extract(
    app: AppHandle,
    state: State<'_, AppState>,
    request: ExtractRequest,
    window: Option<String>,
) -> CmdResult<u32> {
    let archive_id = request.archive_id;
    let password = request.password.clone();
    let (title, work) = ops::extract_work(&app, request).map_err(err)?;
    let spec = JobSpec {
        window,
        archive_id,
        password,
        ..JobSpec::new("extract", title, policy(&state))
    };
    Ok(state.jobs.spawn(&app, spec, work, None))
}

#[tauri::command]
pub fn job_compress(
    app: AppHandle,
    state: State<'_, AppState>,
    request: CompressRequest,
    window: Option<String>,
) -> CmdResult<u32> {
    let sources = request.draft.map(|id| {
        state
            .drafts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .map(|d| d.sources.clone())
            .unwrap_or_default()
    });
    let (title, work) = ops::compress_work(request, sources, threads(&state));
    Ok(state.jobs.spawn(
        &app,
        JobSpec {
            window,
            ..JobSpec::new("compress", title, policy(&state))
        },
        work,
        None,
    ))
}

#[tauri::command]
pub fn job_test(
    app: AppHandle,
    state: State<'_, AppState>,
    archive_id: Option<u32>,
    paths: Vec<String>,
    password: Option<String>,
) -> CmdResult<u32> {
    let opened = match archive_id {
        Some(id) => Some(archive(&state, id)?),
        None => None,
    };
    let (title, work) = ops::test_work(opened, paths, password.clone());
    let spec = JobSpec {
        archive_id,
        password,
        ..JobSpec::new("test", title, policy(&state))
    };
    Ok(state.jobs.spawn(&app, spec, work, None))
}

#[tauri::command]
pub fn job_convert(
    app: AppHandle,
    state: State<'_, AppState>,
    request: ConvertRequest,
    window: Option<String>,
) -> CmdResult<u32> {
    let locale = omnizip_i18n::detect_language(Some(&state.settings().language.ui)).to_owned();
    let (title, work) = ops::convert_work(request, threads(&state), locale).map_err(err)?;
    Ok(state.jobs.spawn(
        &app,
        JobSpec {
            window,
            ..JobSpec::new("convert", title, policy(&state))
        },
        work,
        None,
    ))
}

fn spawn_update(
    app: &AppHandle,
    state: &AppState,
    id: u32,
    kind: &'static str,
    ops_list: Vec<UpdateOp>,
    then: Option<Finish>,
) -> CmdResult<u32> {
    let a = archive(state, id)?;
    if !shared()
        .capabilities(a.info.format)
        .contains(Capabilities::UPDATE)
    {
        return Err(err(Error::new(ErrorCode::UnsupportedOperation)
            .with("format", a.info.format)
            .with("operation", kind)));
    }
    let (title, work, finish) = ops::update_work(app, a, ops_list);
    let finish: Finish = match then {
        None => finish,
        Some(then) => Box::new(move |app, info| {
            finish(app, info);
            then(app, info);
        }),
    };
    let spec = JobSpec {
        archive_id: Some(id),
        ..JobSpec::new(kind, title, policy(state))
    };
    Ok(state.jobs.spawn(app, spec, work, Some(finish)))
}

/// Adds files to an open ZIP below a directory node.
#[tauri::command]
pub fn job_add(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u32,
    paths: Vec<String>,
    dir: NodeId,
) -> CmdResult<u32> {
    let a = archive(&state, id)?;
    let prefix = a.tree.path(dir);
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    if let Some(r) = edit_draft(&app, &state, id, |d| d.add(&paths, &prefix)) {
        return r;
    }
    let ctx = omnizip_core::JobCtx::default();
    let sources = omnizip_core::source::collect(
        &paths,
        &omnizip_core::CollectOptions {
            prefix,
            ..Default::default()
        },
        &ctx,
    )
    .map_err(err)?;
    spawn_update(
        &app,
        &state,
        id,
        "add",
        vec![UpdateOp::Add { sources }],
        None,
    )
}

#[tauri::command]
pub fn job_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u32,
    nodes: Vec<NodeId>,
) -> CmdResult<u32> {
    let a = archive(&state, id)?;
    let paths = nodes
        .iter()
        .map(|&n| a.tree.path(n))
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>();
    if let Some(r) = edit_draft(&app, &state, id, |d| {
        paths.iter().for_each(|p| d.delete(p));
        Ok(())
    }) {
        return r;
    }
    spawn_update(
        &app,
        &state,
        id,
        "delete",
        vec![UpdateOp::Delete { paths }],
        None,
    )
}

/// Moves items into the folder `dir` (dragged onto it in the browser). Items already there are
/// left alone, and an item inside another moved folder moves with that folder.
#[tauri::command]
pub fn job_move(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u32,
    nodes: Vec<NodeId>,
    dir: NodeId,
) -> CmdResult<u32> {
    let moves = ops::move_plan(&archive(&state, id)?.tree, &nodes, dir).map_err(err)?;
    if moves.is_empty() {
        return Ok(0);
    }
    if let Some(r) = edit_draft(&app, &state, id, |d| {
        moves.iter().try_for_each(|(from, to)| d.rename(from, to))
    }) {
        return r;
    }
    let ops = moves
        .into_iter()
        .map(|(from, to)| UpdateOp::Rename { from, to })
        .collect();
    spawn_update(&app, &state, id, "move", ops, None)
}

/// Copies items into a folder of an open archive (another one or the same), or moves them
/// into another archive: dragging between archives in the browser. A move inside one
/// archive is `job_move`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn job_transfer(
    app: AppHandle,
    state: State<'_, AppState>,
    from: u32,
    nodes: Vec<NodeId>,
    to: u32,
    dir: NodeId,
    copy: bool,
) -> CmdResult<u32> {
    if from == to && !copy {
        return job_move(app, state, from, nodes, dir);
    }
    let src = archive(&state, from)?;
    let dst = archive(&state, to)?;
    let unsupported = |a: &OpenedArchive, operation: &str| {
        err(Error::new(ErrorCode::UnsupportedOperation)
            .with("format", a.info.format)
            .with("operation", operation))
    };
    if !ops::writable(&dst) {
        return Err(unsupported(&dst, "add"));
    }
    if !copy && !ops::writable(&src) {
        return Err(unsupported(&src, "delete"));
    }
    let paths = ops::top_paths(&src.tree, &nodes);
    if paths.is_empty() {
        return Ok(0);
    }
    let target = dst.tree.path(dir);
    for p in &paths {
        if from == to && (target == *p || target.starts_with(&format!("{p}/"))) {
            return Err(err(Error::invalid_argument(
                "a folder cannot go into itself",
            )));
        }
        let name = p.rsplit('/').next().unwrap_or(p);
        let dest = if target.is_empty() {
            name.to_owned()
        } else {
            format!("{target}/{name}")
        };
        if dst.tree.find(&dest).is_some() {
            return Err(err(Error::new(ErrorCode::AlreadyExists).with("path", &dest)));
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let scratch = state
        .cache_dir
        .clone()
        .unwrap_or_else(std::env::temp_dir)
        .join("transfer")
        .join(format!("{}-{stamp}", std::process::id()));
    let (title, work, finish) = ops::transfer_work(&app, src, paths, dst, target, !copy, scratch);
    let spec = JobSpec {
        archive_id: Some(to),
        ..JobSpec::new(if copy { "copy" } else { "move" }, title, policy(&state))
    };
    Ok(state.jobs.spawn(&app, spec, work, Some(finish)))
}

/// Picture of a drag out of the window: a PNG data URL, the pointer at (`x`, `y`) in it.
#[derive(Deserialize)]
pub struct DragImage {
    png: String,
    x: i32,
    y: i32,
}

/// Drags items of an open archive out of the window, extracting them where they are dropped
/// (`crate::dragout`); resolves when the drag ends, to whether they were dropped somewhere.
#[tauri::command]
pub async fn drag_out(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: u32,
    nodes: Vec<NodeId>,
    image: Option<DragImage>,
) -> CmdResult<bool> {
    let a = archive(&state, id)?;
    let paths = ops::top_paths(&a.tree, &nodes);
    if paths.is_empty() {
        return Ok(false);
    }
    let image = image.and_then(|i| {
        let png = base64::engine::general_purpose::STANDARD
            .decode(i.png.split_once(',')?.1)
            .ok()?;
        let rgba = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .ok()?
            .to_rgba8();
        Some(dragout::Image {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw(),
            x: i.x,
            y: i.y,
        })
    });
    let policy = policy(&state);
    let own = dragout::window_handle(&window);
    // The system drag loop runs on the thread of the windows, from the event loop rather than
    // from this request: the page goes on talking to the app during the drag.
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let state = handle.state::<AppState>();
        let _ = tx.send(dragout::run(&state, a, paths, policy, image, own));
    })
    .map_err(|e| err(Error::internal(e)))?;
    tauri::async_runtime::spawn_blocking(move || rx.recv())
        .await
        .map_err(|e| err(Error::internal(e)))?
        .map_err(|e| err(Error::internal(e)))?
        .map_err(err)
}

/// Copies or cuts items of an open archive (`crate::clipboard`); returns the job putting them on
/// the system clipboard (0 if none).
#[tauri::command]
pub fn clipboard_copy(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: u32,
    nodes: Vec<NodeId>,
    cut: bool,
) -> CmdResult<u32> {
    let a = archive(&state, id)?;
    let paths = ops::top_paths(&a.tree, &nodes);
    if paths.is_empty() {
        return Ok(0);
    }
    // Items of an archive that cannot change can only be copied.
    let cut = cut && ops::writable(&a);
    let own = dragout::window_handle(&window);
    clipboard::copy(&app, a, paths, cut, own, policy(&state)).map_err(err)
}

/// Pastes into the folder `dir` of an open archive: items copied or cut in an open archive go
/// there as when dragged; files from the file manager are added, and cut ones then go to the
/// recycle bin.
#[tauri::command]
pub fn clipboard_paste(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: u32,
    dir: NodeId,
) -> CmdResult<u32> {
    let own = dragout::window_handle(&window);
    let clip = state.clip.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(c) = clip.filter(clipboard::holds)
        && let Some(src) = state.archive(c.archive)
    {
        let nodes: Vec<NodeId> = c.paths.iter().filter_map(|p| src.tree.find(p)).collect();
        if c.cut {
            // A cut is pasted once.
            clipboard::end(&app, c.generation);
            clipboard::clear(own);
        }
        if nodes.is_empty() {
            return Ok(0);
        }
        return job_transfer(app, state, c.archive, nodes, id, dir, !c.cut);
    }
    let Some((files, cut)) = clipboard::files(own) else {
        return Ok(0);
    };
    let a = archive(&state, id)?;
    let paths: Vec<String> = files
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    // A new archive refers to the files where they are, so they stay; so do files extracted for
    // a copy of an archive closed since.
    if !cut || a.draft || files.iter().any(|f| clipboard::is_scratch(&state, f)) {
        return job_add(app, state, id, paths, dir);
    }
    let ctx = omnizip_core::JobCtx::default();
    let sources = omnizip_core::source::collect(
        &files,
        &omnizip_core::CollectOptions {
            prefix: a.tree.path(dir),
            ..Default::default()
        },
        &ctx,
    )
    .map_err(err)?;
    let then: Finish = Box::new(move |_, info| {
        if info.state == JobState::Done {
            for f in &files {
                let _ = trash::delete(f);
            }
            clipboard::clear(own);
        }
    });
    spawn_update(
        &app,
        &state,
        id,
        "add",
        vec![UpdateOp::Add { sources }],
        Some(then),
    )
}

#[tauri::command]
pub fn job_rename(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u32,
    node: NodeId,
    name: String,
) -> CmdResult<u32> {
    let a = archive(&state, id)?;
    if name.is_empty() || name.contains(['/', '\\']) {
        return Err(err(Error::invalid_argument("invalid name")));
    }
    let from = a.tree.path(node);
    let to = match from.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{name}"),
        None => name,
    };
    if let Some(r) = edit_draft(&app, &state, id, |d| d.rename(&from, &to)) {
        return r;
    }
    spawn_update(
        &app,
        &state,
        id,
        "rename",
        vec![UpdateOp::Rename { from, to }],
        None,
    )
}

#[tauri::command]
pub fn job_cancel(state: State<'_, AppState>, id: u32) {
    state.jobs.cancel(id);
}

#[tauri::command]
pub fn job_pause(app: AppHandle, state: State<'_, AppState>, id: u32) {
    state.jobs.pause(&app, id);
}

#[tauri::command]
pub fn job_resume(app: AppHandle, state: State<'_, AppState>, id: u32) {
    state.jobs.resume(&app, id);
}

#[tauri::command]
pub fn job_list(state: State<'_, AppState>) -> Vec<JobInfo> {
    state.jobs.list()
}

/// Prompts that are still waiting (for windows that opened after the event was sent).
#[tauri::command]
pub fn job_prompts(state: State<'_, AppState>) -> Vec<crate::jobs::PromptEvent> {
    state.jobs.open_prompts()
}

#[tauri::command]
pub fn job_clear(state: State<'_, AppState>) {
    state.jobs.clear_finished();
}

#[tauri::command]
pub fn job_answer(state: State<'_, AppState>, job_id: u32, prompt_id: u32, answer: PromptAnswer) {
    state.jobs.answer(job_id, prompt_id, answer);
}

// ---------------------------------------------------------------------------------------------
// Files and system

#[derive(Serialize)]
pub struct Classified {
    archives: Vec<String>,
    others: Vec<String>,
}

/// Splits dropped paths into archives and other files (`docs/04-ui-design.md` §5.2).
#[tauri::command]
pub fn paths_classify(paths: Vec<String>) -> Classified {
    let (mut archives, mut others) = (Vec::new(), Vec::new());
    for p in paths {
        let path = Path::new(&p);
        if path.is_file()
            && shared()
                .detect(path)
                .is_ok_and(|d| shared().engine(d.format).is_some())
        {
            archives.push(p);
        } else {
            others.push(p);
        }
    }
    Classified { archives, others }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathStats {
    bytes: u64,
    files: u64,
    common_parent: Option<String>,
    suggested_name: String,
}

/// Size of files about to be compressed and a suggested archive name.
#[tauri::command]
pub fn paths_stats(paths: Vec<String>) -> PathStats {
    let ctx = omnizip_core::JobCtx::default();
    let pb: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    let sources = omnizip_core::source::collect(&pb, &Default::default(), &ctx).unwrap_or_default();
    let parent = pb.first().and_then(|p| p.parent()).map(Path::to_path_buf);
    let suggested = match pb.as_slice() {
        [one] => {
            let n = ops::file_name(one);
            if one.is_dir() {
                n
            } else {
                n.rsplit_once('.').map_or(n.clone(), |(s, _)| s.to_owned())
            }
        }
        _ => parent
            .as_deref()
            .map(ops::file_name)
            .unwrap_or_else(|| "archive".to_owned()),
    };
    PathStats {
        bytes: omnizip_core::source::total_size(&sources),
        files: omnizip_core::source::file_count(&sources),
        common_parent: parent.map(|p| p.to_string_lossy().into_owned()),
        suggested_name: suggested,
    }
}

#[tauri::command]
pub fn path_reveal(path: String) -> CmdResult<()> {
    let p = PathBuf::from(&path);
    let r = if p.is_dir() {
        tauri_plugin_opener::open_path(&p, None::<&str>)
    } else {
        tauri_plugin_opener::reveal_item_in_dir(&p)
    };
    r.map_err(|e| err(Error::internal(e)))
}

#[tauri::command]
pub fn path_exists(path: String) -> bool {
    Path::new(&path).exists()
}

#[tauri::command]
pub fn recent_list(state: State<'_, AppState>) -> Vec<RecentItem> {
    let mut r = state.recent.lock().unwrap_or_else(|e| e.into_inner());
    r.retain(|i| Path::new(&i.path).exists());
    r.clone()
}

#[tauri::command]
pub fn recent_remove(state: State<'_, AppState>, path: String) {
    let mut r = state.recent.lock().unwrap_or_else(|e| e.into_inner());
    r.retain(|i| i.path != path);
    state.save_recent(&r);
}

#[tauri::command]
pub fn recent_clear(state: State<'_, AppState>) {
    let mut r = state.recent.lock().unwrap_or_else(|e| e.into_inner());
    r.clear();
    state.save_recent(&r);
}

/// Shows a window once the UI has painted (prevents a white flash).
#[tauri::command]
pub fn window_ready(window: WebviewWindow) {
    let _ = window.show();
    window_focus(window);
}

/// Brings the calling window to the front with keyboard focus inside its page, so a field the
/// page focused can be typed into right away. Transparent (material) windows on Windows do
/// not hand focus to their WebView by themselves.
#[tauri::command]
pub fn window_focus(window: WebviewWindow) {
    let _ = window.set_focus();
    let _ = AsRef::<tauri::Webview>::as_ref(&window).set_focus();
}

/// The request a standalone dialog window was opened for (`None` for other windows).
#[tauri::command]
pub fn shell_request(window: WebviewWindow, state: State<'_, AppState>) -> Option<LaunchRequest> {
    state
        .shell_requests
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(window.label())
        .cloned()
}

/// Context-menu registration state (docs/07-shell-integration.md §7).
#[tauri::command]
pub async fn shell_status() -> CmdResult<ShellStatus> {
    tauri::async_runtime::spawn_blocking(integration::status)
        .await
        .map_err(|e| err(Error::internal(e)))
}

/// Registers (or re-registers with the current menu settings) the context menu for this user,
/// with the Windows 11 top-level menu where the app ships its package (a few seconds).
#[tauri::command]
pub async fn shell_register() -> CmdResult<ShellStatus> {
    tauri::async_runtime::spawn_blocking(|| integration::register().map(|()| integration::status()))
        .await
        .map_err(|e| err(Error::internal(e)))?
        .map_err(err)
}

#[tauri::command]
pub async fn shell_unregister() -> CmdResult<ShellStatus> {
    tauri::async_runtime::spawn_blocking(|| {
        integration::unregister().map(|()| integration::status())
    })
    .await
    .map_err(|e| err(Error::internal(e)))?
    .map_err(err)
}

/// Opens the project's page in the browser (settings, About). The page cannot open addresses
/// itself (docs/09-security.md §7), so this one is fixed here.
#[tauri::command]
pub fn open_homepage() -> CmdResult<()> {
    tauri_plugin_opener::open_url(env!("CARGO_PKG_REPOSITORY"), None::<&str>)
        .map_err(|e| err(Error::internal(e)))
}

/// Opens the download page of the mount driver (WinFsp), for the settings page.
#[tauri::command]
pub fn open_mount_driver_page() -> CmdResult<()> {
    tauri_plugin_opener::open_url("https://winfsp.dev/rel/", None::<&str>)
        .map_err(|e| err(Error::internal(e)))
}

/// Opens the system "default apps" settings (`docs/07-shell-integration.md` §3.4).
#[tauri::command]
pub fn open_default_apps() -> CmdResult<()> {
    let url = if cfg!(windows) {
        "ms-settings:defaultapps"
    } else if cfg!(target_os = "macos") {
        "x-apple.systempreferences:com.apple.preference.general"
    } else {
        return Ok(());
    };
    tauri_plugin_opener::open_url(url, None::<&str>).map_err(|e| err(Error::internal(e)))
}

/// Mount backend state (docs/12-mount.md §9): whether the driver is installed and whether
/// this build can mount at all.
#[tauri::command]
pub fn mount_probe() -> omnizip_vfs::Probe {
    mounts::probe()
}

#[tauri::command]
pub fn mount_list(state: State<'_, AppState>) -> Vec<MountInfo> {
    state.mounts.list()
}

/// Drive letters free for a mount, from Z: downwards.
#[tauri::command]
pub fn mount_free_letters() -> Vec<String> {
    mounts::free_letters()
}

/// Mounts an archive (docs/12-mount.md §2). `PasswordRequired` lets the UI ask and retry.
#[tauri::command]
pub async fn mount_start(app: AppHandle, request: MountRequest) -> CmdResult<MountInfo> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<AppState>().mounts.mount(&app, request)
    })
    .await
    .map_err(|e| err(Error::internal(e)))?
    .map_err(err)
}

/// Unmounts; waits for a file system call in progress (a solid block being decoded).
#[tauri::command]
pub async fn mount_stop(app: AppHandle, id: u64) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || app.state::<AppState>().mounts.unmount(&app, id))
        .await
        .map_err(|e| err(Error::internal(e)))
}
