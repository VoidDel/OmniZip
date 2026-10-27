//! Platform-independent context-menu logic (`docs/07-shell-integration.md` §2): which verbs exist,
//! when they are shown, what they are called, and how they launch the desktop app.

use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};

use omnizip_core::{FormatId, detect, format::archive_stem};
use omnizip_i18n::{Localizer, tr};
use serde::Deserialize;

/// A context-menu verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verb {
    Open,
    ExtractHere,
    ExtractSmart,
    ExtractTo,
    ExtractEach,
    Mount,
    Test,
    Convert,
    AddZip,
    Add7z,
    Add,
}

impl Verb {
    /// Menu order.
    pub const ALL: [Verb; 11] = [
        Verb::Open,
        Verb::ExtractHere,
        Verb::ExtractSmart,
        Verb::ExtractTo,
        Verb::ExtractEach,
        Verb::Mount,
        Verb::Test,
        Verb::Convert,
        Verb::AddZip,
        Verb::Add7z,
        Verb::Add,
    ];

    /// Key in `[shell.items]` of `config.toml`.
    pub const fn key(self) -> &'static str {
        match self {
            Verb::Open => "open",
            Verb::ExtractHere => "extract_here",
            Verb::ExtractSmart => "extract_smart",
            Verb::ExtractTo => "extract_to",
            Verb::ExtractEach => "extract_each",
            Verb::Mount => "mount",
            Verb::Test => "test",
            Verb::Convert => "convert",
            Verb::AddZip => "add_zip",
            Verb::Add7z => "add_7z",
            Verb::Add => "add",
        }
    }

    /// Value of `--shell` for `omnizip-desktop`.
    pub const fn cli(self) -> &'static str {
        match self {
            Verb::Open => "open",
            Verb::ExtractHere => "extract-here",
            Verb::ExtractSmart => "extract-smart",
            Verb::ExtractTo => "extract-to",
            Verb::ExtractEach => "extract-each",
            Verb::Mount => "mount",
            Verb::Test => "test",
            Verb::Convert => "convert",
            Verb::AddZip => "add-zip",
            Verb::Add7z => "add-7z",
            Verb::Add => "add",
        }
    }

    /// Fluent message id (`locales/*/shell.ftl`).
    pub const fn message(self) -> &'static str {
        match self {
            Verb::Open => "menu-open",
            Verb::ExtractHere => "menu-extract-here",
            Verb::ExtractSmart => "menu-extract-smart",
            Verb::ExtractTo => "menu-extract-to",
            Verb::ExtractEach => "menu-extract-each",
            Verb::Mount => "menu-mount",
            Verb::Test => "menu-test",
            Verb::Convert => "menu-convert",
            Verb::AddZip => "menu-add-zip",
            Verb::Add7z => "menu-add-7z",
            Verb::Add => "menu-add",
        }
    }

    /// Index used to derive per-verb class ids.
    pub fn index(self) -> u8 {
        Verb::ALL.iter().position(|v| *v == self).unwrap_or(0) as u8
    }

    pub fn from_index(i: u8) -> Option<Verb> {
        Verb::ALL.get(i as usize).copied()
    }
}

/// What is selected in the file manager.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub archives: usize,
    pub others: usize,
    /// Base name used in titles: the archive stem, the folder name, or the parent folder name.
    pub name: String,
}

/// Paths that need a content sniff are limited so the menu stays fast (`docs/07` §2).
const MAX_SNIFFS: usize = 8;
/// Above this many items the menu does not look at each one.
pub const MAX_ITEMS: usize = 1000;

fn is_archive(p: &Path, sniffs: &mut usize) -> bool {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if FormatId::from_file_name(&name).is_some() {
        return true;
    }
    // No known extension: a quick look at the first bytes of a few files.
    if p.extension().is_none() && *sniffs < MAX_SNIFFS && p.is_file() {
        *sniffs += 1;
        let mut head = [0u8; 600];
        if let Ok(mut f) = std::fs::File::open(p) {
            let n = f.read(&mut head).unwrap_or(0);
            return detect::sniff(&head[..n]).is_some();
        }
    }
    false
}

/// Classifies a selection.
pub fn classify(paths: &[PathBuf]) -> Selection {
    let mut sel = Selection::default();
    let mut sniffs = 0;
    for p in paths.iter().take(MAX_ITEMS) {
        if !p.is_dir() && is_archive(p, &mut sniffs) {
            sel.archives += 1;
        } else {
            sel.others += 1;
        }
    }
    sel.others += paths.len().saturating_sub(MAX_ITEMS);
    sel.name = match paths {
        [one] => {
            let n = one
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if one.is_dir() || sel.archives == 0 && one.extension().is_none() {
                n
            } else if sel.archives == 1 {
                archive_stem(&n)
            } else {
                one.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or(n)
            }
        }
        [first, ..] => first
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive".to_owned()),
        [] => String::new(),
    };
    sel
}

/// Whether a verb applies to a selection.
pub fn applies(verb: Verb, sel: &Selection) -> bool {
    let only_archives = sel.archives > 0 && sel.others == 0;
    match verb {
        Verb::Open | Verb::Mount => sel.archives == 1 && sel.others == 0,
        Verb::ExtractHere | Verb::ExtractSmart | Verb::ExtractTo | Verb::Test | Verb::Convert => {
            only_archives
        }
        Verb::ExtractEach => sel.archives > 1 && sel.others == 0,
        Verb::AddZip | Verb::Add7z | Verb::Add => sel.others > 0,
    }
}

/// Localized title of a verb for a selection.
pub fn title(verb: Verb, sel: &Selection, l: &Localizer) -> String {
    let name = if sel.name.is_empty() {
        "…".to_owned()
    } else {
        sel.name.clone()
    };
    match verb {
        Verb::ExtractSmart | Verb::AddZip | Verb::Add7z => tr!(l, verb.message(), name = name),
        _ => l.get(verb.message()),
    }
}

/// `[shell]` and `[language]` of the shared `config.toml` (`apps/desktop/src-tauri/src/settings.rs`).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct MenuConfig {
    pub shell: ShellSection,
    pub language: LanguageSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ShellSection {
    pub style: String,
    pub icons: bool,
    pub items: BTreeMap<String, bool>,
}

impl Default for ShellSection {
    fn default() -> Self {
        Self {
            style: "cascade".into(),
            icons: true,
            items: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct LanguageSection {
    pub ui: String,
}

impl MenuConfig {
    /// Reads the shared settings file; anything missing or unreadable falls back to defaults.
    pub fn load() -> Self {
        omnizip_core::appdirs::config_file()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Whether the user enabled a verb (`convert` is off by default, everything else on).
    pub fn enabled(&self, verb: Verb) -> bool {
        self.shell
            .items
            .get(verb.key())
            .copied()
            .unwrap_or(verb != Verb::Convert)
    }

    /// Flat layout instead of a submenu.
    pub fn flat(&self) -> bool {
        self.shell.style == "flat"
    }

    pub fn localizer(&self) -> Localizer {
        match self.language.ui.as_str() {
            "" | "auto" => Localizer::detect(None),
            l => Localizer::new(l),
        }
    }
}

/// Writes the selection to a temporary list file (avoids the 32 K command-line limit).
pub fn write_list(paths: &[PathBuf]) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let file = dir.join(format!("omnizip-shell-{}-{stamp}.txt", std::process::id()));
    let mut text = String::new();
    for p in paths {
        text.push_str(&p.to_string_lossy());
        text.push('\n');
    }
    std::fs::write(&file, text)?;
    Ok(file)
}

/// Arguments for `omnizip-desktop`.
pub fn arguments(verb: Verb, list: &Path, cwd: Option<&Path>) -> Vec<String> {
    let mut v = vec!["--shell".to_owned(), verb.cli().to_owned()];
    if let Some(c) = cwd {
        v.push("--cwd".to_owned());
        v.push(c.to_string_lossy().into_owned());
    }
    v.push("--files-from".to_owned());
    v.push(list.to_string_lossy().into_owned());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("photos.zip"), b"PK\x03\x04").unwrap();
        std::fs::write(d.path().join("backup.tar.zst"), b"").unwrap();
        std::fs::write(d.path().join("notes.txt"), b"hi").unwrap();
        std::fs::write(d.path().join("NOEXT"), b"PK\x03\x04rest").unwrap();
        std::fs::create_dir(d.path().join("folder")).unwrap();
        d
    }

    #[test]
    fn classification_and_visibility() {
        let d = tree();
        let p = |n: &str| d.path().join(n);

        let one = classify(&[p("photos.zip")]);
        assert_eq!(
            (one.archives, one.others, one.name.as_str()),
            (1, 0, "photos")
        );
        for v in [
            Verb::Open,
            Verb::ExtractHere,
            Verb::ExtractSmart,
            Verb::Mount,
            Verb::Test,
        ] {
            assert!(applies(v, &one), "{v:?}");
        }
        assert!(!applies(Verb::ExtractEach, &one));
        assert!(!applies(Verb::AddZip, &one));

        let two = classify(&[p("photos.zip"), p("backup.tar.zst")]);
        assert!(applies(Verb::ExtractEach, &two));
        assert!(!applies(Verb::Open, &two));

        let files = classify(&[p("notes.txt"), p("folder")]);
        assert_eq!(files.archives, 0);
        assert!(applies(Verb::AddZip, &files));
        assert!(!applies(Verb::ExtractHere, &files));
        assert_eq!(files.name, d.path().file_name().unwrap().to_string_lossy());

        let folder = classify(&[p("folder")]);
        assert_eq!(folder.name, "folder");

        let sniffed = classify(&[p("NOEXT")]);
        assert_eq!(
            sniffed.archives, 1,
            "content sniff for files without extension"
        );

        let mixed = classify(&[p("photos.zip"), p("notes.txt")]);
        assert!(applies(Verb::Add, &mixed));
        assert!(!applies(Verb::ExtractHere, &mixed));
    }

    #[test]
    fn titles_are_localized() {
        let sel = Selection {
            archives: 1,
            others: 0,
            name: "photos".into(),
        };
        assert_eq!(
            title(Verb::ExtractSmart, &sel, &Localizer::new("zh-CN")),
            "解压到“photos\\”"
        );
        assert_eq!(
            title(Verb::AddZip, &sel, &Localizer::new("en-US")),
            "Add to \"photos.zip\""
        );
        assert_eq!(
            title(Verb::Mount, &sel, &Localizer::new("ja-JP")),
            "マウント"
        );
    }

    #[test]
    fn config_defaults_and_overrides() {
        let c: MenuConfig = toml::from_str(
            "[shell]\nstyle = \"flat\"\n[shell.items]\ntest = false\n[language]\nui = \"zh-TW\"\n",
        )
        .unwrap();
        assert!(c.flat());
        assert!(!c.enabled(Verb::Test));
        assert!(c.enabled(Verb::Open));
        assert!(!MenuConfig::default().enabled(Verb::Convert));
        assert_eq!(c.localizer().lang(), "zh-TW");
    }

    #[test]
    fn list_and_arguments() {
        let paths = vec![
            PathBuf::from("C:\\a b\\x.zip"),
            PathBuf::from("C:\\中文\\y.7z"),
        ];
        let list = write_list(&paths).unwrap();
        assert_eq!(
            std::fs::read_to_string(&list).unwrap(),
            "C:\\a b\\x.zip\nC:\\中文\\y.7z\n"
        );
        let args = arguments(Verb::ExtractHere, &list, None);
        assert_eq!(args[..2], ["--shell".to_owned(), "extract-here".to_owned()]);
        assert_eq!(args.last().unwrap(), &list.to_string_lossy());
        std::fs::remove_file(list).unwrap();
        for v in Verb::ALL {
            assert_eq!(Verb::from_index(v.index()), Some(v));
        }
    }
}
