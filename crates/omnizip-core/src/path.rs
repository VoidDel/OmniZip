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

/// Symbolic links followed while resolving one link before it counts as a loop (Linux `ELOOP`).
const MAX_LINK_HOPS: usize = 40;

/// Whether a link target is absolute or names a drive (`/x`, `\x`, `C:`), never inside a root.
fn absolute_target(t: &str) -> bool {
    t.starts_with(['/', '\\']) || t.as_bytes().get(1) == Some(&b':')
}

/// Resolves the target of a symbolic link in the directory `base` (components below a root)
/// as the system would: components that are themselves links are followed (`link_at` gives the
/// target of the link at a path, `None` for anything else), and `..` after a followed link
/// leaves where that link points, not where it is. Comparing the target text alone is not
/// enough: with `sub/up -> ..`, the target `../up/..` of a link in `sub/x/` reads as `sub` but
/// leads to the root's parent.
///
/// Returns the components the target resolves to, or `None` when it leaves the root, is
/// absolute, or loops.
pub fn resolve_link(
    base: &[String],
    target: &str,
    mut link_at: impl FnMut(&[String]) -> Option<String>,
) -> Option<Vec<String>> {
    if absolute_target(target) {
        return None;
    }
    let mut at = base.to_vec();
    let mut todo: std::collections::VecDeque<String> =
        target.split(['/', '\\']).map(str::to_owned).collect();
    let mut hops = 0;
    while let Some(c) = todo.pop_front() {
        match c.as_str() {
            "" | "." => {}
            ".." => {
                at.pop()?;
            }
            _ => {
                at.push(c);
                if let Some(t) = link_at(&at) {
                    hops += 1;
                    if hops > MAX_LINK_HOPS || absolute_target(&t) {
                        return None;
                    }
                    at.pop();
                    for part in t.split(['/', '\\']).rev() {
                        todo.push_front(part.to_owned());
                    }
                }
            }
        }
    }
    Some(at)
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
    // Repairs can lengthen names (a `_` prefix, NFC): the result must pass the limit as well.
    let chars: usize = components.iter().map(|c| c.chars().count() + 1).sum();
    if chars - 1 > MAX_PATH_CHARS {
        return Err(reject(PathIssue::TooLong));
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
        // A removed character may have kept a combining mark from its base: normalize again,
        // or sanitizing the result would change it once more.
        s = s.nfc().collect();
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
    // Truncate before the Windows rules: a cut can end in a space or a dot, which Windows then
    // drops (two entries landing on one file), and sanitizing the result would cut again.
    if s.len() > MAX_COMPONENT_BYTES {
        issues.push(PathIssue::TooLong);
        s = truncate_keep_extension(&s, MAX_COMPONENT_BYTES);
    }
    if os == TargetOs::Windows {
        if trim_windows_end(&mut s) {
            issues.push(PathIssue::InvalidCharacters);
        }
        if is_windows_reserved(&s) {
            issues.push(PathIssue::ReservedName);
            s.insert(0, '_');
            // The prefix can push a 255-byte name over; the leading `_` keeps it unreserved.
            if s.len() > MAX_COMPONENT_BYTES {
                s = truncate_keep_extension(&s, MAX_COMPONENT_BYTES);
                trim_windows_end(&mut s);
            }
        }
    }
    if s.is_empty() || s == "." || s == ".." {
        issues.push(PathIssue::InvalidCharacters);
        s = "_".to_owned();
    }
    (s, issues)
}

/// Drops the trailing dots and spaces Windows would drop itself. Returns whether any were.
fn trim_windows_end(s: &mut String) -> bool {
    let keep = s.trim_end_matches(['.', ' ']).len();
    let trimmed = keep != s.len();
    s.truncate(keep);
    trimmed
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

    #[test]
    fn links_resolve_through_other_links() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let links: std::collections::HashMap<&str, &str> = [
            ("sub/up", ".."),
            ("cur", "releases/v2"),
            ("loop", "loop"),
            ("abs", "/etc"),
            ("drive", "C:\\Windows"),
        ]
        .into();
        let at = |p: &[String]| links.get(p.join("/").as_str()).map(|t| t.to_string());
        // Plain targets.
        assert_eq!(
            resolve_link(&s(&["a"]), "../b.txt", at),
            Some(s(&["b.txt"]))
        );
        assert_eq!(
            resolve_link(&s(&["a"]), "c/./d", at),
            Some(s(&["a", "c", "d"]))
        );
        assert_eq!(resolve_link(&s(&["a"]), "../../etc/passwd", at), None);
        assert_eq!(resolve_link(&[], "/etc/passwd", at), None);
        assert_eq!(resolve_link(&[], "\\server\\share", at), None);
        assert_eq!(resolve_link(&[], "C:\\Windows", at), None);
        // `..` after a link leaves where the link points: `sub/up` is the root, so its parent
        // is outside, though the text `sub/x/../up/..` reads as `sub`.
        assert_eq!(resolve_link(&s(&["sub", "x"]), "../up/..", at), None);
        assert_eq!(
            resolve_link(&s(&["sub", "x"]), "../up/file", at),
            Some(s(&["file"]))
        );
        assert_eq!(resolve_link(&s(&["sub"]), "up", at), Some(vec![]));
        assert_eq!(
            resolve_link(&s(&["bin"]), "../cur/bin/app", at),
            Some(s(&["releases", "v2", "bin", "app"]))
        );
        // Links that loop or lead to an absolute path.
        assert_eq!(resolve_link(&[], "loop/x", at), None);
        assert_eq!(resolve_link(&[], "abs/passwd", at), None);
        assert_eq!(resolve_link(&[], "drive", at), None);
    }

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

    /// Sanitizing a sanitized name changes nothing (the nightly fuzzer's #1 #4 #6 #9 #11).
    #[test]
    fn repairs_are_final() {
        let again = |name: &str, os| {
            let once = ok(name, os);
            assert_eq!(ok(&once, os), once, "{name:?}");
            once
        };
        // The cut lands in a run of spaces: Windows would drop them, so they go now.
        let s = again(
            &format!("{}{}x", "a".repeat(250), " ".repeat(20)),
            TargetOs::Windows,
        );
        assert_eq!(s, "a".repeat(250));
        // On Unix the spaces are legal and stay.
        let s = again(
            &format!("{}{}x", "a".repeat(250), " ".repeat(20)),
            TargetOs::Unix,
        );
        assert_eq!(s.len(), MAX_COMPONENT_BYTES);
        // A reserved 255-byte name: the `_` prefix is kept within the limit.
        let s = again(&format!("CON.{}", "b".repeat(251)), TargetOs::Windows);
        assert!(s.starts_with("_CON.") && s.len() == MAX_COMPONENT_BYTES);
        // A removed zero-width character no longer separates e from its combining accent.
        assert_eq!(again("e\u{200B}\u{0301}", TargetOs::Unix), "\u{00e9}");
    }

    #[test]
    fn numbering() {
        assert_eq!(numbered_name("a.txt", 2), "a (2).txt");
        assert_eq!(numbered_name("Makefile", 3), "Makefile (3)");
        assert_eq!(numbered_name(".env", 2), ".env (2)");
    }
}
