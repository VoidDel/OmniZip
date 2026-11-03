//! FUSE requests on a [`VirtualFs`].
//!
//! The kernel addresses nodes by inode number: inode = node + 1, FUSE's root being 1 and the
//! file system's 0. Node ids never change while mounted (a writable layer keeps a node's id when
//! it is copied up or moved), so `forget` has nothing to drop. Each request runs on one of the
//! session's threads; a panic in the file system becomes `EIO` for that request only.
//!
//! The kernel caches attributes, names and data: attributes and names for a second, as on
//! Windows (sizes a format does not record become known once decoded), and file data until the
//! file changes (`FOPEN_KEEP_CACHE`; everything that changes a file goes through the kernel).
//! Listings come with attributes (`READDIRPLUS`), which halves the time of listing a tree.

use std::{
    cell::RefCell,
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, InitFlags,
    KernelConfig, LockOwner, OpenAccMode, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate,
    ReplyData, ReplyDirectory, ReplyDirectoryPlus, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs,
    ReplyWrite, Request, TimeOrNow, WriteFlags,
};
use omnizip_vfs::{Attr, FsError, FsResult, NodeId, VirtualFs};

/// How long the kernel keeps attributes and names.
const TTL: Duration = Duration::from_secs(1);

/// Largest read or write request.
pub(crate) const MAX_IO: u32 = 1 << 20;

/// Directory items fetched from the file system at a time.
const PAGE: usize = 256;

pub(crate) struct Volume {
    fs: Arc<dyn VirtualFs>,
    writable: bool,
    uid: u32,
    gid: u32,
    /// Where a writable volume keeps its changes, for the free space it reports.
    layer: Option<PathBuf>,
}

thread_local! {
    /// A read buffer per session thread.
    static BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn node(ino: INodeNo) -> NodeId {
    (u64::from(ino) - 1) as NodeId
}

fn ino(node: NodeId) -> INodeNo {
    INodeNo(u64::from(node) + 1)
}

/// Runs a file system call; a panic in it is an I/O error.
fn call<T>(f: impl FnOnce() -> FsResult<T>) -> Result<T, Errno> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(Errno::from_i32(e.errno())),
        Err(_) => {
            tracing::error!("mount: the file system panicked");
            Err(Errno::EIO)
        }
    }
}

fn kind(a: &Attr) -> FileType {
    match a.kind {
        omnizip_vfs::FileType::Dir => FileType::Directory,
        omnizip_vfs::FileType::File => FileType::RegularFile,
        omnizip_vfs::FileType::Symlink => FileType::Symlink,
    }
}

impl Volume {
    pub(crate) fn new(fs: Arc<dyn VirtualFs>) -> Self {
        // SAFETY: no arguments, cannot fail.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        Self {
            writable: fs.writable(),
            layer: fs.layer_dir(),
            fs,
            uid,
            gid,
        }
    }

    /// What the kernel sees of a node: owned by the user who mounted it, and on a writable
    /// volume writable by them (an archive's own modes would refuse the changes the layer
    /// takes).
    fn attr(&self, a: &Attr) -> FileAttr {
        let mut perm = (a.mode & 0o7777) as u16;
        if self.writable {
            perm |= 0o200;
        }
        FileAttr {
            ino: ino(a.node),
            size: a.size,
            blocks: a.size.div_ceil(512),
            atime: a.mtime,
            mtime: a.mtime,
            ctime: a.mtime,
            crtime: a.mtime,
            kind: kind(a),
            perm,
            nlink: a.nlink,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    /// Opens a file this volume created, for `create`.
    fn create_open(&self, parent: NodeId, name: &OsStr) -> FsResult<(Attr, u64)> {
        let a = self.fs.create(parent, name, false)?;
        let fh = self
            .fs
            .open(a.node, omnizip_vfs::OpenFlags { write: true })?;
        Ok((a, fh.0))
    }

    /// Removes `name` of `parent` when it is a directory (`dir`) or is not.
    fn remove(&self, parent: INodeNo, name: &OsStr, dir: bool) -> Result<(), Errno> {
        call(|| {
            let a = self.fs.lookup(node(parent), name)?;
            match (dir, a.is_dir()) {
                (true, false) => Err(FsError::NotDir),
                (false, true) => Err(FsError::IsDir),
                _ => self.fs.remove(node(parent), name),
            }
        })
    }

    /// The items of a directory from `offset`, `.` and `..` first, until `add` reports the
    /// reply full. Offsets 1 and 2 stand for `.` and `..`; an item's is its cursor plus 2.
    fn list(
        &self,
        dir: INodeNo,
        offset: u64,
        mut add: impl FnMut(u64, &str, Option<&Attr>) -> bool,
    ) -> Result<(), Errno> {
        for (off, name) in [(1, "."), (2, "..")] {
            if offset < off && add(off, name, None) {
                return Ok(());
            }
        }
        let mut cursor = offset.saturating_sub(2);
        loop {
            let page = call(|| self.fs.readdir(node(dir), cursor, PAGE))?;
            if page.is_empty() {
                return Ok(());
            }
            for e in page {
                if add(e.cursor + 2, &e.name, Some(&e.attr)) {
                    return Ok(());
                }
                cursor = e.cursor;
            }
        }
    }
}

impl Filesystem for Volume {
    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> std::io::Result<()> {
        // Larger requests than the default 128 KiB: a third fewer round trips on large files.
        let _ = config.set_max_write(MAX_IO);
        let _ = config.set_max_readahead(MAX_IO);
        let _ = config.set_max_background(64);
        let wanted = InitFlags::FUSE_PARALLEL_DIROPS
            | InitFlags::FUSE_CACHE_SYMLINKS
            | InitFlags::FUSE_DO_READDIRPLUS
            | InitFlags::FUSE_READDIRPLUS_AUTO;
        let _ = config.add_capabilities(wanted & config.capabilities());
        Ok(())
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        match call(|| self.fs.lookup(node(parent), name)) {
            Ok(a) => reply.entry(&TTL, &self.attr(&a), Generation(0)),
            Err(e) => reply.error(e),
        }
    }

    fn getattr(&self, _req: &Request, i: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match call(|| self.fs.getattr(node(i))) {
            Ok(a) => reply.attr(&TTL, &self.attr(&a)),
            Err(e) => reply.error(e),
        }
    }

    /// Size and time; modes and owners are not kept (as on Windows), so changing them succeeds
    /// without effect, which keeps `cp -p` and the like from failing.
    fn setattr(
        &self,
        _req: &Request,
        i: INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let r = call(|| {
            let n = node(i);
            if let Some(size) = size {
                self.fs.set_size(n, size)?;
            }
            if let Some(t) = mtime {
                self.fs.set_mtime(
                    n,
                    match t {
                        TimeOrNow::SpecificTime(t) => t,
                        TimeOrNow::Now => SystemTime::now(),
                    },
                )?;
            }
            self.fs.getattr(n)
        });
        match r {
            Ok(a) => reply.attr(&TTL, &self.attr(&a)),
            Err(e) => reply.error(e),
        }
    }

    fn readlink(&self, _req: &Request, i: INodeNo, reply: ReplyData) {
        match call(|| self.fs.readlink(node(i))) {
            Ok(p) => reply.data(p.as_os_str().as_bytes()),
            Err(e) => reply.error(e),
        }
    }

    fn mknod(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        _rdev: u32,
        reply: ReplyEntry,
    ) {
        // Regular files only: the layer keeps no devices, pipes or sockets.
        if mode & libc::S_IFMT != libc::S_IFREG {
            return reply.error(Errno::EPERM);
        }
        match call(|| self.fs.create(node(parent), name, false)) {
            Ok(a) => reply.entry(&TTL, &self.attr(&a), Generation(0)),
            Err(e) => reply.error(e),
        }
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        match call(|| self.fs.create(node(parent), name, true)) {
            Ok(a) => reply.entry(&TTL, &self.attr(&a), Generation(0)),
            Err(e) => reply.error(e),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        match self.remove(parent, name, false) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        match self.remove(parent, name, true) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        if flags.intersects(RenameFlags::RENAME_EXCHANGE | RenameFlags::RENAME_WHITEOUT) {
            return reply.error(Errno::EINVAL);
        }
        let replace = !flags.contains(RenameFlags::RENAME_NOREPLACE);
        match call(|| {
            self.fs
                .rename(node(parent), name, node(newparent), newname, replace)
        }) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn open(&self, _req: &Request, i: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let write = flags.acc_mode() != OpenAccMode::O_RDONLY;
        match call(|| self.fs.open(node(i), omnizip_vfs::OpenFlags { write })) {
            Ok(fh) => reply.opened(FileHandle(fh.0), FopenFlags::FOPEN_KEEP_CACHE),
            Err(e) => reply.error(e),
        }
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        if mode & libc::S_IFMT != libc::S_IFREG && mode & libc::S_IFMT != 0 {
            return reply.error(Errno::EPERM);
        }
        match call(|| self.create_open(node(parent), name)) {
            Ok((a, fh)) => reply.created(
                &TTL,
                &self.attr(&a),
                Generation(0),
                FileHandle(fh),
                FopenFlags::FOPEN_KEEP_CACHE,
            ),
            Err(e) => reply.error(e),
        }
    }

    fn read(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let size = size.min(MAX_IO) as usize;
        BUF.with_borrow_mut(|buf| {
            if buf.len() < size {
                buf.resize(size, 0);
            }
            let fh = omnizip_vfs::FileHandle(fh.0);
            match call(|| self.fs.read_into(fh, offset, &mut buf[..size])) {
                Ok(n) => reply.data(&buf[..n]),
                Err(e) => reply.error(e),
            }
        })
    }

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        match call(|| self.fs.write(omnizip_vfs::FileHandle(fh.0), offset, data)) {
            Ok(n) => reply.written(n),
            Err(e) => reply.error(e),
        }
    }

    /// Every `close`: nothing to do, the layer is written through.
    fn flush(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _lock: LockOwner,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    fn fsync(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        match call(|| self.fs.flush(omnizip_vfs::FileHandle(fh.0))) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let _ = call(|| {
            self.fs.release(omnizip_vfs::FileHandle(fh.0));
            Ok(())
        });
        reply.ok();
    }

    fn opendir(&self, _req: &Request, i: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        match call(|| self.fs.getattr(node(i))) {
            // A read-only volume's listings never change: the kernel may keep them.
            Ok(a) if a.is_dir() => reply.opened(
                FileHandle(0),
                if self.writable {
                    FopenFlags::empty()
                } else {
                    FopenFlags::FOPEN_CACHE_DIR
                },
            ),
            Ok(_) => reply.error(Errno::ENOTDIR),
            Err(e) => reply.error(e),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        dir: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let r = self.list(dir, offset, |off, name, a| {
            let (i, k) = a.map_or((dir, FileType::Directory), |a| (ino(a.node), kind(a)));
            reply.add(i, off, k, name)
        });
        match r {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn readdirplus(
        &self,
        _req: &Request,
        dir: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectoryPlus,
    ) {
        let here = match call(|| self.fs.getattr(node(dir))) {
            Ok(a) => self.attr(&a),
            Err(e) => return reply.error(e),
        };
        let r = self.list(dir, offset, |off, name, a| {
            // The kernel takes no entry from `.` and `..`, only their names.
            let attr = a.map_or(here, |a| self.attr(a));
            reply.add(attr.ino, off, name, &TTL, &attr, Generation(0))
        });
        match r {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    /// The archive's size, nothing free on a read-only volume; on a writable one, the free
    /// space of the disk holding the layer.
    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let s = match call(|| self.fs.statfs()) {
            Ok(s) => s,
            Err(e) => return reply.error(e),
        };
        let bs = u64::from(s.block_size.max(512));
        let (mut blocks, mut free) = (s.total_bytes.div_ceil(bs), 0);
        if let Some(layer) = &self.layer
            && let Some(avail) = free_bytes(layer)
        {
            free = avail / bs;
            blocks += free;
        }
        reply.statfs(
            blocks,
            free,
            free,
            s.files + s.dirs + 1,
            0,
            bs as u32,
            s.max_name_len,
            bs as u32,
        );
    }
}

/// Bytes available to this user on the disk holding `dir`.
fn free_bytes(dir: &std::path::Path) -> Option<u64> {
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: all-zero is a valid statvfs; `path` is NUL-terminated.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(st.f_bavail as u64 * st.f_frsize as u64)
}
