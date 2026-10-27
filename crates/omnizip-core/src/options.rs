//! Options for opening, extracting and creating archives (`docs/03-formats.md` §6).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{FormatId, encoding::NameEncoding};

/// Options for opening an archive.
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    /// Password to try first (the interaction callback is asked when it is missing or wrong).
    pub password: Option<String>,
    /// Force a file-name encoding instead of auto-detection.
    pub encoding: Option<NameEncoding>,
    /// BCP 47 locale of the user, used as a hint for encoding detection.
    pub locale: Option<String>,
}

/// How entry paths map onto the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PathMode {
    /// Keep the directory structure.
    #[default]
    Full,
    /// Put every file directly into the destination.
    Flat,
}

/// What to do when the destination file exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum OverwriteMode {
    /// Ask through [`crate::Interaction::conflict`].
    #[default]
    Ask,
    Skip,
    Overwrite,
    /// Overwrite only when the archive copy is newer.
    OverwriteOlder,
    /// Keep both: the extracted file gets a numbered name.
    Rename,
}

/// Symbolic-link handling on extraction (`docs/09-security.md` §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SymlinkPolicy {
    /// Create the link only when its target stays inside the destination (default on Unix).
    #[default]
    CreateIfInside,
    /// Write a regular file containing the target path (default on Windows).
    AsFile,
    Skip,
}

impl SymlinkPolicy {
    /// Platform default.
    pub const fn platform_default() -> Self {
        if cfg!(windows) {
            SymlinkPolicy::AsFile
        } else {
            SymlinkPolicy::CreateIfInside
        }
    }
}

/// Options for extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExtractOptions {
    pub destination: PathBuf,
    pub path_mode: PathMode,
    /// Smart extraction: wrap the content in a folder named after the archive unless the
    /// archive already has a single top-level item.
    pub smart_folder: bool,
    pub overwrite: OverwriteMode,
    /// Keep files whose checksum fails, renamed with a `.broken` marker in the report.
    pub keep_broken: bool,
    pub restore_mtime: bool,
    pub restore_permissions: bool,
    pub symlinks: SymlinkPolicy,
    /// Copy the archive's origin mark (Mark-of-the-Web / quarantine) to extracted files.
    pub propagate_origin: bool,
    /// Drop dangerous path parts instead of refusing the entry.
    pub strip_unsafe_paths: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            destination: PathBuf::from("."),
            path_mode: PathMode::Full,
            smart_folder: true,
            overwrite: OverwriteMode::Ask,
            keep_broken: false,
            restore_mtime: true,
            restore_permissions: true,
            symlinks: SymlinkPolicy::platform_default(),
            propagate_origin: true,
            strip_unsafe_paths: false,
        }
    }
}

/// Seven compression levels shown in every UI (`docs/03-formats.md` §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Store,
    Fastest,
    Fast,
    #[default]
    Normal,
    Better,
    Best,
    Ultra,
}

impl Level {
    /// All levels in order.
    pub const ALL: [Level; 7] = [
        Level::Store,
        Level::Fastest,
        Level::Fast,
        Level::Normal,
        Level::Better,
        Level::Best,
        Level::Ultra,
    ];

    /// 0 (store) … 6 (ultra).
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Level from index 0–6 (clamped).
    pub fn from_index(i: usize) -> Self {
        Self::ALL[i.min(6)]
    }

    /// Parses `store`, `fast`, … or a 7-Zip style number `0`–`9`.
    pub fn parse(s: &str) -> Option<Level> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "store" | "0" => Level::Store,
            "fastest" | "1" => Level::Fastest,
            "fast" | "2" | "3" => Level::Fast,
            "normal" | "4" | "5" => Level::Normal,
            "better" | "6" | "7" => Level::Better,
            "best" | "8" | "9" => Level::Best,
            "ultra" | "max" | "maximum" => Level::Ultra,
            _ => return None,
        })
    }

    /// Lower-case name.
    pub const fn name(self) -> &'static str {
        match self {
            Level::Store => "store",
            Level::Fastest => "fastest",
            Level::Fast => "fast",
            Level::Normal => "normal",
            Level::Better => "better",
            Level::Best => "best",
            Level::Ultra => "ultra",
        }
    }
}

/// Options for creating an archive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WriteOptions {
    pub format: FormatId,
    pub level: Level,
    /// Method override by name (`Deflate`, `LZMA2`, `Zstandard`, …). `None` = format default.
    pub method: Option<String>,
    /// Worker threads; `0` = automatic.
    pub threads: u32,
    /// 7z solid compression.
    pub solid: bool,
    #[serde(skip_serializing)]
    pub password: Option<String>,
    /// Also encrypt the file list (7z).
    pub encrypt_names: bool,
    /// Split the output into volumes of this many bytes (`name.7z.001`, …).
    pub volume_size: Option<u64>,
    pub comment: Option<String>,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            format: FormatId::Zip,
            level: Level::Normal,
            method: None,
            threads: 0,
            solid: true,
            password: None,
            encrypt_names: false,
            volume_size: None,
            comment: None,
        }
    }
}

impl WriteOptions {
    /// Effective thread count (`0` resolves to the available parallelism).
    pub fn effective_threads(&self) -> u32 {
        if self.threads > 0 {
            self.threads
        } else {
            std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
        }
    }
}

/// One modification for in-place updates.
#[derive(Debug, Clone)]
pub enum UpdateOp {
    /// Add files under `dir` inside the archive (empty = root). Existing entries with the same
    /// path are replaced.
    Add { sources: Vec<crate::source::Source> },
    /// Delete entries whose path equals the given path or lies below it.
    Delete { paths: Vec<String> },
    /// Rename an entry (or a directory and everything below it).
    Rename { from: String, to: String },
}
