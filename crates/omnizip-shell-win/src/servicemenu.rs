//! The context menu of Dolphin (KDE) as service menus (`docs/07-shell-integration.md` §5.2):
//! `.desktop` files in `$XDG_DATA_HOME/kio/servicemenus`, generated from the same verbs,
//! settings and titles as the other menus.
//!
//! A service menu is static: Dolphin shows its actions when every selected item has one of its
//! MIME types and the number of items fits. So there is one file per rule of [`applies`]
//! (one archive; archives; several archives; anything), and titles that cannot name the
//! selection ("Add to a ZIP archive" instead of "Add to "photos.zip""). "Eject" needs to know
//! whether a folder is a mount, which a service menu cannot ask: Dolphin has no such item. The
//! files are written again whenever the menu settings change.
//!
//! [`applies`]: crate::menu::applies

use std::path::Path;

use crate::menu::{MenuConfig, Verb};

/// What the file names of the service menus start with.
pub const FILE_PREFIX: &str = "omnizip-";

/// MIME types of the archives OmniZip reads (shared-mime-info names, with the aliases older
/// systems use).
pub const ARCHIVE_MIME_TYPES: &[&str] = &[
    "application/zip",
    "application/x-zip-compressed",
    "application/x-7z-compressed",
    "application/vnd.rar",
    "application/x-rar",
    "application/x-tar",
    "application/x-compressed-tar",
    "application/x-bzip-compressed-tar",
    "application/x-bzip2-compressed-tar",
    "application/x-xz-compressed-tar",
    "application/x-zstd-compressed-tar",
    "application/x-lz4-compressed-tar",
    "application/gzip",
    "application/x-gzip",
    "application/x-bzip",
    "application/x-bzip2",
    "application/x-xz",
    "application/x-lzma",
    "application/zstd",
    "application/x-zstd",
    "application/x-lz4",
];

/// How many items a service menu wants.
enum Count {
    Exactly(u32),
    AtLeast(u32),
}

/// One service menu: the verbs that share a rule.
struct Group {
    file: &'static str,
    verbs: &'static [Verb],
    /// Archives only, or anything.
    archives: bool,
    count: Count,
}

/// In menu order, as far as the rules allow.
const GROUPS: [Group; 4] = [
    Group {
        file: "omnizip-1-archive.desktop",
        verbs: &[Verb::Open, Verb::Mount],
        archives: true,
        count: Count::Exactly(1),
    },
    Group {
        file: "omnizip-2-archives.desktop",
        verbs: &[
            Verb::ExtractHere,
            Verb::ExtractSmart,
            Verb::ExtractTo,
            Verb::Test,
            Verb::Convert,
        ],
        archives: true,
        count: Count::AtLeast(1),
    },
    Group {
        file: "omnizip-3-each.desktop",
        verbs: &[Verb::ExtractEach],
        archives: true,
        count: Count::AtLeast(2),
    },
    Group {
        file: "omnizip-4-add.desktop",
        verbs: &[Verb::AddZip, Verb::Add7z, Verb::Add],
        archives: false,
        count: Count::AtLeast(1),
    },
];

/// Icon of a verb, from the freedesktop and Breeze icon themes.
fn icon(verb: Verb) -> &'static str {
    match verb {
        Verb::Open => "document-open",
        Verb::ExtractHere | Verb::ExtractSmart | Verb::ExtractTo | Verb::ExtractEach => {
            "archive-extract"
        }
        Verb::Mount => "drive-removable-media",
        Verb::Test => "dialog-ok",
        Verb::Convert => "document-save-as",
        Verb::AddZip | Verb::Add7z | Verb::Add => "archive-insert",
        Verb::Unmount => "media-eject",
    }
}

/// A title that does not name the selection.
fn title(verb: Verb, l: &omnizip_i18n::Localizer) -> String {
    l.get(match verb {
        Verb::ExtractSmart => "menu-extract-smart-folder",
        Verb::AddZip => "menu-add-zip-any",
        Verb::Add7z => "menu-add-7z-any",
        v => v.message(),
    })
}

/// A `.desktop` string value: backslashes and line breaks escaped.
fn value(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// One argument of `Exec` as the Desktop Entry Specification asks: as it is when it has none
/// of the reserved characters (some tools, `xdg-mime` among them, take the program's path
/// literally and would not find it in quotes), otherwise quoted, with `"`, `` ` ``, `$` and `\`
/// escaped inside; `%` doubled either way; then escaped as a string value.
pub fn exec_arg(s: &str) -> String {
    const RESERVED: &str = " \t\n\"'\\><~|&;$*?#()`";
    let quote = s.is_empty() || s.chars().any(|c| RESERVED.contains(c));
    let mut q = String::new();
    if quote {
        q.push('"');
    }
    for c in s.chars() {
        match c {
            '"' | '`' | '$' | '\\' if quote => {
                q.push('\\');
                q.push(c);
            }
            '%' => q.push_str("%%"),
            c => q.push(c),
        }
    }
    if quote {
        q.push('"');
    }
    value(&q)
}

/// The service menus for the settings `cfg`, launching `app` (`omnizip-desktop`): file names
/// and contents. Groups whose verbs are all turned off get no file.
pub fn service_menus(cfg: &MenuConfig, app: &Path) -> Vec<(&'static str, String)> {
    let l = cfg.localizer();
    let app = app.to_string_lossy();
    let mut out = Vec::new();
    for g in &GROUPS {
        let verbs: Vec<Verb> = g
            .verbs
            .iter()
            .copied()
            .filter(|v| cfg.enabled(*v))
            .collect();
        if verbs.is_empty() {
            continue;
        }
        let mut t = String::from("[Desktop Entry]\nType=Service\n");
        t.push_str("X-KDE-ServiceTypes=KonqPopupMenu/Plugin\n");
        if g.archives {
            t.push_str(&format!("MimeType={};\n", ARCHIVE_MIME_TYPES.join(";")));
        } else {
            t.push_str("MimeType=all/all;\n");
        }
        // Files on this computer only, as in the other file managers.
        t.push_str("X-KDE-Protocols=file\n");
        match g.count {
            Count::Exactly(n) => t.push_str(&format!("X-KDE-RequiredNumberOfUrls={n}\n")),
            Count::AtLeast(n) => t.push_str(&format!("X-KDE-MinNumberOfUrls={n}\n")),
        }
        // Next to Dolphin's own items, not under "Actions".
        t.push_str("X-KDE-Priority=TopLevel\n");
        if !cfg.flat() {
            t.push_str(&format!("X-KDE-Submenu={}\n", value(&l.get("menu-root"))));
        }
        let ids: Vec<&str> = verbs.iter().map(|v| v.cli()).collect();
        t.push_str(&format!("Actions={};\n", ids.join(";")));
        for v in verbs {
            t.push_str(&format!("\n[Desktop Action {}]\n", v.cli()));
            t.push_str(&format!("Name={}\n", value(&title(v, &l))));
            if cfg.shell.icons {
                t.push_str(&format!("Icon={}\n", icon(v)));
            }
            // Gone with the app: the item disappears rather than failing.
            t.push_str(&format!("TryExec={}\n", value(&app)));
            t.push_str(&format!("Exec={} --shell {} %F\n", exec_arg(&app), v.cli()));
        }
        out.push((g.file, t));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::menu::{LanguageSection, ShellSection};

    fn config(style: &str, icons: bool, off: &[&str]) -> MenuConfig {
        MenuConfig {
            shell: ShellSection {
                style: style.into(),
                icons,
                items: off
                    .iter()
                    .map(|k| (k.to_string(), false))
                    .collect::<BTreeMap<_, _>>(),
            },
            language: LanguageSection { ui: "zh-CN".into() },
        }
    }

    #[test]
    fn one_file_per_rule_with_the_settings_applied() {
        let app = Path::new("/opt/OmniZip/omnizip-desktop");
        let menus = service_menus(&config("cascade", true, &[]), app);
        let files: Vec<&str> = menus.iter().map(|m| m.0).collect();
        assert_eq!(
            files,
            [
                "omnizip-1-archive.desktop",
                "omnizip-2-archives.desktop",
                "omnizip-3-each.desktop",
                "omnizip-4-add.desktop"
            ]
        );
        let one = &menus[0].1;
        assert!(one.contains("X-KDE-RequiredNumberOfUrls=1\n"));
        assert!(one.contains("X-KDE-Submenu=OmniZip\n"));
        assert!(one.contains("Actions=open;mount;\n"));
        assert!(one.contains("Name=挂载\n"));
        assert!(one.contains("Icon=drive-removable-media\n"));
        assert!(
            one.contains("Exec=/opt/OmniZip/omnizip-desktop --shell mount %F\n"),
            "{one}"
        );
        assert!(one.contains("MimeType=application/zip;"));
        // "Convert" is off unless turned on.
        assert!(
            menus[1]
                .1
                .contains("Actions=extract-here;extract-smart;extract-to;test;\n")
        );
        assert!(menus[1].1.contains("Name=解压到同名文件夹\n"));
        assert!(menus[2].1.contains("X-KDE-MinNumberOfUrls=2\n"));
        let add = &menus[3].1;
        assert!(add.contains("MimeType=all/all;\n"));
        assert!(add.contains("Name=添加为 ZIP 压缩包\n"));
        // No item has a name in it, and none ejects.
        assert!(
            !menus
                .iter()
                .any(|m| m.1.contains("{ $name }") || m.1.contains("unmount"))
        );
    }

    #[test]
    fn flat_without_icons_and_turned_off_items() {
        let menus = service_menus(
            &config("flat", false, &["extract_each", "open"]),
            Path::new("/tmp/a b/omnizip-desktop"),
        );
        assert_eq!(menus.len(), 3);
        assert!(
            !menus
                .iter()
                .any(|m| m.1.contains("X-KDE-Submenu") || m.1.contains("Icon="))
        );
        assert!(menus[0].1.contains("Actions=mount;\n"));
    }

    #[test]
    fn exec_arguments_are_quoted() {
        assert_eq!(
            exec_arg("/opt/OmniZip/omnizip-desktop"),
            "/opt/OmniZip/omnizip-desktop"
        );
        assert_eq!(exec_arg("/x/100%/y"), "/x/100%%/y");
        assert_eq!(exec_arg("/a b/c"), "\"/a b/c\"");
        assert_eq!(exec_arg("/x/100%/$y"), "\"/x/100%%/\\\\$y\"");
        assert_eq!(exec_arg(r#"/q"\"#), r#""/q\\"\\\\""#);
    }
}
