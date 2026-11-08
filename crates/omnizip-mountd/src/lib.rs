//! `omnizip-mountd`, the mount daemon (`docs/12-mount.md` §4): it hosts a user's mounted
//! archives so that they stay mounted after OmniZip exits. This library holds what the daemon
//! and its clients share: the messages, opening an archive as a volume, saving the changes of a
//! writable mount, and the client with its transport: named pipes on Windows, a Unix socket on
//! Linux.

pub mod proto;
pub mod save;
pub mod volume;

#[cfg(windows)]
mod admin_dir;
#[cfg(any(windows, target_os = "linux"))]
pub mod client;
#[cfg(any(windows, target_os = "linux"))]
pub mod daemon;
#[cfg(windows)]
pub mod pipe;
#[cfg(target_os = "linux")]
pub mod sock;

pub use proto::{Level, MountSpec, MountStatus, Request, Response, SavedLayer};
pub use volume::open_volume;
