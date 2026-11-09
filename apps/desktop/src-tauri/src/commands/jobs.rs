//! Jobs: extract, compress, test, convert, editing archives, drag and drop, clipboard.

use super::*;

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
    password: Option<Secret>,
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
    let skip = state.settings().formats.skip_compressed;
    let (title, work) = ops::convert_work(request, threads(&state), skip, locale).map_err(err)?;
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

/// Adds files to an open ZIP below a directory node. Listing the added folders walks them.
#[tauri::command]
pub async fn job_add(app: AppHandle, id: u32, paths: Vec<String>, dir: NodeId) -> CmdResult<u32> {
    with_state(app, move |app, state| add(app, state, id, paths, dir)).await
}

fn add(
    app: &AppHandle,
    state: &AppState,
    id: u32,
    paths: Vec<String>,
    dir: NodeId,
) -> CmdResult<u32> {
    let a = archive(state, id)?;
    let prefix = a.tree.path(dir);
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    if let Some(r) = edit_draft(app, state, id, |d| d.add(&paths, &prefix)) {
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
    spawn_update(app, state, id, "add", vec![UpdateOp::Add { sources }], None)
}

#[tauri::command]
pub async fn job_delete(app: AppHandle, id: u32, nodes: Vec<NodeId>) -> CmdResult<u32> {
    with_state(app, move |app, state| delete_items(app, state, id, nodes)).await
}

/// Deletes items of an open archive (also when items cut to the clipboard were moved away).
pub(crate) fn delete_items(
    app: &AppHandle,
    state: &AppState,
    id: u32,
    nodes: Vec<NodeId>,
) -> CmdResult<u32> {
    let a = archive(state, id)?;
    let paths = nodes
        .iter()
        .map(|&n| a.tree.path(n))
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>();
    if let Some(r) = edit_draft(app, state, id, |d| {
        paths.iter().for_each(|p| d.delete(p));
        Ok(())
    }) {
        return r;
    }
    spawn_update(
        app,
        state,
        id,
        "delete",
        vec![UpdateOp::Delete { paths }],
        None,
    )
}

/// Moves items into the folder `dir` (dragged onto it in the browser). Items already there are
/// left alone, and an item inside another moved folder moves with that folder.
#[tauri::command]
pub async fn job_move(app: AppHandle, id: u32, nodes: Vec<NodeId>, dir: NodeId) -> CmdResult<u32> {
    with_state(app, move |app, state| {
        move_items(app, state, id, nodes, dir)
    })
    .await
}

fn move_items(
    app: &AppHandle,
    state: &AppState,
    id: u32,
    nodes: Vec<NodeId>,
    dir: NodeId,
) -> CmdResult<u32> {
    let moves = ops::move_plan(&archive(state, id)?.tree, &nodes, dir).map_err(err)?;
    if moves.is_empty() {
        return Ok(0);
    }
    if let Some(r) = edit_draft(app, state, id, |d| {
        moves.iter().try_for_each(|(from, to)| d.rename(from, to))
    }) {
        return r;
    }
    let ops = moves
        .into_iter()
        .map(|(from, to)| UpdateOp::Rename { from, to })
        .collect();
    spawn_update(app, state, id, "move", ops, None)
}

/// Copies items into a folder of an open archive (another one or the same), or moves them
/// into another archive: dragging between archives in the browser. A move inside one
/// archive is `job_move`.
#[tauri::command]
pub async fn job_transfer(
    app: AppHandle,
    from: u32,
    nodes: Vec<NodeId>,
    to: u32,
    dir: NodeId,
    copy: bool,
) -> CmdResult<u32> {
    with_state(app, move |app, state| {
        transfer(app, state, from, nodes, to, dir, copy)
    })
    .await
}

#[allow(clippy::too_many_arguments)]
fn transfer(
    app: &AppHandle,
    state: &AppState,
    from: u32,
    nodes: Vec<NodeId>,
    to: u32,
    dir: NodeId,
    copy: bool,
) -> CmdResult<u32> {
    if from == to && !copy {
        return move_items(app, state, from, nodes, dir);
    }
    let src = archive(state, from)?;
    let dst = archive(state, to)?;
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
    let (title, work, finish) = ops::transfer_work(app, src, paths, dst, target, !copy, scratch);
    let spec = JobSpec {
        archive_id: Some(to),
        ..JobSpec::new(if copy { "copy" } else { "move" }, title, policy(state))
    };
    Ok(state.jobs.spawn(app, spec, work, Some(finish)))
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
/// the system clipboard (0 if none). Opening the clipboard may wait for another program, and the
/// files of an earlier copy are deleted.
#[tauri::command]
pub async fn clipboard_copy(
    app: AppHandle,
    window: WebviewWindow,
    id: u32,
    nodes: Vec<NodeId>,
    cut: bool,
) -> CmdResult<u32> {
    let own = dragout::window_handle(&window);
    with_state(app, move |app, state| {
        let a = archive(state, id)?;
        let paths = ops::top_paths(&a.tree, &nodes);
        if paths.is_empty() {
            return Ok(0);
        }
        // Items of an archive that cannot change can only be copied.
        let cut = cut && ops::writable(&a);
        clipboard::copy(app, a, paths, cut, own, policy(state)).map_err(err)
    })
    .await
}

/// Pastes into the folder `dir` of an open archive: items copied or cut in an open archive go
/// there as when dragged; files from the file manager are added, and cut ones then go to the
/// recycle bin.
#[tauri::command]
pub async fn clipboard_paste(
    app: AppHandle,
    window: WebviewWindow,
    id: u32,
    dir: NodeId,
) -> CmdResult<u32> {
    let own = dragout::window_handle(&window);
    with_state(app, move |app, state| paste(app, state, own, id, dir)).await
}

fn paste(app: &AppHandle, state: &AppState, own: isize, id: u32, dir: NodeId) -> CmdResult<u32> {
    let clip = state.clip.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(c) = clip.filter(clipboard::holds)
        && let Some(src) = state.archive(c.archive)
    {
        let nodes: Vec<NodeId> = c.paths.iter().filter_map(|p| src.tree.find(p)).collect();
        if c.cut {
            // A cut is pasted once.
            clipboard::end(app, c.generation);
            clipboard::clear(own);
        }
        if nodes.is_empty() {
            return Ok(0);
        }
        return transfer(app, state, c.archive, nodes, id, dir, !c.cut);
    }
    let Some((files, cut)) = clipboard::files(own) else {
        return Ok(0);
    };
    let a = archive(state, id)?;
    let paths: Vec<String> = files
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    // A new archive refers to the files where they are, so they stay; so do files extracted for
    // a copy of an archive closed since.
    if !cut || a.draft || files.iter().any(|f| clipboard::is_scratch(state, f)) {
        return add(app, state, id, paths, dir);
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
        app,
        state,
        id,
        "add",
        vec![UpdateOp::Add { sources }],
        Some(then),
    )
}

#[tauri::command]
pub async fn job_rename(app: AppHandle, id: u32, node: NodeId, name: String) -> CmdResult<u32> {
    with_state(app, move |app, state| rename(app, state, id, node, name)).await
}

fn rename(
    app: &AppHandle,
    state: &AppState,
    id: u32,
    node: NodeId,
    name: String,
) -> CmdResult<u32> {
    let a = archive(state, id)?;
    if name.is_empty() || name.contains(['/', '\\']) {
        return Err(err(Error::invalid_argument("invalid name")));
    }
    let from = a.tree.path(node);
    let to = match from.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{name}"),
        None => name,
    };
    if let Some(r) = edit_draft(app, state, id, |d| d.rename(&from, &to)) {
        return r;
    }
    spawn_update(
        app,
        state,
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
