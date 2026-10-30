//! A writable layer over a read-only volume (`docs/12-mount.md` §6). Whatever is written goes to
//! a directory on disk, the layer; the volume below is never changed. Lookups and listings merge
//! the two: what the layer holds wins, and entries of the volume that were deleted or moved away
//! stay hidden. A file of the volume is copied into the layer the first time it is changed
//! (written, truncated, its time set, or moved), and from then on every handle reads the copy.
//! A directory created where a deleted one was does not show the old contents.
//!
//! [`OverlayFs::changes`] lists what differs, for saving into the archive. New items that systems
//! put on volumes on their own (recycle bins, `desktop.ini`, `.DS_Store`, …) stay out of it:
//! [`is_system_item`].

use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::SystemTime,
};

use serde::{Deserialize, Serialize};

use crate::{
    Attr, DirEntry, FileHandle, FileType, FsError, FsResult, FsStats, NameRules, NodeId, OpenFlags,
    ROOT, VirtualFs,
    pos::{read_full_at, write_at},
};

/// Ids of the nodes the layer creates; below are the lower volume's own.
const FIRST_NEW: NodeId = 1 << 31;

#[derive(Debug, Clone)]
struct Node {
    /// Path from the root as the volume spells it, `/`-separated; empty for the root.
    path: String,
    kind: FileType,
    /// The lower node this one shows or replaces. `None` for what the layer created, and for a
    /// directory created where a deleted one was (its old contents stay hidden).
    lower: Option<NodeId>,
    /// Exists in the layer (created there, or copied up).
    upper: bool,
    /// Size and modification time of what the layer holds for this node, kept up to date as
    /// it changes, so attributes never have to be asked of the disk (as OverlayFS takes them
    /// from the upper inode).
    meta: Option<(u64, SystemTime)>,
}

struct Open {
    node: NodeId,
    /// The lower volume's handle, for reading what has not been copied up.
    lower: Option<FileHandle>,
}

/// The layer's file of a node with open handles, shared by them: opened once (or kept from
/// [`VirtualFs::create`]) and closed with the last handle.
#[derive(Default)]
struct UpperFile {
    file: Option<Arc<File>>,
    handles: usize,
}

struct Whiteout {
    /// As the volume spells it.
    path: String,
    /// The archive entry.
    entry: String,
}

#[derive(Default)]
struct State {
    nodes: HashMap<NodeId, Node>,
    /// Folded path → node.
    paths: HashMap<String, NodeId>,
    /// Lower entries deleted or moved away, by folded path.
    whiteouts: HashMap<String, Whiteout>,
    opens: HashMap<u64, Open>,
    uppers: HashMap<NodeId, UpperFile>,
    next_node: NodeId,
    next_handle: u64,
}

/// What differs from the lower volume, as `/`-separated paths. Content of `files` is at the
/// same path in the layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Changes {
    /// Files new or changed.
    pub files: Vec<String>,
    /// New directories, including ones created where a deleted one was.
    pub dirs: Vec<String>,
    /// Lower entries deleted or moved away; a directory with everything in it.
    pub deleted: Vec<String>,
    /// Archive entries to remove before `files` and `dirs` are added: those of `deleted`,
    /// and those of changed files shown under another name (an added file replaces the entry
    /// of the same path by itself).
    pub entries: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.dirs.is_empty() && self.deleted.is_empty()
    }
}

/// A writable volume: `lower` with a layer in `layer`.
pub struct OverlayFs {
    lower: Arc<dyn VirtualFs>,
    layer: PathBuf,
    names: NameRules,
    state: Mutex<State>,
    /// Copy-ups run one at a time, outside the state lock (decoding may take a while).
    copying: Mutex<()>,
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

fn name_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, n)| n)
}

fn name_str(name: &OsStr) -> FsResult<&str> {
    let n = name.to_str().ok_or(FsError::Invalid)?;
    if n.is_empty() || n == "." || n == ".." || n.contains(['/', '\\']) {
        return Err(FsError::Invalid);
    }
    Ok(n)
}

impl OverlayFs {
    /// The layer goes in `layer`, created when missing.
    pub fn new(lower: Arc<dyn VirtualFs>, layer: PathBuf) -> FsResult<Self> {
        fs::create_dir_all(&layer)?;
        lower.getattr(ROOT)?;
        let mut state = State {
            next_node: FIRST_NEW,
            next_handle: 1,
            ..State::default()
        };
        state.nodes.insert(
            ROOT,
            Node {
                path: String::new(),
                kind: FileType::Dir,
                lower: Some(ROOT),
                upper: true,
                meta: None,
            },
        );
        state.paths.insert(String::new(), ROOT);
        Ok(Self {
            lower,
            layer,
            names: NameRules::current(),
            state: Mutex::new(state),
            copying: Mutex::new(()),
        })
    }

    /// The directory holding the layer.
    pub fn layer_dir(&self) -> &Path {
        &self.layer
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn key(&self, path: &str) -> String {
        self.names.fold(path).into_owned()
    }

    fn on_disk(&self, path: &str) -> PathBuf {
        path.split('/')
            .filter(|c| !c.is_empty())
            .fold(self.layer.clone(), |p, c| p.join(c))
    }

    fn node(state: &State, id: NodeId) -> FsResult<Node> {
        state.nodes.get(&id).cloned().ok_or(FsError::NotFound)
    }

    /// The child `name` of `parent`, registered on first sight.
    fn find(&self, state: &mut State, parent: NodeId, name: &str) -> FsResult<NodeId> {
        let p = Self::node(state, parent)?;
        if p.kind != FileType::Dir {
            return Err(FsError::NotDir);
        }
        let path = join(&p.path, name);
        let key = self.key(&path);
        if let Some(&id) = state.paths.get(&key) {
            return Ok(id);
        }
        if state.whiteouts.contains_key(&key) {
            return Err(FsError::NotFound);
        }
        let lower_parent = p.lower.ok_or(FsError::NotFound)?;
        let (attr, spelled) = self.lower.lookup_named(lower_parent, OsStr::new(name))?;
        state.nodes.insert(
            attr.node,
            Node {
                path: join(&p.path, &spelled),
                kind: attr.kind,
                lower: Some(attr.node),
                upper: false,
                meta: None,
            },
        );
        state.paths.insert(key, attr.node);
        Ok(attr.node)
    }

    fn attr(&self, state: &State, id: NodeId) -> FsResult<Attr> {
        let n = state.nodes.get(&id).ok_or(FsError::NotFound)?;
        if n.upper && !(n.kind == FileType::Dir && n.lower.is_some()) {
            let (size, mtime) = match n.meta {
                Some(m) => m,
                None => {
                    let meta = fs::metadata(self.on_disk(&n.path))?;
                    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    (meta.len(), mtime)
                }
            };
            let dir = n.kind == FileType::Dir;
            return Ok(Attr {
                node: id,
                kind: n.kind,
                size: if dir { 0 } else { size },
                mtime,
                mode: if dir { 0o755 } else { 0o644 },
                nlink: 1,
            });
        }
        let lower = n.lower.ok_or(FsError::NotFound)?;
        Ok(Attr {
            node: id,
            ..self.lower.getattr(lower)?
        })
    }

    /// Records what the layer now holds for a node.
    fn set_meta(
        &self,
        id: NodeId,
        change: impl FnOnce(Option<(u64, SystemTime)>) -> (u64, SystemTime),
    ) {
        if let Some(n) = self.lock().nodes.get_mut(&id) {
            n.meta = Some(change(n.meta));
        }
    }

    /// The children of a directory: the layer's, and the lower volume's not hidden.
    fn entries(&self, state: &mut State, dir: NodeId) -> FsResult<Vec<(String, NodeId)>> {
        let d = Self::node(state, dir)?;
        if d.kind != FileType::Dir {
            return Err(FsError::NotDir);
        }
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        if let Some(lower) = d.lower {
            for e in self.lower.readdir(lower, 0, usize::MAX)? {
                let key = self.key(&join(&d.path, &e.name));
                if !seen.insert(key.clone()) {
                    continue;
                }
                if let Some(&id) = state.paths.get(&key) {
                    out.push((name_of(&state.nodes[&id].path).to_owned(), id));
                } else if !state.whiteouts.contains_key(&key) {
                    state.nodes.insert(
                        e.attr.node,
                        Node {
                            path: join(&d.path, &e.name),
                            kind: e.attr.kind,
                            lower: Some(e.attr.node),
                            upper: false,
                            meta: None,
                        },
                    );
                    state.paths.insert(key, e.attr.node);
                    out.push((e.name, e.attr.node));
                }
            }
        }
        if d.upper
            && let Ok(rd) = fs::read_dir(self.on_disk(&d.path))
        {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let key = self.key(&join(&d.path, &name));
                // Only what the layer made: not a copy-up in progress.
                if let Some(&id) = state.paths.get(&key)
                    && seen.insert(key)
                {
                    out.push((name, id));
                }
            }
        }
        Ok(out)
    }

    /// Creates a directory's counterpart in the layer, and its parents'.
    fn make_upper_dir(&self, state: &mut State, dir: NodeId) -> FsResult<()> {
        let d = Self::node(state, dir)?;
        if d.upper {
            return Ok(());
        }
        let parent = *state
            .paths
            .get(&self.key(parent_of(&d.path)))
            .ok_or(FsError::NotFound)?;
        self.make_upper_dir(state, parent)?;
        match fs::create_dir(self.on_disk(&d.path)) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e.into()),
            _ => {}
        }
        if let Some(n) = state.nodes.get_mut(&dir) {
            n.upper = true;
        }
        Ok(())
    }

    /// Makes the layer hold a file's content (`empty`: as an empty file, without decoding).
    fn copy_up(&self, id: NodeId, empty: bool) -> FsResult<()> {
        let n = Self::node(&self.lock(), id)?;
        if n.upper || n.kind != FileType::File {
            return Ok(());
        }
        let _one = self.copying.lock().unwrap_or_else(|e| e.into_inner());
        let n = Self::node(&self.lock(), id)?;
        if n.upper {
            return Ok(());
        }
        let lower = n.lower.ok_or(FsError::NotFound)?;
        {
            let mut state = self.lock();
            let parent = *state
                .paths
                .get(&self.key(parent_of(&n.path)))
                .ok_or(FsError::NotFound)?;
            self.make_upper_dir(&mut state, parent)?;
        }
        let dest = self.on_disk(&n.path);
        let tmp = dest.with_file_name(format!(".{}.omnizip-copy", name_of(&n.path)));
        let attr = self.lower.getattr(lower)?;
        let mut offset = 0u64;
        {
            let mut out = File::create(&tmp)?;
            if !empty {
                let fh = self.lower.open(lower, OpenFlags::default())?;
                let copied = loop {
                    match self.lower.read(fh, offset, 1 << 20) {
                        Ok(data) if data.is_empty() => break Ok(()),
                        Ok(data) => {
                            if let Err(e) = out.write_all(&data) {
                                break Err(e.into());
                            }
                            offset += data.len() as u64;
                        }
                        Err(e) => break Err(e),
                    }
                };
                self.lower.release(fh);
                if let Err(e) = copied {
                    drop(out);
                    let _ = fs::remove_file(&tmp);
                    return Err(e);
                }
            }
            out.set_modified(attr.mtime)?;
        }
        fs::rename(&tmp, &dest)?;
        if let Some(n) = self.lock().nodes.get_mut(&id) {
            n.upper = true;
            n.meta = Some((offset, attr.mtime));
        }
        Ok(())
    }

    /// Copies a directory and everything in it into the layer (before it moves).
    fn copy_up_tree(&self, id: NodeId) -> FsResult<()> {
        let kind = Self::node(&self.lock(), id)?.kind;
        match kind {
            FileType::File => return self.copy_up(id, false),
            // The layer holds files and directories only.
            FileType::Symlink => return Err(FsError::Access),
            FileType::Dir => {}
        }
        let children = {
            let mut state = self.lock();
            self.make_upper_dir(&mut state, id)?;
            self.entries(&mut state, id)?
        };
        for (_, child) in children {
            self.copy_up_tree(child)?;
        }
        Ok(())
    }

    /// Removes a node (a directory only when empty), hiding the lower entry it shows.
    fn remove_node(&self, state: &mut State, id: NodeId) -> FsResult<()> {
        let n = Self::node(state, id)?;
        if n.kind == FileType::Dir && !self.entries(state, id)?.is_empty() {
            return Err(FsError::NotEmpty);
        }
        // Handles still open on it (deleted when its last one closes) keep it in the layer
        // only as long as they need it.
        state.uppers.remove(&id);
        if n.upper {
            let p = self.on_disk(&n.path);
            match if n.kind == FileType::Dir {
                fs::remove_dir(&p)
            } else {
                fs::remove_file(&p)
            } {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        let key = self.key(&n.path);
        self.whiteout(state, &n);
        state.paths.remove(&key);
        state.nodes.remove(&id);
        Ok(())
    }

    /// Hides the lower entry a node shows, gone from its place.
    fn whiteout(&self, state: &mut State, n: &Node) {
        if let Some(lower) = n.lower {
            state.whiteouts.insert(
                self.key(&n.path),
                Whiteout {
                    path: n.path.clone(),
                    entry: self
                        .lower
                        .archive_path(lower)
                        .unwrap_or_else(|| n.path.clone()),
                },
            );
        }
    }

    /// The node a handle is open on.
    fn handle_node(&self, fh: FileHandle) -> FsResult<NodeId> {
        Ok(self.lock().opens.get(&fh.0).ok_or(FsError::BadHandle)?.node)
    }

    /// What a handle reads: its node, the lower handle, and whether the layer holds the node.
    fn handle_source(&self, fh: FileHandle) -> FsResult<(NodeId, Option<FileHandle>, bool)> {
        let state = self.lock();
        let o = state.opens.get(&fh.0).ok_or(FsError::BadHandle)?;
        let upper = state.nodes.get(&o.node).ok_or(FsError::NotFound)?.upper;
        Ok((o.node, o.lower, upper))
    }

    /// The layer's file of an upper node: the one its handles share, opened on first use (and
    /// kept while any handle is open), or opened just for the caller.
    fn upper_file(&self, node: NodeId) -> FsResult<Arc<File>> {
        let mut state = self.lock();
        if let Some(f) = state.uppers.get(&node).and_then(|u| u.file.clone()) {
            return Ok(f);
        }
        let path = self.on_disk(&Self::node(&state, node)?.path);
        let f = Arc::new(File::options().read(true).write(true).open(path)?);
        if let Some(u) = state.uppers.get_mut(&node) {
            u.file = Some(f.clone());
        }
        Ok(f)
    }

    /// What differs from the lower volume.
    pub fn changes(&self) -> Changes {
        let state = self.lock();
        let mut c = Changes::default();
        for n in state.nodes.values().filter(|n| is_change(n)) {
            match n.kind {
                FileType::File => {
                    c.files.push(n.path.clone());
                    if let Some(entry) = n.lower.and_then(|l| self.lower.archive_path(l))
                        && entry != n.path
                    {
                        c.entries.push(entry);
                    }
                }
                FileType::Dir => c.dirs.push(n.path.clone()),
                FileType::Symlink => {}
            }
        }
        // What was in a deleted directory goes with it.
        for (key, w) in &state.whiteouts {
            if !key
                .match_indices('/')
                .any(|(i, _)| state.whiteouts.contains_key(&key[..i]))
            {
                c.deleted.push(w.path.clone());
                c.entries.push(w.entry.clone());
            }
        }
        c.files.sort();
        c.dirs.sort();
        c.deleted.sort();
        c.entries.sort();
        c
    }

    /// Whether anything differs from the lower volume.
    pub fn is_dirty(&self) -> bool {
        let state = self.lock();
        !state.whiteouts.is_empty() || state.nodes.values().any(is_change)
    }
}

/// Whether a node is a change to save: what the layer holds for it, unless it is a new item a
/// system put there on its own ([`is_system_item`]). A file of the archive that was changed
/// counts whatever its name.
fn is_change(n: &Node) -> bool {
    n.upper
        && match n.kind {
            FileType::File => n.lower.is_some() || !is_system_item(&n.path),
            FileType::Dir => n.lower.is_none() && !is_system_item(&n.path),
            FileType::Symlink => false,
        }
}

/// Items systems put on a volume on their own: recycle bins and indexing data at the root
/// (Windows, macOS, Linux desktops), and in any folder Explorer's folder settings and thumbnail
/// caches, Finder's metadata and Office's lock files. New ones are not saved into the archive.
pub fn is_system_item(path: &str) -> bool {
    const ROOT_DIRS: &[&str] = &[
        "$recycle.bin",
        "recycler",
        "recycled",
        "system volume information",
        ".trashes",
        ".trash",
        ".spotlight-v100",
        ".fseventsd",
        ".temporaryitems",
        ".documentrevisions-v100",
    ];
    const FILES: &[&str] = &[
        "desktop.ini",
        "thumbs.db",
        "ehthumbs.db",
        "ehthumbs_vista.db",
        ".ds_store",
        ".directory",
    ];
    let top = path.split('/').next().unwrap_or("").to_ascii_lowercase();
    // `.Trash-1000`: a Linux desktop's trash on a removable volume.
    let user_trash = top
        .strip_prefix(".trash-")
        .is_some_and(|uid| !uid.is_empty() && uid.bytes().all(|b| b.is_ascii_digit()));
    if ROOT_DIRS.contains(&top.as_str()) || user_trash {
        return true;
    }
    let name = name_of(path).to_ascii_lowercase();
    // `._name`: macOS resource forks; `~$name.docx`: Office's "who has it open".
    FILES.contains(&name.as_str()) || name.starts_with("._") || name.starts_with("~$")
}

impl VirtualFs for OverlayFs {
    fn lookup(&self, parent: NodeId, name: &OsStr) -> FsResult<Attr> {
        let mut state = self.lock();
        let id = self.find(&mut state, parent, name.to_str().ok_or(FsError::NotFound)?)?;
        self.attr(&state, id)
    }

    fn lookup_named(&self, parent: NodeId, name: &OsStr) -> FsResult<(Attr, String)> {
        let mut state = self.lock();
        let id = self.find(&mut state, parent, name.to_str().ok_or(FsError::NotFound)?)?;
        Ok((
            self.attr(&state, id)?,
            name_of(&state.nodes[&id].path).to_owned(),
        ))
    }

    fn getattr(&self, node: NodeId) -> FsResult<Attr> {
        self.attr(&self.lock(), node)
    }

    fn readdir(&self, dir: NodeId, cursor: u64, max: usize) -> FsResult<Vec<DirEntry>> {
        let mut state = self.lock();
        let all = self.entries(&mut state, dir)?;
        let start = (cursor as usize).min(all.len());
        let end = start.saturating_add(max).min(all.len());
        all[start..end]
            .iter()
            .enumerate()
            .map(|(i, (name, id))| {
                Ok(DirEntry {
                    name: name.clone(),
                    attr: self.attr(&state, *id)?,
                    cursor: (start + i) as u64 + 1,
                })
            })
            .collect()
    }

    fn open(&self, node: NodeId, _flags: OpenFlags) -> FsResult<FileHandle> {
        let mut state = self.lock();
        let n = Self::node(&state, node)?;
        if n.kind == FileType::Dir {
            return Err(FsError::IsDir);
        }
        let lower = match (n.upper, n.lower) {
            (false, Some(l)) => Some(self.lower.open(l, OpenFlags::default())?),
            _ => None,
        };
        let h = state.next_handle;
        state.next_handle += 1;
        state.opens.insert(h, Open { node, lower });
        state.uppers.entry(node).or_default().handles += 1;
        Ok(FileHandle(h))
    }

    fn read(&self, fh: FileHandle, offset: u64, size: u32) -> FsResult<Vec<u8>> {
        let (_, lower, upper) = self.handle_source(fh)?;
        if !upper {
            return self
                .lower
                .read(lower.ok_or(FsError::BadHandle)?, offset, size);
        }
        let mut buf = vec![0; size as usize];
        let n = self.read_into(fh, offset, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn read_into(&self, fh: FileHandle, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        let (node, lower, upper) = self.handle_source(fh)?;
        if !upper {
            return self
                .lower
                .read_into(lower.ok_or(FsError::BadHandle)?, offset, buf);
        }
        let f = self.upper_file(node)?;
        Ok(read_full_at(&f, buf, offset)?)
    }

    fn release(&self, fh: FileHandle) {
        let lower = {
            let mut state = self.lock();
            let Some(open) = state.opens.remove(&fh.0) else {
                return;
            };
            if let Some(u) = state.uppers.get_mut(&open.node) {
                u.handles = u.handles.saturating_sub(1);
                if u.handles == 0 {
                    state.uppers.remove(&open.node);
                }
            }
            open.lower
        };
        if let Some(l) = lower {
            self.lower.release(l);
        }
    }

    fn readlink(&self, node: NodeId) -> FsResult<PathBuf> {
        let n = Self::node(&self.lock(), node)?;
        match (n.upper, n.lower) {
            (false, Some(l)) => self.lower.readlink(l),
            _ => Err(FsError::Invalid),
        }
    }

    fn statfs(&self) -> FsResult<FsStats> {
        self.lower.statfs()
    }

    fn writable(&self) -> bool {
        true
    }

    fn layer_dir(&self) -> Option<PathBuf> {
        Some(self.layer.clone())
    }

    fn create(&self, parent: NodeId, name: &OsStr, dir: bool) -> FsResult<Attr> {
        let name = name_str(name)?;
        let mut state = self.lock();
        match self.find(&mut state, parent, name) {
            Ok(_) => return Err(FsError::Exists),
            Err(FsError::NotFound) => {}
            Err(e) => return Err(e),
        }
        self.make_upper_dir(&mut state, parent)?;
        let path = join(&Self::node(&state, parent)?.path, name);
        let disk = self.on_disk(&path);
        let file = if dir {
            fs::create_dir(&disk)?;
            None
        } else {
            // Kept for the handles about to open it.
            let f = File::options()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&disk)?;
            Some(Arc::new(f))
        };
        let id = state.next_node;
        state.next_node += 1;
        state.nodes.insert(
            id,
            Node {
                path: path.clone(),
                kind: if dir { FileType::Dir } else { FileType::File },
                lower: None,
                upper: true,
                meta: Some((0, SystemTime::now())),
            },
        );
        if file.is_some() {
            state.uppers.insert(id, UpperFile { file, handles: 0 });
        }
        state.paths.insert(self.key(&path), id);
        self.attr(&state, id)
    }

    fn write(&self, fh: FileHandle, offset: u64, data: &[u8]) -> FsResult<u32> {
        let node = self.handle_node(fh)?;
        self.copy_up(node, false)?;
        let f = self.upper_file(node)?;
        let mut n = 0;
        while n < data.len() {
            n += write_at(&f, &data[n..], offset + n as u64)?;
        }
        let end = offset + n as u64;
        self.set_meta(node, |m| {
            (m.map_or(end, |(size, _)| size.max(end)), SystemTime::now())
        });
        Ok(n as u32)
    }

    fn set_size(&self, node: NodeId, size: u64) -> FsResult<()> {
        self.copy_up(node, size == 0)?;
        self.upper_file(node)?.set_len(size)?;
        self.set_meta(node, |_| (size, SystemTime::now()));
        Ok(())
    }

    fn set_mtime(&self, node: NodeId, mtime: SystemTime) -> FsResult<()> {
        let n = Self::node(&self.lock(), node)?;
        if n.kind != FileType::File {
            // Directory times are not kept.
            return Ok(());
        }
        self.copy_up(node, false)?;
        self.upper_file(node)?.set_modified(mtime)?;
        self.set_meta(node, |m| (m.map_or(0, |(size, _)| size), mtime));
        Ok(())
    }

    fn remove(&self, parent: NodeId, name: &OsStr) -> FsResult<()> {
        let mut state = self.lock();
        let id = self.find(&mut state, parent, name.to_str().ok_or(FsError::NotFound)?)?;
        self.remove_node(&mut state, id)
    }

    fn rename(
        &self,
        parent: NodeId,
        name: &OsStr,
        new_parent: NodeId,
        new_name: &OsStr,
        replace: bool,
    ) -> FsResult<()> {
        let new_name = name_str(new_name)?;
        let id = self.find(
            &mut self.lock(),
            parent,
            name.to_str().ok_or(FsError::NotFound)?,
        )?;
        let old = Self::node(&self.lock(), id)?;
        let dest = Self::node(&self.lock(), new_parent)?;
        if dest.kind != FileType::Dir {
            return Err(FsError::NotDir);
        }
        let new_path = join(&dest.path, new_name);
        let (old_key, new_key) = (self.key(&old.path), self.key(&new_path));
        if new_key.starts_with(&format!("{old_key}/")) {
            // Into itself.
            return Err(FsError::Invalid);
        }
        if old_key != new_key {
            let mut state = self.lock();
            match self.find(&mut state, new_parent, new_name) {
                Ok(_) if !replace => return Err(FsError::Exists),
                Ok(target) if Self::node(&state, target)?.kind == FileType::Dir => {
                    return Err(FsError::Access);
                }
                Ok(target) => self.remove_node(&mut state, target)?,
                Err(FsError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        // What moves is the layer's: copy it up first.
        self.copy_up_tree(id)?;
        let mut state = self.lock();
        self.make_upper_dir(&mut state, new_parent)?;
        fs::rename(self.on_disk(&old.path), self.on_disk(&new_path))?;
        self.whiteout(&mut state, &old);
        let prefix = format!("{old_key}/");
        let moved: Vec<NodeId> = state
            .paths
            .iter()
            .filter(|(k, _)| **k == old_key || k.starts_with(&prefix))
            .map(|(_, &id)| id)
            .collect();
        let mut moved: Vec<(NodeId, String)> = moved
            .into_iter()
            .filter_map(|m| {
                let n = state.nodes.get_mut(&m)?;
                let old_k = self.key(&n.path);
                n.path = format!("{new_path}{}", &n.path[old.path.len()..]);
                // It is all in the layer now: the lower entries stay where they were.
                n.lower = None;
                if n.meta.is_none() {
                    // A directory copied up, from now on a new one.
                    n.meta = Some((0, SystemTime::now()));
                }
                Some((m, old_k))
            })
            .collect();
        for (_, old_k) in &moved {
            state.paths.remove(old_k);
        }
        for (m, _) in moved.drain(..) {
            let k = self.key(&state.nodes[&m].path);
            state.paths.insert(k, m);
        }
        Ok(())
    }

    fn flush(&self, fh: FileHandle) -> FsResult<()> {
        let node = self.handle_node(fh)?;
        let file = self.lock().uppers.get(&node).and_then(|u| u.file.clone());
        if let Some(f) = file {
            f.sync_data()?;
        }
        Ok(())
    }
}
