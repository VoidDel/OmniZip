//! Platform backends (`docs/12-mount.md` §3) and their installation probe.
//!
//! A backend turns a [`VirtualFs`] into a drive letter, a `/Volumes` entry or a FUSE mount.
//! The backends themselves need a driver or SDK (WinFsp, libfuse3, FSKit) and live in
//! separate crates; this module holds what the UI, the CLI and the daemon share: the trait,
//! mount options, default mount points and [`probe`], which reports whether the system side
//! is installed.

use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use bitflags::bitflags;
use serde::Serialize;

use crate::{VirtualFs, names::NameRules, names::display_name};

bitflags! {
    /// What a backend can do.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct BackendCaps: u32 {
        const DRIVE_LETTER = 1;
        const DIR_MOUNTPOINT = 1 << 1;
        const WRITABLE = 1 << 2;
        const SYMLINKS = 1 << 3;
        const CASE_SENSITIVE = 1 << 4;
        const XATTR = 1 << 5;
    }
}

/// Backend implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    WinFsp,
    Fuse,
    FsKit,
    NfsLoopback,
}

impl BackendKind {
    /// The primary backend of the running system.
    pub const fn current() -> Self {
        if cfg!(windows) {
            BackendKind::WinFsp
        } else if cfg!(target_os = "macos") {
            BackendKind::FsKit
        } else {
            BackendKind::Fuse
        }
    }
}

/// State of the system side of a backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendHealth {
    Ready,
    /// The driver or helper is missing (WinFsp, `fusermount3`).
    NotInstalled,
    /// Installed but switched off (FSKit extension in System Settings).
    NeedsEnable,
    VersionMismatch,
    /// Not available in this environment (Flatpak sandbox, unsupported OS).
    Unsupported,
}

/// Result of [`probe`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    pub kind: BackendKind,
    pub health: BackendHealth,
    /// What was found (driver path) or what is missing.
    pub detail: Option<String>,
    /// This build contains the backend. Mounting needs `built` and a ready system side.
    pub built: bool,
}

impl Probe {
    /// Whether a mount can be attempted.
    pub fn can_mount(&self) -> bool {
        self.built && self.health == BackendHealth::Ready
    }
}

/// Checks the system side of the platform's primary backend. `built` says whether the caller
/// links that backend (each backend lives in its own crate).
pub fn probe(built: bool) -> Probe {
    let kind = BackendKind::current();
    let (health, detail) = probe_system();
    Probe {
        kind,
        health,
        detail,
        built,
    }
}

/// The WinFsp library for this architecture, as registered by the WinFsp installer (in the
/// 32-bit view of HKLM). `None` when WinFsp is not installed.
#[cfg(windows)]
pub fn winfsp_dll() -> Option<PathBuf> {
    let dir = ["SOFTWARE\\WOW6432Node\\WinFsp", "SOFTWARE\\WinFsp"]
        .iter()
        .find_map(|k| {
            windows_registry::LOCAL_MACHINE
                .open(k)
                .and_then(|k| k.get_string("InstallDir"))
                .ok()
        })?;
    let dll = if cfg!(target_arch = "aarch64") {
        "winfsp-a64.dll"
    } else {
        "winfsp-x64.dll"
    };
    Some(Path::new(&dir).join("bin").join(dll))
}

/// A DWORD setting of WinFsp (`HKLM\SOFTWARE\WinFsp`, 32-bit view first), such as
/// `MountUseMountmgrFromFSD`.
#[cfg(windows)]
pub fn winfsp_setting(name: &str) -> Option<u32> {
    ["SOFTWARE\\WOW6432Node\\WinFsp", "SOFTWARE\\WinFsp"]
        .iter()
        .find_map(|k| {
            windows_registry::LOCAL_MACHINE
                .open(k)
                .and_then(|k| k.get_u32(name))
                .ok()
        })
}

#[cfg(windows)]
fn probe_system() -> (BackendHealth, Option<String>) {
    let Some(path) = winfsp_dll() else {
        return (BackendHealth::NotInstalled, Some("WinFsp".into()));
    };
    if path.is_file() {
        (BackendHealth::Ready, Some(path.display().to_string()))
    } else {
        (
            BackendHealth::NotInstalled,
            Some(path.display().to_string()),
        )
    }
}

#[cfg(target_os = "macos")]
fn probe_system() -> (BackendHealth, Option<String>) {
    // FSKit availability is decided by the M0 test (docs/12-mount.md §12).
    (
        BackendHealth::Unsupported,
        Some("FSKit backend pending M0 evaluation".into()),
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn probe_system() -> (BackendHealth, Option<String>) {
    if std::env::var_os("FLATPAK_ID").is_some() {
        return (BackendHealth::Unsupported, Some("flatpak".into()));
    }
    if !Path::new("/dev/fuse").exists() {
        return (BackendHealth::NotInstalled, Some("/dev/fuse".into()));
    }
    let helper = std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join("fusermount3"))
            .find(|f| f.is_file())
    });
    match helper {
        Some(h) => (BackendHealth::Ready, Some(h.display().to_string())),
        None => (BackendHealth::NotInstalled, Some("fusermount3".into())),
    }
}

#[cfg(not(any(windows, unix)))]
fn probe_system() -> (BackendHealth, Option<String>) {
    (BackendHealth::Unsupported, None)
}

/// Options of one mount. Cache budgets and warm-up belong to the file system
/// ([`crate::FsOptions`]), not to the backend.
#[derive(Debug, Clone)]
pub struct MountOptions {
    /// Drive letter (`Z:`) or directory; chosen automatically when `None`. On Windows a
    /// directory must not exist or be empty; an empty one is put back after unmounting.
    pub mount_point: Option<PathBuf>,
    /// Volume label (the file system's own label when `None`).
    pub label: Option<String>,
    /// Always `true` until the ZIP overlay lands (v1.x).
    pub read_only: bool,
    /// Mark files offline so Explorer does not read them for thumbnails.
    pub offline_attribute: bool,
}

impl Default for MountOptions {
    fn default() -> Self {
        Self {
            mount_point: None,
            label: None,
            read_only: true,
            offline_attribute: false,
        }
    }
}

/// Why a mount failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountError {
    /// The driver or its library is missing or unusable.
    NotInstalled(String),
    /// The drive letter is taken, or the directory is not empty.
    InUse(PathBuf),
    /// Every drive letter is taken.
    NoDriveLetter,
    /// The system refused for another reason.
    System(String),
}

impl fmt::Display for MountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MountError::NotInstalled(what) => write!(f, "mount backend not available: {what}"),
            MountError::InUse(p) => write!(f, "mount point in use: {}", p.display()),
            MountError::NoDriveLetter => f.write_str("no free drive letter"),
            MountError::System(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for MountError {}

/// A live mount. Dropping it takes the volume down; files still open in it fail afterwards.
pub trait Mount: Send + Sync {
    /// Drive letter (`Z:`) or directory.
    fn mount_point(&self) -> &Path;
    /// `false` once the system has taken the volume down on its own (for example because the
    /// driver was uninstalled).
    fn is_alive(&self) -> bool;
    /// A drive visible to every session (on Windows, one registered with the Mount Manager).
    fn global(&self) -> bool {
        false
    }
}

/// A platform backend.
pub trait MountBackend: Send + Sync {
    fn kind(&self) -> BackendKind;
    fn capabilities(&self) -> BackendCaps;
    fn health(&self) -> BackendHealth;
    fn mount(
        &self,
        fs: Arc<dyn VirtualFs>,
        opts: &MountOptions,
    ) -> Result<Box<dyn Mount>, MountError>;
}

/// First free drive letter from `Z:` downwards, skipping `A:` to `C:`.
pub fn pick_drive_letter(in_use: impl Fn(char) -> bool) -> Option<char> {
    ('D'..='Z').rev().find(|&c| !in_use(c))
}

/// Default mount point for a volume labelled `label` on the running system.
pub fn default_mount_point(label: &str) -> Option<PathBuf> {
    let name = display_name(label, NameRules::current());
    if cfg!(windows) {
        pick_drive_letter(|c| Path::new(&format!("{c}:\\")).exists())
            .map(|c| PathBuf::from(format!("{c}:")))
    } else if cfg!(target_os = "macos") {
        Some(Path::new("/Volumes").join(name))
    } else {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
        Some(runtime.join("omnizip").join("mnt").join(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_letters_count_down_from_z() {
        assert_eq!(pick_drive_letter(|_| false), Some('Z'));
        assert_eq!(pick_drive_letter(|c| c >= 'X'), Some('W'));
        assert_eq!(pick_drive_letter(|_| true), None);
        assert_eq!(pick_drive_letter(|c| c != 'D'), Some('D'));
    }

    #[test]
    fn probe_reports_the_platform_backend() {
        let p = probe(false);
        assert_eq!(p.kind, BackendKind::current());
        // Without the backend linked in, nothing can be mounted regardless of the driver.
        assert!(!p.built);
        assert!(!p.can_mount());
        let p = probe(true);
        assert_eq!(p.can_mount(), p.health == BackendHealth::Ready);
    }

    #[test]
    fn default_mount_point_is_platform_shaped() {
        if let Some(p) = default_mount_point("photos:2026") {
            let s = p.display().to_string();
            if cfg!(windows) {
                assert!(s.len() == 2 && s.ends_with(':'), "{s}");
            } else {
                assert!(s.ends_with("photos:2026"), "{s}");
            }
        }
    }
}
