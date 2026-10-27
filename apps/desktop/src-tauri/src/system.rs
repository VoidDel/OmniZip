//! Facts about the host system that the UI needs.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    /// `win`, `mac` or `linux`.
    pub platform: &'static str,
    /// System accent color (`#RRGGBB`) when the OS exposes one.
    pub accent_color: Option<String>,
    /// Window materials (Mica / vibrancy) are available.
    pub material_supported: bool,
    /// BCP 47 UI locale.
    pub locale: String,
    pub home_dir: Option<String>,
    pub downloads_dir: Option<String>,
    pub desktop_dir: Option<String>,
    pub path_separator: &'static str,
    pub app_version: &'static str,
}

pub const fn platform() -> &'static str {
    if cfg!(windows) {
        "win"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

/// Windows build number (`22000` and later is Windows 11).
#[cfg(windows)]
pub fn windows_build() -> u32 {
    windows_registry::LOCAL_MACHINE
        .open(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
        .and_then(|k| k.get_string("CurrentBuildNumber"))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

#[cfg(windows)]
pub fn accent_color() -> Option<String> {
    // DWM stores the accent as 0xAABBGGRR.
    let v = windows_registry::CURRENT_USER
        .open(r"Software\Microsoft\Windows\DWM")
        .and_then(|k| k.get_u32("AccentColor"))
        .ok()?;
    Some(format!(
        "#{:02X}{:02X}{:02X}",
        v & 0xFF,
        (v >> 8) & 0xFF,
        (v >> 16) & 0xFF
    ))
}

#[cfg(not(windows))]
pub fn accent_color() -> Option<String> {
    None
}

/// Whether apps should use dark colors, per the OS setting (`None` when unknown).
#[cfg(windows)]
pub fn prefers_dark() -> Option<bool> {
    windows_registry::CURRENT_USER
        .open(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_u32("AppsUseLightTheme"))
        .ok()
        .map(|v| v == 0)
}

#[cfg(not(windows))]
pub fn prefers_dark() -> Option<bool> {
    None
}

pub fn material_supported() -> bool {
    #[cfg(windows)]
    {
        windows_build() >= 22000
    }
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        false
    }
}

fn user_dir(kind: &str) -> Option<String> {
    let dirs = directories::UserDirs::new()?;
    let p = match kind {
        "home" => Some(dirs.home_dir().to_path_buf()),
        "downloads" => dirs.download_dir().map(|p| p.to_path_buf()),
        "desktop" => dirs.desktop_dir().map(|p| p.to_path_buf()),
        _ => None,
    }?;
    Some(p.to_string_lossy().into_owned())
}

pub fn info() -> SystemInfo {
    SystemInfo {
        platform: platform(),
        accent_color: accent_color(),
        material_supported: material_supported(),
        locale: sys_locale().unwrap_or_else(|| "en-US".to_owned()),
        home_dir: user_dir("home"),
        downloads_dir: user_dir("downloads"),
        desktop_dir: user_dir("desktop"),
        path_separator: if cfg!(windows) { "\\" } else { "/" },
        app_version: env!("CARGO_PKG_VERSION"),
    }
}

fn sys_locale() -> Option<String> {
    let lang = omnizip_i18n::detect_language(None);
    Some(lang.to_owned())
}
