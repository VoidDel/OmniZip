//! Security policy limits (`docs/09-security.md` §4).

use serde::{Deserialize, Serialize};

use crate::{Error, ErrorCode};

/// Limits applied while opening and extracting untrusted archives.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SecurityPolicy {
    /// Expansion ratio (unpacked / archive size) that triggers the bomb check.
    pub max_ratio: u64,
    /// The ratio check only applies when the unpacked total exceeds this many bytes.
    pub ratio_min_bytes: u64,
    /// Skip the ratio check entirely (`--allow-high-ratio`).
    pub allow_high_ratio: bool,
    /// Maximum number of entries.
    pub max_entries: u64,
    /// Maximum nesting depth when opening archives inside archives.
    pub max_nesting: u32,
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        Self {
            max_ratio: 1000,
            ratio_min_bytes: 1 << 30,
            allow_high_ratio: false,
            max_entries: 10_000_000,
            max_nesting: 8,
        }
    }
}

impl SecurityPolicy {
    /// Whether an archive of `archive_size` bytes declaring `unpacked` bytes looks like a bomb.
    pub fn is_high_ratio(&self, unpacked: u64, archive_size: u64) -> bool {
        if self.allow_high_ratio || unpacked <= self.ratio_min_bytes {
            return false;
        }
        unpacked / archive_size.max(1) > self.max_ratio
    }

    /// Error used when the ratio check fails and the user did not confirm.
    pub fn bomb_error(&self, unpacked: u64, archive_size: u64) -> Error {
        Error::new(ErrorCode::CompressionBomb)
            .with("ratio", unpacked / archive_size.max(1))
            .with("size", unpacked)
    }

    /// Checks the entry count.
    pub fn check_entries(&self, count: u64) -> Result<(), Error> {
        if count > self.max_entries {
            return Err(Error::new(ErrorCode::TooManyEntries).with("count", count));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio() {
        let p = SecurityPolicy::default();
        assert!(!p.is_high_ratio(500 << 20, 1), "small totals never trigger");
        assert!(p.is_high_ratio(4 << 30, 1 << 20));
        assert!(!p.is_high_ratio(4 << 30, 1 << 30));
        let p = SecurityPolicy {
            allow_high_ratio: true,
            ..p
        };
        assert!(!p.is_high_ratio(4 << 30, 1));
    }
}
