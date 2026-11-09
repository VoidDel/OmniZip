//! An archive as a read-only file system (`docs/12-mount.md` §3, §5, §6).
//!
//! Directory structure comes from the core [`TreeIndex`]; names are cleaned per directory on
//! first use ([`crate::names`]). Files stored as they are (plain tar, stored ZIP entries) are
//! read in place from the archive file, neither decoded nor cached. Everything else is decoded
//! by the volume's workers ([`crate::decoder`]) into the [`ContentCache`] and served by offset
//! afterwards; a read waits only for the bytes it wants ([`Partial`]).
//!
//! What one decoding pass covers depends on the archive (mount class): an entry of its own
//! stream (B) is one pass, and reading a folder out in archive order queues the next entries
//! ahead of the reads. A solid archive (C) or a single stream (D) decodes from the start of its
//! block or stream whichever entry is wanted, so the first access decodes everything that fits
//! the cache in one pass: one per solid block when the engine knows them, which decode side by
//! side with the block that was read first.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    ffi::OsStr,
    fs::File,
    io,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use omnizip_core::{
    ArchiveInfo, ArchiveReader, CancelToken, Entry, EntryId, EntryKind, FixedPassword, Interaction,
    JobCtx, MountClass, NoInteraction, NullProgress, Secret, SecurityPolicy, TreeIndex,
    path::{logical_components, resolve_link},
};

use crate::{
    Attr, DirEntry, FileHandle, FileType, FsError, FsResult, FsStats, NodeId, OpenFlags, VirtualFs,
    cache::{CacheLimits, Content, ContentCache, Partial, Progress, STALL_TIMEOUT},
    decoder::{CacheSink, Pool, PoolOptions, SharedReader, decode},
    names::{NameRules, assign},
};

/// Read-ahead (DwarFS's sequential-access detector, `docs/12-mount.md` §5): a file read from
/// its start within this many entries after one read before suggests a folder being copied
/// out in archive order.
const SEQ_GAP: EntryId = 8;
/// How far read-ahead keeps ahead of the reads: entries, and bytes of them.
const AHEAD_ENTRIES: EntryId = 64;
const AHEAD_BYTES: u64 = 32 << 20;
/// Entries read ahead are decoded in passes of neighbours, which lie next to each other in the
/// archive file: up to this many, and this many bytes.
const PASS_ENTRIES: usize = 8;
const PASS_BYTES: u64 = 1 << 20;
/// A warm-up of an archive whose entries decode on their own goes in passes this large.
const WARM_PASS_ENTRIES: usize = 256;
const WARM_PASS_BYTES: u64 = 64 << 20;

/// Decoding passes a volume runs at once by default (`docs/12-mount.md` §4).
pub fn default_workers() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get().min(4))
}

/// How symbolic links are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkMode {
    /// As links when the target is inside the volume, otherwise as small files holding the
    /// target text (Linux, macOS).
    Native,
    /// Always as files holding the target text (Windows).
    AsFiles,
}

/// Options of one mounted archive.
#[derive(Debug, Clone)]
pub struct FsOptions {
    pub names: NameRules,
    pub links: LinkMode,
    pub cache: CacheLimits,
    /// Parent directory of the spool files (system temp when `None`).
    pub cache_dir: Option<PathBuf>,
    /// Volume label (the archive name without extension when `None`).
    pub label: Option<String>,
    /// Open handles per volume. A handle stays open well after a program closes its file while
    /// the system caches the file's data (Windows closes it when the cache lets go), so copying
    /// a large folder out keeps many open at once.
    pub max_handles: usize,
    /// Password for encrypted entries. Without one, reading them fails with
    /// [`FsError::Access`].
    pub password: Option<Secret>,
    /// Decode a whole solid archive on first access when it fits the disk budget.
    pub warm_solid: bool,
    /// Limits on untrusted content: the expansion ratio caps what one entry may unpack to.
    pub policy: SecurityPolicy,
    /// A read waiting on a decode fails with [`FsError::Io`] when the decoder makes no progress
    /// for this long (`docs/12-mount.md` §7).
    pub stall_timeout: Duration,
    /// Decoding passes run at once (worker threads, each with its own reader of the archive).
    pub workers: usize,
}

impl Default for FsOptions {
    fn default() -> Self {
        Self {
            names: NameRules::current(),
            links: if cfg!(windows) {
                LinkMode::AsFiles
            } else {
                LinkMode::Native
            },
            cache: CacheLimits::default(),
            cache_dir: None,
            label: None,
            max_handles: 1 << 20,
            password: None,
            warm_solid: true,
            stall_timeout: STALL_TIMEOUT,
            workers: default_workers(),
            policy: SecurityPolicy::default(),
        }
    }
}

/// What read-ahead has seen and done.
#[derive(Debug, Default)]
struct ReadAhead {
    /// Entries most recently read from their start.
    recent: VecDeque<EntryId>,
    /// Entries before this one have been looked at for read-ahead.
    to: EntryId,
}

/// Display names of one directory.
#[derive(Debug)]
struct DirView {
    names: Vec<String>,
    nodes: Vec<NodeId>,
    /// Folded name → position.
    index: HashMap<String, usize>,
}

/// Where a node's bytes come from.
#[derive(Debug, Clone)]
enum Data {
    None,
    Entry(EntryId),
    Text(Arc<[u8]>),
}

#[derive(Debug, Clone)]
struct Shape {
    attr: Attr,
    data: Data,
    link: Option<String>,
}

/// A mounted archive.
pub struct ArchiveFs {
    /// The reader everything but decoding uses (decoding workers have clones of it).
    reader: SharedReader,
    info: ArchiveInfo,
    entries: Vec<Entry>,
    tree: TreeIndex,
    opts: FsOptions,
    dirs: RwLock<HashMap<NodeId, Arc<DirView>>>,
    cache: Arc<ContentCache>,
    pool: Pool,
    /// Per entry, the solid block holding it (class C, when the engine knows its blocks).
    blocks: Vec<Option<u32>>,
    read_ahead: Mutex<ReadAhead>,
    /// Stops decoding when the volume goes away.
    cancel: CancelToken,
    handles: Mutex<HashMap<u64, NodeId>>,
    next_handle: Mutex<u64>,
    interaction: Arc<dyn Interaction>,
    fallback_time: SystemTime,
    stats: FsStats,
    /// The warm-up has been queued. Held while it is, so that reads arriving meanwhile wait
    /// for its passes instead of starting passes of their own over the same blocks.
    warmed: Mutex<bool>,
    /// The archive file, for reading stored entries in place (an archive of one file).
    file: Option<File>,
    /// Per entry, where its data lies as it is in [`file`](Self::file), asked once.
    stored: Vec<OnceLock<Option<(u64, u64)>>>,
}

impl Drop for ArchiveFs {
    /// A decode still running in the background stops (and lets go of the archive file).
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl std::fmt::Debug for ArchiveFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveFs")
            .field("path", &self.info.path)
            .field("entries", &self.entries.len())
            .finish()
    }
}

/// 32-bit FNV-1a, for the volume serial number.
fn fnv1a(parts: &[&[u8]]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for p in parts {
        for b in *p {
            h ^= u32::from(*b);
            h = h.wrapping_mul(0x0100_0193);
        }
    }
    h
}

impl ArchiveFs {
    /// Wraps an opened archive.
    pub fn new(reader: Box<dyn ArchiveReader>, opts: FsOptions) -> io::Result<Self> {
        let info = reader.info().clone();
        let entries = reader.entries().to_vec();
        let tree = TreeIndex::build(&entries);
        let cache = Arc::new(ContentCache::new(opts.cache, opts.cache_dir.as_deref())?);
        let meta = std::fs::metadata(&info.path).ok();
        let fallback_time = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .unwrap_or(UNIX_EPOCH);
        let since = fallback_time
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let label = opts.label.clone().unwrap_or_else(|| volume_label(&info));
        let root = tree.root();
        let stats = FsStats {
            total_bytes: root.size,
            files: root.files,
            dirs: (tree.len() as u64).saturating_sub(root.files + 1),
            block_size: 4096,
            max_name_len: 255,
            label,
            fs_name: "OmniZip",
            serial: fnv1a(&[
                info.path.to_string_lossy().as_bytes(),
                &info.archive_size.to_le_bytes(),
                &since.to_le_bytes(),
            ]),
        };
        let interaction: Arc<dyn Interaction> = match &opts.password {
            Some(p) => Arc::new(FixedPassword(p.clone())),
            None => Arc::new(NoInteraction),
        };
        let file = if info.volumes <= 1 {
            File::open(&info.path).ok()
        } else {
            None
        };
        let stored = std::iter::repeat_with(OnceLock::new)
            .take(entries.len())
            .collect();
        let blocks = if info.format.mount_class(info.solid) == MountClass::C {
            entries.iter().map(|e| reader.solid_block(e.id)).collect()
        } else {
            Vec::new()
        };
        let reader: SharedReader = Arc::new(Mutex::new(reader));
        let cancel = CancelToken::new();
        let pool = Pool::new(
            reader.clone(),
            cache.clone(),
            PoolOptions {
                workers: opts.workers,
                stall: opts.stall_timeout,
                interaction: interaction.clone(),
                cancel: cancel.clone(),
                policy: opts.policy.clone(),
            },
        );
        Ok(Self {
            file,
            stored,
            reader,
            info,
            entries,
            tree,
            opts,
            dirs: RwLock::new(HashMap::new()),
            cache,
            pool,
            blocks,
            read_ahead: Mutex::default(),
            cancel,
            handles: Mutex::new(HashMap::new()),
            next_handle: Mutex::new(1),
            interaction,
            fallback_time,
            stats,
            warmed: Mutex::new(false),
        })
    }

    /// Archive facts.
    pub fn info(&self) -> &ArchiveInfo {
        &self.info
    }

    /// Random-access class of the archive (`docs/12-mount.md` §5).
    pub fn mount_class(&self) -> MountClass {
        self.info.format.mount_class(self.info.solid)
    }

    /// The content cache (usage is shown in the mount list).
    pub fn cache(&self) -> &ContentCache {
        &self.cache
    }

    /// Number of open handles.
    pub fn open_handles(&self) -> usize {
        self.handles.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    fn ctx(&self) -> JobCtx {
        JobCtx::new(Arc::new(NullProgress), self.interaction.clone())
            .with_cancel(self.cancel.clone())
            .with_policy(self.opts.policy.clone())
    }

    /// Decoding passes run at once, at most: one when the engine cannot clone its reader.
    pub fn workers(&self) -> usize {
        self.pool.max_workers()
    }

    /// Whether the first access decodes everything that fits the cache (`docs/12-mount.md`
    /// §5): any entry of a single stream decodes from its start, and so does any entry of a
    /// solid block (a setting).
    fn one_pass(&self) -> bool {
        match self.mount_class() {
            MountClass::C => self.opts.warm_solid,
            MountClass::D => true,
            MountClass::A | MountClass::B => false,
        }
    }

    /// The decode a read of `id` waits on: under way, or queued now with the passes the archive
    /// calls for.
    fn request(&self, id: EntryId) -> Arc<Partial> {
        if let Some(p) = self.pool.pending(id) {
            return p;
        }
        if !self.one_pass() {
            return self.pool.request(id, vec![(vec![id], false)]);
        }
        let mut warmed = self.warmed.lock().unwrap_or_else(|e| e.into_inner());
        if *warmed {
            drop(warmed);
            return self.pool.request(id, vec![(vec![id], false)]);
        }
        *warmed = true;
        self.pool.request(id, self.warm_passes(Some(id)))
    }

    /// Read-ahead after a read of `id` from its start (class B): when the entries just before
    /// it were read too, the small entries after it are queued for decoding ahead of the reads
    /// (DwarFS's sequential-access detector). Reads of a folder being copied out then find
    /// their files decoded, by several workers at once.
    fn read_ahead(&self, id: EntryId) {
        let passes = {
            let mut ra = self.read_ahead.lock().unwrap_or_else(|e| e.into_inner());
            let sequential = ra.recent.iter().any(|&r| r < id && id - r <= SEQ_GAP);
            if ra.recent.len() >= SEQ_GAP as usize {
                ra.recent.pop_front();
            }
            ra.recent.push_back(id);
            if !sequential {
                return;
            }
            let end = id
                .saturating_add(AHEAD_ENTRIES)
                .min(self.entries.len() as EntryId);
            // Go on from where the last look stopped, unless the reads went back before it.
            let mut at = if ra.to > id && ra.to <= end {
                ra.to
            } else {
                id + 1
            };
            let wanted = |e: &Entry| self.ahead_candidate(e);
            let mut ahead: u64 = self.entries[id as usize + 1..at as usize]
                .iter()
                .filter(|e| wanted(e))
                .map(|e| e.size)
                .sum();
            let (mut passes, mut pass, mut pass_bytes) = (Vec::new(), Vec::new(), 0);
            while at < end && ahead < AHEAD_BYTES {
                let e = &self.entries[at as usize];
                at += 1;
                if !wanted(e) {
                    continue;
                }
                if pass.len() >= PASS_ENTRIES
                    || !pass.is_empty() && pass_bytes + e.size > PASS_BYTES
                {
                    passes.push(std::mem::take(&mut pass));
                    pass_bytes = 0;
                }
                pass.push(e.id);
                pass_bytes += e.size;
                ahead += e.size;
            }
            if !pass.is_empty() {
                passes.push(pass);
            }
            ra.to = at;
            passes
        };
        if !passes.is_empty() {
            self.pool.ahead(passes);
        }
    }

    /// Whether read-ahead decodes an entry: small files only. Large ones stream when they are
    /// read, and stored ones are read in place.
    fn ahead_candidate(&self, e: &Entry) -> bool {
        let stored = e
            .method
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case("store") || m.eq_ignore_ascii_case("copy"))
            || matches!(self.stored[e.id as usize].get(), Some(Some(_)));
        e.kind == EntryKind::File
            && e.size_known
            && e.size > 0
            && e.size <= self.cache.limits().inline_max
            && !stored
    }

    fn dir_view(&self, dir: NodeId) -> FsResult<Arc<DirView>> {
        let node = self.tree.node(dir).ok_or(FsError::NotFound)?;
        if !node.is_dir {
            return Err(FsError::NotDir);
        }
        if let Some(v) = self
            .dirs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&dir)
        {
            return Ok(v.clone());
        }
        let nodes: Vec<NodeId> = node.children.clone();
        let names = assign(
            nodes
                .iter()
                .map(|&c| self.tree.node(c).map_or("", |n| n.name.as_str())),
            self.opts.names,
        );
        let index = names
            .iter()
            .enumerate()
            .map(|(i, n)| (self.opts.names.fold(n).into_owned(), i))
            .collect();
        let view = Arc::new(DirView {
            names,
            nodes,
            index,
        });
        self.dirs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(dir, view.clone());
        Ok(view)
    }

    /// The listing of `parent` and the position of `name` in it.
    fn find(&self, parent: NodeId, name: &OsStr) -> FsResult<(Arc<DirView>, usize)> {
        let view = self.dir_view(parent)?;
        let name = name.to_str().ok_or(FsError::NotFound)?;
        let i = *view
            .index
            .get(self.opts.names.fold(name).as_ref())
            .ok_or(FsError::NotFound)?;
        Ok((view, i))
    }

    /// Resolves a link target to a node: relative to the link's directory for symbolic links,
    /// relative to the archive root for hard links (archive member names, taken literally).
    /// `None` when it leaves the volume. A symbolic link is resolved as the system resolves it
    /// once shown as a link: through other links of the archive, so that `..` after one of them
    /// cannot lead out of the volume while the text reads as inside.
    fn link_node(&self, node: NodeId, target: &str, from_root: bool) -> Option<NodeId> {
        if target.contains(':') {
            return None;
        }
        let base: Vec<String> = if from_root {
            Vec::new()
        } else {
            let parent = self.tree.node(node)?.parent?;
            logical_components(&self.tree.path(parent))
                .into_iter()
                .map(str::to_owned)
                .collect()
        };
        let parts = resolve_link(&base, target, |at| {
            if from_root {
                return None;
            }
            let e = self.tree.node(self.tree.find(&at.join("/"))?)?.entry?;
            let e = self.entries.get(e as usize)?;
            (e.kind == EntryKind::Symlink).then(|| e.link_target.clone().unwrap_or_default())
        })?;
        self.tree.find(&parts.join("/"))
    }

    fn shape(&self, id: NodeId) -> FsResult<Shape> {
        let node = self.tree.node(id).ok_or(FsError::NotFound)?;
        let entry = node.entry.and_then(|e| self.entries.get(e as usize));
        let mtime = node
            .mtime
            .or(entry.and_then(|e| e.mtime))
            .unwrap_or(self.fallback_time);
        let mut attr = Attr {
            node: id,
            kind: FileType::File,
            size: node.size,
            mtime,
            mode: 0o444,
            nlink: 1,
        };
        if node.is_dir {
            attr.kind = FileType::Dir;
            attr.mode = 0o555;
            attr.nlink = 2;
            return Ok(Shape {
                attr,
                data: Data::None,
                link: None,
            });
        }
        let Some(e) = entry else {
            return Ok(Shape {
                attr,
                data: Data::None,
                link: None,
            });
        };
        // Read-only volume: keep read and execute bits, never write / setuid / setgid.
        attr.mode = e.unix_mode.map_or(0o444, |m| (m & 0o555) | 0o444);
        let shape = match e.kind {
            EntryKind::File => Shape {
                attr: Attr {
                    // Single-stream formats may not record the size: report it once decoded.
                    size: if e.size_known {
                        e.size
                    } else {
                        self.cache.cached_len(e.id).unwrap_or(e.size)
                    },
                    ..attr
                },
                data: Data::Entry(e.id),
                link: None,
            },
            EntryKind::Symlink => {
                let target = e.link_target.clone().unwrap_or_default();
                let inside = self.link_node(id, &target, false).is_some();
                if inside && self.opts.links == LinkMode::Native {
                    Shape {
                        attr: Attr {
                            kind: FileType::Symlink,
                            size: target.len() as u64,
                            mode: 0o777,
                            ..attr
                        },
                        data: Data::None,
                        link: Some(target),
                    }
                } else {
                    let text: Arc<[u8]> = target.into_bytes().into();
                    Shape {
                        attr: Attr {
                            size: text.len() as u64,
                            ..attr
                        },
                        data: Data::Text(text),
                        link: None,
                    }
                }
            }
            EntryKind::Hardlink => {
                // Shown as an independent file with the target's content.
                let target = e
                    .link_target
                    .as_deref()
                    .and_then(|t| self.link_node(id, t, true))
                    .and_then(|n| self.tree.node(n))
                    .and_then(|n| n.entry)
                    .and_then(|t| self.entries.get(t as usize))
                    .filter(|t| t.kind == EntryKind::File);
                match target {
                    Some(t) => Shape {
                        attr: Attr {
                            size: t.size,
                            ..attr
                        },
                        data: Data::Entry(t.id),
                        link: None,
                    },
                    None => Shape {
                        attr: Attr { size: 0, ..attr },
                        data: Data::None,
                        link: None,
                    },
                }
            }
            EntryKind::Dir | EntryKind::Other => Shape {
                attr: Attr { size: 0, ..attr },
                data: Data::None,
                link: None,
            },
        };
        Ok(shape)
    }

    /// Entries a full warm-up decodes, in archive order: every file not yet cached, within the
    /// disk budget.
    fn warm_targets(&self) -> Vec<EntryId> {
        let mut budget = self.cache.limits().disk;
        let mut out = Vec::new();
        for e in &self.entries {
            if e.kind != EntryKind::File || self.cache.contains(e.id) {
                continue;
            }
            if e.size > budget {
                break;
            }
            budget -= e.size;
            out.push(e.id);
        }
        out
    }

    /// The passes of a warm-up (targets, and whether they go ahead of reads). A solid block or
    /// a stream decodes only from its start, so decoding one entry passes over the ones before
    /// it: caching everything that fits keeps reading a whole folder from decoding the same
    /// start over and over. Each solid block is a pass of its own when the engine knows them
    /// (they decode side by side); otherwise the whole archive is one. `first`, the entry a read
    /// waits for, is in the pass that comes first; without it, every pass goes ahead of reads.
    fn warm_passes(&self, first: Option<EntryId>) -> Vec<(Vec<EntryId>, bool)> {
        let mut targets = self.warm_targets();
        if let Some(id) = first
            && !targets.contains(&id)
        {
            targets.push(id);
        }
        let ahead = first.is_none();
        let block_of = |id: EntryId| self.blocks.get(id as usize).copied().flatten();
        if self.mount_class() == MountClass::C && targets.iter().any(|&id| block_of(id).is_some()) {
            let mut by_block: BTreeMap<Option<u32>, Vec<EntryId>> = BTreeMap::new();
            for id in targets {
                by_block.entry(block_of(id)).or_default().push(id);
            }
            let lead = first.map(block_of);
            let mut passes: Vec<(Vec<EntryId>, bool)> = Vec::new();
            if let Some(b) = lead
                && let Some(t) = by_block.remove(&b)
            {
                passes.push((t, false));
            }
            passes.extend(by_block.into_values().map(|t| (t, true)));
            return passes;
        }
        if self.mount_class() == MountClass::B {
            // Entries decode on their own: in passes the workers share.
            let mut passes = Vec::new();
            let (mut pass, mut bytes) = (Vec::new(), 0);
            for id in targets {
                let size = self.entries[id as usize].size;
                if !pass.is_empty()
                    && (pass.len() >= WARM_PASS_ENTRIES || bytes + size > WARM_PASS_BYTES)
                {
                    passes.push((std::mem::take(&mut pass), ahead));
                    bytes = 0;
                }
                pass.push(id);
                bytes += size;
            }
            if !pass.is_empty() {
                passes.push((pass, ahead));
            }
            return passes;
        }
        vec![(targets, ahead)]
    }

    /// Decodes every file into the cache, as far as the disk budget allows ("warm up", useful
    /// before seeking around a solid archive), and waits for it. Reads go first meanwhile.
    /// Returns the number of entries now cached.
    pub fn warm(&self) -> FsResult<usize> {
        let queued = {
            let mut warmed = self.warmed.lock().unwrap_or_else(|e| e.into_inner());
            *warmed = true;
            let passes = self.warm_passes(None).into_iter().map(|(t, _)| t).collect();
            self.pool.ahead(passes)
        };
        let mut result = Ok(());
        for p in queued {
            if let Err(e) = p.wait_end() {
                result = result.and(Err(e));
            }
        }
        result.map(|()| self.cache.len())
    }

    /// Decodes one encrypted file, so that a missing or wrong password shows when mounting
    /// rather than on every read ([`FsError::Access`]). In a solid archive that is the first
    /// one, which needs the least of its block decoded; otherwise the smallest. `Ok` when
    /// nothing is encrypted.
    pub fn check_password(&self) -> FsResult<()> {
        let mut encrypted = self
            .entries
            .iter()
            .filter(|e| e.encrypted && e.kind == EntryKind::File);
        let probe = if self.mount_class() == MountClass::C {
            encrypted.next()
        } else {
            encrypted.min_by_key(|e| e.size)
        };
        let Some(e) = probe else { return Ok(()) };
        if self.cache.contains(e.id) {
            return Ok(());
        }
        let (partials, progress) = (HashMap::new(), Progress::default());
        let mut sink = CacheSink::detached(
            &self.cache,
            HashSet::from([e.id]),
            &partials,
            &progress,
            self.info.archive_size,
        );
        let mut reader = self.reader.lock().unwrap_or_else(|e| e.into_inner());
        decode(reader.as_mut(), &mut sink, &self.ctx())
    }

    /// Where an entry's data lies as it is in the archive file, asked of the reader once.
    /// When an engine without clones decodes on that reader and it is busy, the entry is taken
    /// as not stored this time: decoding it is right too, and the read does not wait for the
    /// decode in progress.
    fn stored(&self, id: EntryId) -> Option<(u64, u64)> {
        self.file.as_ref()?;
        let slot = self.stored.get(id as usize)?;
        if let Some(known) = slot.get() {
            return *known;
        }
        let mut reader = if self.pool.decodes_on_shared_reader() {
            match self.reader.try_lock() {
                Ok(r) => r,
                Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return None,
            }
        } else {
            self.reader.lock().unwrap_or_else(|e| e.into_inner())
        };
        *slot.get_or_init(|| reader.stored_range(id))
    }

    /// Reads a stored entry in place into `buf`, from `offset` and never past `declared`.
    fn read_stored(
        &self,
        (start, len): (u64, u64),
        declared: u64,
        offset: u64,
        buf: &mut [u8],
    ) -> FsResult<usize> {
        let f = self.file.as_ref().ok_or(FsError::Io)?;
        let n = len
            .min(declared)
            .saturating_sub(offset)
            .min(buf.len() as u64) as usize;
        if crate::pos::read_full_at(f, &mut buf[..n], start + offset)? < n {
            // The archive is shorter than its directory says.
            return Err(FsError::Io);
        }
        Ok(n)
    }

    fn handle_node(&self, fh: FileHandle) -> FsResult<NodeId> {
        self.handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&fh.0)
            .copied()
            .ok_or(FsError::BadHandle)
    }
}

/// The volume label for an archive: its file name without archive extensions.
pub fn volume_label(info: &ArchiveInfo) -> String {
    let name = info
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "OmniZip".into());
    let lower = name.to_ascii_lowercase();
    let cut = info
        .format
        .extensions()
        .iter()
        .filter(|ext| lower.ends_with(**ext) && ext.len() < name.len())
        .map(|ext| ext.len())
        .max()
        .unwrap_or(0);
    let label = name[..name.len() - cut].to_owned();
    short_label(if label.is_empty() { &name } else { &label })
}

/// Default labels wider than this (in columns, CJK characters count two) are shortened: Explorer
/// shows a drive's name and letter on one line above its capacity, and a longer name wraps and
/// pushes the capacity out of the drive's tile.
const LABEL_COLUMNS: usize = 20;
/// Columns kept from the end of a shortened label: dates and versions tell releases apart.
const LABEL_TAIL: usize = 8;

/// `label` shortened in the middle with "…" to [`LABEL_COLUMNS`] columns.
pub fn short_label(label: &str) -> String {
    let width = |c: char| {
        let wide = matches!(c as u32,
            0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x2_0000..=0x3_FFFD);
        if wide { 2 } else { 1 }
    };
    if label.chars().map(width).sum::<usize>() <= LABEL_COLUMNS {
        return label.to_owned();
    }
    let mut tail = Vec::new();
    let mut used = 0;
    for c in label.chars().rev() {
        if used + width(c) > LABEL_TAIL {
            break;
        }
        used += width(c);
        tail.push(c);
    }
    let room = LABEL_COLUMNS - 1 - used;
    let mut head = String::new();
    let mut used = 0;
    for c in label.chars() {
        if used + width(c) > room {
            break;
        }
        used += width(c);
        head.push(c);
    }
    let tail: String = tail.into_iter().rev().collect();
    format!("{}…{}", head.trim_end(), tail.trim_start())
}

impl VirtualFs for ArchiveFs {
    fn lookup(&self, parent: NodeId, name: &OsStr) -> FsResult<Attr> {
        let (view, i) = self.find(parent, name)?;
        Ok(self.shape(view.nodes[i])?.attr)
    }

    fn lookup_named(&self, parent: NodeId, name: &OsStr) -> FsResult<(Attr, String)> {
        let (view, i) = self.find(parent, name)?;
        Ok((self.shape(view.nodes[i])?.attr, view.names[i].clone()))
    }

    fn getattr(&self, node: NodeId) -> FsResult<Attr> {
        Ok(self.shape(node)?.attr)
    }

    fn readdir(&self, dir: NodeId, cursor: u64, max: usize) -> FsResult<Vec<DirEntry>> {
        let view = self.dir_view(dir)?;
        let start = (cursor as usize).min(view.nodes.len());
        let end = start.saturating_add(max).min(view.nodes.len());
        (start..end)
            .map(|i| {
                Ok(DirEntry {
                    name: view.names[i].clone(),
                    attr: self.shape(view.nodes[i])?.attr,
                    cursor: i as u64 + 1,
                })
            })
            .collect()
    }

    fn open(&self, node: NodeId, flags: OpenFlags) -> FsResult<FileHandle> {
        if flags.write {
            return Err(FsError::ReadOnly);
        }
        self.tree.node(node).ok_or(FsError::NotFound)?;
        let mut handles = self.handles.lock().unwrap_or_else(|e| e.into_inner());
        if handles.len() >= self.opts.max_handles {
            return Err(FsError::TooManyOpen);
        }
        let mut next = self.next_handle.lock().unwrap_or_else(|e| e.into_inner());
        let fh = *next;
        *next += 1;
        handles.insert(fh, node);
        Ok(FileHandle(fh))
    }

    fn read(&self, fh: FileHandle, offset: u64, size: u32) -> FsResult<Vec<u8>> {
        let shape = self.shape(self.handle_node(fh)?)?;
        // Sized to what is there, when the size is known.
        let known = match shape.data {
            Data::Entry(id) => self.entries[id as usize].size_known,
            _ => true,
        };
        let cap = if known {
            u64::from(size).min(shape.attr.size.saturating_sub(offset))
        } else {
            u64::from(size)
        };
        let mut buf = vec![0; cap as usize];
        let n = self.read_into(fh, offset, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn read_into(&self, fh: FileHandle, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        let node = self.handle_node(fh)?;
        let shape = self.shape(node)?;
        match (shape.attr.kind, shape.data) {
            (FileType::Dir, _) => Err(FsError::IsDir),
            (_, Data::None) => Ok(0),
            (_, Data::Text(t)) => Ok(Content::Mem(t).read_into(offset, buf)?),
            (_, Data::Entry(id)) => {
                let known = self.entries[id as usize].size_known;
                if offset >= shape.attr.size && known {
                    return Ok(0);
                }
                if let Some(range) = self.stored(id) {
                    return self.read_stored(range, shape.attr.size, offset, buf);
                }
                if offset == 0 && self.mount_class() == MountClass::B {
                    self.read_ahead(id);
                }
                if let Some(c) = self.cache.get(id) {
                    return Ok(c.read_into(offset, buf)?);
                }
                // Never past the declared size: a read there would wait for the end.
                let n = if known {
                    shape.attr.size.saturating_sub(offset).min(buf.len() as u64) as usize
                } else {
                    buf.len()
                };
                self.request(id).read_into(offset, &mut buf[..n])
            }
        }
    }

    fn release(&self, fh: FileHandle) {
        self.handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&fh.0);
    }

    fn readlink(&self, node: NodeId) -> FsResult<PathBuf> {
        self.shape(node)?
            .link
            .map(PathBuf::from)
            .ok_or(FsError::Invalid)
    }

    fn statfs(&self) -> FsResult<FsStats> {
        Ok(self.stats.clone())
    }

    fn archive_path(&self, node: NodeId) -> Option<String> {
        self.tree.node(node).map(|_| self.tree.path(node))
    }
}

#[cfg(test)]
mod tests {
    use super::short_label;

    #[test]
    fn long_labels_are_shortened_in_the_middle() {
        assert_eq!(short_label("photos-2026"), "photos-2026");
        assert_eq!(short_label("12345678901234567890"), "12345678901234567890");
        assert_eq!(
            short_label("NJ_Torque_MES_Release_20261028"),
            "NJ_Torque_M…20261028"
        );
        // CJK characters take two columns.
        assert_eq!(short_label("项目资料"), "项目资料");
        assert_eq!(
            short_label("这是一个非常长的压缩包名称用于测试"),
            "这是一个非…用于测试"
        );
    }
}
