//! `omnizip-mountd`, the mount daemon (`docs/12-mount.md` §4): it hosts a user's mounted
//! archives so that they stay mounted after OmniZip exits. This library holds what the daemon
//! and its clients share: the messages, opening an archive as a volume, and on Windows the
//! pipes and the client.

pub mod proto;
pub mod volume;

#[cfg(windows)]
pub mod client;
#[cfg(windows)]
pub mod daemon;
#[cfg(windows)]
pub mod pipe;

pub use proto::{Level, MountSpec, MountStatus, Request, Response, SavedMount, saved_list_path};
pub use volume::open_volume;
