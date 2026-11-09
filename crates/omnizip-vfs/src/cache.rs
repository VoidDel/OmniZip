//! Content cache (`docs/12-mount.md` §5).
//!
//! Most archive codecs only decode forward, but a mounted volume is read at random offsets
//! (video seeking, databases, `mmap`). An entry is therefore decoded once into the cache and
//! then served by offset: small entries live in memory, large ones in a private spool file.
//! Both pools have a budget; the least recently used entries are dropped first. Content that
//! a reader still holds stays valid after eviction (it is reference counted), and a spool
//! file is deleted when its last holder lets go.
//!
//! Every entry being decoded has a [`Partial`] its readers wait on; a large one can be read
//! while it is decoded: readers wait only until the bytes they ask for are in the spool file,
//! not for the whole entry.

use std::{
    collections::HashMap,
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use omnizip_core::EntryId;
use tempfile::{NamedTempFile, TempDir, TempPath};

use crate::{FsError, FsResult};

/// Budgets of the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheLimits {
    /// Memory pool (default 256 MiB).
    pub memory: u64,
    /// Spool-file pool (default 2 GiB).
    pub disk: u64,
    /// Entries up to this size are kept in memory (default 8 MiB).
    pub inline_max: u64,
    /// Free space spool files leave on the disk holding them (default 1 GiB). The budgets
    /// above are kept by dropping finished entries, and one entry may be larger than the whole
    /// pool (a film in a ZIP): this is what keeps a large or lying entry from filling the disk.
    pub disk_reserve: u64,
}

impl Default for CacheLimits {
    fn default() -> Self {
        Self {
            memory: 256 << 20,
            disk: 2 << 30,
            inline_max: 8 << 20,
            disk_reserve: 1 << 30,
        }
    }
}

/// A spool file and its length.
#[derive(Debug)]
pub struct Spool {
    file: File,
    len: u64,
    _path: TempPath,
}

/// Decoded content of one entry.
#[derive(Debug, Clone)]
pub enum Content {
    Mem(Arc<[u8]>),
    Disk(Arc<Spool>),
}

impl Content {
    /// Length in bytes.
    pub fn len(&self) -> u64 {
        match self {
            Content::Mem(b) => b.len() as u64,
            Content::Disk(s) => s.len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn on_disk(&self) -> bool {
        matches!(self, Content::Disk(_))
    }

    /// Up to `size` bytes at `offset` (shorter at the end).
    pub fn read_at(&self, offset: u64, size: usize) -> io::Result<Vec<u8>> {
        let n = (size as u64).min(self.len().saturating_sub(offset)) as usize;
        let mut buf = vec![0; n];
        self.read_into(offset, &mut buf)?;
        Ok(buf)
    }

    /// Copies up to `buf.len()` bytes at `offset` into `buf`; the number copied (fewer at the
    /// end).
    pub fn read_into(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let len = self.len();
        if offset >= len {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(len - offset) as usize;
        match self {
            Content::Mem(b) => {
                buf[..n].copy_from_slice(&b[offset as usize..offset as usize + n]);
            }
            Content::Disk(s) => {
                if crate::pos::read_full_at(&s.file, &mut buf[..n], offset)? < n {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
            }
        }
        Ok(n)
    }
}

/// How long a read waits on a decoder that makes no progress before it fails
/// (`docs/12-mount.md` §7): Explorer and other programs hang while a read is pending.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// How often a waiting read looks at the progress of the decoding pass.
const PROGRESS_POLL: Duration = Duration::from_secs(1);

/// What one decoding pass has produced so far, for every entry it decodes. An entry deep in a
/// solid block or a tar.gz waits while the pass decodes the entries before it: that is the
/// pass's progress, which keeps the entry's readers from taking it for a stuck decoder.
#[derive(Debug, Default)]
pub struct Progress {
    bytes: AtomicU64,
    /// A file the engine writes itself (RAR), whose growth counts too.
    writing: Mutex<Option<PathBuf>>,
}

impl Progress {
    /// Counts `n` bytes produced.
    pub fn add(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::Relaxed);
    }

    /// Sets or clears the file the engine is writing.
    pub fn writing(&self, path: Option<&Path>) {
        *self.writing.lock().unwrap_or_else(|e| e.into_inner()) = path.map(Path::to_owned);
    }

    /// Bytes produced so far.
    pub fn value(&self) -> u64 {
        let file = self
            .writing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map_or(0, |m| m.len());
        self.bytes.load(Ordering::Relaxed).saturating_add(file)
    }
}

/// An entry being decoded, whose readers wait until it is done or, for a large entry spooled as
/// it is decoded, until the bytes they want are written.
pub struct Partial {
    /// A handle on the spool file, set when it is created.
    file: OnceLock<File>,
    state: Mutex<PartialState>,
    grew: Condvar,
    /// A read fails when the decoding pass, once started, produces nothing for this long.
    stall: Duration,
    /// The decoding pass this entry is part of.
    progress: Arc<Progress>,
}

#[derive(Default)]
struct PartialState {
    /// The decoder has the archive and is decoding (before that it waits for another decode to
    /// let go of the archive, which is that decode's progress, not this one's).
    started: bool,
    written: u64,
    /// The finished content, or why decoding stopped.
    done: Option<FsResult<Content>>,
}

impl Default for Partial {
    fn default() -> Self {
        Self::new(STALL_TIMEOUT, Arc::default())
    }
}

impl Partial {
    /// A decode, part of the pass making `progress`, whose readers give up after `stall`
    /// without progress.
    pub fn new(stall: Duration, progress: Arc<Progress>) -> Self {
        Self {
            file: OnceLock::new(),
            state: Mutex::default(),
            grew: Condvar::new(),
            stall,
            progress,
        }
    }

    /// One that has already ended with `content`.
    pub fn finished(content: Content) -> Self {
        let p = Self::default();
        p.end(Ok(content));
        p
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PartialState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn advance(&self, written: u64) {
        self.lock().written = written;
        self.grew.notify_all();
    }

    /// The decoder has started: from now on a read that sees no progress for the stall time
    /// fails instead of waiting for good (a decoder stuck on corrupt data).
    pub fn start(&self) {
        self.lock().started = true;
        self.grew.notify_all();
    }

    /// Records how decoding ended; only the first end counts.
    pub fn end(&self, result: FsResult<Content>) {
        let mut st = self.lock();
        if st.done.is_none() {
            st.done = Some(result);
        }
        drop(st);
        self.grew.notify_all();
    }

    /// Whether decoding has ended.
    pub fn ended(&self) -> bool {
        self.lock().done.is_some()
    }

    /// Waits until decoding ends, however long that takes (warming up a volume); the content or
    /// why it stopped.
    pub fn wait_end(&self) -> FsResult<Content> {
        let mut st = self.lock();
        loop {
            if let Some(done) = &st.done {
                return done.clone();
            }
            st = self.grew.wait(st).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Copies up to `buf.len()` bytes at `offset` into `buf`, waiting until they are decoded
    /// (or decoding ends); the number copied, fewer only at the end of the entry. Fails with
    /// [`FsError::Io`] when the decoding pass, once started, makes no progress for the stall
    /// time. Waiting for the pass to start (behind other decodes) has no limit.
    pub fn read_into(&self, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let want = offset.saturating_add(buf.len() as u64);
        let mut st = self.lock();
        let mut seen = (st.started, st.written, self.progress.value());
        let mut since = Instant::now();
        while st.written < want && st.done.is_none() {
            if !st.started {
                st = self.grew.wait(st).unwrap_or_else(|e| e.into_inner());
                continue;
            }
            let now = (st.started, st.written, self.progress.value());
            if now != seen {
                seen = now;
                since = Instant::now();
            }
            let left = self.stall.saturating_sub(since.elapsed());
            if left.is_zero() {
                tracing::warn!(
                    secs = self.stall.as_secs(),
                    "mount: the decoder made no progress, failing the read"
                );
                return Err(FsError::Io);
            }
            // Progress on other entries of the pass wakes nobody: look at it now and then.
            st = self
                .grew
                .wait_timeout(st, left.min(PROGRESS_POLL))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        match &st.done {
            Some(Ok(c)) => {
                let c = c.clone();
                drop(st);
                return Ok(c.read_into(offset, buf)?);
            }
            Some(Err(e)) => return Err(*e),
            None => {}
        }
        drop(st);
        let f = self.file.get().ok_or(FsError::Io)?;
        if crate::pos::read_full_at(f, buf, offset)? < buf.len() {
            return Err(FsError::Io);
        }
        Ok(buf.len())
    }
}

/// Collects decoded bytes, moving to a spool file once they outgrow memory.
///
/// Writing past `cap` fails: an entry may not produce more than it declared, nor more than
/// the expansion-ratio limit allows (bomb guard). Spooling fails when the disk would be left
/// with less than the reserve ([`CacheLimits::disk_reserve`]).
pub struct ContentBuilder {
    mem: Vec<u8>,
    file: Option<NamedTempFile>,
    len: u64,
    inline_max: u64,
    cap: u64,
    dir: PathBuf,
    reserve: u64,
    /// The spool file may grow to this length before the free space is looked at again.
    checked_to: u64,
    /// Readers of the entry while it is being decoded: the spool file starts with the first
    /// byte, and every write is published.
    partial: Option<Arc<Partial>>,
}

/// Error text when an entry outgrows its cap (see [`ContentBuilder`]).
pub const OVERSIZE: &str = "entry produced more data than it declared";

/// Error text when spooling would eat into the disk reserve.
pub const NO_SPACE: &str = "the disk holding the mount cache is nearly full";

/// Spool bytes written between two looks at the free space.
const SPACE_CHECK_EVERY: u64 = 64 << 20;

impl ContentBuilder {
    /// Whether this builder's cap was hit.
    pub fn is_oversize(e: &io::Error) -> bool {
        e.kind() == io::ErrorKind::FileTooLarge
    }

    /// Fails before the spool file grows by `more` bytes when that could leave the disk with
    /// less than the reserve. The free space is looked at once per stretch of
    /// [`SPACE_CHECK_EVERY`] bytes, and the whole stretch has to fit.
    fn ensure_space(&mut self, more: u64) -> io::Result<()> {
        if self.len + more <= self.checked_to {
            return Ok(());
        }
        let stretch = SPACE_CHECK_EVERY.max(more);
        if let Some(free) = crate::pos::free_space(&self.dir)
            && free < self.reserve.saturating_add(stretch)
        {
            return Err(io::Error::new(io::ErrorKind::StorageFull, NO_SPACE));
        }
        self.checked_to = self.len + stretch;
        Ok(())
    }

    /// Finishes into content.
    pub fn finish(self) -> io::Result<Content> {
        match self.file {
            None => Ok(Content::Mem(self.mem.into())),
            Some(mut f) => {
                f.flush()?;
                let (file, path) = f.into_parts();
                Ok(Content::Disk(Arc::new(Spool {
                    file,
                    len: self.len,
                    _path: path,
                })))
            }
        }
    }

    /// Takes over a file written by an engine that can only write to paths (RAR).
    pub fn adopt(path: TempPath, cap: u64, inline_max: u64) -> io::Result<Content> {
        let file = File::open(&path)?;
        let len = file.metadata()?.len();
        if len > cap {
            return Err(io::Error::new(io::ErrorKind::FileTooLarge, OVERSIZE));
        }
        if len <= inline_max {
            let data = std::fs::read(&path)?;
            return Ok(Content::Mem(data.into()));
        }
        Ok(Content::Disk(Arc::new(Spool {
            file,
            len,
            _path: path,
        })))
    }
}

impl Write for ContentBuilder {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.len + buf.len() as u64 > self.cap {
            return Err(io::Error::new(io::ErrorKind::FileTooLarge, OVERSIZE));
        }
        let spool = self.file.is_some()
            || self.partial.is_some()
            || self.len + buf.len() as u64 > self.inline_max;
        if spool {
            self.ensure_space(buf.len() as u64)?;
        }
        if spool && self.file.is_none() {
            let f = tempfile::Builder::new()
                .prefix("entry-")
                .tempfile_in(&self.dir)?;
            // Every write says where it goes: a duplicated handle (the readers') shares the
            // file position, which their reads move.
            crate::pos::write_all_at(f.as_file(), &self.mem, 0)?;
            self.mem = Vec::new();
            if let Some(p) = &self.partial {
                let _ = p.file.set(f.as_file().try_clone()?);
            }
            self.file = Some(f);
        }
        match &self.file {
            Some(f) => crate::pos::write_all_at(f.as_file(), buf, self.len)?,
            None => self.mem.extend_from_slice(buf),
        }
        self.len += buf.len() as u64;
        if let Some(p) = &self.partial {
            p.advance(self.len);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.file {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

#[derive(Debug)]
struct Slot {
    content: Content,
    last_use: u64,
}

#[derive(Debug, Default)]
struct Inner {
    slots: HashMap<EntryId, Slot>,
    tick: u64,
    memory: u64,
    disk: u64,
}

/// The cache of one mounted volume.
#[derive(Debug)]
pub struct ContentCache {
    limits: CacheLimits,
    dir: TempDir,
    inner: Mutex<Inner>,
}

impl ContentCache {
    /// A cache whose spool files live in a new private directory below `parent`
    /// (the system temporary directory when `None`).
    pub fn new(limits: CacheLimits, parent: Option<&Path>) -> io::Result<Self> {
        let mut b = tempfile::Builder::new();
        b.prefix("omnizip-mount-");
        let dir = match parent {
            Some(p) => {
                std::fs::create_dir_all(p)?;
                b.tempdir_in(p)?
            }
            None => b.tempdir()?,
        };
        Ok(Self {
            limits,
            dir,
            inner: Mutex::new(Inner::default()),
        })
    }

    pub fn limits(&self) -> CacheLimits {
        self.limits
    }

    /// Directory holding the spool files.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A builder for one entry that may produce at most `cap` bytes.
    pub fn builder(&self, cap: u64) -> ContentBuilder {
        ContentBuilder {
            mem: Vec::new(),
            file: None,
            len: 0,
            inline_max: self.limits.inline_max,
            cap,
            dir: self.dir.path().to_owned(),
            reserve: self.limits.disk_reserve,
            checked_to: 0,
            partial: None,
        }
    }

    /// A builder whose bytes `partial`'s readers get as they are written.
    pub fn streaming_builder(&self, cap: u64, partial: Arc<Partial>) -> ContentBuilder {
        ContentBuilder {
            partial: Some(partial),
            ..self.builder(cap)
        }
    }

    /// Cached content, marked as just used.
    pub fn get(&self, id: EntryId) -> Option<Content> {
        let mut g = self.lock();
        g.tick += 1;
        let tick = g.tick;
        g.slots.get_mut(&id).map(|s| {
            s.last_use = tick;
            s.content.clone()
        })
    }

    pub fn contains(&self, id: EntryId) -> bool {
        self.lock().slots.contains_key(&id)
    }

    /// Length of cached content, without marking it as used.
    pub fn cached_len(&self, id: EntryId) -> Option<u64> {
        self.lock().slots.get(&id).map(|s| s.content.len())
    }

    /// Stores content, then drops least recently used entries until both pools fit.
    /// The entry just stored is never dropped, even when it alone exceeds a budget.
    pub fn insert(&self, id: EntryId, content: Content) -> Content {
        let mut g = self.lock();
        g.tick += 1;
        let tick = g.tick;
        let size = content.len();
        if content.on_disk() {
            g.disk += size;
        } else {
            g.memory += size;
        }
        if let Some(old) = g.slots.insert(
            id,
            Slot {
                content: content.clone(),
                last_use: tick,
            },
        ) {
            Self::forget(&mut g, &old.content);
        }
        for disk in [false, true] {
            loop {
                let (used, limit) = if disk {
                    (g.disk, self.limits.disk)
                } else {
                    (g.memory, self.limits.memory)
                };
                if used <= limit {
                    break;
                }
                let victim = g
                    .slots
                    .iter()
                    .filter(|(k, s)| **k != id && s.content.on_disk() == disk)
                    .min_by_key(|(_, s)| s.last_use)
                    .map(|(k, _)| *k);
                let Some(v) = victim else { break };
                if let Some(old) = g.slots.remove(&v) {
                    Self::forget(&mut g, &old.content);
                }
            }
        }
        content
    }

    fn forget(g: &mut Inner, c: &Content) {
        if c.on_disk() {
            g.disk -= c.len();
        } else {
            g.memory -= c.len();
        }
    }

    /// Bytes in use: (memory, disk).
    pub fn usage(&self) -> (u64, u64) {
        let g = self.lock();
        (g.memory, g.disk)
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.lock().slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drops everything (unmount, or the source archive changed).
    pub fn clear(&self) {
        let mut g = self.lock();
        g.slots.clear();
        g.memory = 0;
        g.disk = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(memory: u64, disk: u64, inline_max: u64) -> ContentCache {
        ContentCache::new(
            CacheLimits {
                memory,
                disk,
                inline_max,
                disk_reserve: 0,
            },
            None,
        )
        .unwrap()
    }

    fn content(c: &ContentCache, data: &[u8]) -> Content {
        let mut b = c.builder(u64::MAX);
        b.write_all(data).unwrap();
        b.finish().unwrap()
    }

    #[test]
    fn small_in_memory_large_on_disk() {
        let c = cache(1 << 20, 1 << 20, 16);
        let small = content(&c, b"0123456789");
        assert!(matches!(small, Content::Mem(_)));
        let big: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let large = content(&c, &big);
        assert!(matches!(large, Content::Disk(_)));
        assert_eq!(large.len(), 1000);
        assert_eq!(large.read_at(990, 64).unwrap(), &big[990..]);
        assert_eq!(large.read_at(500, 3).unwrap(), &big[500..503]);
        assert!(large.read_at(1000, 10).unwrap().is_empty());
        assert_eq!(small.read_at(3, 4).unwrap(), b"3456");
    }

    #[test]
    fn cap_stops_oversized_entries() {
        let c = cache(1 << 20, 1 << 20, 4);
        let mut b = c.builder(8);
        b.write_all(b"12345678").unwrap();
        let e = b.write_all(b"9").unwrap_err();
        assert!(ContentBuilder::is_oversize(&e));
    }

    #[test]
    fn spooling_keeps_the_disk_reserve() {
        let free = crate::pos::free_space(std::env::temp_dir().as_path()).expect("free space");
        let c = ContentCache::new(
            CacheLimits {
                inline_max: 4,
                // More than the disk has: no spool file may grow.
                disk_reserve: free.saturating_add(1 << 40),
                ..CacheLimits::default()
            },
            None,
        )
        .unwrap();
        let mut b = c.builder(u64::MAX);
        b.write_all(b"1234").unwrap(); // in memory
        let e = b.write_all(b"5").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::StorageFull);
        assert_eq!(std::fs::read_dir(c.dir()).unwrap().count(), 0);
    }

    #[test]
    fn least_recently_used_is_dropped_first() {
        let c = cache(25, 1 << 20, 100);
        c.insert(1, content(&c, &[1; 10]));
        c.insert(2, content(&c, &[2; 10]));
        let held = c.get(2).unwrap();
        assert!(c.get(1).is_some()); // 1 is now more recent than 2
        c.insert(3, content(&c, &[3; 10]));
        assert!(c.contains(1) && c.contains(3) && !c.contains(2));
        assert_eq!(c.usage().0, 20);
        // Evicted content stays readable for whoever still holds it.
        assert_eq!(held.read_at(0, 3).unwrap(), [2, 2, 2]);
    }

    #[test]
    fn spool_files_disappear_with_their_last_holder() {
        let c = cache(1 << 20, 15, 4);
        c.insert(1, content(&c, &[1; 10]));
        let spools = || std::fs::read_dir(c.dir()).unwrap().count();
        assert_eq!(spools(), 1);
        let held = c.get(1).unwrap();
        c.insert(2, content(&c, &[2; 10]));
        assert!(!c.contains(1));
        assert_eq!(spools(), 2);
        drop(held);
        assert_eq!(spools(), 1);
        assert_eq!(c.usage(), (0, 10));
    }
}
