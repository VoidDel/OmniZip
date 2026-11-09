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

    /// The most an archive of `archive_size` bytes may unpack to before
    /// [`is_high_ratio`](Self::is_high_ratio) trips: a hard cap where nobody can be asked to
    /// confirm (a mounted volume).
    pub fn max_unpacked(&self, archive_size: u64) -> u64 {
        if self.allow_high_ratio {
            return u64::MAX;
        }
        // Below (ratio + 1) × size the integer ratio is at most `max_ratio`.
        match archive_size
            .max(1)
            .checked_mul(self.max_ratio.saturating_add(1))
        {
            Some(limit) => self.ratio_min_bytes.max(limit - 1),
            None => u64::MAX,
        }
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
        assert_eq!(p.max_unpacked(1), u64::MAX);
    }

    #[test]
    fn max_unpacked_is_the_last_size_that_passes() {
        let p = SecurityPolicy::default();
        for size in [0, 1, 1000, 1 << 20, 3 << 20, 7_777_777, 1 << 40, u64::MAX] {
            let max = p.max_unpacked(size);
            assert!(!p.is_high_ratio(max, size), "{size}");
            if max < u64::MAX {
                assert!(p.is_high_ratio(max + 1, size), "{size}");
            }
        }
        assert_eq!(
            p.max_unpacked(1 << 10),
            1 << 30,
            "small archives get the floor"
        );
    }
}
