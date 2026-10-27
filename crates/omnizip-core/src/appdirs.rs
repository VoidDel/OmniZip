//! Per-user locations shared by the desktop app, the CLI and the shell integration, so all of them
//! read the same `config.toml` (`docs/02-architecture.md` §4.6).
//!
//! | | Windows | macOS | Linux |
//! |---|---|---|---|
//! | config | `%APPDATA%\OmniZip` | `~/Library/Application Support/OmniZip` | `$XDG_CONFIG_HOME/omnizip` |
//! | data | `%LOCALAPPDATA%\OmniZip` | `~/Library/Application Support/OmniZip` | `$XDG_DATA_HOME/omnizip` |
//! | cache | `%LOCALAPPDATA%\OmniZip\cache` | `~/Library/Caches/OmniZip` | `$XDG_CACHE_HOME/omnizip` |

use std::{env, path::PathBuf};

fn env_dir(var: &str) -> Option<PathBuf> {
    env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home() -> Option<PathBuf> {
    env_dir("HOME").or_else(|| env_dir("USERPROFILE"))
}

/// Overrides everything (portable installs, tests): `OMNIZIP_HOME`.
fn portable() -> Option<PathBuf> {
    env_dir("OMNIZIP_HOME")
}

/// Directory holding `config.toml` and user themes.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(p) = portable() {
        return Some(p);
    }
    if cfg!(windows) {
        env_dir("APPDATA").map(|p| p.join("OmniZip"))
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Application Support/OmniZip"))
    } else {
        env_dir("XDG_CONFIG_HOME")
            .or_else(|| home().map(|h| h.join(".config")))
            .map(|p| p.join("omnizip"))
    }
}

/// Directory for recent files and other state.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(p) = portable() {
        return Some(p.join("data"));
    }
    if cfg!(windows) {
        env_dir("LOCALAPPDATA").map(|p| p.join("OmniZip"))
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Application Support/OmniZip"))
    } else {
        env_dir("XDG_DATA_HOME")
            .or_else(|| home().map(|h| h.join(".local/share")))
            .map(|p| p.join("omnizip"))
    }
}

/// Directory for previews, files opened from archives, and logs.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(p) = portable() {
        return Some(p.join("cache"));
    }
    if cfg!(windows) {
        env_dir("LOCALAPPDATA").map(|p| p.join("OmniZip").join("cache"))
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Caches/OmniZip"))
    } else {
        env_dir("XDG_CACHE_HOME")
            .or_else(|| home().map(|h| h.join(".cache")))
            .map(|p| p.join("omnizip"))
    }
}

/// Path of the shared settings file.
pub fn config_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}
