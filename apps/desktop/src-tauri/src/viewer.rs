//! Who previews an entry when Space is pressed (Settings › General › Preview with): the built-in
//! preview, Omni Look, another preview app (PowerToys Peek, QuickLook, Seer), a command of the
//! user's, or the system's program for the file. Everything but the built-in preview gets the entry
//! as a file in the cache.
//!
//! Omni Look is found through `HKCU\Software\OmniLook` (`Exe`, `ProtocolVersion` ≥ 1) and called as
//! `omnilook.exe preview <file> --title "<archive> › <path>"`; the same file again closes its
//! preview (OmniLook `docs/development-plan.md` §6).

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::Serialize;

/// The preview apps found on this computer (their executables), for the settings page.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Viewers {
    pub omnilook: Option<PathBuf>,
    pub peek: Option<PathBuf>,
    pub quicklook: Option<PathBuf>,
    pub seer: Option<PathBuf>,
}

/// What a preview resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Viewer {
    Builtin,
    OmniLook(PathBuf),
    /// Another preview app, given the file as its only argument.
    App(PathBuf),
    /// The user's command line; `{file}` stands for the file.
    Command(String),
    /// The system's program for the file.
    System,
}

/// The lowest Omni Look call protocol OmniZip speaks (`preview <file> --title …`).
#[cfg(windows)]
const OMNILOOK_PROTOCOL: u32 = 1;

#[cfg(windows)]
fn omnilook() -> Option<PathBuf> {
    let key = windows_registry::CURRENT_USER
        .open(r"Software\OmniLook")
        .ok()?;
    if key.get_u32("ProtocolVersion").ok()? < OMNILOOK_PROTOCOL {
        return None;
    }
    let exe = PathBuf::from(key.get_string("Exe").ok()?);
    exe.is_file().then_some(exe)
}

/// The first of `candidates` that exists.
#[cfg(windows)]
fn first(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|p| p.is_file())
}

/// Where the installers put the apps: per user (`%LOCALAPPDATA%`) or for everyone.
#[cfg(windows)]
fn installed(local: &[&str], machine: &[&str]) -> Option<PathBuf> {
    let local_root = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let machine_roots: Vec<PathBuf> = ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .collect();
    let mut all = Vec::new();
    if let Some(root) = &local_root {
        all.extend(local.iter().map(|p| root.join(p)));
    }
    for root in &machine_roots {
        all.extend(machine.iter().map(|p| root.join(p)));
    }
    first(all)
}

#[cfg(windows)]
pub fn detect() -> Viewers {
    Viewers {
        omnilook: omnilook(),
        // PowerToys installs Peek beside its WinUI 3 apps; `PowerToys.Peek.UI.exe <file>`.
        peek: installed(
            &[r"PowerToys\WinUI3Apps\PowerToys.Peek.UI.exe"],
            &[r"PowerToys\WinUI3Apps\PowerToys.Peek.UI.exe"],
        ),
        quicklook: installed(
            &[r"Programs\QuickLook\QuickLook.exe"],
            &[r"QuickLook\QuickLook.exe"],
        ),
        seer: installed(&[r"Programs\Seer\Seer.exe"], &[r"Seer\Seer.exe"]),
    }
}

#[cfg(not(windows))]
pub fn detect() -> Viewers {
    Viewers::default()
}

/// The viewer for the setting (`auto`, `builtin`, `omnilook`, `peek`, `quicklook`, `seer`,
/// `custom` or `system`). An app that is not installed (any more) falls back to the built-in
/// preview, as does an empty command; `auto` is Omni Look when it is installed.
pub fn resolve(setting: &str, command: &str, found: &Viewers) -> Viewer {
    let app = |p: &Option<PathBuf>| p.clone().map_or(Viewer::Builtin, Viewer::App);
    match setting {
        "auto" | "omnilook" => found
            .omnilook
            .clone()
            .map_or(Viewer::Builtin, Viewer::OmniLook),
        "peek" => app(&found.peek),
        "quicklook" => app(&found.quicklook),
        "seer" => app(&found.seer),
        "custom" if !command.trim().is_empty() => Viewer::Command(command.trim().to_owned()),
        "system" => Viewer::System,
        _ => Viewer::Builtin,
    }
}

/// Splits a command line into words: spaces separate them, double quotes group them.
fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut any) = (false, false);
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// The program and arguments of the user's command for `file`: `{file}` is replaced wherever it
/// appears; without one, the file is the last argument.
pub fn command_line(line: &str, file: &Path) -> Option<(String, Vec<String>)> {
    let file = file.to_string_lossy();
    let mut w = words(line);
    if w.is_empty() {
        return None;
    }
    if w.iter().any(|a| a.contains("{file}")) {
        for a in &mut w {
            *a = a.replace("{file}", &file);
        }
    } else {
        w.push(file.into_owned());
    }
    let program = w.remove(0);
    Some((program, w))
}

fn spawn(program: impl AsRef<std::ffi::OsStr>, args: &[&std::ffi::OsStr]) -> std::io::Result<()> {
    let mut c = Command::new(program);
    c.args(args)
        // Nothing is inherited: a viewer that stays running must not hold OmniZip's handles.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: a console program shows no console.
        c.creation_flags(0x0800_0000);
    }
    // Not waited for: the preview apps return at once or keep running; either is fine.
    c.spawn().map(drop)
}

/// Shows `file` with `viewer`; `title` is what Omni Look displays instead of the cache path.
pub fn launch(viewer: &Viewer, file: &Path, title: &str) -> std::io::Result<()> {
    match viewer {
        Viewer::Builtin => Ok(()),
        Viewer::OmniLook(exe) => spawn(
            exe,
            &[
                "preview".as_ref(),
                file.as_os_str(),
                "--title".as_ref(),
                title.as_ref(),
            ],
        ),
        Viewer::App(exe) => spawn(exe, &[file.as_os_str()]),
        Viewer::Command(line) => {
            let (program, args) = command_line(line, file).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command")
            })?;
            let args: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_ref()).collect();
            spawn(program, &args)
        }
        Viewer::System => tauri_plugin_opener::open_path(file, None::<&str>)
            .map_err(|e| std::io::Error::other(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_resolve_to_what_is_installed() {
        let none = Viewers::default();
        assert_eq!(resolve("auto", "", &none), Viewer::Builtin);
        assert_eq!(resolve("omnilook", "", &none), Viewer::Builtin);
        assert_eq!(resolve("peek", "", &none), Viewer::Builtin);
        assert_eq!(resolve("custom", "  ", &none), Viewer::Builtin);
        assert_eq!(resolve("system", "", &none), Viewer::System);
        assert_eq!(resolve("bogus", "", &none), Viewer::Builtin);
        let some = Viewers {
            omnilook: Some("ol.exe".into()),
            seer: Some("seer.exe".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve("auto", "", &some),
            Viewer::OmniLook("ol.exe".into())
        );
        assert_eq!(resolve("builtin", "", &some), Viewer::Builtin);
        assert_eq!(resolve("seer", "", &some), Viewer::App("seer.exe".into()));
        assert_eq!(
            resolve("custom", " x {file} ", &some),
            Viewer::Command("x {file}".into())
        );
    }

    #[test]
    fn command_lines() {
        let f = Path::new(r"C:\Cache\a b.txt");
        assert_eq!(
            command_line(r#""C:\Program Files\V\v.exe" --open "{file}" -q"#, f),
            Some((
                r"C:\Program Files\V\v.exe".into(),
                vec!["--open".into(), r"C:\Cache\a b.txt".into(), "-q".into()]
            ))
        );
        assert_eq!(
            command_line("viewer --x", f),
            Some((
                "viewer".into(),
                vec!["--x".into(), r"C:\Cache\a b.txt".into()]
            ))
        );
        assert_eq!(
            command_line("v --file={file}", f),
            Some(("v".into(), vec![r"--file=C:\Cache\a b.txt".into()]))
        );
        assert_eq!(command_line("   ", f), None);
        assert_eq!(words(r#"a "" b"#), vec!["a", "", "b"]);
    }
}
