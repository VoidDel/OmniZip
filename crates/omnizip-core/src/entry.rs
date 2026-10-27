//! Archive entries as seen by every front end.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Index of an entry inside one opened archive (stable for the lifetime of the reader).
pub type EntryId = u32;

/// Kind of an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Hardlink,
    /// Device nodes, FIFOs and other special files. Never extracted.
    Other,
}

/// One entry of an archive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Position in [`crate::ArchiveReader::entries`].
    pub id: EntryId,
    /// Name bytes exactly as stored (used to re-decode with another encoding).
    #[serde(skip)]
    pub raw_name: Vec<u8>,
    /// Decoded name with `/` separators, as stored (not yet sanitized for extraction).
    pub path: String,
    pub kind: EntryKind,
    /// Uncompressed size in bytes (0 when unknown, see `size_known`).
    pub size: u64,
    /// Whether `size` is exact. Single-stream formats (xz, zstd) may not record it.
    pub size_known: bool,
    /// Compressed size when the format records it per entry.
    pub packed_size: Option<u64>,
    #[serde(with = "opt_millis")]
    pub mtime: Option<SystemTime>,
    pub unix_mode: Option<u32>,
    pub win_attrs: Option<u32>,
    pub crc32: Option<u32>,
    pub encrypted: bool,
    /// Compression method name (`Deflate`, `LZMA2`, …).
    pub method: Option<String>,
    /// Target of a symbolic or hard link, when known without reading data.
    pub link_target: Option<String>,
    pub comment: Option<String>,
}

impl Entry {
    /// A new file entry with defaults; engines fill in what they know.
    pub fn new(id: EntryId, path: String, kind: EntryKind) -> Self {
        Self {
            id,
            raw_name: path.as_bytes().to_vec(),
            path,
            kind,
            size: 0,
            size_known: true,
            packed_size: None,
            mtime: None,
            unix_mode: None,
            win_attrs: None,
            crc32: None,
            encrypted: false,
            method: None,
            link_target: None,
            comment: None,
        }
    }

    /// Whether this is a directory.
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }

    /// Last path component.
    pub fn name(&self) -> &str {
        self.path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&self.path)
    }
}

/// Serializes `Option<SystemTime>` as milliseconds since the Unix epoch.
pub mod opt_millis {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &Option<SystemTime>, s: S) -> Result<S::Ok, S::Error> {
        match t {
            Some(t) => s.serialize_some(&to_millis(*t)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<SystemTime>, D::Error> {
        Ok(Option::<i64>::deserialize(d)?.map(from_millis))
    }
}

/// Milliseconds since the Unix epoch (negative before 1970).
pub fn to_millis(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis().min(i64::MAX as u128) as i64,
        Err(e) => -(e.duration().as_millis().min(i64::MAX as u128) as i64),
    }
}

/// Inverse of [`to_millis`].
pub fn from_millis(ms: i64) -> SystemTime {
    if ms >= 0 {
        UNIX_EPOCH + Duration::from_millis(ms as u64)
    } else {
        UNIX_EPOCH - Duration::from_millis(ms.unsigned_abs())
    }
}
