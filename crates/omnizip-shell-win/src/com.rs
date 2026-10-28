//! COM server loaded into `explorer.exe` (`docs/07-shell-integration.md` §3.2).
//!
//! Hard rules for code that runs inside Explorer: no UI, no network, no archive parsing, no
//! blocking work beyond a few small file reads, and no panic may unwind across the COM boundary.

#![allow(non_snake_case)]

use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicIsize, AtomicUsize, Ordering},
    },
};

use omnizip_i18n::Localizer;
use windows::{
    Win32::{
        Foundation::{
            CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_NOTIMPL, E_POINTER,
            HINSTANCE, S_FALSE, S_OK,
        },
        System::{
            Com::{CoTaskMemFree, IBindCtx, IClassFactory, IClassFactory_Impl},
            LibraryLoader::GetModuleFileNameW,
            SystemServices::DLL_PROCESS_ATTACH,
        },
        UI::Shell::{
            ECF_DEFAULT, ECF_HASSUBCOMMANDS, ECS_ENABLED, ECS_HIDDEN, IEnumExplorerCommand,
            IEnumExplorerCommand_Impl, IExplorerCommand, IExplorerCommand_Impl, IShellItemArray,
            SHStrDupW, SIGDN_FILESYSPATH,
        },
    },
    core::{
        BOOL, GUID, HRESULT, HSTRING, IUnknown, Interface, PCWSTR, PWSTR, Ref, Result, implement,
    },
};

use crate::menu::{self, MAX_ITEMS, MenuConfig, Verb};

/// Class id of the root command (the "OmniZip" submenu).
pub const CLSID_ROOT: GUID = GUID::from_u128(0x6f1e_7a52_3c4b_4d1e_9a7f_0b2c_3d4e_5f00);

/// Class id of one verb, used for the flat menu layout.
pub fn clsid_for(verb: Option<Verb>) -> GUID {
    match verb {
        None => CLSID_ROOT,
        Some(v) => GUID::from_u128(CLSID_ROOT.to_u128() + 1 + u128::from(v.index())),
    }
}

fn verb_for(clsid: &GUID) -> Option<Option<Verb>> {
    if *clsid == CLSID_ROOT {
        return Some(None);
    }
    let delta = clsid.to_u128().checked_sub(CLSID_ROOT.to_u128() + 1)?;
    u8::try_from(delta)
        .ok()
        .and_then(Verb::from_index)
        .map(Some)
}

static MODULE: AtomicIsize = AtomicIsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static LOCKS: AtomicUsize = AtomicUsize::new(0);

/// Path of this DLL.
pub fn module_path() -> Option<PathBuf> {
    let h = MODULE.load(Ordering::Relaxed);
    let mut buf = [0u16; 1024];
    let module = (h != 0).then_some(windows::Win32::Foundation::HMODULE(h as *mut c_void));
    let n = unsafe { GetModuleFileNameW(module, &mut buf) } as usize;
    (n > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

/// Whether apps (and so the context menu) use the dark theme.
fn dark_theme() -> bool {
    windows_registry::CURRENT_USER
        .open(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_u32("AppsUseLightTheme"))
        .is_ok_and(|light| light == 0)
}

/// The icon of a menu item for the current theme (`menu-icons\<verb>-light.ico` / `-dark.ico`
/// next to the app, `scripts/gen-menu-icons.mjs`).
fn menu_icon(verb: Verb) -> Option<PathBuf> {
    let dir = module_path()?.parent()?.to_path_buf();
    let theme = if dark_theme() { "dark" } else { "light" };
    let name = format!("{}-{theme}.ico", verb.cli());
    // Installed layout: the DLL may sit in a versioned `shell\<ver>` folder below the app.
    [
        dir.join("menu-icons"),
        dir.join("..").join("..").join("menu-icons"),
    ]
    .into_iter()
    .map(|d| d.join(&name))
    .find(|p| p.is_file())
}

/// `omnizip-desktop.exe` next to this DLL (or `OMNIZIP_DESKTOP_EXE` for development).
pub fn desktop_exe() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OMNIZIP_DESKTOP_EXE") {
        return Some(PathBuf::from(p));
    }
    let dir = module_path()?.parent()?.to_path_buf();
    // Installed layout: the DLL sits in a versioned `shell\<ver>` folder below the app.
    [
        dir.join("omnizip-desktop.exe"),
        dir.join("..").join("..").join("omnizip-desktop.exe"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// Runs `f`, turning panics into `E_FAIL`.
fn guard<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Err(E_FAIL.into()))
}

fn dup(s: &str) -> Result<PWSTR> {
    let h = HSTRING::from(s);
    unsafe { SHStrDupW(PCWSTR(h.as_ptr())) }
}

fn selected_paths(items: Option<&IShellItemArray>) -> Vec<PathBuf> {
    let Some(arr) = items else { return Vec::new() };
    let n = unsafe { arr.GetCount() }
        .unwrap_or(0)
        .min(MAX_ITEMS as u32 + 1);
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let Ok(item) = (unsafe { arr.GetItemAt(i) }) else {
            continue;
        };
        let Ok(name) = (unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }) else {
            continue;
        };
        let path = unsafe { name.to_string() }.unwrap_or_default();
        unsafe { CoTaskMemFree(Some(name.0 as *const c_void)) };
        if !path.is_empty() {
            out.push(PathBuf::from(path));
        }
    }
    out
}

/// One menu command: the root submenu (`verb == None`) or a verb.
#[implement(IExplorerCommand)]
pub struct Command {
    verb: Option<Verb>,
    cfg: MenuConfig,
    l: Localizer,
}

impl Command {
    pub fn new(verb: Option<Verb>, cfg: MenuConfig) -> Self {
        LIVE.fetch_add(1, Ordering::Relaxed);
        let l = cfg.localizer();
        Self { verb, cfg, l }
    }

    fn visible_verbs(&self) -> Vec<Verb> {
        Verb::ALL
            .into_iter()
            .filter(|v| self.cfg.enabled(*v))
            .collect()
    }
}

impl Drop for Command {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

impl IExplorerCommand_Impl for Command_Impl {
    fn GetTitle(&self, items: Ref<IShellItemArray>) -> Result<PWSTR> {
        guard(|| match self.verb {
            None => dup(&self.l.get("menu-root")),
            Some(v) => {
                let sel = menu::classify(&selected_paths(items.as_ref()));
                dup(&menu::title(v, &sel, &self.l))
            }
        })
    }

    fn GetIcon(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        guard(|| {
            if !self.cfg.shell.icons {
                return Err(E_NOTIMPL.into());
            }
            // The submenu and "Open with OmniZip" carry the app icon, the other items their own.
            let icon = match self.verb {
                None | Some(Verb::Open) => desktop_exe(),
                Some(v) => menu_icon(v),
            };
            let icon = icon.ok_or_else(|| windows::core::Error::from(E_NOTIMPL))?;
            dup(&format!("{},0", icon.display()))
        })
    }

    fn GetToolTip(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        guard(|| dup(&self.l.get("menu-tooltip")))
    }

    fn GetCanonicalName(&self) -> Result<GUID> {
        Ok(clsid_for(self.verb))
    }

    fn GetState(&self, items: Ref<IShellItemArray>, _ok_to_be_slow: BOOL) -> Result<u32> {
        guard(|| {
            let sel = menu::classify(&selected_paths(items.as_ref()));
            let show = match self.verb {
                None => self.visible_verbs().iter().any(|v| menu::applies(*v, &sel)),
                Some(v) => self.cfg.enabled(v) && menu::applies(v, &sel),
            };
            Ok(if show {
                ECS_ENABLED.0 as u32
            } else {
                ECS_HIDDEN.0 as u32
            })
        })
    }

    fn Invoke(&self, items: Ref<IShellItemArray>, _bind: Ref<IBindCtx>) -> Result<()> {
        guard(|| {
            let Some(verb) = self.verb else { return Ok(()) };
            let paths = selected_paths(items.as_ref());
            if paths.is_empty() {
                return Ok(());
            }
            let exe = desktop_exe().ok_or_else(|| windows::core::Error::from(E_FAIL))?;
            let list = menu::write_list(&paths).map_err(|_| windows::core::Error::from(E_FAIL))?;
            let cwd = paths[0].parent().map(PathBuf::from);
            std::process::Command::new(exe)
                .args(menu::arguments(verb, &list, cwd.as_deref()))
                .spawn()
                .map(|_| ())
                .map_err(|_| E_FAIL.into())
        })
    }

    fn GetFlags(&self) -> Result<u32> {
        Ok(match self.verb {
            None => ECF_HASSUBCOMMANDS.0 as u32,
            Some(_) => ECF_DEFAULT.0 as u32,
        })
    }

    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        guard(|| match self.verb {
            None => Ok(Commands::new(self.visible_verbs(), self.cfg.clone(), 0).into()),
            Some(_) => Err(E_NOTIMPL.into()),
        })
    }
}

/// Enumerates the verbs of the submenu.
#[implement(IEnumExplorerCommand)]
pub struct Commands {
    verbs: Vec<Verb>,
    cfg: MenuConfig,
    pos: AtomicUsize,
}

impl Commands {
    fn new(verbs: Vec<Verb>, cfg: MenuConfig, pos: usize) -> Self {
        LIVE.fetch_add(1, Ordering::Relaxed);
        Self {
            verbs,
            cfg,
            pos: AtomicUsize::new(pos),
        }
    }
}

impl Drop for Commands {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

// The signatures come from the windows crate and cannot be `unsafe`. The COM contract makes
// the caller (Explorer) responsible for valid pointers; null pointers are checked first.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
impl IEnumExplorerCommand_Impl for Commands_Impl {
    fn Next(&self, celt: u32, out: *mut Option<IExplorerCommand>, fetched: *mut u32) -> HRESULT {
        if out.is_null() {
            return E_POINTER;
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut n = 0u32;
            while n < celt {
                let i = self.pos.load(Ordering::Relaxed);
                let Some(&verb) = self.verbs.get(i) else {
                    break;
                };
                self.pos.store(i + 1, Ordering::Relaxed);
                let cmd: IExplorerCommand = Command::new(Some(verb), self.cfg.clone()).into();
                unsafe { out.add(n as usize).write(Some(cmd)) };
                n += 1;
            }
            n
        }));
        let Ok(n) = result else { return E_FAIL };
        if !fetched.is_null() {
            unsafe { fetched.write(n) };
        }
        if n == celt { S_OK } else { S_FALSE }
    }

    fn Skip(&self, celt: u32) -> Result<()> {
        self.pos.fetch_add(celt as usize, Ordering::Relaxed);
        Ok(())
    }

    fn Reset(&self) -> Result<()> {
        self.pos.store(0, Ordering::Relaxed);
        Ok(())
    }

    fn Clone(&self) -> Result<IEnumExplorerCommand> {
        Ok(Commands::new(
            self.verbs.clone(),
            self.cfg.clone(),
            self.pos.load(Ordering::Relaxed),
        )
        .into())
    }
}

/// Creates commands for one class id.
#[implement(IClassFactory)]
pub struct Factory {
    verb: Option<Verb>,
}

// The signatures come from the windows crate and cannot be `unsafe`. The COM contract makes
// the caller (Explorer) responsible for valid pointers; null pointers are checked first.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> Result<()> {
        if ppv.is_null() || riid.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { ppv.write(std::ptr::null_mut()) };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        guard(|| {
            // Settings are re-read for every menu, so changes apply without restarting Explorer.
            let unknown: IUnknown = Command::new(self.verb, MenuConfig::load()).into();
            unsafe { unknown.query(riid, ppv) }.ok()
        })
    }

    fn LockServer(&self, lock: BOOL) -> Result<()> {
        if lock.as_bool() {
            LOCKS.fetch_add(1, Ordering::Relaxed);
        } else {
            LOCKS.fetch_sub(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

/// Number of live COM objects (for tests).
pub fn live_objects() -> usize {
    LIVE.load(Ordering::Relaxed)
}

static INIT: OnceLock<()> = OnceLock::new();

#[unsafe(no_mangle)]
extern "system" fn DllMain(instance: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        MODULE.store(instance.0 as isize, Ordering::Relaxed);
        INIT.get_or_init(|| ());
    }
    BOOL(1)
}

/// # Safety
/// Called by COM with valid pointers.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    clsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if clsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    unsafe { ppv.write(std::ptr::null_mut()) };
    let Some(verb) = verb_for(unsafe { &*clsid }) else {
        return CLASS_E_CLASSNOTAVAILABLE;
    };
    let factory: IClassFactory = Factory { verb }.into();
    unsafe { factory.query(riid, ppv) }
}

#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if LIVE.load(Ordering::Relaxed) == 0 && LOCKS.load(Ordering::Relaxed) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

/// `regsvr32 omnizip_shell.dll`: per-user registration with the current menu settings.
#[unsafe(no_mangle)]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    let Some(dll) = module_path() else {
        return E_FAIL;
    };
    match catch_unwind(|| crate::register::register(&dll, &MenuConfig::load())) {
        Ok(Ok(())) => S_OK,
        _ => E_FAIL,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    match catch_unwind(crate::register::unregister) {
        Ok(Ok(())) => S_OK,
        _ => E_FAIL,
    }
}
