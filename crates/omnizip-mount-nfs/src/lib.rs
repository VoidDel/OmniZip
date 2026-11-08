//! macOS mount backend (`docs/12-mount.md` §3): any [`omnizip_vfs::VirtualFs`] as a volume,
//! through an NFSv3 server on the loopback interface and the system's own NFS client.
//!
//! Each volume gets a server of its own (`nfsserve`, on `127.0.0.1` and a port the system
//! picks), which `/sbin/mount_nfs` mounts on an empty folder, by default
//! `~/Library/Application Support/OmniZip/Volumes/<label>` ([`mount_root`]). Nothing is
//! installed, no kernel extension is loaded and nothing needs signing, unlike an FSKit
//! extension, which stays the plan once the app is signed (§12).
//!
//! The server answers anyone on the machine who reaches its port, so it hands out nothing
//! without a secret: the export path is random and is mounted once (a second MOUNT is refused),
//! and every file handle carries a tag keyed with a random key, so that handles cannot be made
//! up. On other systems the crate only exports [`BUILT`].

/// Whether this build contains the backend.
pub const BUILT: bool = cfg!(target_os = "macos");

#[cfg(target_os = "macos")]
mod fs;
#[cfg(target_os = "macos")]
mod volume;

#[cfg(target_os = "macos")]
pub use volume::{Nfs, clear_stale, mount_root};
