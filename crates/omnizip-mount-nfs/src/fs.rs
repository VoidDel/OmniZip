//! NFSv3 requests on a [`VirtualFs`].
//!
//! NFS is stateless where the file system is not: the client names files by handle and reads
//! them by offset, without opening them. Open file handles are therefore kept per node and
//! closed after a while without use ([`IDLE`]). Listings are paged by the id of the last item
//! sent, so a listing is kept between pages. File ids are node ids plus one (the client takes 0
//! for "no file"); node ids never change while mounted. The calls into the file system block
//! (decoding), so they run on tokio's blocking threads; a panic in one fails that request only.
//!
//! The server hands out nothing without a secret (see the crate documentation): MOUNT works
//! once, and handles carry a tag keyed with a random key.

use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    hash::{BuildHasher, RandomState},
    os::unix::ffi::{OsStrExt, OsStringExt},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use nfsserve::{
    nfs::{
        fattr3, fileid3, filename3, fsinfo3, ftype3, nfs_fh3, nfspath3, nfsstat3, nfstime3,
        post_op_attr, sattr3, set_mtime, set_size3, specdata3,
    },
    vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities},
};
use omnizip_vfs::{Attr, FileHandle, FileType, FsError, NodeId, OpenFlags, ROOT, VirtualFs};
use unicode_normalization::UnicodeNormalization;

/// Open file handles unused for this long are closed.
const IDLE: Duration = Duration::from_secs(10);

/// Largest read or write: what the volume tells the client (`rsize`, `wsize`).
const MAX_IO: u32 = 1 << 20;

/// Items fetched from the file system at a time while listing a directory.
const PAGE: usize = 1024;

/// A file handle of the file system kept open for NFS reads and writes.
struct Open {
    fh: FileHandle,
    write: bool,
    used: Instant,
}

/// A directory listing kept while the client pages through it.
struct Listing {
    items: Arc<Vec<DirEntry>>,
    made: Instant,
}

pub(crate) struct Inner {
    fs: Arc<dyn VirtualFs>,
    writable: bool,
    uid: u32,
    gid: u32,
    fsid: u64,
    /// Keys of the handle tag.
    keys: (RandomState, RandomState),
    /// The one MOUNT has been answered.
    mounted: AtomicBool,
    opens: Mutex<HashMap<NodeId, Open>>,
    last_sweep: Mutex<Instant>,
    listings: Mutex<HashMap<NodeId, Listing>>,
    /// Parent of each node seen, for `..`.
    parents: Mutex<HashMap<NodeId, NodeId>>,
}

/// The volume as an NFS server sees it.
#[derive(Clone)]
pub(crate) struct NfsVolume(pub(crate) Arc<Inner>);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn fileid(node: NodeId) -> fileid3 {
    u64::from(node) + 1
}

fn node(id: fileid3) -> Result<NodeId, nfsstat3> {
    id.checked_sub(1)
        .and_then(|n| NodeId::try_from(n).ok())
        .ok_or(nfsstat3::NFS3ERR_STALE)
}

fn status(e: FsError) -> nfsstat3 {
    match e {
        FsError::NotFound => nfsstat3::NFS3ERR_NOENT,
        FsError::NotDir => nfsstat3::NFS3ERR_NOTDIR,
        FsError::IsDir => nfsstat3::NFS3ERR_ISDIR,
        FsError::ReadOnly => nfsstat3::NFS3ERR_ROFS,
        FsError::Access => nfsstat3::NFS3ERR_ACCES,
        FsError::Invalid => nfsstat3::NFS3ERR_INVAL,
        FsError::Exists => nfsstat3::NFS3ERR_EXIST,
        FsError::NotEmpty => nfsstat3::NFS3ERR_NOTEMPTY,
        FsError::BadHandle | FsError::TooManyOpen | FsError::Io | FsError::Interrupted => {
            nfsstat3::NFS3ERR_IO
        }
    }
}

/// A name from the client: NFC, as the volume's names are (the mount asks the client for NFC
/// too; this covers names that are not UTF-8 or come from another client).
fn name(n: &filename3) -> OsString {
    match std::str::from_utf8(&n.0) {
        Ok(s) => OsString::from(s.nfc().collect::<String>()),
        Err(_) => OsString::from_vec(n.0.clone()),
    }
}

fn time(t: SystemTime) -> nfstime3 {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    nfstime3 {
        seconds: u32::try_from(d.as_secs()).unwrap_or(u32::MAX),
        nseconds: d.subsec_nanos(),
    }
}

impl NfsVolume {
    pub(crate) fn new(fs: Arc<dyn VirtualFs>, fsid: u64) -> Self {
        // SAFETY: no arguments, cannot fail.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        Self(Arc::new(Inner {
            writable: fs.writable(),
            fs,
            uid,
            gid,
            fsid,
            keys: (RandomState::new(), RandomState::new()),
            mounted: AtomicBool::new(false),
            opens: Mutex::new(HashMap::new()),
            last_sweep: Mutex::new(Instant::now()),
            listings: Mutex::new(HashMap::new()),
            parents: Mutex::new(HashMap::new()),
        }))
    }

    /// Runs `f` on a blocking thread; a panic in it is an I/O error.
    async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Inner) -> Result<T, nfsstat3> + Send + 'static,
    ) -> Result<T, nfsstat3> {
        let inner = self.0.clone();
        tokio::task::spawn_blocking(move || {
            catch_unwind(AssertUnwindSafe(|| f(&inner))).unwrap_or_else(|_| {
                tracing::error!("mount: the file system panicked");
                Err(nfsstat3::NFS3ERR_IO)
            })
        })
        .await
        .unwrap_or(Err(nfsstat3::NFS3ERR_IO))
    }
}

impl Inner {
    /// What the client sees of a node: owned by the user who mounted it, and on a writable
    /// volume writable by them.
    fn fattr(&self, a: &Attr) -> fattr3 {
        let mut mode = a.mode & 0o7777;
        if self.writable {
            mode |= 0o200;
        }
        let t = time(a.mtime);
        fattr3 {
            ftype: match a.kind {
                FileType::Dir => ftype3::NF3DIR,
                FileType::File => ftype3::NF3REG,
                FileType::Symlink => ftype3::NF3LNK,
            },
            mode,
            nlink: a.nlink,
            uid: self.uid,
            gid: self.gid,
            size: a.size,
            used: a.size,
            rdev: specdata3::default(),
            fsid: self.fsid,
            fileid: fileid(a.node),
            atime: t,
            mtime: t,
            ctime: t,
        }
    }

    fn getattr(&self, node: NodeId) -> Result<fattr3, nfsstat3> {
        match self.fs.getattr(node) {
            Ok(a) => Ok(self.fattr(&a)),
            // A handle of a node gone since.
            Err(FsError::NotFound) => Err(nfsstat3::NFS3ERR_STALE),
            Err(e) => Err(status(e)),
        }
    }

    fn lookup(&self, dir: NodeId, n: &OsStr) -> Result<Attr, nfsstat3> {
        let a = self.fs.lookup(dir, n).map_err(status)?;
        lock(&self.parents).insert(a.node, dir);
        Ok(a)
    }

    /// A file handle of the file system for `node`, open for writing when `write` is set.
    fn handle(&self, node: NodeId, write: bool) -> Result<FileHandle, nfsstat3> {
        self.sweep();
        let mut opens = lock(&self.opens);
        if let Some(o) = opens.get_mut(&node)
            && (o.write || !write)
        {
            o.used = Instant::now();
            return Ok(o.fh);
        }
        let fh = self.fs.open(node, OpenFlags { write }).map_err(status)?;
        if let Some(old) = opens.insert(
            node,
            Open {
                fh,
                write,
                used: Instant::now(),
            },
        ) {
            self.fs.release(old.fh);
        }
        Ok(fh)
    }

    /// Closes the handles unused for [`IDLE`].
    fn sweep(&self) {
        {
            let mut last = lock(&self.last_sweep);
            if last.elapsed() < IDLE / 2 {
                return;
            }
            *last = Instant::now();
        }
        let idle: Vec<FileHandle> = {
            let mut opens = lock(&self.opens);
            let gone: Vec<NodeId> = opens
                .iter()
                .filter(|(_, o)| o.used.elapsed() >= IDLE)
                .map(|(n, _)| *n)
                .collect();
            gone.iter()
                .filter_map(|n| opens.remove(n))
                .map(|o| o.fh)
                .collect()
        };
        for fh in idle {
            self.fs.release(fh);
        }
    }

    /// Closes the handle of `node`, if open.
    fn close(&self, node: NodeId) {
        let open = lock(&self.opens).remove(&node);
        if let Some(o) = open {
            self.fs.release(o.fh);
        }
    }

    /// Closes every handle (the volume is going away).
    pub(crate) fn close_all(&self) {
        let all: Vec<Open> = lock(&self.opens).drain().map(|(_, o)| o).collect();
        for o in all {
            self.fs.release(o.fh);
        }
    }

    /// A directory changed: its listing is fetched again.
    fn changed(&self, dir: NodeId) {
        lock(&self.listings).remove(&dir);
    }

    /// The whole listing of `dir`, kept for the pages after the first.
    fn listing(&self, dir: NodeId, fresh: bool) -> Result<Arc<Vec<DirEntry>>, nfsstat3> {
        if !fresh && let Some(l) = lock(&self.listings).get(&dir) {
            return Ok(l.items.clone());
        }
        let mut items = Vec::new();
        let mut cursor = 0;
        loop {
            let page = self.fs.readdir(dir, cursor, PAGE).map_err(status)?;
            let Some(last) = page.last() else { break };
            cursor = last.cursor;
            let full = page.len() == PAGE;
            let mut parents = lock(&self.parents);
            for e in page {
                parents.insert(e.attr.node, dir);
                items.push(DirEntry {
                    fileid: fileid(e.attr.node),
                    name: e.name.into_bytes().into(),
                    attr: self.fattr(&e.attr),
                });
            }
            if !full {
                break;
            }
        }
        let items = Arc::new(items);
        let mut listings = lock(&self.listings);
        // Listings nobody pages through any more.
        listings.retain(|_, l| l.made.elapsed() < Duration::from_secs(60));
        listings.insert(
            dir,
            Listing {
                items: items.clone(),
                made: Instant::now(),
            },
        );
        Ok(items)
    }

    /// Applies the size and modification time of `s` (mode and owner are taken and ignored,
    /// as on the other platforms: `cp -p` must not fail).
    fn apply(&self, node: NodeId, s: &sattr3) -> Result<(), nfsstat3> {
        if let set_size3::size(n) = s.size {
            self.fs.set_size(node, n).map_err(status)?;
        }
        match s.mtime {
            set_mtime::SET_TO_CLIENT_TIME(t) => {
                let t = UNIX_EPOCH
                    + Duration::from_secs(u64::from(t.seconds))
                    + Duration::from_nanos(u64::from(t.nseconds));
                self.fs.set_mtime(node, t).map_err(status)?;
            }
            set_mtime::SET_TO_SERVER_TIME => {
                self.fs.set_mtime(node, SystemTime::now()).map_err(status)?;
            }
            set_mtime::DONT_CHANGE => {}
        }
        Ok(())
    }

    fn tag(&self, id: fileid3) -> [u8; 16] {
        let mut t = [0; 16];
        t[..8].copy_from_slice(&self.keys.0.hash_one(id).to_le_bytes());
        t[8..].copy_from_slice(&self.keys.1.hash_one(id).to_le_bytes());
        t
    }
}

#[async_trait]
impl NFSFileSystem for NfsVolume {
    fn capabilities(&self) -> VFSCapabilities {
        if self.0.writable {
            VFSCapabilities::ReadWrite
        } else {
            VFSCapabilities::ReadOnly
        }
    }

    fn root_dir(&self) -> fileid3 {
        fileid(ROOT)
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let dir = node(dirid)?;
        let n = name(filename);
        self.run(move |v| match n.as_bytes() {
            b"." => Ok(dirid),
            b".." => Ok(fileid(lock(&v.parents).get(&dir).copied().unwrap_or(ROOT))),
            _ => v.lookup(dir, &n).map(|a| fileid(a.node)),
        })
        .await
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        let n = node(id)?;
        self.run(move |v| v.getattr(n)).await
    }

    async fn setattr(&self, id: fileid3, setattr: sattr3) -> Result<fattr3, nfsstat3> {
        let n = node(id)?;
        self.run(move |v| {
            v.apply(n, &setattr)?;
            v.getattr(n)
        })
        .await
    }

    async fn read(
        &self,
        id: fileid3,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        let n = node(id)?;
        let count = count.min(MAX_IO);
        self.run(move |v| {
            let fh = v.handle(n, false)?;
            let data = v.fs.read(fh, offset, count).map_err(status)?;
            // Shorter than asked only at the end.
            let eof = (data.len() as u64) < u64::from(count);
            Ok((data, eof))
        })
        .await
    }

    async fn write(&self, id: fileid3, offset: u64, data: &[u8]) -> Result<fattr3, nfsstat3> {
        let n = node(id)?;
        let data = data.to_vec();
        self.run(move |v| {
            let fh = v.handle(n, true)?;
            v.fs.write(fh, offset, &data).map_err(status)?;
            v.getattr(n)
        })
        .await
    }

    async fn create(
        &self,
        dirid: fileid3,
        filename: &filename3,
        attr: sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        let dir = node(dirid)?;
        let n = name(filename);
        self.run(move |v| {
            // An open without O_EXCL of a file that exists opens that file.
            let a = match v.fs.create(dir, &n, false) {
                Ok(a) => a,
                Err(FsError::Exists) => {
                    let a = v.lookup(dir, &n)?;
                    if a.is_dir() {
                        return Err(nfsstat3::NFS3ERR_ISDIR);
                    }
                    a
                }
                Err(e) => return Err(status(e)),
            };
            lock(&v.parents).insert(a.node, dir);
            v.changed(dir);
            v.apply(a.node, &attr)?;
            Ok((fileid(a.node), v.getattr(a.node)?))
        })
        .await
    }

    async fn create_exclusive(
        &self,
        dirid: fileid3,
        filename: &filename3,
    ) -> Result<fileid3, nfsstat3> {
        let dir = node(dirid)?;
        let n = name(filename);
        self.run(move |v| {
            let a = v.fs.create(dir, &n, false).map_err(status)?;
            lock(&v.parents).insert(a.node, dir);
            v.changed(dir);
            Ok(fileid(a.node))
        })
        .await
    }

    async fn mkdir(
        &self,
        dirid: fileid3,
        dirname: &filename3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        let dir = node(dirid)?;
        let n = name(dirname);
        self.run(move |v| {
            let a = v.fs.create(dir, &n, true).map_err(status)?;
            lock(&v.parents).insert(a.node, dir);
            v.changed(dir);
            Ok((fileid(a.node), v.fattr(&a)))
        })
        .await
    }

    async fn remove(&self, dirid: fileid3, filename: &filename3) -> Result<(), nfsstat3> {
        let dir = node(dirid)?;
        let n = name(filename);
        self.run(move |v| {
            if let Ok(a) = v.fs.lookup(dir, &n) {
                v.close(a.node);
            }
            v.fs.remove(dir, &n).map_err(status)?;
            v.changed(dir);
            Ok(())
        })
        .await
    }

    async fn rename(
        &self,
        from_dirid: fileid3,
        from_filename: &filename3,
        to_dirid: fileid3,
        to_filename: &filename3,
    ) -> Result<(), nfsstat3> {
        let (from, to) = (node(from_dirid)?, node(to_dirid)?);
        let (a, b) = (name(from_filename), name(to_filename));
        self.run(move |v| {
            // A file replaced by the move is closed.
            if let Ok(old) = v.fs.lookup(to, &b) {
                v.close(old.node);
            }
            v.fs.rename(from, &a, to, &b, true).map_err(status)?;
            if let Ok(moved) = v.fs.lookup(to, &b) {
                lock(&v.parents).insert(moved.node, to);
            }
            v.changed(from);
            v.changed(to);
            Ok(())
        })
        .await
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let dir = node(dirid)?;
        self.run(move |v| {
            // A new listing starts from the file system; later pages continue the kept one.
            let mut items = v.listing(dir, start_after == 0)?;
            let start = if start_after == 0 {
                0
            } else {
                match items.iter().position(|e| e.fileid == start_after) {
                    Some(i) => i + 1,
                    None => {
                        items = v.listing(dir, true)?;
                        items
                            .iter()
                            .position(|e| e.fileid == start_after)
                            .map(|i| i + 1)
                            .ok_or(nfsstat3::NFS3ERR_BAD_COOKIE)?
                    }
                }
            };
            let end = (start + max_entries).min(items.len());
            Ok(ReadDirResult {
                entries: items[start..end]
                    .iter()
                    .map(|e| DirEntry {
                        fileid: e.fileid,
                        name: e.name.0.clone().into(),
                        attr: e.attr,
                    })
                    .collect(),
                end: end == items.len(),
            })
        })
        .await
    }

    async fn symlink(
        &self,
        _dirid: fileid3,
        _linkname: &filename3,
        _symlink: &nfspath3,
        _attr: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(if self.0.writable {
            nfsstat3::NFS3ERR_NOTSUPP
        } else {
            nfsstat3::NFS3ERR_ROFS
        })
    }

    async fn readlink(&self, id: fileid3) -> Result<nfspath3, nfsstat3> {
        let n = node(id)?;
        self.run(move |v| {
            let target = v.fs.readlink(n).map_err(status)?;
            Ok(target.into_os_string().into_vec().into())
        })
        .await
    }

    async fn fsinfo(&self, root_fileid: fileid3) -> Result<fsinfo3, nfsstat3> {
        let attr = match self.getattr(root_fileid).await {
            Ok(a) => post_op_attr::attributes(a),
            Err(_) => post_op_attr::Void,
        };
        Ok(fsinfo3 {
            obj_attributes: attr,
            rtmax: MAX_IO,
            rtpref: MAX_IO,
            rtmult: 4096,
            wtmax: MAX_IO,
            wtpref: MAX_IO,
            wtmult: 4096,
            dtpref: 64 * 1024,
            maxfilesize: u64::MAX >> 1,
            time_delta: nfstime3 {
                seconds: 0,
                nseconds: 1,
            },
            properties: nfsserve::nfs::FSF_SYMLINK
                | nfsserve::nfs::FSF_HOMOGENEOUS
                | nfsserve::nfs::FSF_CANSETTIME,
        })
    }

    /// The file id and a tag only this server can make.
    fn id_to_fh(&self, id: fileid3) -> nfs_fh3 {
        let mut data = id.to_le_bytes().to_vec();
        data.extend_from_slice(&self.0.tag(id));
        nfs_fh3 { data }
    }

    fn fh_to_id(&self, fh: &nfs_fh3) -> Result<fileid3, nfsstat3> {
        if fh.data.len() != 24 {
            return Err(nfsstat3::NFS3ERR_BADHANDLE);
        }
        let (id, tag) = fh.data.split_at(8);
        let id = u64::from_le_bytes(id.try_into().map_err(|_| nfsstat3::NFS3ERR_BADHANDLE)?);
        // Compared in full whatever differs, so that timing tells nothing.
        let diff = self
            .0
            .tag(id)
            .iter()
            .zip(tag)
            .fold(0u8, |d, (a, b)| d | (a ^ b));
        if diff == 0 {
            Ok(id)
        } else {
            Err(nfsstat3::NFS3ERR_BADHANDLE)
        }
    }

    /// Answers the one MOUNT of the volume (by `mount_nfs`, with the secret export path); any
    /// later one is refused.
    async fn path_to_id(&self, path: &[u8]) -> Result<fileid3, nfsstat3> {
        if self.0.mounted.swap(true, Ordering::SeqCst) {
            tracing::warn!("mount: a second MOUNT of the volume was refused");
            return Err(nfsstat3::NFS3ERR_ACCES);
        }
        if path.iter().all(|&b| b == b'/') {
            Ok(self.root_dir())
        } else {
            Err(nfsstat3::NFS3ERR_NOENT)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use omnizip_core::{CollectOptions, FormatId, JobCtx, OpenOptions, WriteOptions, source};
    use omnizip_vfs::{ArchiveFs, FsOptions};

    use super::*;

    fn volume(dir: &Path) -> NfsVolume {
        let src = dir.join("a.txt");
        std::fs::write(&src, "a").unwrap();
        let ctx = JobCtx::default();
        let sources = source::collect(&[src], &CollectOptions::default(), &ctx).unwrap();
        let zip = dir.join("a.zip");
        let wo = WriteOptions {
            format: FormatId::Zip,
            ..Default::default()
        };
        omnizip_formats::shared()
            .create(&zip, &sources, &wo, &ctx)
            .unwrap();
        let reader = omnizip_formats::shared()
            .open(&zip, &OpenOptions::default(), &ctx)
            .unwrap();
        NfsVolume::new(
            Arc::new(ArchiveFs::new(reader, FsOptions::default()).unwrap()),
            1,
        )
    }

    /// Only `mount_nfs`, with the secret export path, gets the root handle: once.
    #[test]
    fn the_volume_is_mounted_once() {
        let d = tempfile::tempdir().unwrap();
        let v = volume(d.path());
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert!(matches!(rt.block_on(v.path_to_id(b"/")), Ok(1)));
        assert!(matches!(
            rt.block_on(v.path_to_id(b"/")),
            Err(nfsstat3::NFS3ERR_ACCES)
        ));
    }

    /// Handles cannot be made up: not from a file id, not by changing one, not from another
    /// volume, not in `nfsserve`'s own format.
    #[test]
    fn handles_cannot_be_made_up() {
        let d = tempfile::tempdir().unwrap();
        let v = volume(d.path());
        let root = v.id_to_fh(1);
        assert!(matches!(v.fh_to_id(&root), Ok(1)));
        let mut changed = root.clone();
        changed.data[20] ^= 1;
        assert!(v.fh_to_id(&changed).is_err());
        let mut other_id = v.id_to_fh(2);
        other_id.data[..8].copy_from_slice(&1u64.to_le_bytes());
        assert!(v.fh_to_id(&other_id).is_err());
        let w = volume(d.path());
        assert!(w.fh_to_id(&root).is_err());
        let plain = nfs_fh3 {
            data: [0u64.to_le_bytes(), 1u64.to_le_bytes()].concat(),
        };
        assert!(v.fh_to_id(&plain).is_err());
    }
}
