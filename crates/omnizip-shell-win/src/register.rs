//! Per-user registration under `HKCU\Software\Classes` (no administrator rights needed).
//!
//! The same COM class serves the classic context menu (through the `ExplorerCommandHandler`
//! value of a static verb, Windows 7 and later) and the Windows 11 menu (through the package
//! manifest of the MSIX / sparse package, `platform/windows/msix/AppxManifest.xml`).

use std::path::Path;

use windows::{
    Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify},
    core::GUID,
};
use windows_registry::CURRENT_USER;

use crate::{
    com::clsid_for,
    menu::{MenuConfig, Verb},
};

/// Root of per-user class registrations.
pub const CLASSES: &str = r"Software\Classes";
/// File-system objects (files and folders) share this ProgID.
const TARGET: &str = r"AllFilesystemObjects\shell";
const VERB_KEY: &str = "OmniZip";
/// Drives: "Eject" on the drives OmniZip mounted (hidden on the others), like a mounted ISO.
const DRIVE_VERB: &str = r"Drive\shell\OmniZip.unmount";

/// `{6F1E7A52-3C4B-4D1E-9A7F-0B2C3D4E5F00}`
pub fn guid_string(g: GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7]
    )
}

fn err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

fn register_class(base: &str, clsid: GUID, dll: &Path, name: &str) -> std::io::Result<()> {
    let key = CURRENT_USER
        .create(format!(r"{base}\CLSID\{}", guid_string(clsid)))
        .map_err(err)?;
    key.set_string("", name).map_err(err)?;
    let server = key.create("InprocServer32").map_err(err)?;
    server.set_string("", dll.to_string_lossy()).map_err(err)?;
    server
        .set_string("ThreadingModel", "Apartment")
        .map_err(err)?;
    Ok(())
}

/// Registers below `base` (tests use a scratch key instead of `Software\Classes`).
pub fn register_at(base: &str, dll: &Path, cfg: &MenuConfig) -> std::io::Result<()> {
    unregister_at(base)?;
    register_class(base, clsid_for(None), dll, "OmniZip context menu")?;
    for v in Verb::ALL {
        register_class(
            base,
            clsid_for(Some(v)),
            dll,
            &format!("OmniZip {}", v.cli()),
        )?;
    }
    if cfg.flat() {
        for v in Verb::ALL
            .into_iter()
            .filter(|v| v.in_submenu() && cfg.enabled(*v))
        {
            let key = CURRENT_USER
                .create(format!(r"{base}\{TARGET}\{VERB_KEY}.{}", v.key()))
                .map_err(err)?;
            key.set_string("ExplorerCommandHandler", guid_string(clsid_for(Some(v))))
                .map_err(err)?;
        }
    } else {
        let key = CURRENT_USER
            .create(format!(r"{base}\{TARGET}\{VERB_KEY}"))
            .map_err(err)?;
        key.set_string("ExplorerCommandHandler", guid_string(clsid_for(None)))
            .map_err(err)?;
        key.set_string("MUIVerb", "OmniZip").map_err(err)?;
    }
    register_drive_at(base, dll, cfg)
}

/// "Eject" on the drives OmniZip mounted, in the registry whichever menu is in use: the
/// context-menu package cannot reach drives (its manifest takes file types and folders only,
/// `desktop5:ItemType`), so with the package this is all that is registered here.
pub fn register_drive_at(base: &str, dll: &Path, cfg: &MenuConfig) -> std::io::Result<()> {
    let _ = CURRENT_USER.remove_tree(format!(r"{base}\{DRIVE_VERB}"));
    if !cfg.enabled(Verb::Unmount) {
        return Ok(());
    }
    let clsid = clsid_for(Some(Verb::Unmount));
    register_class(base, clsid, dll, "OmniZip unmount")?;
    let key = CURRENT_USER
        .create(format!(r"{base}\{DRIVE_VERB}"))
        .map_err(err)?;
    key.set_string("ExplorerCommandHandler", guid_string(clsid))
        .map_err(err)?;
    Ok(())
}

/// [`register_drive_at`] for the current user.
pub fn register_drive(dll: &Path, cfg: &MenuConfig) -> std::io::Result<()> {
    register_drive_at(CLASSES, dll, cfg)
}

/// Removes everything [`register_at`] created.
pub fn unregister_at(base: &str) -> std::io::Result<()> {
    let _ = CURRENT_USER.remove_tree(format!(r"{base}\CLSID\{}", guid_string(clsid_for(None))));
    for v in Verb::ALL {
        let _ =
            CURRENT_USER.remove_tree(format!(r"{base}\CLSID\{}", guid_string(clsid_for(Some(v)))));
        let _ = CURRENT_USER.remove_tree(format!(r"{base}\{TARGET}\{VERB_KEY}.{}", v.key()));
    }
    let _ = CURRENT_USER.remove_tree(format!(r"{base}\{TARGET}\{VERB_KEY}"));
    let _ = CURRENT_USER.remove_tree(format!(r"{base}\{DRIVE_VERB}"));
    Ok(())
}

fn notify() {
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

/// Registers the context menu for the current user.
pub fn register(dll: &Path, cfg: &MenuConfig) -> std::io::Result<()> {
    register_at(CLASSES, dll, cfg)?;
    notify();
    Ok(())
}

/// Removes the context menu for the current user.
pub fn unregister() -> std::io::Result<()> {
    unregister_at(CLASSES)?;
    notify();
    Ok(())
}

/// Registration state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub registered: bool,
    /// DLL path recorded in the registry.
    pub dll: Option<String>,
}

pub fn status_at(base: &str) -> Status {
    let dll = CURRENT_USER
        .open(format!(
            r"{base}\CLSID\{}\InprocServer32",
            guid_string(clsid_for(None))
        ))
        .and_then(|k| k.get_string(""))
        .ok();
    let verb = CURRENT_USER
        .open(format!(r"{base}\{TARGET}\{VERB_KEY}"))
        .is_ok()
        || Verb::ALL.iter().any(|v| {
            CURRENT_USER
                .open(format!(r"{base}\{TARGET}\{VERB_KEY}.{}", v.key()))
                .is_ok()
        });
    Status {
        registered: dll.is_some() && verb,
        dll,
    }
}

/// Registration state for the current user.
pub fn status() -> Status {
    status_at(CLASSES)
}
