//! OmniZip as the program that opens archives, on Linux (`docs/07-shell-integration.md` §5.1).
//!
//! Desktops take the default program of a MIME type from `mimeapps.list` files (the XDG MIME
//! Applications specification): the user's own, then the system's, desktop-specific files
//! (`kde-mimeapps.list`) before generic ones. Nothing asks the user, so the settings page sets
//! the default itself: OmniZip goes first in the user's list for the types of a format, before
//! the programs listed already, and taking it out again brings the previous default back. What
//! the page shows is read from the files, so it stays true when the default is changed
//! elsewhere.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::{Command, Stdio},
};

use omnizip_core::Error;

/// Desktop entry id of OmniZip (`platform/linux/omnizip-desktop.desktop`).
const APP: &str = "omnizip-desktop.desktop";

const SECTION: &str = "[Default Applications]";

/// MIME types of a format, by the key the settings use (the last part of its extension). A
/// compressed tar goes with its compression, as the settings page shows them together.
fn mime_types(key: &str) -> &'static [&'static str] {
    match key {
        "zip" => &["application/zip", "application/x-zip-compressed"],
        "7z" => &["application/x-7z-compressed"],
        "rar" => &["application/vnd.rar", "application/x-rar"],
        "tar" => &["application/x-tar"],
        "gz" | "tgz" => &[
            "application/gzip",
            "application/x-gzip",
            "application/x-compressed-tar",
        ],
        "bz2" => &[
            "application/x-bzip",
            "application/x-bzip2",
            "application/x-bzip-compressed-tar",
            "application/x-bzip2-compressed-tar",
        ],
        "xz" => &["application/x-xz", "application/x-xz-compressed-tar"],
        "zst" => &[
            "application/zstd",
            "application/x-zstd",
            "application/x-zstd-compressed-tar",
        ],
        "lz4" => &["application/x-lz4", "application/x-lz4-compressed-tar"],
        "lzma" => &["application/x-lzma"],
        "br" => &[
            "application/x-brotli",
            "application/x-brotli-compressed-tar",
        ],
        _ => &[],
    }
}

/// The keys `mime_types` knows.
const KEYS: &[&str] = &[
    "zip", "7z", "rar", "tar", "gz", "bz2", "xz", "zst", "lz4", "lzma", "br",
];

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

fn env_dirs(var: &str, default: &str) -> Vec<PathBuf> {
    let v = std::env::var(var).ok().filter(|v| !v.is_empty());
    v.as_deref()
        .unwrap_or(default)
        .split(':')
        .filter(|d| d.starts_with('/'))
        .map(PathBuf::from)
        .collect()
}

fn config_home() -> Option<PathBuf> {
    env_dir("XDG_CONFIG_HOME").or_else(|| env_dir("HOME").map(|h| h.join(".config")))
}

fn data_home() -> Option<PathBuf> {
    env_dir("XDG_DATA_HOME").or_else(|| env_dir("HOME").map(|h| h.join(".local/share")))
}

/// `kde-mimeapps.list` and the like for the running desktop, then `mimeapps.list`.
fn list_names() -> Vec<String> {
    let mut names: Vec<String> = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| format!("{}-mimeapps.list", d.to_lowercase()))
        .collect();
    names.push("mimeapps.list".into());
    names
}

/// The `mimeapps.list` files in the order the specification reads them.
fn lists() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = config_home().into_iter().collect();
    dirs.extend(env_dirs("XDG_CONFIG_DIRS", "/etc/xdg"));
    dirs.extend(data_home().map(|d| d.join("applications")));
    dirs.extend(
        env_dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share")
            .into_iter()
            .map(|d| d.join("applications")),
    );
    let names = list_names();
    dirs.iter()
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .collect()
}

/// Whether a desktop entry is installed.
fn installed(id: &str) -> bool {
    data_home()
        .into_iter()
        .chain(env_dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share"))
        .any(|d| d.join("applications").join(id).is_file())
}

/// The programs `[Default Applications]` of `text` lists for `mime`, if it lists any.
fn defaults_in(text: &str, mime: &str) -> Option<Vec<String>> {
    let mut in_section = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_section = l == SECTION;
        } else if in_section
            && let Some((k, v)) = l.split_once('=')
            && k.trim() == mime
        {
            return Some(
                v.split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect(),
            );
        }
    }
    None
}

/// `text` with the defaults of `mime` replaced by `apps` (removed when empty), the rest of the
/// file as it was.
fn with_defaults(text: &str, mime: &str, apps: &[String]) -> String {
    let entry = (!apps.is_empty()).then(|| format!("{mime}={};", apps.join(";")));
    let mut out: Vec<String> = Vec::new();
    let (mut in_section, mut seen_section, mut done) = (false, false, false);
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            if in_section && !done {
                // Not listed yet: at the end of the section, before its blank lines.
                let at = out
                    .iter()
                    .rposition(|x| !x.trim().is_empty())
                    .map_or(out.len(), |i| i + 1);
                if let Some(e) = &entry {
                    out.insert(at, e.clone());
                }
                done = true;
            }
            in_section = l == SECTION;
            seen_section |= in_section;
        } else if in_section && l.split_once('=').is_some_and(|(k, _)| k.trim() == mime) {
            if !done && let Some(e) = &entry {
                out.push(e.clone());
            }
            done = true;
            continue;
        }
        out.push(line.to_owned());
    }
    if !done && let Some(e) = entry {
        if !seen_section {
            if out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push(String::new());
            }
            out.push(SECTION.to_owned());
        }
        out.push(e);
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// The program that opens `mime`: the first installed one of the first list that names one.
fn effective(mime: &str) -> Option<String> {
    lists().iter().find_map(|p| {
        let text = std::fs::read_to_string(p).ok()?;
        defaults_in(&text, mime)?.into_iter().find(|a| installed(a))
    })
}

/// For every format the page can set: whether OmniZip opens all its types.
pub fn status() -> BTreeMap<String, bool> {
    KEYS.iter()
        .map(|k| {
            let on = mime_types(k)
                .iter()
                .all(|m| effective(m).as_deref() == Some(APP));
            ((*k).to_owned(), on)
        })
        .collect()
}

/// Makes OmniZip open the formats `keys` (`on`), or takes it out of their defaults again.
/// Changes the user's own lists only: the generic one, and a desktop-specific one where it
/// names a default for the type (it would win over the generic one).
pub fn set(keys: &[String], on: bool) -> Result<(), Error> {
    if on {
        // The entry the default names must exist (it does unless a package installs it).
        crate::desktop_entry::ensure();
    }
    let home = config_home().ok_or_else(|| Error::internal("no home folder"))?;
    let names = list_names();
    for (i, name) in names.iter().enumerate() {
        let generic = i == names.len() - 1;
        let path = home.join(name);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.is_empty() && !(generic && on) {
            continue;
        }
        let mut new = text.clone();
        for key in keys {
            for mime in mime_types(key) {
                let listed = defaults_in(&new, mime);
                if !generic && listed.is_none() {
                    continue;
                }
                let mut apps: Vec<String> = listed
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|a| a != APP)
                    .collect();
                if on {
                    apps.insert(0, APP.to_owned());
                }
                new = with_defaults(&new, mime, &apps);
            }
        }
        if new != text {
            std::fs::create_dir_all(&home).map_err(|e| Error::io(e, &home))?;
            std::fs::write(&path, new).map_err(|e| Error::io(e, &path))?;
        }
    }
    // KDE reads the lists through its cache. (Not from tests: their lists are not the user's.)
    if !cfg!(test)
        && let Ok(mut child) = Command::new("kbuildsycoca6")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    {
        let _ = child.wait();
    }
    Ok(())
}

/// The first program `path` lists for `mime`.
#[cfg(test)]
fn first_in(path: &std::path::Path, mime: &str) -> Option<String> {
    defaults_in(&std::fs::read_to_string(path).ok()?, mime)?
        .into_iter()
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_read_from_their_section_only() {
        let t = "[Added Associations]\napplication/zip=a.desktop;\n\n[Default Applications]\napplication/zip=org.kde.ark.desktop;b.desktop;\n";
        assert_eq!(
            defaults_in(t, "application/zip"),
            Some(vec!["org.kde.ark.desktop".into(), "b.desktop".into()])
        );
        assert_eq!(defaults_in(t, "application/x-7z-compressed"), None);
    }

    #[test]
    fn other_lines_stay_as_they_were() {
        let t = "[Added Associations]\nx/y=a.desktop;\n\n[Default Applications]\napplication/zip=org.kde.ark.desktop;\ntext/plain=kate.desktop;\n\n[Removed Associations]\nz/w=c.desktop;\n";
        let on = with_defaults(
            t,
            "application/zip",
            &[APP.into(), "org.kde.ark.desktop".into()],
        );
        assert_eq!(
            on,
            t.replace(
                "application/zip=org.kde.ark.desktop;",
                "application/zip=omnizip-desktop.desktop;org.kde.ark.desktop;"
            )
        );
        // A new type goes at the end of the section, before the blank line.
        let added = with_defaults(t, "application/x-7z-compressed", &[APP.into()]);
        assert!(added.contains("text/plain=kate.desktop;\napplication/x-7z-compressed=omnizip-desktop.desktop;\n\n[Removed Associations]"), "{added}");
        // Nothing left: the line goes.
        assert_eq!(with_defaults(&added, "application/x-7z-compressed", &[]), t);
        // No section yet.
        assert_eq!(
            with_defaults(
                "[Added Associations]\nx/y=a.desktop;\n",
                "application/zip",
                &[APP.into()]
            ),
            "[Added Associations]\nx/y=a.desktop;\n\n[Default Applications]\napplication/zip=omnizip-desktop.desktop;\n"
        );
        assert_eq!(
            with_defaults("", "application/zip", &[APP.into()]),
            "[Default Applications]\napplication/zip=omnizip-desktop.desktop;\n"
        );
    }

    #[test]
    fn setting_and_unsetting_restores_the_previous_default() {
        let d = tempfile::tempdir().unwrap();
        // SAFETY: the only test of this binary that reads these variables.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", d.path().join("config"));
            std::env::set_var("XDG_DATA_HOME", d.path().join("data"));
            std::env::set_var("XDG_CURRENT_DESKTOP", "KDE");
        }
        let config = d.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        // A desktop-specific default wins over the generic list, so it is changed too.
        std::fs::write(
            config.join("kde-mimeapps.list"),
            "[Default Applications]\napplication/x-7z-compressed=org.kde.ark.desktop;\n",
        )
        .unwrap();
        set(&["7z".into(), "zip".into()], true).unwrap();
        let generic = config.join("mimeapps.list");
        assert_eq!(first_in(&generic, "application/zip").as_deref(), Some(APP));
        assert_eq!(
            first_in(
                &config.join("kde-mimeapps.list"),
                "application/x-7z-compressed"
            )
            .as_deref(),
            Some(APP)
        );
        set(&["7z".into(), "zip".into()], false).unwrap();
        assert_eq!(first_in(&generic, "application/zip"), None);
        assert_eq!(
            std::fs::read_to_string(config.join("kde-mimeapps.list")).unwrap(),
            "[Default Applications]\napplication/x-7z-compressed=org.kde.ark.desktop;\n"
        );
    }
}
