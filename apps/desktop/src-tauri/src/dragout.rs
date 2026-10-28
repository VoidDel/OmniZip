//! Dragging items of an open archive out of the window, onto the desktop, a file manager or any
//! other drop target (`docs/04-ui-design.md` §5.2).
//!
//! The drag leaves the window as a system drag of files in a new scratch folder, and the items
//! start extracting there at once. Letting go over a target that takes files waits for the
//! extraction, then hands the files over; letting go anywhere else, or Esc, stops it and removes
//! the folder. Dropped files stay until a later drag or the next start sweeps them up: the target
//! may still be copying them after the drop.

use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, SystemTime},
};

use omnizip_core::{
    CancelToken, Error, FixedPassword, Interaction, JobCtx, NoInteraction, NullProgress,
    SecurityPolicy,
};

use crate::{
    ops,
    state::{AppState, OpenedArchive},
};

/// The picture that follows the pointer: RGBA pixels, the pointer at (`x`, `y`).
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub x: i32,
    pub y: i32,
}

/// Folder holding the scratch folders of drags.
fn root(state: &AppState) -> PathBuf {
    state
        .cache_dir
        .clone()
        .unwrap_or_else(std::env::temp_dir)
        .join("dragout")
}

/// Removes the scratch folders of drags older than an hour (a drop target may still be copying
/// from newer ones).
pub fn sweep(state: &AppState) {
    let Ok(dirs) = std::fs::read_dir(root(state)) else {
        return;
    };
    let Some(old) = SystemTime::now().checked_sub(Duration::from_secs(3600)) else {
        return;
    };
    for d in dirs.flatten() {
        if d.metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t < old)
        {
            let _ = std::fs::remove_dir_all(d.path());
        }
    }
}

/// Handle of the window the drag leaves (`0` where there is none to tell).
pub fn window_handle(window: &tauri::WebviewWindow) -> isize {
    #[cfg(windows)]
    {
        window.hwnd().map_or(0, |h| h.0 as isize)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        0
    }
}

/// Drags the items at `paths` of `src` out of the window `own`; returns once the drag ends,
/// whether they were dropped somewhere. Runs on the thread of the windows (the system drag loop
/// needs it).
pub fn run(
    state: &AppState,
    src: Arc<OpenedArchive>,
    paths: Vec<String>,
    policy: SecurityPolicy,
    image: Option<Image>,
    own: isize,
) -> Result<bool, Error> {
    sweep(state);
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = root(state).join(format!("{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(e, &dir))?;
    // The names in the archive, for targets to look at during the drag; the files as extracted
    // (named validly for the file system) take their place at the drop.
    let files: Vec<PathBuf> = paths
        .iter()
        .map(|p| dir.join(p.rsplit('/').next().unwrap_or(p)))
        .collect();

    // Nobody can answer a question during the drag: without the password the extraction fails.
    let password = src
        .password
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let interaction: Arc<dyn Interaction> = match password {
        Some(p) => Arc::new(FixedPassword(p)),
        None => Arc::new(NoInteraction),
    };
    let cancel = CancelToken::new();
    let ctx = JobCtx::new(Arc::new(NullProgress), interaction)
        .with_cancel(cancel.clone())
        .with_policy(policy);
    let opts = ops::export_options(state, dir.clone());
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ = tx.send(ops::extract_items(&src, &paths, &opts, &ctx).map(|_| ()));
    });
    let dropped = platform::drag(&dir, &files, rx, image.as_ref(), own);
    if !matches!(dropped, Ok(true)) {
        cancel.cancel();
        std::thread::spawn(move || {
            let _ = worker.join();
            let _ = std::fs::remove_dir_all(&dir);
        });
    }
    dropped
}

#[cfg(not(windows))]
mod platform {
    use std::{
        path::{Path, PathBuf},
        sync::mpsc::Receiver,
    };

    use omnizip_core::{Error, ErrorCode};

    use super::Image;

    pub fn drag(
        _dir: &Path,
        _files: &[PathBuf],
        _extraction: Receiver<Result<(), Error>>,
        _image: Option<&Image>,
        _own: isize,
    ) -> Result<bool, Error> {
        Err(Error::new(ErrorCode::UnsupportedOperation).with("operation", "drag-out"))
    }
}

#[cfg(windows)]
#[allow(non_snake_case)]
mod platform {
    use std::{
        cell::{Cell, RefCell},
        mem::{ManuallyDrop, size_of},
        path::{Path, PathBuf},
        sync::mpsc::{Receiver, TryRecvError},
    };

    use ::windows::{
        Win32::{
            Foundation::{
                COLORREF, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS,
                GlobalFree, POINT, S_OK, SIZE,
            },
            Graphics::Gdi::{
                BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDIBSection, DIB_RGB_COLORS,
                DeleteObject,
            },
            System::{
                Com::{
                    CLSCTX_INPROC_SERVER, CoCreateInstance, DVASPECT_CONTENT, FORMATETC,
                    IDataObject, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
                },
                Ole::{
                    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, DoDragDrop,
                    IDropSource, IDropSource_Impl,
                },
                SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS},
            },
            UI::{
                Shell::{
                    CLSID_DragDropHelper, DSH_ALLOWDROPDESCRIPTIONTEXT, IDragSourceHelper2,
                    SHCreateDataObject, SHDRAGIMAGE,
                },
                WindowsAndMessaging::{
                    DispatchMessageW, GA_ROOT, GetAncestor, GetCursorPos, IDC_NO, LoadCursorW, MSG,
                    MsgWaitForMultipleObjects, PM_QS_PAINT, PM_QS_POSTMESSAGE, PM_QS_SENDMESSAGE,
                    PM_REMOVE, PeekMessageW, QS_PAINT, QS_POSTMESSAGE, QS_SENDMESSAGE, SetCursor,
                    TranslateMessage, WindowFromPoint,
                },
            },
        },
        core::{BOOL, ComObject, HRESULT, IUnknown, implement},
    };
    use omnizip_core::Error;

    use super::Image;

    /// The source end of the system drag.
    #[implement(IDropSource)]
    struct Source {
        data: IDataObject,
        dir: PathBuf,
        extraction: Receiver<Result<(), Error>>,
        /// What the target under the pointer would do with the files.
        effect: Cell<DROPEFFECT>,
        /// Set when the button is let go over a target: whether the files went there.
        outcome: RefCell<Option<Result<bool, Error>>>,
        /// The window the drag left.
        own: isize,
    }

    impl Source {
        /// Whether the pointer is over the window the drag left: the files dropped there would
        /// come straight back in, so it takes nothing.
        fn over_own_window(&self) -> bool {
            let mut pt = POINT::default();
            unsafe {
                GetCursorPos(&mut pt).is_ok()
                    && GetAncestor(WindowFromPoint(pt), GA_ROOT).0 as isize == self.own
            }
        }

        /// Waits for the extraction, the windows of the thread kept painted meanwhile (input
        /// stays queued for the drag loop).
        fn wait(&self) -> Result<(), Error> {
            loop {
                match self.extraction.try_recv() {
                    Ok(r) => return r,
                    Err(TryRecvError::Disconnected) => {
                        return Err(Error::internal("the extraction stopped"));
                    }
                    Err(TryRecvError::Empty) => unsafe {
                        let _ = MsgWaitForMultipleObjects(
                            None,
                            false,
                            50,
                            QS_PAINT | QS_POSTMESSAGE | QS_SENDMESSAGE,
                        );
                        let mut msg = MSG::default();
                        let which = PM_REMOVE | PM_QS_PAINT | PM_QS_POSTMESSAGE | PM_QS_SENDMESSAGE;
                        while PeekMessageW(&mut msg, None, 0, 0, which).as_bool() {
                            let _ = TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    },
                }
            }
        }

        /// Waits for the files, then lists them as extracted for the target.
        fn hand_over(&self) -> Result<bool, Error> {
            self.wait()?;
            let files: Vec<PathBuf> = std::fs::read_dir(&self.dir)
                .map_err(|e| Error::io(e, &self.dir))?
                .flatten()
                .map(|e| e.path())
                .collect();
            if files.is_empty() {
                return Ok(false);
            }
            set_files(&self.data, &files).map_err(Error::internal)?;
            Ok(true)
        }
    }

    impl IDropSource_Impl for Source_Impl {
        fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
            if escape.as_bool() {
                return DRAGDROP_S_CANCEL;
            }
            if keys.0 & MK_LBUTTON.0 != 0 {
                return S_OK;
            }
            // Let go: over a target that takes files, they go there once extracted.
            if self.effect.get() == DROPEFFECT_NONE || self.over_own_window() {
                return DRAGDROP_S_CANCEL;
            }
            let outcome = self.hand_over();
            let go = matches!(outcome, Ok(true));
            *self.outcome.borrow_mut() = Some(outcome);
            if go {
                DRAGDROP_S_DROP
            } else {
                DRAGDROP_S_CANCEL
            }
        }

        fn GiveFeedback(&self, effect: DROPEFFECT) -> HRESULT {
            self.effect.set(effect);
            if self.over_own_window()
                && let Ok(no) = unsafe { LoadCursorW(None, IDC_NO) }
            {
                unsafe { SetCursor(Some(no)) };
                return S_OK;
            }
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    pub fn drag(
        dir: &Path,
        files: &[PathBuf],
        extraction: Receiver<Result<(), Error>>,
        image: Option<&Image>,
        own: isize,
    ) -> Result<bool, Error> {
        let data: IDataObject = unsafe { SHCreateDataObject(None, None, None::<&IDataObject>) }
            .map_err(Error::internal)?;
        set_files(&data, files).map_err(Error::internal)?;
        if let Some(image) = image {
            // Without its picture the drag still works.
            let _ = set_image(&data, image);
        }
        let source = ComObject::new(Source {
            data: data.clone(),
            dir: dir.to_owned(),
            extraction,
            effect: Cell::new(DROPEFFECT_NONE),
            outcome: RefCell::new(None),
            own,
        });
        let mut effect = DROPEFFECT_NONE;
        unsafe {
            DoDragDrop(
                &data,
                &source.to_interface::<IDropSource>(),
                DROPEFFECT_COPY,
                &mut effect,
            )
        }
        .ok()
        .map_err(Error::internal)?;
        source.outcome.take().unwrap_or(Ok(false))
    }

    /// Lists `files` in the drag (as the file manager does).
    fn set_files(data: &IDataObject, files: &[PathBuf]) -> ::windows::core::Result<()> {
        let mem = crate::clipboard::hdrop(files)?;
        unsafe {
            let format = FORMATETC {
                cfFormat: CF_HDROP.0,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            };
            let medium = STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: mem },
                pUnkForRelease: ManuallyDrop::new(None),
            };
            // The data object owns the memory once it took it.
            data.SetData(&format, &medium, true).inspect_err(|_| {
                let _ = GlobalFree(Some(mem));
            })
        }
    }

    /// Gives the drag its picture; targets describe below it what a drop does ("Copy to
    /// Desktop").
    fn set_image(data: &IDataObject, image: &Image) -> ::windows::core::Result<()> {
        unsafe {
            let helper: IDragSourceHelper2 = CoCreateInstance(
                &CLSID_DragDropHelper,
                None::<&IUnknown>,
                CLSCTX_INPROC_SERVER,
            )?;
            helper.SetFlags(DSH_ALLOWDROPDESCRIPTIONTEXT.0 as u32)?;
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: image.width as i32,
                    // Top-down rows.
                    biHeight: -(image.height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
            // Premultiplied BGRA.
            let out = std::slice::from_raw_parts_mut(bits.cast::<u8>(), image.rgba.len());
            for (o, px) in out
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(image.rgba.as_chunks::<4>().0)
            {
                let a = u32::from(px[3]);
                o[0] = (u32::from(px[2]) * a / 255) as u8;
                o[1] = (u32::from(px[1]) * a / 255) as u8;
                o[2] = (u32::from(px[0]) * a / 255) as u8;
                o[3] = px[3];
            }
            let shdi = SHDRAGIMAGE {
                sizeDragImage: SIZE {
                    cx: image.width as i32,
                    cy: image.height as i32,
                },
                ptOffset: POINT {
                    x: image.x,
                    y: image.y,
                },
                hbmpDragImage: bitmap,
                // No color key: the alpha channel says what shows.
                crColorKey: COLORREF(0xFFFF_FFFF),
            };
            // The helper owns the bitmap once it took it.
            helper.InitializeFromBitmap(&shdi, data).inspect_err(|_| {
                let _ = DeleteObject(bitmap.into());
            })
        }
    }
}
