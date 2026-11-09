//! Archives open in the browser: opening, new archives, listing, entries, previews.

use super::*;

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
pub async fn archive_summary(app: AppHandle, id: u32) -> CmdResult<ArchiveSummary> {
    with_state(app, move |_, state| Ok(summary(&*archive(state, id)?))).await
}

/// Closes an archive. Letting go of a large one and deleting a new archive's scratch folders
/// take a while, so it happens off the main thread.
#[tauri::command]
pub async fn archive_close(app: AppHandle, id: u32) -> CmdResult<()> {
    with_state(app, move |_, state| {
        let opened = state
            .archives
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        drop(opened);
        let draft = state
            .drafts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        for dir in draft.map(|d| d.scratch).unwrap_or_default() {
            let _ = std::fs::remove_dir_all(dir);
        }
        Ok(())
    })
    .await
}

/// Starts a new archive (`crate::draft`), optionally with files and folders in it, and opens
/// it for browsing.
#[tauri::command]
pub async fn draft_new(
    app: AppHandle,
    name: String,
    paths: Vec<String>,
) -> CmdResult<ArchiveSummary> {
    with_state(app, move |_, state| {
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
    })
    .await
}

/// Creates an empty folder in a new archive.
#[tauri::command]
pub async fn draft_mkdir(app: AppHandle, id: u32, dir: NodeId, name: String) -> CmdResult<()> {
    if name.is_empty() || name.contains(['/', '\\']) {
        return Err(err(Error::invalid_argument("invalid name")));
    }
    with_state(app, move |app, state| {
        let dir = archive(state, id)?.tree.path(dir);
        edit_draft(app, state, id, |d| d.mkdir(&dir, &name))
            .unwrap_or_else(|| Err(err(Error::invalid_argument("not a new archive"))))
            .map(|_| ())
    })
    .await
}

/// Applies an edit to a new archive and refreshes its listing. Drafts live in memory, so there
/// is no job (the returned job id is 0); `None` when `id` is not a draft.
pub(super) fn edit_draft(
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
/// Sorting and searching walk the tree: a large archive takes a while.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn archive_list(
    app: AppHandle,
    id: u32,
    dir: NodeId,
    sort: String,
    desc: bool,
    offset: usize,
    limit: usize,
    query: Option<String>,
) -> CmdResult<ListPage> {
    with_state(app, move |_, state| {
        list(
            &*archive(state, id)?,
            dir,
            &sort,
            desc,
            offset,
            limit,
            query,
        )
    })
    .await
}

fn list(
    a: &OpenedArchive,
    dir: NodeId,
    sort: &str,
    desc: bool,
    offset: usize,
    limit: usize,
    query: Option<String>,
) -> CmdResult<ListPage> {
    let key = sort_key(sort);
    let limit = limit.min(2000);
    let page = match query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        Some(q) => a.tree.search(dir, q, key, desc, offset, limit),
        None => a.tree.list(dir, key, desc, offset, limit),
    };
    let searching = query.as_deref().is_some_and(|q| !q.trim().is_empty());
    let rows = page
        .items
        .iter()
        .filter_map(|&n| row(a, n, searching))
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
pub async fn archive_dirs(app: AppHandle, id: u32, node: NodeId) -> CmdResult<Vec<TreeItem>> {
    with_state(app, move |_, state| dirs(&*archive(state, id)?, node)).await
}

fn dirs(a: &OpenedArchive, node: NodeId) -> CmdResult<Vec<TreeItem>> {
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
