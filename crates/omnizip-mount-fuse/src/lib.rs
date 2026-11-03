//! Linux mount backend (`docs/12-mount.md` §3): any [`omnizip_vfs::VirtualFs`] as a folder,
//! through FUSE3.
//!
//! The crate speaks the FUSE protocol on `/dev/fuse` itself (`fuser`, without libfuse) and
//! mounts through the setuid helper `fusermount3`, which every distribution ships with its fuse3
//! package; nothing is needed to build. A volume is mounted on an empty folder, by default
//! `$XDG_RUNTIME_DIR/omnizip/mnt/<label>`, with the file system type `fuse.omnizip`
//! ([`FS_TYPE`]), by which the context menu recognizes it. On other systems the crate only
//! exports [`BUILT`] and [`FS_TYPE`].

/// Whether this build contains the backend.
pub const BUILT: bool = cfg!(target_os = "linux");

/// File system type of the volumes, as `/proc/self/mountinfo` lists them.
pub const FS_TYPE: &str = "fuse.omnizip";

#[cfg(target_os = "linux")]
mod fs;
#[cfg(target_os = "linux")]
mod volume;

#[cfg(target_os = "linux")]
pub use volume::{Fuse, clear_stale, mount_root};
