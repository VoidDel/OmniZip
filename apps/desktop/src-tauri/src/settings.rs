//! User settings, persisted as TOML (`docs/02-architecture.md` §4.6). The same file is read by
//! the CLI and the shell extension (language, menu items), so keys are stable snake_case.

use std::{collections::BTreeMap, fs, path::Path};

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub schema_version: u32,
    pub general: General,
    pub appearance: Appearance,
    pub language: Language,
    pub formats: Formats,
    pub shell: Shell,
    pub security: Security,
    pub mount: Mount,
    pub advanced: Advanced,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            general: General::default(),
            appearance: Appearance::default(),
            language: Language::default(),
            formats: Formats::default(),
            shell: Shell::default(),
            security: Security::default(),
            mount: Mount::default(),
            advanced: Advanced::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct General {
    /// `home` or `last`.
    pub startup: String,
    /// `ask`, `same` (archive folder) or `downloads`.
    pub extract_to: String,
    /// `exit` or `tray`.
    pub close_behavior: String,
    pub notify: bool,
    /// Open the destination folder after extracting.
    pub open_after_extract: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            startup: "home".into(),
            extract_to: "ask".into(),
            close_behavior: "exit".into(),
            notify: true,
            open_after_extract: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Appearance {
    /// `system`, `light` or `dark`.
    pub mode: String,
    pub theme_light: String,
    pub theme_dark: String,
    /// `theme`, `system` or `#RRGGBB`.
    pub accent: String,
    pub material: bool,
    /// Windows 11 material: `mica`, `mica-alt` or `acrylic` (macOS always uses vibrancy).
    pub material_kind: String,
    /// `compact`, `comfortable` or `spacious`.
    pub density: String,
    /// `mono` or `duotone`.
    pub icon_style: String,
    pub font_size: u32,
    pub sidebar_collapsed: bool,
    /// `table` or `grid`.
    pub view_mode: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            mode: "system".into(),
            theme_light: "aurora".into(),
            theme_dark: "midnight".into(),
            accent: "theme".into(),
            material: true,
            material_kind: "mica".into(),
            density: "comfortable".into(),
            icon_style: "duotone".into(),
            font_size: 14,
            sidebar_collapsed: false,
            view_mode: "table".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Language {
    /// `auto` or a BCP 47 tag (`zh-CN`, `zh-TW`, `en-US`, `ja-JP`).
    pub ui: String,
    /// `auto`, `kb` (1000) or `kib` (1024).
    pub size_unit: String,
    /// `auto` or an encoding label for legacy archive names.
    pub default_encoding: String,
}

impl Default for Language {
    fn default() -> Self {
        Self {
            ui: "auto".into(),
            size_unit: "auto".into(),
            default_encoding: "auto".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Formats {
    pub default_format: String,
    pub default_level: String,
    /// Extensions associated with OmniZip (`zip` → true).
    pub associations: BTreeMap<String, bool>,
}

impl Default for Formats {
    fn default() -> Self {
        let associations = ["zip", "7z", "rar", "tar", "gz", "xz", "zst", "bz2", "tgz"]
            .iter()
            .map(|e| ((*e).to_owned(), true))
            .collect();
        Self {
            default_format: "zip".into(),
            default_level: "normal".into(),
            associations,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Shell {
    /// `cascade` or `flat`.
    pub style: String,
    pub icons: bool,
    /// Visible menu items (`open`, `extract_here`, `extract_smart`, `extract_to`, `mount`,
    /// `test`, `convert`, `add_zip`, `add_7z`, `add`).
    pub items: BTreeMap<String, bool>,
}

impl Default for Shell {
    fn default() -> Self {
        let items = [
            ("open", true),
            ("extract_here", true),
            ("extract_smart", true),
            ("extract_to", true),
            ("extract_each", true),
            ("mount", true),
            ("test", true),
            ("convert", false),
            ("add_zip", true),
            ("add_7z", true),
            ("add", true),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), *v))
        .collect();
        Self {
            style: "cascade".into(),
            icons: true,
            items,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Security {
    pub max_ratio: u64,
    pub propagate_origin: bool,
    /// `inside`, `file` or `skip`.
    pub symlinks: String,
}

impl Default for Security {
    fn default() -> Self {
        Self {
            max_ratio: 1000,
            propagate_origin: true,
            symlinks: if cfg!(windows) {
                "file".into()
            } else {
                "inside".into()
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Mount {
    pub cache_gb: u32,
    pub unmount_on_exit: bool,
    pub restore_on_start: bool,
    pub warm_solid: bool,
    pub offline_attribute: bool,
}

impl Default for Mount {
    fn default() -> Self {
        Self {
            cache_gb: 2,
            unmount_on_exit: true,
            restore_on_start: false,
            warm_solid: true,
            offline_attribute: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Advanced {
    pub threads: u32,
    pub log_level: String,
}

impl Default for Advanced {
    fn default() -> Self {
        Self {
            threads: 0,
            log_level: "info".into(),
        }
    }
}

/// Loads settings; a damaged file is kept as `config.toml.bak` and defaults are used.
pub fn load(path: &Path) -> Settings {
    let Ok(text) = fs::read_to_string(path) else {
        return Settings::default();
    };
    match toml::from_str::<Settings>(&text) {
        Ok(mut s) => {
            s.schema_version = SCHEMA_VERSION;
            s
        }
        Err(e) => {
            tracing::warn!("settings unreadable ({e}); using defaults");
            let _ = fs::copy(path, path.with_extension("toml.bak"));
            Settings::default()
        }
    }
}

/// Saves settings atomically.
pub fn save(path: &Path, s: &Settings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let text = toml::to_string_pretty(s).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.toml");
        assert_eq!(load(&p), Settings::default());
        let mut s = Settings::default();
        s.appearance.mode = "dark".into();
        s.shell.items.insert("convert".into(), true);
        save(&p, &s).unwrap();
        assert_eq!(load(&p), s);
        // Unknown and missing keys are tolerated.
        fs::write(&p, "[appearance]\nmode = \"light\"\nfuture_key = 1\n").unwrap();
        let s = load(&p);
        assert_eq!(s.appearance.mode, "light");
        assert_eq!(s.appearance.theme_dark, "midnight");
    }

    #[test]
    fn damaged_file_is_backed_up() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.toml");
        fs::write(&p, "this is = = not toml").unwrap();
        assert_eq!(load(&p), Settings::default());
        assert!(d.path().join("config.toml.bak").exists());
    }
}
