//! OmniZip's desktop entry and icon on Linux (`docs/07-shell-integration.md` §5).
//!
//! A Wayland compositor shows a window with the icon of the desktop entry named after the
//! window's app id, here the program's name (`omnizip-desktop`); the icon a window sets itself
//! reaches X11 only. A package installs `platform/linux/omnizip-desktop.desktop` and the icons
//! for everyone. A program run from elsewhere (a development build, an AppImage, a tarball)
//! installs them for the user at start-up, pointing at itself, and points them at it again when
//! it moves. A development build's entry stays out of the application menu; it claims the
//! archive types all the same, so that it can be made their default (`defaults`).

use std::{
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use omnizip_shell::servicemenu::exec_arg;

/// The app id, the entry's name and the icon's.
const NAME: &str = "omnizip-desktop";

const ENTRY: &str = include_str!("../../../../platform/linux/omnizip-desktop.desktop");

/// The icon in the sizes of the hicolor theme, and drawn.
const ICONS: &[(&str, &[u8])] = &[
    ("32x32", include_bytes!("../icons/32x32.png")),
    ("64x64", include_bytes!("../icons/64x64.png")),
    ("128x128", include_bytes!("../icons/128x128.png")),
    ("256x256", include_bytes!("../icons/128x128@2x.png")),
    ("512x512", include_bytes!("../icons/icon.png")),
];
const SVG: &[u8] = include_bytes!("../../../../icons/app-icon.svg");

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// `$XDG_DATA_HOME`, `~/.local/share` by default.
fn data_home() -> Option<PathBuf> {
    env_dir("XDG_DATA_HOME").or_else(|| env_dir("HOME").map(|h| h.join(".local/share")))
}

/// Whether a package installed the entry (`$XDG_DATA_DIRS`).
fn installed_by_package() -> bool {
    let dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.split(':').filter(|d| !d.is_empty()).any(|d| {
        Path::new(d)
            .join("applications")
            .join(format!("{NAME}.desktop"))
            .is_file()
    })
}

/// The program to start: the AppImage itself rather than the folder it is unpacked to, which
/// changes on every run.
fn program() -> Option<PathBuf> {
    env_dir("APPIMAGE").or_else(|| std::env::current_exe().ok())
}

/// The entry for the program at `exe`; `dev` for a development build.
fn entry(exe: &Path, dev: bool) -> String {
    let mut out = String::new();
    for line in ENTRY.lines() {
        if line.starts_with("Exec=") {
            out.push_str(&format!("Exec={} %F\n", exec_arg(&exe.to_string_lossy())));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if dev {
        out.push_str("NoDisplay=true\n");
    }
    out
}

/// Writes `data` to `path` unless it holds it already; whether it wrote.
fn write(path: &Path, data: &[u8]) -> io::Result<bool> {
    if std::fs::read(path).is_ok_and(|old| old == data) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, data)?;
    Ok(true)
}

/// Installs or updates the user's entry and icons, unless a package installed them.
pub fn ensure() {
    if installed_by_package() {
        return;
    }
    let (Some(data), Some(exe)) = (data_home(), program()) else {
        return;
    };
    let entry = entry(&exe, cfg!(debug_assertions));
    let mut files: Vec<(PathBuf, &[u8])> = vec![(
        data.join("applications").join(format!("{NAME}.desktop")),
        entry.as_bytes(),
    )];
    let icons = data.join("icons/hicolor");
    for (size, png) in ICONS {
        files.push((
            icons.join(size).join("apps").join(format!("{NAME}.png")),
            png,
        ));
    }
    files.push((icons.join("scalable/apps").join(format!("{NAME}.svg")), SVG));
    let mut changed = false;
    for (path, bytes) in files {
        match write(&path, bytes) {
            Ok(w) => changed |= w,
            Err(e) => tracing::warn!(path = %path.display(), "desktop entry: {e}"),
        }
    }
    // (Not from tests: their entries are not the user's.)
    if changed && !cfg!(test) {
        // Desktops read entries through caches: KDE's, and the MIME one for "Open with".
        for (tool, arg) in [
            ("kbuildsycoca6", None),
            ("update-desktop-database", Some(data.join("applications"))),
        ] {
            let mut cmd = Command::new(tool);
            cmd.args(arg)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Ok(mut child) = cmd.spawn() {
                let _ = child.wait();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_points_at_the_program() {
        let e = entry(Path::new("/opt/Omni Zip/omnizip-desktop"), false);
        assert!(
            e.contains("\nExec=\"/opt/Omni Zip/omnizip-desktop\" %F\n"),
            "{e}"
        );
        assert!(e.contains("\nIcon=omnizip-desktop\n"));
        assert!(e.contains("\nMimeType=application/zip;"));
        assert!(!e.contains("NoDisplay"));
        // A development build: no menu item, but the file types (it can be their default).
        let dev = entry(Path::new("/src/target/debug/omnizip-desktop"), true);
        assert!(dev.ends_with("NoDisplay=true\n") && dev.contains("\nMimeType=application/zip;"));
    }

    #[test]
    fn the_entry_claims_the_archives_the_menus_do() {
        let line = ENTRY.lines().find(|l| l.starts_with("MimeType=")).unwrap();
        let listed: Vec<&str> = line["MimeType=".len()..]
            .trim_end_matches(';')
            .split(';')
            .collect();
        assert_eq!(listed, omnizip_shell::servicemenu::ARCHIVE_MIME_TYPES);
    }
}
