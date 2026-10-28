//! Copy, cut and paste (`docs/04-ui-design.md` §5.1).
//!
//! Items copied or cut in an archive go on the system clipboard as files: they are extracted into
//! a scratch folder first, so that the file manager or any other app can paste them. Pasted into
//! an open archive, they are copied or moved there as when dragged. Files copied or cut in the
//! file manager are added to the archive they are pasted into; cut ones then go to the recycle
//! bin.
//!
//! An item cut from an archive and pasted elsewhere leaves the archive once its extracted copy has
//! left the scratch folder: the target moved it.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use omnizip_core::{Error, NodeId, SecurityPolicy};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::{
    jobs::{Finish, JobSpec, JobState, Work},
    ops,
    state::{AppState, OpenedArchive},
};

#[cfg(windows)]
pub use platform::hdrop;
pub use platform::{clear, files};

/// Items copied or cut from an open archive.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    pub archive: u32,
    pub paths: Vec<String>,
    pub cut: bool,
    /// Sequence number of the system clipboard while it holds these items.
    #[serde(skip)]
    pub seq: u32,
    /// Tells copies apart: what is left to do for an older one stops.
    #[serde(skip)]
    pub generation: u64,
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Folder holding the scratch folders of copies.
fn root(state: &AppState) -> PathBuf {
    state
        .cache_dir
        .clone()
        .unwrap_or_else(std::env::temp_dir)
        .join("clipboard")
}

/// Removes the scratch folders of copies, but those holding one of `keep`.
fn sweep(root: &Path, keep: &[PathBuf]) {
    let Ok(dirs) = std::fs::read_dir(root) else {
        return;
    };
    for d in dirs.flatten() {
        let dir = d.path();
        if !keep.iter().any(|k| k.starts_with(&dir)) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// At start: the scratch folders of earlier copies go, but the one whose files are on the
/// clipboard.
pub fn sweep_unused(state: &AppState) {
    let keep = files(0).map(|(f, _)| f).unwrap_or_default();
    sweep(&root(state), &keep);
}

/// Whether `path` is one of the files extracted for a copy.
pub fn is_scratch(state: &AppState, path: &Path) -> bool {
    path.starts_with(root(state))
}

/// Whether the system clipboard still holds the items (it always does where the app keeps them
/// itself).
pub fn holds(c: &Clip) -> bool {
    platform::sequence() == c.seq
}

/// Ends a copy or cut, unless a newer one came: the windows stop dimming cut items.
pub fn end(app: &AppHandle, generation: u64) {
    let state = app.state::<AppState>();
    let mut slot = state.clip.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_some_and(|c| c.generation == generation) {
        *slot = None;
        let _ = app.emit("clipboard://changed", None::<Clip>);
    }
}

/// Copies (or cuts) the items at `paths` of `a`, the clipboard owned by the window `own`. Returns
/// the job extracting them for the system clipboard (0 where the app keeps them itself).
pub fn copy(
    app: &AppHandle,
    a: Arc<OpenedArchive>,
    paths: Vec<String>,
    cut: bool,
    own: isize,
    policy: SecurityPolicy,
) -> Result<u32, Error> {
    let state = app.state::<AppState>();
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    // Nothing to paste elsewhere until the extracted files are there.
    clear(own);
    let clip = Clip {
        archive: a.id,
        paths: paths.clone(),
        cut,
        seq: platform::sequence(),
        generation,
    };
    *state.clip.lock().unwrap_or_else(|e| e.into_inner()) = Some(clip.clone());
    let _ = app.emit("clipboard://changed", Some(&clip));
    if !platform::SYSTEM {
        return Ok(0);
    }

    // Earlier copies are off the clipboard now.
    let base = root(&state);
    sweep(&base, &[]);
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = base.join(format!("{}-{stamp}", std::process::id()));
    let opts = ops::export_options(&state, dir.clone());
    let title = ops::file_name(&a.path);
    let password = a.password.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let work: Work = {
        let dir = dir.clone();
        Box::new(move |ctx| {
            std::fs::create_dir_all(&dir).map_err(|e| Error::io(e, &dir))?;
            let report = ops::extract_items(&a, &paths, &opts, ctx)?;
            Ok((report, None))
        })
    };
    let finish: Finish = Box::new(move |app, info| {
        if info.state != JobState::Done {
            return;
        }
        let state = app.state::<AppState>();
        let mut slot = state.clip.lock().unwrap_or_else(|e| e.into_inner());
        // Not if another copy came since, or something else went on the clipboard.
        let Some(c) = slot
            .as_mut()
            .filter(|c| c.generation == generation && holds(c))
        else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        let files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        if files.is_empty() || !platform::set_files(own, &files, cut) {
            return;
        }
        c.seq = platform::sequence();
        if cut {
            // Extracted copies of the cut items (one renamed for the file system is not followed:
            // it stays in the archive).
            let items = c
                .paths
                .iter()
                .map(|p| (dir.join(p.rsplit('/').next().unwrap_or(p)), p.clone()))
                .filter(|(f, _)| f.exists())
                .collect();
            watch(app.clone(), generation, c.archive, items);
        }
    });
    let spec = JobSpec {
        password,
        ..JobSpec::new(if cut { "clipcut" } else { "clipcopy" }, title, policy)
    };
    Ok(state.jobs.spawn(app, spec, work, Some(finish)))
}

/// Takes cut items out of their archive once the paste moved their extracted copies away
/// (`items`: extracted copy and path in the archive). The paste is over when they are all gone, or
/// when the clipboard no longer holds them (the target empties it after moving them, another copy
/// replaces them).
fn watch(app: AppHandle, generation: u64, archive: u32, items: Vec<(PathBuf, String)>) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(400));
            let state = app.state::<AppState>();
            let clip = state.clip.lock().unwrap_or_else(|e| e.into_inner()).clone();
            // Pasted in the app, or another copy came.
            let Some(c) = clip.filter(|c| c.generation == generation) else {
                return;
            };
            if items.iter().all(|(f, _)| !f.exists()) || !holds(&c) {
                break;
            }
        }
        let gone: Vec<&String> = items
            .iter()
            .filter(|(f, _)| !f.exists())
            .map(|(_, p)| p)
            .collect();
        let state = app.state::<AppState>();
        if let Some(a) = state.archive(archive) {
            let nodes: Vec<NodeId> = gone.iter().filter_map(|p| a.tree.find(p)).collect();
            if !nodes.is_empty() {
                let _ = crate::commands::job_delete(app.clone(), state, archive, nodes);
            }
        }
        end(&app, generation);
    });
}

#[cfg(not(windows))]
mod platform {
    use std::path::PathBuf;

    /// The app keeps copied items itself.
    pub const SYSTEM: bool = false;

    pub fn sequence() -> u32 {
        0
    }

    pub fn clear(_own: isize) {}

    pub fn set_files(_own: isize, _files: &[PathBuf], _cut: bool) -> bool {
        false
    }

    /// Files on the clipboard, and whether they were cut.
    pub fn files(_own: isize) -> Option<(Vec<PathBuf>, bool)> {
        None
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        ffi::{OsString, c_void},
        mem::size_of,
        os::windows::ffi::{OsStrExt, OsStringExt},
        path::PathBuf,
        time::Duration,
    };

    use ::windows::{
        Win32::{
            Foundation::{E_OUTOFMEMORY, GlobalFree, HANDLE, HGLOBAL, HWND, POINT},
            System::{
                DataExchange::{
                    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
                    OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
                },
                Memory::{GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc, GlobalLock, GlobalUnlock},
                Ole::{CF_HDROP, DROPEFFECT_COPY, DROPEFFECT_MOVE},
            },
            UI::Shell::{DROPFILES, DragQueryFileW, HDROP},
        },
        core::w,
    };

    /// Copied items go on the system clipboard.
    pub const SYSTEM: bool = true;

    pub fn sequence() -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }

    /// Opens the clipboard for the window `own` (another app may have it open for a moment).
    fn open(own: isize) -> bool {
        let owner = (own != 0).then_some(HWND(own as *mut c_void));
        for _ in 0..10 {
            if unsafe { OpenClipboard(owner) }.is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    pub fn clear(own: isize) {
        if open(own) {
            unsafe {
                let _ = EmptyClipboard();
                let _ = CloseClipboard();
            }
        }
    }

    /// Format telling whether files on the clipboard were copied or cut.
    fn preferred_effect() -> u32 {
        unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) }
    }

    fn alloc(bytes: usize) -> ::windows::core::Result<(HGLOBAL, *mut u8)> {
        unsafe {
            let mem = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes)?;
            let p = GlobalLock(mem);
            if p.is_null() {
                let _ = GlobalFree(Some(mem));
                return Err(E_OUTOFMEMORY.into());
            }
            Ok((mem, p.cast()))
        }
    }

    /// Memory holding `files` as a file list (CF_HDROP), for the clipboard or a drag.
    pub fn hdrop(files: &[PathBuf]) -> ::windows::core::Result<HGLOBAL> {
        let mut list: Vec<u16> = Vec::new();
        for f in files {
            list.extend(f.as_os_str().encode_wide());
            list.push(0);
        }
        list.push(0);
        let head = size_of::<DROPFILES>();
        let (mem, p) = alloc(head + list.len() * 2)?;
        unsafe {
            p.cast::<DROPFILES>().write(DROPFILES {
                pFiles: head as u32,
                pt: POINT::default(),
                fNC: false.into(),
                fWide: true.into(),
            });
            std::ptr::copy_nonoverlapping(list.as_ptr(), p.add(head).cast::<u16>(), list.len());
            let _ = GlobalUnlock(mem);
        }
        Ok(mem)
    }

    /// Hands `mem` over to the clipboard (freed if it does not take it).
    fn put(format: u32, mem: HGLOBAL) -> ::windows::core::Result<()> {
        unsafe {
            SetClipboardData(format, Some(HANDLE(mem.0)))
                .map(|_| ())
                .inspect_err(|_| {
                    let _ = GlobalFree(Some(mem));
                })
        }
    }

    /// Puts `files` on the clipboard, copied or cut.
    pub fn set_files(own: isize, files: &[PathBuf], cut: bool) -> bool {
        if !open(own) {
            return false;
        }
        let effect = if cut {
            DROPEFFECT_MOVE.0
        } else {
            DROPEFFECT_COPY.0
        };
        let done = (|| -> ::windows::core::Result<()> {
            unsafe { EmptyClipboard()? };
            put(u32::from(CF_HDROP.0), hdrop(files)?)?;
            let (mem, p) = alloc(size_of::<u32>())?;
            unsafe {
                p.cast::<u32>().write_unaligned(effect);
                let _ = GlobalUnlock(mem);
            }
            put(preferred_effect(), mem)
        })();
        unsafe {
            let _ = CloseClipboard();
        }
        done.is_ok()
    }

    /// Files on the clipboard, and whether they were cut.
    pub fn files(own: isize) -> Option<(Vec<PathBuf>, bool)> {
        if !open(own) {
            return None;
        }
        let found = unsafe {
            GetClipboardData(u32::from(CF_HDROP.0)).ok().map(|h| {
                let drop = HDROP(h.0);
                let files = (0..DragQueryFileW(drop, u32::MAX, None))
                    .map(|i| {
                        let len = DragQueryFileW(drop, i, None) as usize;
                        let mut buf = vec![0u16; len + 1];
                        DragQueryFileW(drop, i, Some(&mut buf));
                        PathBuf::from(OsString::from_wide(&buf[..len]))
                    })
                    .collect();
                let effect = GetClipboardData(preferred_effect()).ok().and_then(|h| {
                    let mem = HGLOBAL(h.0);
                    let p = GlobalLock(mem).cast::<u32>();
                    if p.is_null() {
                        return None;
                    }
                    let v = p.read_unaligned();
                    let _ = GlobalUnlock(mem);
                    Some(v)
                });
                (files, effect.is_some_and(|v| v & DROPEFFECT_MOVE.0 != 0))
            })
        };
        unsafe {
            let _ = CloseClipboard();
        }
        found
    }
}
