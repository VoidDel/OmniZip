//! Names as the volume shows them (`docs/12-mount.md` §6).
//!
//! The same cleaning rules as extraction, applied per component: characters the target
//! system forbids become `_`, Windows device names get a `_` suffix, long names are cut
//! while keeping the extension, and names that collide (case-insensitively on Windows and
//! macOS) get ` (2)`, ` (3)`, … in archive order.

use std::{borrow::Cow, collections::HashSet};

use omnizip_core::path::{MAX_COMPONENT_BYTES, numbered_name, truncate_keep_extension};

/// Naming rules of the system the volume is mounted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameRules {
    /// Apply Windows character and device-name rules.
    pub windows: bool,
    /// Names differing only in case are the same name.
    pub case_insensitive: bool,
}

impl NameRules {
    /// Rules of the running system.
    pub const fn current() -> Self {
        Self {
            windows: cfg!(windows),
            case_insensitive: cfg!(any(windows, target_os = "macos")),
        }
    }

    /// Key used to compare names.
    pub fn fold<'a>(&self, name: &'a str) -> Cow<'a, str> {
        if self.case_insensitive && name.chars().any(char::is_uppercase) {
            Cow::Owned(name.to_lowercase())
        } else {
            Cow::Borrowed(name)
        }
    }
}

impl Default for NameRules {
    fn default() -> Self {
        Self::current()
    }
}

const WINDOWS_DEVICES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Makes one stored name usable on the target system.
pub fn display_name(raw: &str, rules: NameRules) -> String {
    let mut s: String = raw
        .chars()
        .map(|c| {
            let forbidden = c == '/'
                || c.is_control()
                || (rules.windows && matches!(c, '<' | '>' | ':' | '"' | '\\' | '|' | '?' | '*'));
            if forbidden { '_' } else { c }
        })
        .collect();
    if s.is_empty() || s == "." || s == ".." {
        return "_".into();
    }
    if rules.windows {
        // Win32 drops trailing dots and spaces, which would make the file unreachable.
        if s.ends_with(['.', ' ']) {
            s.pop();
            s.push('_');
        }
        let stem_end = s.find('.').unwrap_or(s.len());
        if WINDOWS_DEVICES.contains(&s[..stem_end].to_ascii_lowercase().as_str()) {
            s.insert(stem_end, '_');
        }
    }
    if s.len() > MAX_COMPONENT_BYTES {
        s = truncate_keep_extension(&s, MAX_COMPONENT_BYTES);
    }
    s
}

/// Unique display names for the children of one directory, in the given order.
pub fn assign<'a>(raw: impl IntoIterator<Item = &'a str>, rules: NameRules) -> Vec<String> {
    let mut taken: HashSet<String> = HashSet::new();
    raw.into_iter()
        .map(|r| {
            let base = display_name(r, rules);
            let mut name = base.clone();
            let mut n = 2;
            while !taken.insert(rules.fold(&name).into_owned()) {
                // Leave room for the " (n)" suffix.
                let room = truncate_keep_extension(&base, MAX_COMPONENT_BYTES - 12);
                name = numbered_name(&room, n);
                n += 1;
            }
            name
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: NameRules = NameRules {
        windows: true,
        case_insensitive: true,
    };
    const UNIX: NameRules = NameRules {
        windows: false,
        case_insensitive: false,
    };

    #[test]
    fn windows_names_are_cleaned() {
        assert_eq!(display_name("a:b?.txt", WIN), "a_b_.txt");
        assert_eq!(display_name("trailing.", WIN), "trailing_");
        assert_eq!(display_name("space ", WIN), "space_");
        assert_eq!(display_name("CON.txt", WIN), "CON_.txt");
        assert_eq!(display_name("nul", WIN), "nul_");
        assert_eq!(display_name("console.txt", WIN), "console.txt");
        assert_eq!(display_name("..", WIN), "_");
        assert_eq!(display_name("tab\there", WIN), "tab_here");
    }

    #[test]
    fn unix_keeps_what_unix_allows() {
        assert_eq!(display_name("a:b?.txt", UNIX), "a:b?.txt");
        assert_eq!(display_name("CON", UNIX), "CON");
        assert_eq!(display_name("trailing.", UNIX), "trailing.");
    }

    #[test]
    fn long_names_keep_their_extension() {
        let long = format!("{}.mp4", "x".repeat(400));
        let d = display_name(&long, UNIX);
        assert!(d.len() <= MAX_COMPONENT_BYTES);
        assert!(d.ends_with(".mp4"));
    }

    #[test]
    fn collisions_are_numbered_in_order() {
        let names = assign(["Read.me", "read.me", "README", "a:b", "a_b"], WIN);
        assert_eq!(
            names,
            ["Read.me", "read (2).me", "README", "a_b", "a_b (2)"]
        );
        let names = assign(["Read.me", "read.me"], UNIX);
        assert_eq!(names, ["Read.me", "read.me"]);
    }
}
