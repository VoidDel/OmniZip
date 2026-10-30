//! What OmniZip and `omnizip-mountd` say to each other: one JSON object per line, a request
//! from the client answered by one response (a `Watch` connection keeps receiving `Changed`).

use std::path::PathBuf;

use omnizip_core::{Error, ErrorReport};
pub use omnizip_vfs::Changes;
use serde::{Deserialize, Serialize};

/// Which daemon: the user's own, or the elevated one that hosts "mount as administrator".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    User,
    Admin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Request {
    /// Mounts an archive, or returns its mount when it is mounted already.
    Mount {
        spec: MountSpec,
    },
    /// Unmounts; the changes of a writable mount are kept for saving when `keep_changes` is set
    /// (`Unmounted`), and discarded otherwise.
    Unmount {
        id: u64,
        #[serde(default)]
        keep_changes: bool,
    },
    /// Unmounts everything, except writable mounts with changes when `keep_changed` is set.
    UnmountAll {
        #[serde(default)]
        keep_changed: bool,
    },
    List,
    /// Keeps the connection, sending `Changed` whenever the list changes.
    Watch,
}

/// An archive to mount, with the settings that apply to it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountSpec {
    pub archive: PathBuf,
    /// `Z:` or a directory; the next free drive letter when absent.
    pub mount_point: Option<PathBuf>,
    /// The volume label; derived from the archive name when absent.
    pub label: Option<String>,
    pub password: Option<String>,
    /// Entry name encoding (`NameEncoding::label`); detected when absent.
    pub encoding: Option<String>,
    /// Mark files offline so Explorer does not read them for thumbnails.
    pub offline_attribute: bool,
    /// Disk cache limit in bytes.
    pub cache_bytes: u64,
    /// Decode a solid archive into the cache right away.
    pub warm: bool,
    /// Writes go to a temporary layer, saved into the archive or discarded at unmount.
    #[serde(default)]
    pub writable: bool,
}

/// A mount as the daemon reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountStatus {
    pub id: u64,
    pub archive: PathBuf,
    pub label: String,
    /// `Z:` or a directory.
    pub mount_point: PathBuf,
    /// Format name (`ZIP`, `7z`, …).
    pub format: String,
    /// Bytes decoded into the cache (memory and disk).
    pub cache_bytes: u64,
    /// A solid archive is being decoded into the cache.
    pub warming: bool,
    /// A drive registered with the Mount Manager, visible to every session.
    pub global: bool,
    /// Accepts writes into a temporary layer.
    pub writable: bool,
    /// The layer holds changes.
    pub dirty: bool,
    /// The changes can be saved into the archive (`save::can_save`).
    pub saveable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "camelCase")]
pub enum Response {
    Mounted {
        mount: MountStatus,
    },
    Mounts {
        mounts: Vec<MountStatus>,
    },
    /// The changes kept by `Unmount`, if any.
    Unmounted {
        saved: Option<SavedLayer>,
    },
    Done,
    Changed,
    Failed {
        error: ErrorReport,
    },
}

/// The changes of a writable mount, kept at unmount for saving into the archive. The layer
/// directory holds the changed files at their volume paths; whoever saves removes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedLayer {
    pub archive: PathBuf,
    pub layer: PathBuf,
    pub changes: Changes,
    /// The archive's password and name encoding, as mounted.
    pub password: Option<String>,
    pub encoding: Option<String>,
}

/// Where kept layers wait to be saved: a directory no daemon clears at start-up.
pub fn saving_dir() -> Option<PathBuf> {
    omnizip_core::appdirs::cache_dir().map(|d| d.join("mount-saving"))
}

impl Response {
    pub fn failed(e: &Error) -> Self {
        Self::Failed {
            error: e.to_report(),
        }
    }

    /// The daemon's error as `Err`.
    pub fn into_result(self) -> Result<Self, Error> {
        match self {
            Self::Failed { error } => Err(Error::from_report(&error)),
            r => Ok(r),
        }
    }
}

#[cfg(test)]
mod tests {
    use omnizip_core::ErrorCode;

    use super::*;

    #[test]
    fn requests_and_responses_round_trip_as_lines() {
        let req = Request::Mount {
            spec: MountSpec {
                archive: "D:\\a b\\相册.zip".into(),
                mount_point: Some("Z:".into()),
                password: Some("pw".into()),
                cache_bytes: 2 << 30,
                ..Default::default()
            },
        };
        let line = serde_json::to_string(&req).unwrap();
        assert!(!line.contains('\n'));
        let Request::Mount { spec } = serde_json::from_str(&line).unwrap() else {
            panic!("{line}")
        };
        assert_eq!(spec.archive, PathBuf::from("D:\\a b\\相册.zip"));
        assert_eq!(spec.cache_bytes, 2 << 30);

        let e = Error::new(ErrorCode::WrongPassword).with("path", "a.zip");
        let line = serde_json::to_string(&Response::failed(&e)).unwrap();
        let back = serde_json::from_str::<Response>(&line)
            .unwrap()
            .into_result()
            .unwrap_err();
        assert_eq!(back.code(), ErrorCode::WrongPassword);
        assert_eq!(back.param("path"), Some("a.zip"));
    }
}
