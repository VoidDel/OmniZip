//! Command-line protocol of the desktop executable (`docs/07-shell-integration.md` §6):
//!
//! ```text
//! omnizip-desktop [--open <archive>]
//!                 [--shell open|extract-here|extract-smart|extract-to|extract-each|test|mount|
//!                          add-zip|add-7z|add|convert]
//!                 [--cwd <dir>] [--files-from <list-file>] [<paths>…]
//! ```

use std::path::{Path, PathBuf};

use serde::Serialize;

/// A shell verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verb {
    Open,
    ExtractHere,
    ExtractSmart,
    ExtractTo,
    ExtractEach,
    Test,
    Mount,
    AddZip,
    Add7z,
    Add,
    Convert,
}

impl Verb {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "open" => Verb::Open,
            "extract-here" => Verb::ExtractHere,
            "extract-smart" => Verb::ExtractSmart,
            "extract-to" => Verb::ExtractTo,
            "extract-each" => Verb::ExtractEach,
            "test" => Verb::Test,
            "mount" => Verb::Mount,
            "add-zip" => Verb::AddZip,
            "add-7z" => Verb::Add7z,
            "add" => Verb::Add,
            "convert" => Verb::Convert,
            _ => return None,
        })
    }

    /// Verbs that show one dialog in its own small window, without the main window. The window
    /// turns into the progress window once the job starts.
    pub fn is_dialog(self) -> bool {
        matches!(self, Verb::ExtractTo | Verb::Add | Verb::Convert)
    }

    /// Verbs that run in a small progress window without the main window.
    pub fn is_background(self) -> bool {
        matches!(
            self,
            Verb::ExtractHere
                | Verb::ExtractSmart
                | Verb::ExtractEach
                | Verb::Test
                | Verb::AddZip
                | Verb::Add7z
        )
    }
}

/// A parsed invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub verb: Verb,
    pub paths: Vec<PathBuf>,
}

/// What the main window should show after start-up or a forwarded launch.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LaunchRequest {
    Open { path: String },
    Extract { paths: Vec<String> },
    Compress { paths: Vec<String> },
    Mount { path: String },
    Convert { paths: Vec<String> },
}

fn read_list(file: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let out = text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect();
    // Lists written by the shell extension live in the temp directory; remove them once read.
    if file.starts_with(std::env::temp_dir()) {
        let _ = std::fs::remove_file(file);
    }
    out
}

/// Parses process arguments (the first one is the executable).
pub fn parse(args: &[String], cwd: &Path) -> Option<Invocation> {
    let mut verb = None;
    let mut base = cwd.to_path_buf();
    let mut paths = Vec::new();
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--shell" => verb = it.next().and_then(|v| Verb::parse(v)),
            "--cwd" => {
                if let Some(d) = it.next() {
                    base = PathBuf::from(d);
                }
            }
            "--files-from" => {
                if let Some(f) = it.next() {
                    paths.extend(read_list(Path::new(f)));
                }
            }
            "--open" => {
                verb = Some(Verb::Open);
                if let Some(p) = it.next() {
                    paths.push(PathBuf::from(p));
                }
            }
            // Flags injected by the OS or WebView (for example macOS -psn_…) are ignored.
            s if s.starts_with('-') => {}
            s => paths.push(PathBuf::from(s)),
        }
    }
    let paths: Vec<PathBuf> = paths
        .into_iter()
        .map(|p| if p.is_absolute() { p } else { base.join(p) })
        .collect();
    if paths.is_empty() {
        return None;
    }
    Some(Invocation {
        verb: verb.unwrap_or(Verb::Open),
        paths,
    })
}

/// The main-window request for a foreground verb.
pub fn to_request(inv: &Invocation) -> Option<LaunchRequest> {
    let all = || {
        inv.paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    let first = inv.paths.first()?.to_string_lossy().into_owned();
    Some(match inv.verb {
        Verb::Open => LaunchRequest::Open { path: first },
        Verb::ExtractTo => LaunchRequest::Extract { paths: all() },
        Verb::Add => LaunchRequest::Compress { paths: all() },
        Verb::Mount => LaunchRequest::Mount { path: first },
        Verb::Convert => LaunchRequest::Convert { paths: all() },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        std::iter::once("omnizip-desktop")
            .chain(v.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn parsing() {
        let cwd = std::env::temp_dir();
        assert_eq!(parse(&args(&[]), &cwd), None);
        let inv = parse(&args(&["a.zip"]), &cwd).unwrap();
        assert_eq!(inv.verb, Verb::Open);
        assert_eq!(inv.paths, vec![cwd.join("a.zip")]);
        let inv = parse(
            &args(&["--shell", "extract-here", "--cwd", "/x", "b.7z"]),
            &cwd,
        )
        .unwrap();
        assert_eq!(inv.verb, Verb::ExtractHere);
        assert_eq!(inv.paths, vec![PathBuf::from("/x").join("b.7z")]);
        assert!(inv.verb.is_background());
        // "Extract to…", "Add to archive…" and "Convert to…" get a standalone dialog window, not
        // the main one.
        for v in [Verb::ExtractTo, Verb::Add, Verb::Convert] {
            assert!(v.is_dialog() && !v.is_background());
        }
        for v in [Verb::Open, Verb::ExtractHere, Verb::Add7z, Verb::Mount] {
            assert!(!v.is_dialog());
        }
    }

    #[test]
    fn files_from_list() {
        let list = std::env::temp_dir().join(format!("omnizip-test-{}.txt", std::process::id()));
        std::fs::write(&list, "\u{feff}C:\\a.zip\r\nC:\\b.zip\r\n\r\n").unwrap();
        let inv = parse(
            &args(&["--shell", "test", "--files-from", list.to_str().unwrap()]),
            Path::new("/"),
        )
        .unwrap();
        assert_eq!(inv.paths.len(), 2);
        assert!(!list.exists(), "temp list removed after reading");
    }
}
