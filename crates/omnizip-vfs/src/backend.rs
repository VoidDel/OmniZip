//! Platform backends (`docs/12-mount.md` §3) and their installation probe.
//!
//! A backend turns a [`VirtualFs`] into a drive letter, a `/Volumes` entry or a FUSE mount.
//! The backends themselves need a driver or SDK (WinFsp, libfuse3, FSKit) and live in
//! separate crates; this module holds what the UI, the CLI and the daemon share: the trait,
//! mount options, default mount points and [`probe`], which reports whether the system side
//! is installed.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use bitflags::bitflags;
use serde::Serialize;

use crate::{FsResult, VirtualFs, cache::CacheLimits, names::NameRules, names::display_name};

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

/// Backends compiled into this build. The WinFsp, FUSE and FSKit backends are planned
/// (`docs/11-roadmap.md`) and not part of this build yet.
pub const BUILT_BACKENDS: &[BackendKind] = &[];

/// Checks the system side of the platform's primary backend.
pub fn probe() -> Probe {
    let kind = BackendKind::current();
    let (health, detail) = probe_system();
    Probe {
        kind,
        health,
        detail,
        built: BUILT_BACKENDS.contains(&kind),
    }
}

#[cfg(windows)]
fn probe_system() -> (BackendHealth, Option<String>) {
    // WinFsp registers itself in the 32-bit view of HKLM.
    let dir = ["SOFTWARE\\WOW6432Node\\WinFsp", "SOFTWARE\\WinFsp"]
        .iter()
        .find_map(|k| {
            windows_registry::LOCAL_MACHINE
                .open(k)
                .and_then(|k| k.get_string("InstallDir"))
                .ok()
        });
    let Some(dir) = dir else {
        return (BackendHealth::NotInstalled, Some("WinFsp".into()));
    };
    let dll = if cfg!(target_arch = "aarch64") {
        "winfsp-a64.dll"
    } else {
        "winfsp-x64.dll"
    };
    let path = Path::new(&dir).join("bin").join(dll);
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

/// Options of one mount.
#[derive(Debug, Clone)]
pub struct MountOptions {
    /// Drive letter (`Z:`) or directory; chosen automatically when `None`.
    pub mount_point: Option<PathBuf>,
    pub label: Option<String>,
    /// Always `true` until the ZIP overlay lands (v1.x).
    pub read_only: bool,
    /// Mark files offline so Explorer does not read them for thumbnails.
    pub offline_attribute: bool,
    pub cache: CacheLimits,
    /// Decode everything into the cache right after mounting.
    pub warm: bool,
}

impl Default for MountOptions {
    fn default() -> Self {
        Self {
            mount_point: None,
            label: None,
            read_only: true,
            offline_attribute: false,
            cache: CacheLimits::default(),
            warm: false,
        }
    }
}

/// A live mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MountHandle {
    pub id: u64,
    pub archive: PathBuf,
    pub mount_point: PathBuf,
}

/// A platform backend.
pub trait MountBackend: Send + Sync {
    fn kind(&self) -> BackendKind;
    fn capabilities(&self) -> BackendCaps;
    fn health(&self) -> BackendHealth;
    fn mount(&self, fs: Arc<dyn VirtualFs>, opts: &MountOptions) -> FsResult<MountHandle>;
    fn unmount(&self, handle: &MountHandle, force: bool) -> FsResult<()>;
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
        let p = probe();
        assert_eq!(p.kind, BackendKind::current());
        // No backend is compiled in yet, so nothing can be mounted regardless of the driver.
        assert!(!p.built);
        assert!(!p.can_mount());
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
