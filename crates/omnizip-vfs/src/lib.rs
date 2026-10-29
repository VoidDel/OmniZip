//! OmniZip mount core (`docs/12-mount.md`).
//!
//! * [`VirtualFs`] — the node-based interface every platform backend drives (WinFsp on
//!   Windows, FUSE on Linux, FSKit or an NFS loopback on macOS);
//! * [`ArchiveFs`] — any archive OmniZip can read, presented as a read-only file system;
//! * [`cache`] — turns sequential decoders into random access (memory for small entries,
//!   spool files for large ones, both bounded and least-recently-used);
//! * [`names`] — how stored names are shown on the target system;
//! * [`backend`] — the backend trait and the installation probe used by the UI and the CLI.
//!
//! The interface only takes [`NodeId`]s, never path strings, so `..` cannot escape the
//! volume. Backends that receive paths (WinFsp) resolve them with [`resolve`].

pub mod archivefs;
pub mod backend;
pub mod cache;
pub mod names;

use std::{ffi::OsStr, fmt, path::PathBuf, time::SystemTime};

pub use archivefs::{ArchiveFs, FsOptions, LinkMode};
pub use backend::{
    BackendCaps, BackendHealth, BackendKind, Mount, MountBackend, MountError, MountOptions, Probe,
    probe,
};
pub use cache::{CacheLimits, Content, ContentCache};
pub use names::NameRules;

/// A node of the volume. The root is [`ROOT`].
pub type NodeId = omnizip_core::NodeId;

/// Root node id.
pub const ROOT: NodeId = 0;

/// An open file or directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileHandle(pub u64);

/// What a node looks like to the operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Dir,
    File,
    Symlink,
}

/// Node attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    pub node: NodeId,
    pub kind: FileType,
    /// Bytes: the declared uncompressed size, or the link text for symbolic links.
    pub size: u64,
    pub mtime: SystemTime,
    /// Unix permission bits: never writable, never setuid or setgid.
    pub mode: u32,
    pub nlink: u32,
}

impl Attr {
    pub fn is_dir(&self) -> bool {
        self.kind == FileType::Dir
    }
}

/// One item of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub attr: Attr,
    /// Pass back to [`VirtualFs::readdir`] to continue after this item.
    pub cursor: u64,
}

/// How a node is opened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpenFlags {
    pub write: bool,
}

/// Volume information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsStats {
    pub total_bytes: u64,
    pub files: u64,
    pub dirs: u64,
    pub block_size: u32,
    pub max_name_len: u32,
    /// Volume label: the archive name without its extension.
    pub label: String,
    /// File-system name shown by the system.
    pub fs_name: &'static str,
    /// Volume serial number, derived from the archive's path, size and time.
    pub serial: u32,
}

/// File-system errors, mapped by backends to `errno` or `NTSTATUS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    NotFound,
    NotDir,
    IsDir,
    ReadOnly,
    /// A password is missing or wrong.
    Access,
    BadHandle,
    TooManyOpen,
    Invalid,
    /// Decoding failed, the data is corrupt, or an entry produced more than it declared.
    Io,
    Interrupted,
}

impl FsError {
    /// POSIX `errno` (FUSE, NFS).
    pub const fn errno(self) -> i32 {
        match self {
            FsError::NotFound => 2,
            FsError::Interrupted => 4,
            FsError::Io => 5,
            FsError::BadHandle => 9,
            FsError::Access => 13,
            FsError::NotDir => 20,
            FsError::IsDir => 21,
            FsError::Invalid => 22,
            FsError::TooManyOpen => 24,
            FsError::ReadOnly => 30,
        }
    }

    /// `NTSTATUS` (WinFsp).
    pub const fn ntstatus(self) -> u32 {
        match self {
            FsError::NotFound => 0xC000_0034,    // STATUS_OBJECT_NAME_NOT_FOUND
            FsError::NotDir => 0xC000_0103,      // STATUS_NOT_A_DIRECTORY
            FsError::IsDir => 0xC000_00BA,       // STATUS_FILE_IS_A_DIRECTORY
            FsError::ReadOnly => 0xC000_00A2,    // STATUS_MEDIA_WRITE_PROTECTED
            FsError::Access => 0xC000_0022,      // STATUS_ACCESS_DENIED
            FsError::BadHandle => 0xC000_0008,   // STATUS_INVALID_HANDLE
            FsError::TooManyOpen => 0xC000_011F, // STATUS_TOO_MANY_OPENED_FILES
            FsError::Invalid => 0xC000_000D,     // STATUS_INVALID_PARAMETER
            FsError::Io => 0xC000_00E9,          // STATUS_UNEXPECTED_IO_ERROR
            FsError::Interrupted => 0xC000_0120, // STATUS_CANCELLED
        }
    }
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for FsError {}

impl From<omnizip_core::Error> for FsError {
    fn from(e: omnizip_core::Error) -> Self {
        use omnizip_core::ErrorCode as C;
        match e.code() {
            C::NotFound => FsError::NotFound,
            C::WrongPassword | C::PasswordRequired | C::PermissionDenied => FsError::Access,
            C::Cancelled => FsError::Interrupted,
            C::InvalidArgument => FsError::Invalid,
            _ => FsError::Io,
        }
    }
}

impl From<std::io::Error> for FsError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::NotFound => FsError::NotFound,
            std::io::ErrorKind::PermissionDenied => FsError::Access,
            std::io::ErrorKind::Interrupted => FsError::Interrupted,
            _ => FsError::Io,
        }
    }
}

pub type FsResult<T> = Result<T, FsError>;

/// A read-only file system that a backend can mount.
///
/// Every method may be called from several threads at once.
pub trait VirtualFs: Send + Sync {
    /// Child `name` of directory `parent`, case-folded when the volume is case-insensitive.
    fn lookup(&self, parent: NodeId, name: &OsStr) -> FsResult<Attr>;
    fn getattr(&self, node: NodeId) -> FsResult<Attr>;
    /// Up to `max` items after `cursor` (`0` starts at the beginning), without `.` and `..`.
    fn readdir(&self, dir: NodeId, cursor: u64, max: usize) -> FsResult<Vec<DirEntry>>;
    fn open(&self, node: NodeId, flags: OpenFlags) -> FsResult<FileHandle>;
    /// Up to `size` bytes at `offset`; shorter only at the end of the file.
    fn read(&self, fh: FileHandle, offset: u64, size: u32) -> FsResult<Vec<u8>>;
    fn release(&self, fh: FileHandle);
    fn readlink(&self, node: NodeId) -> FsResult<PathBuf>;
    fn statfs(&self) -> FsResult<FsStats>;
}

/// Resolves a volume-relative path (`/` or `\` separated) by successive lookups.
pub fn resolve(fs: &dyn VirtualFs, path: &str) -> FsResult<Attr> {
    let mut attr = fs.getattr(ROOT)?;
    for comp in path
        .split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != ".")
    {
        if comp == ".." {
            return Err(FsError::Invalid);
        }
        if !attr.is_dir() {
            return Err(FsError::NotDir);
        }
        attr = fs.lookup(attr.node, OsStr::new(comp))?;
    }
    Ok(attr)
}

/// Reads a whole file through the interface, `chunk` bytes at a time.
pub fn read_all(fs: &dyn VirtualFs, node: NodeId, chunk: u32) -> FsResult<Vec<u8>> {
    let fh = fs.open(node, OpenFlags::default())?;
    let mut out = Vec::new();
    let result = loop {
        match fs.read(fh, out.len() as u64, chunk) {
            Ok(b) if b.is_empty() => break Ok(()),
            Ok(b) => out.extend_from_slice(&b),
            Err(e) => break Err(e),
        }
    };
    fs.release(fh);
    result.map(|()| out)
}
