//! What OmniZip and `omnizip-mountd` say to each other: one JSON object per line, a request
//! from the client answered by one response (a `Watch` connection keeps receiving `Changed`).

use std::path::PathBuf;

use omnizip_core::{Error, ErrorReport};
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
    Unmount {
        id: u64,
    },
    UnmountAll,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "camelCase")]
pub enum Response {
    Mounted { mount: MountStatus },
    Mounts { mounts: Vec<MountStatus> },
    Done,
    Changed,
    Failed { error: ErrorReport },
}

/// A mount to restore at start-up. The user's daemon keeps the list of its mounts in
/// `mounts.json` in the data directory, written at every change.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedMount {
    pub archive: String,
    pub mount_point: String,
    pub label: String,
}

/// Where the user's daemon saves its list.
pub fn saved_list_path() -> Option<PathBuf> {
    omnizip_core::appdirs::data_dir().map(|d| d.join("mounts.json"))
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
