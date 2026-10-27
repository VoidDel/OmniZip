//! Entry-path sanitization (`docs/09-security.md` §3).
//!
//! Every entry path goes through the same pipeline before anything touches the file system:
//! separator normalization → rejection of absolute / drive / UNC / `..` paths → per-component
//! clean-up (control and bidi characters, Windows-reserved names and characters, trailing dots,
//! length) → NFC normalization. The result is a list of plain components that can only ever be
//! joined *below* the destination directory.

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::{Error, ErrorCode};

/// Which file-system rules to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetOs {
    Windows,
    Unix,
}

impl TargetOs {
    /// The OS this binary runs on.
    pub const fn current() -> Self {
        if cfg!(windows) {
            TargetOs::Windows
        } else {
            TargetOs::Unix
        }
    }
}

/// What to do with `..` components and absolute prefixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UnsafePathMode {
    /// Refuse the entry (default).
    #[default]
    Reject,
    /// Drop dangerous components and keep the rest (explicit CLI opt-in only).
    Strip,
}

/// A problem found while sanitizing (used for warnings even when the path was repaired).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PathIssue {
    Absolute,
    DrivePrefix,
    ParentDir,
    Empty,
    ReservedName,
    InvalidCharacters,
    BidiControl,
    TooLong,
}

/// Outcome of sanitization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedPath {
    /// Safe components, never empty, never `.` or `..`.
    pub components: Vec<String>,
    /// Issues that were repaired (the path differs from the original).
    pub repaired: Vec<PathIssue>,
}

impl SanitizedPath {
    /// Components joined with `/`.
    pub fn to_slash_path(&self) -> String {
        self.components.join("/")
    }

    /// The last component.
    pub fn file_name(&self) -> &str {
        self.components.last().map_or("", String::as_str)
    }
}

/// Maximum bytes per component (common to NTFS/ext4/APFS, counted in UTF-8 here).
pub const MAX_COMPONENT_BYTES: usize = 255;
/// Maximum total path length accepted from an archive.
pub const MAX_PATH_CHARS: usize = 32_000;

/// Splits an archive entry name into logical components for display (no repairs, no
/// rejection): separators are normalized and empty / `.` components dropped.
pub fn logical_components(name: &str) -> Vec<&str> {
    name.split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != ".")
        .collect()
}

/// Sanitizes an archive entry name.
pub fn sanitize(name: &str, os: TargetOs, mode: UnsafePathMode) -> Result<SanitizedPath, Error> {
    let reject = |issue: PathIssue| {
        Error::new(ErrorCode::PathEscape)
            .with("path", name)
            .with("issue", issue_name(issue))
    };
    if name.chars().count() > MAX_PATH_CHARS {
        return Err(reject(PathIssue::TooLong));
    }
    let mut repaired = Vec::new();
    let normalized = name.replace('\\', "/");
    let mut rest = normalized.as_str();

    // UNC (`//server/share`) and device paths (`//?/C:/`).
    if rest.starts_with("//") {
        if mode == UnsafePathMode::Reject {
            return Err(reject(PathIssue::DrivePrefix));
        }
        repaired.push(PathIssue::DrivePrefix);
        rest = rest.trim_start_matches('/');
        // Drop the server / device component.
        rest = rest.split_once('/').map_or("", |(_, r)| r);
    }
    // Drive letters (`C:` / `C:/`). On Unix `a:b` is an ordinary name.
    let b = rest.as_bytes();
    if os == TargetOs::Windows && b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        if mode == UnsafePathMode::Reject {
            return Err(reject(PathIssue::DrivePrefix));
        }
        repaired.push(PathIssue::DrivePrefix);
        rest = &rest[2..];
    }
    if rest.starts_with('/') {
        if mode == UnsafePathMode::Reject {
            return Err(reject(PathIssue::Absolute));
        }
        repaired.push(PathIssue::Absolute);
        rest = rest.trim_start_matches('/');
    }

    let mut components = Vec::new();
    for raw in rest.split('/') {
        if raw.is_empty() || raw == "." {
            continue;
        }
        if raw == ".." {
            if mode == UnsafePathMode::Reject {
                return Err(reject(PathIssue::ParentDir));
            }
            repaired.push(PathIssue::ParentDir);
            continue;
        }
        let (clean, issues) = clean_component(raw, os);
        repaired.extend(issues);
        components.push(clean);
    }
    if components.is_empty() {
        return Err(reject(PathIssue::Empty));
    }
    repaired.sort_by_key(|i| *i as u8);
    repaired.dedup();
    Ok(SanitizedPath {
        components,
        repaired,
    })
}

fn issue_name(i: PathIssue) -> &'static str {
    match i {
        PathIssue::Absolute => "absolute",
        PathIssue::DrivePrefix => "drive-prefix",
        PathIssue::ParentDir => "parent-dir",
        PathIssue::Empty => "empty",
        PathIssue::ReservedName => "reserved-name",
        PathIssue::InvalidCharacters => "invalid-characters",
        PathIssue::BidiControl => "bidi-control",
        PathIssue::TooLong => "too-long",
    }
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{061C}')
}

fn is_zero_width(c: char) -> bool {
    matches!(
        c,
        '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}'
    )
}

const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "COM¹", "COM²", "COM³", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
    "LPT9", "LPT¹", "LPT²", "LPT³", "CONIN$", "CONOUT$",
];

fn is_windows_reserved(component: &str) -> bool {
    // Reserved with or without an extension: "con.txt" is just as reserved as "CON".
    let stem = component
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ');
    WINDOWS_RESERVED
        .iter()
        .any(|r| r.eq_ignore_ascii_case(stem))
}

fn clean_component(raw: &str, os: TargetOs) -> (String, Vec<PathIssue>) {
    let mut issues = Vec::new();
    // NFC normalization is silent: it is not a repair the user needs to hear about.
    let mut s: String = raw.nfc().collect();
    if s.chars().any(is_bidi_control) {
        issues.push(PathIssue::BidiControl);
        s = s
            .chars()
            .map(|c| if is_bidi_control(c) { '_' } else { c })
            .collect();
    }
    if s.chars().any(is_zero_width) {
        issues.push(PathIssue::InvalidCharacters);
        s.retain(|c| !is_zero_width(c));
    }
    let bad_char = |c: char| {
        c.is_control()
            || c == '\0'
            || os == TargetOs::Windows && matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')
    };
    if s.chars().any(bad_char) {
        issues.push(PathIssue::InvalidCharacters);
        s = s
            .chars()
            .map(|c| if bad_char(c) { '_' } else { c })
            .collect();
    }
    if os == TargetOs::Windows {
        let trimmed = s.trim_end_matches(['.', ' ']);
        if trimmed.len() != s.len() {
            issues.push(PathIssue::InvalidCharacters);
            s = trimmed.to_owned();
        }
        if is_windows_reserved(&s) {
            issues.push(PathIssue::ReservedName);
            s.insert(0, '_');
        }
    }
    if s.is_empty() || s == "." || s == ".." {
        issues.push(PathIssue::InvalidCharacters);
        s = "_".to_owned();
    }
    if s.len() > MAX_COMPONENT_BYTES {
        issues.push(PathIssue::TooLong);
        s = truncate_keep_extension(&s, MAX_COMPONENT_BYTES);
    }
    (s, issues)
}

/// Truncates a file name to `max` bytes on a char boundary, preserving a short extension.
pub fn truncate_keep_extension(name: &str, max: usize) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && name.len() - i <= 16 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let budget = max.saturating_sub(ext.len());
    let mut end = budget.min(stem.len());
    while end > 0 && !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &stem[..end], ext)
}

/// Produces a non-existing sibling name: `a.txt` → `a (2).txt`, `a (3).txt`, …
pub fn numbered_name(file_name: &str, n: u32) -> String {
    match file_name.rfind('.') {
        Some(i) if i > 0 => format!("{} ({n}){}", &file_name[..i], &file_name[i..]),
        _ => format!("{file_name} ({n})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(name: &str, os: TargetOs) -> String {
        sanitize(name, os, UnsafePathMode::Reject)
            .unwrap()
            .to_slash_path()
    }

    fn err(name: &str) -> ErrorCode {
        sanitize(name, TargetOs::Windows, UnsafePathMode::Reject)
            .unwrap_err()
            .code()
    }

    #[test]
    fn plain_paths_pass() {
        assert_eq!(ok("a/b/c.txt", TargetOs::Unix), "a/b/c.txt");
        assert_eq!(ok("a\\b\\c.txt", TargetOs::Windows), "a/b/c.txt");
        assert_eq!(ok("./a//b/./c", TargetOs::Unix), "a/b/c");
        assert_eq!(ok("dir/", TargetOs::Unix), "dir");
    }

    #[test]
    fn zip_slip_variants_are_rejected() {
        for bad in [
            "../evil.txt",
            "a/../../evil.txt",
            "/etc/passwd",
            "\\Windows\\System32\\x.dll",
            "C:/Windows/x.dll",
            "C:x.dll",
            "\\\\server\\share\\x",
            "//?/C:/x",
            "..",
            "a/..",
        ] {
            assert_eq!(err(bad), ErrorCode::PathEscape, "{bad}");
        }
        assert_eq!(err(""), ErrorCode::PathEscape);
        assert_eq!(err("./."), ErrorCode::PathEscape);
    }

    #[test]
    fn strip_mode_keeps_the_safe_tail() {
        let s = sanitize("../../a/b.txt", TargetOs::Unix, UnsafePathMode::Strip).unwrap();
        assert_eq!(s.to_slash_path(), "a/b.txt");
        assert!(s.repaired.contains(&PathIssue::ParentDir));
        let s = sanitize("C:\\x\\y", TargetOs::Windows, UnsafePathMode::Strip).unwrap();
        assert_eq!(s.to_slash_path(), "x/y");
        let s = sanitize(
            "\\\\srv\\share\\f",
            TargetOs::Windows,
            UnsafePathMode::Strip,
        )
        .unwrap();
        assert_eq!(s.to_slash_path(), "share/f");
    }

    #[test]
    fn windows_rules() {
        assert_eq!(ok("CON", TargetOs::Windows), "_CON");
        assert_eq!(ok("dir/con.txt", TargetOs::Windows), "dir/_con.txt");
        assert_eq!(ok("lpt1.tar.gz", TargetOs::Windows), "_lpt1.tar.gz");
        assert_eq!(ok("console.txt", TargetOs::Windows), "console.txt");
        assert_eq!(ok("a<b>:c|d?e*f\"g", TargetOs::Windows), "a_b__c_d_e_f_g");
        assert_eq!(ok("trailing. ", TargetOs::Windows), "trailing");
        assert_eq!(ok("dots...", TargetOs::Windows), "dots");
        assert_eq!(ok("...", TargetOs::Windows), "_");
        // Unix keeps characters that are legal there.
        assert_eq!(ok("a:b", TargetOs::Unix), "a:b");
        assert_eq!(ok("CON", TargetOs::Unix), "CON");
    }

    #[test]
    fn control_and_bidi_characters() {
        let s = sanitize(
            "evil\u{202E}gpj.exe",
            TargetOs::Unix,
            UnsafePathMode::Reject,
        )
        .unwrap();
        assert_eq!(s.to_slash_path(), "evil_gpj.exe");
        assert!(s.repaired.contains(&PathIssue::BidiControl));
        assert_eq!(ok("a\u{0007}b", TargetOs::Unix), "a_b");
        assert_eq!(ok("zero\u{200B}width", TargetOs::Unix), "zerowidth");
    }

    #[test]
    fn nfc_normalization() {
        // "é" as e + combining acute (NFD, as written by macOS) becomes the NFC code point.
        assert_eq!(
            ok("caf\u{0065}\u{0301}.txt", TargetOs::Unix),
            "caf\u{00e9}.txt"
        );
    }

    #[test]
    fn long_components_are_truncated() {
        let long = format!("{}.txt", "长".repeat(200));
        let s = ok(&long, TargetOs::Unix);
        assert!(s.len() <= MAX_COMPONENT_BYTES);
        assert!(s.ends_with(".txt"));
        assert!(sanitize(&"a/".repeat(20_000), TargetOs::Unix, UnsafePathMode::Reject).is_err());
    }

    #[test]
    fn numbering() {
        assert_eq!(numbered_name("a.txt", 2), "a (2).txt");
        assert_eq!(numbered_name("Makefile", 3), "Makefile (3)");
        assert_eq!(numbered_name(".env", 2), ".env (2)");
    }
}
