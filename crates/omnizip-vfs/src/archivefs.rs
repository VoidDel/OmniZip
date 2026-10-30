//! An archive as a read-only file system (`docs/12-mount.md` §3, §5, §6).
//!
//! Directory structure comes from the core [`TreeIndex`]; names are cleaned per directory on
//! first use ([`crate::names`]); file data is decoded on the first read of each entry into the
//! [`ContentCache`] and served by offset afterwards. Solid archives (mount class C) and
//! single-stream ones (D) are decoded in one pass on first access instead, since every entry
//! would otherwise restart the solid block or the stream. Files stored as they are (plain tar,
//! stored ZIP entries) are read in place from the archive file, neither decoded nor cached.

use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs::File,
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use omnizip_core::{
    ArchiveInfo, ArchiveReader, Entry, EntryData, EntryId, EntryKind, Error, ErrorCode,
    ExtractSink, FixedPassword, Interaction, JobCtx, MountClass, NoInteraction, NullProgress,
    TreeIndex, path::logical_components,
};

use crate::{
    Attr, DirEntry, FileHandle, FileType, FsError, FsResult, FsStats, NodeId, OpenFlags, VirtualFs,
    cache::{CacheLimits, Content, ContentBuilder, ContentCache},
    names::{NameRules, assign},
};

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
    pub password: Option<String>,
    /// Decode a whole solid archive on first access when it fits the disk budget.
    pub warm_solid: bool,
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
        }
    }
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
    reader: Mutex<Box<dyn ArchiveReader>>,
    info: ArchiveInfo,
    entries: Vec<Entry>,
    tree: TreeIndex,
    opts: FsOptions,
    dirs: RwLock<HashMap<NodeId, Arc<DirView>>>,
    cache: ContentCache,
    handles: Mutex<HashMap<u64, NodeId>>,
    next_handle: Mutex<u64>,
    interaction: Arc<dyn Interaction>,
    fallback_time: SystemTime,
    stats: FsStats,
    warmed: AtomicBool,
    /// The archive file, for reading stored entries in place (an archive of one file).
    file: Option<File>,
    /// Per entry, where its data lies as it is in [`file`](Self::file), asked once.
    stored: Vec<OnceLock<Option<(u64, u64)>>>,
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
        let cache = ContentCache::new(opts.cache, opts.cache_dir.as_deref())?;
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
        Ok(Self {
            file,
            stored,
            reader: Mutex::new(reader),
            info,
            entries,
            tree,
            opts,
            dirs: RwLock::new(HashMap::new()),
            cache,
            handles: Mutex::new(HashMap::new()),
            next_handle: Mutex::new(1),
            interaction,
            fallback_time,
            stats,
            warmed: AtomicBool::new(false),
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
    /// relative to the archive root for hard links. `None` when it leaves the volume.
    fn link_node(&self, node: NodeId, target: &str, from_root: bool) -> Option<NodeId> {
        if target.starts_with('/') || target.starts_with('\\') || target.contains(':') {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        if !from_root {
            let parent = self.tree.node(node)?.parent?;
            let base = self.tree.path(parent);
            parts.extend(logical_components(&base).into_iter().map(str::to_owned));
        }
        for comp in target.split(['/', '\\']) {
            match comp {
                "" | "." => {}
                ".." => {
                    parts.pop()?;
                }
                c => parts.push(c.to_owned()),
            }
        }
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

    /// Decoded content of an entry, decoding it on first use.
    fn content(&self, id: EntryId) -> FsResult<Content> {
        if let Some(c) = self.cache.get(id) {
            return Ok(c);
        }
        let mut reader = self.reader.lock().unwrap_or_else(|e| e.into_inner());
        // Another thread may have decoded it while we waited.
        if let Some(c) = self.cache.get(id) {
            return Ok(c);
        }
        // A solid block, or an archive that is one compressed stream (tar.gz), decodes only
        // from its start: decoding one entry passes over the ones before it. Caching everything
        // that fits in one pass keeps reading a whole folder from decoding the same start over
        // and over. A stream has no other way around it; for solid archives it is a setting.
        let one_pass = match self.mount_class() {
            MountClass::C => self.opts.warm_solid,
            MountClass::D => true,
            MountClass::A | MountClass::B => false,
        };
        let mut targets: HashSet<EntryId> = HashSet::from([id]);
        if one_pass && !self.warmed.swap(true, Ordering::SeqCst) {
            targets.extend(self.warm_targets());
        }
        self.decode(reader.as_mut(), targets)?;
        drop(reader);
        self.cache.get(id).ok_or(FsError::Io)
    }

    /// Entries a full warm-up decodes: every file not yet cached, within the disk budget.
    fn warm_targets(&self) -> HashSet<EntryId> {
        let mut budget = self.cache.limits().disk;
        let mut out = HashSet::new();
        for e in &self.entries {
            if e.kind != EntryKind::File || self.cache.contains(e.id) {
                continue;
            }
            if e.size > budget {
                break;
            }
            budget -= e.size;
            out.insert(e.id);
        }
        out
    }

    fn decode(&self, reader: &mut dyn ArchiveReader, targets: HashSet<EntryId>) -> FsResult<()> {
        let mut sink = CacheSink {
            fs: self,
            pending: targets,
        };
        match reader.extract_to(&mut sink, &self.ctx()) {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!(code = e.code().id(), "mount: decoding failed");
                Err(e.into())
            }
        }
    }

    /// Decodes every file into the cache in one pass, as far as the disk budget allows
    /// ("warm up", useful before seeking around a solid archive). Returns the number of
    /// entries now cached.
    pub fn warm(&self) -> FsResult<usize> {
        let mut reader = self.reader.lock().unwrap_or_else(|e| e.into_inner());
        self.warmed.store(true, Ordering::SeqCst);
        let targets = self.warm_targets();
        if !targets.is_empty() {
            self.decode(reader.as_mut(), targets)?;
        }
        Ok(self.cache.len())
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
        let mut reader = self.reader.lock().unwrap_or_else(|e| e.into_inner());
        self.decode(reader.as_mut(), HashSet::from([e.id]))
    }

    /// Where an entry's data lies as it is in the archive file, asked of the reader once.
    fn stored(&self, id: EntryId) -> Option<(u64, u64)> {
        self.file.as_ref()?;
        *self.stored.get(id as usize)?.get_or_init(|| {
            self.reader
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .stored_range(id)
        })
    }

    /// Reads a stored entry in place: up to `size` bytes at `offset`, never past `declared`.
    fn read_stored(
        &self,
        (start, len): (u64, u64),
        declared: u64,
        offset: u64,
        size: u32,
    ) -> FsResult<Vec<u8>> {
        let f = self.file.as_ref().ok_or(FsError::Io)?;
        let n = len
            .min(declared)
            .saturating_sub(offset)
            .min(u64::from(size)) as usize;
        let mut buf = vec![0; n];
        if crate::pos::read_full_at(f, &mut buf, start + offset)? < n {
            // The archive is shorter than its directory says.
            return Err(FsError::Io);
        }
        Ok(buf)
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
    if label.is_empty() { name } else { label }
}

/// Receives decoded entries into the cache.
struct CacheSink<'a> {
    fs: &'a ArchiveFs,
    pending: HashSet<EntryId>,
}

impl ExtractSink for CacheSink<'_> {
    fn wants(&self, entry: &Entry) -> bool {
        self.pending.contains(&entry.id)
    }

    fn entry(
        &mut self,
        entry: &Entry,
        data: EntryData<'_>,
        ctx: &JobCtx,
    ) -> omnizip_core::Result<()> {
        ctx.check()?;
        self.pending.remove(&entry.id);
        let cache = &self.fs.cache;
        let limits = cache.limits();
        // Never more than declared (bomb guard); unknown sizes are bounded by the disk budget.
        let cap = if entry.size_known {
            entry.size
        } else {
            limits.disk
        };
        let oversize = |e: io::Error| {
            if ContentBuilder::is_oversize(&e) {
                Error::new(ErrorCode::CompressionBomb).with("path", &entry.path)
            } else {
                Error::from(e)
            }
        };
        let content = match data {
            EntryData::Stream(r) => {
                let mut b = cache.builder(cap);
                io::copy(r, &mut b).map_err(oversize)?;
                b.finish()?
            }
            EntryData::WriteTo(w) => {
                let path = tempfile::Builder::new()
                    .prefix("entry-")
                    .tempfile_in(cache.dir())?
                    .into_temp_path();
                w(&path)?;
                ContentBuilder::adopt(path, cap, limits.inline_max).map_err(oversize)?
            }
            EntryData::None => Content::Mem(Arc::from(&[][..])),
        };
        cache.insert(entry.id, content);
        Ok(())
    }

    fn done(&self) -> bool {
        self.pending.is_empty()
    }
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
        let node = self.handle_node(fh)?;
        let shape = self.shape(node)?;
        match (shape.attr.kind, shape.data) {
            (FileType::Dir, _) => Err(FsError::IsDir),
            (_, Data::None) => Ok(Vec::new()),
            (_, Data::Text(t)) => Ok(Content::Mem(t).read_at(offset, size as usize)?),
            (_, Data::Entry(id)) => {
                if offset >= shape.attr.size && self.entries[id as usize].size_known {
                    return Ok(Vec::new());
                }
                if let Some(range) = self.stored(id) {
                    return self.read_stored(range, shape.attr.size, offset, size);
                }
                Ok(self.content(id)?.read_at(offset, size as usize)?)
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
