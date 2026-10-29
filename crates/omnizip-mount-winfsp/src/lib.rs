//! Windows mount backend (`docs/12-mount.md` §3): any [`omnizip_vfs::VirtualFs`] as a
//! read-only drive letter or directory, through WinFsp.
//!
//! The WinFsp library is loaded at run time from the WinFsp installation, so OmniZip starts,
//! and everything but mounting works, on systems without WinFsp. [`ffi`] declares only what a
//! read-only file system needs, following `inc/winfsp/fsctl.h` and `winfsp.h` of WinFsp 2.x;
//! no WinFsp SDK or `libclang` is needed to build.
//!
//! The volume is a disk volume (`WinFsp.Disk`), not a network one: only disk volumes can be
//! mounted on directories. On other systems the crate only exports [`BUILT`].

/// Whether this build contains the backend.
pub const BUILT: bool = cfg!(windows);

#[cfg(windows)]
mod ffi;
#[cfg(windows)]
mod volume;

#[cfg(windows)]
pub use volume::{WinFsp, free_drive_letters};
