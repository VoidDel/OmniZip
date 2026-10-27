//! Directory tree over an archive's entries (`docs/02-architecture.md` §4.5).
//!
//! The tree lives in Rust; front ends ask for one sorted page of one directory at a time, so a
//! million-entry archive costs the UI no more memory than a ten-entry one.

use std::{cmp::Ordering, collections::HashMap, time::SystemTime};

use serde::{Deserialize, Serialize};

use crate::{
    entry::{Entry, EntryId, EntryKind},
    path::logical_components,
};

/// Index of a node in a [`TreeIndex`]. The root is always `0`.
pub type NodeId = u32;

/// One file or directory in the tree. Directories that are implied by deeper paths but have no
/// entry of their own get `entry: None`.
#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub entry: Option<EntryId>,
    pub is_dir: bool,
    /// Total uncompressed size (subtree sum for directories).
    pub size: u64,
    /// Total compressed size (subtree sum for directories; 0 when unknown).
    pub packed: u64,
    /// Number of files in the subtree (1 for a file).
    pub files: u64,
    pub mtime: Option<SystemTime>,
    /// Any entry in the subtree is encrypted.
    pub encrypted: bool,
}

/// Sort columns of the file list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Packed,
    Mtime,
    Type,
}

/// A page of a directory listing.
#[derive(Debug, Clone, Serialize)]
pub struct Page {
    /// Number of items in the whole (filtered) listing.
    pub total: usize,
    pub items: Vec<NodeId>,
}

/// The tree.
#[derive(Debug, Clone)]
pub struct TreeIndex {
    nodes: Vec<Node>,
}

impl TreeIndex {
    /// Root node id.
    pub const ROOT: NodeId = 0;

    /// Builds the tree from entries.
    pub fn build(entries: &[Entry]) -> Self {
        let mut nodes = vec![Node {
            name: String::new(),
            parent: None,
            children: Vec::new(),
            entry: None,
            is_dir: true,
            size: 0,
            packed: 0,
            files: 0,
            mtime: None,
            encrypted: false,
        }];
        // (parent, name) → directory node, so deep paths resolve in O(depth).
        let mut dirs: HashMap<(NodeId, String), NodeId> = HashMap::new();

        for e in entries {
            let comps = logical_components(&e.path);
            if comps.is_empty() {
                continue;
            }
            let mut parent = Self::ROOT;
            let last = comps.len() - 1;
            for (i, comp) in comps.iter().enumerate() {
                let is_last = i == last;
                if is_last && !e.is_dir() {
                    let id = nodes.len() as NodeId;
                    nodes.push(Node {
                        name: (*comp).to_owned(),
                        parent: Some(parent),
                        children: Vec::new(),
                        entry: Some(e.id),
                        is_dir: false,
                        size: e.size,
                        packed: e.packed_size.unwrap_or(0),
                        files: u64::from(e.kind != EntryKind::Dir),
                        mtime: e.mtime,
                        encrypted: e.encrypted,
                    });
                    nodes[parent as usize].children.push(id);
                    break;
                }
                let key = (parent, (*comp).to_owned());
                let id = match dirs.get(&key) {
                    Some(&id) => id,
                    None => {
                        let id = nodes.len() as NodeId;
                        nodes.push(Node {
                            name: (*comp).to_owned(),
                            parent: Some(parent),
                            children: Vec::new(),
                            entry: None,
                            is_dir: true,
                            size: 0,
                            packed: 0,
                            files: 0,
                            mtime: None,
                            encrypted: false,
                        });
                        nodes[parent as usize].children.push(id);
                        dirs.insert(key, id);
                        id
                    }
                };
                if is_last {
                    // An explicit directory entry: attach its metadata.
                    let n = &mut nodes[id as usize];
                    n.entry = Some(e.id);
                    n.mtime = e.mtime.or(n.mtime);
                }
                parent = id;
            }
        }

        // Aggregate bottom-up. Children always have larger ids than their parents.
        for id in (1..nodes.len()).rev() {
            let (size, packed, files, enc, mtime) = {
                let n = &nodes[id];
                (n.size, n.packed, n.files, n.encrypted, n.mtime)
            };
            if let Some(p) = nodes[id].parent {
                let p = &mut nodes[p as usize];
                p.size += size;
                p.packed += packed;
                p.files += files;
                p.encrypted |= enc;
                if p.entry.is_none() {
                    p.mtime = match (p.mtime, mtime) {
                        (Some(a), Some(b)) => Some(a.max(b)),
                        (a, b) => a.or(b),
                    };
                }
            }
        }
        Self { nodes }
    }

    /// Number of nodes including the root.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the archive is empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    /// A node.
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id as usize)
    }

    /// The root node.
    pub fn root(&self) -> &Node {
        &self.nodes[0]
    }

    /// Children of a directory.
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.node(id).map_or(&[], |n| n.children.as_slice())
    }

    /// Ancestors from the root (excluded) down to `id` (included).
    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            if c == Self::ROOT {
                break;
            }
            out.push(c);
            cur = self.nodes.get(c as usize).and_then(|n| n.parent);
        }
        out.reverse();
        out
    }

    /// Slash-separated path of a node (empty for the root).
    pub fn path(&self, id: NodeId) -> String {
        self.ancestors(id)
            .iter()
            .map(|&a| self.nodes[a as usize].name.as_str())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Finds a node by slash-separated path.
    pub fn find(&self, path: &str) -> Option<NodeId> {
        let mut cur = Self::ROOT;
        for comp in logical_components(path) {
            cur = *self
                .children(cur)
                .iter()
                .find(|&&c| self.nodes[c as usize].name == comp)?;
        }
        Some(cur)
    }

    /// One sorted page of a directory. Directories always come first.
    pub fn list(
        &self,
        dir: NodeId,
        key: SortKey,
        descending: bool,
        offset: usize,
        limit: usize,
    ) -> Page {
        let mut items = self.children(dir).to_vec();
        self.sort(&mut items, key, descending);
        let total = items.len();
        let items = items.into_iter().skip(offset).take(limit).collect();
        Page { total, items }
    }

    /// Case-insensitive substring search over names in the subtree of `under`, breadth first.
    /// Returns at most `limit` results (sorted by the given key) and the total match count.
    pub fn search(
        &self,
        under: NodeId,
        query: &str,
        key: SortKey,
        descending: bool,
        offset: usize,
        limit: usize,
    ) -> Page {
        let q = query.to_lowercase();
        let mut hits = Vec::new();
        let mut queue = std::collections::VecDeque::from([under]);
        while let Some(id) = queue.pop_front() {
            for &c in self.children(id) {
                let n = &self.nodes[c as usize];
                if n.name.to_lowercase().contains(&q) {
                    hits.push(c);
                }
                if n.is_dir {
                    queue.push_back(c);
                }
            }
        }
        self.sort(&mut hits, key, descending);
        let total = hits.len();
        Page {
            total,
            items: hits.into_iter().skip(offset).take(limit).collect(),
        }
    }

    /// Every entry id in the subtrees of `ids` (directories expand recursively), in id order.
    pub fn collect_entries(&self, ids: &[NodeId]) -> Vec<EntryId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = ids.to_vec();
        while let Some(id) = stack.pop() {
            let Some(n) = self.node(id) else { continue };
            if let Some(e) = n.entry {
                out.push(e);
            }
            stack.extend_from_slice(&n.children);
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Smart extraction (`docs/03-formats.md` §6.2): `true` when the archive should be unpacked
    /// into a new folder named after it, `false` when its content is a single top-level item.
    pub fn needs_wrapper_folder(&self) -> bool {
        self.root().children.len() != 1
    }

    /// Name of the single top-level item, when there is exactly one.
    pub fn single_top_level(&self) -> Option<&str> {
        match self.root().children.as_slice() {
            [only] => Some(self.nodes[*only as usize].name.as_str()),
            _ => None,
        }
    }

    fn sort(&self, items: &mut [NodeId], key: SortKey, descending: bool) {
        items.sort_by(|&a, &b| {
            let (na, nb) = (&self.nodes[a as usize], &self.nodes[b as usize]);
            // Directories first regardless of direction.
            match (na.is_dir, nb.is_dir) {
                (true, false) => return Ordering::Less,
                (false, true) => return Ordering::Greater,
                _ => {}
            }
            let ord = match key {
                SortKey::Name => Ordering::Equal,
                SortKey::Size => na.size.cmp(&nb.size),
                SortKey::Packed => na.packed.cmp(&nb.packed),
                SortKey::Mtime => na.mtime.cmp(&nb.mtime),
                SortKey::Type => extension(&na.name).cmp(&extension(&nb.name)),
            }
            .then_with(|| natural_cmp(&na.name, &nb.name));
            if descending { ord.reverse() } else { ord }
        });
    }
}

fn extension(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[i + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// Case-insensitive natural ordering: `file2` < `file10`.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    bi.next();
                }
                let ta = na.trim_start_matches('0');
                let tb = nb.trim_start_matches('0');
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                ai.next();
                bi.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: u32, path: &str, size: u64) -> Entry {
        let mut e = Entry::new(id, path.into(), EntryKind::File);
        e.size = size;
        e.packed_size = Some(size / 2);
        e
    }

    fn dir(id: u32, path: &str) -> Entry {
        Entry::new(id, path.into(), EntryKind::Dir)
    }

    fn sample() -> TreeIndex {
        TreeIndex::build(&[
            file(0, "src/main.rs", 100),
            file(1, "src/lib/a.rs", 50),
            dir(2, "src/"),
            file(3, "README.md", 10),
            file(4, "file10.txt", 1),
            file(5, "file2.txt", 1),
            dir(6, "empty/"),
        ])
    }

    #[test]
    fn builds_implicit_dirs_and_aggregates() {
        let t = sample();
        let src = t.find("src").unwrap();
        let n = t.node(src).unwrap();
        assert!(n.is_dir);
        assert_eq!(
            n.entry,
            Some(2),
            "explicit dir entry is attached to the implicit node"
        );
        assert_eq!(n.size, 150);
        assert_eq!(n.files, 2);
        assert_eq!(t.root().size, 162);
        assert_eq!(t.root().packed, 50 + 25 + 5);
        assert_eq!(t.path(t.find("src/lib/a.rs").unwrap()), "src/lib/a.rs");
        assert_eq!(t.find("nope"), None);
    }

    #[test]
    fn listing_sorts_dirs_first_and_naturally() {
        let t = sample();
        let p = t.list(TreeIndex::ROOT, SortKey::Name, false, 0, 10);
        let names: Vec<_> = p
            .items
            .iter()
            .map(|&i| t.node(i).unwrap().name.clone())
            .collect();
        assert_eq!(
            names,
            ["empty", "src", "file2.txt", "file10.txt", "README.md"]
        );
        let p = t.list(TreeIndex::ROOT, SortKey::Size, true, 0, 10);
        let first_file = t.node(p.items[2]).unwrap();
        assert_eq!(first_file.name, "README.md");
        let p = t.list(TreeIndex::ROOT, SortKey::Name, false, 3, 1);
        assert_eq!(p.total, 5);
        assert_eq!(t.node(p.items[0]).unwrap().name, "file10.txt");
    }

    #[test]
    fn search_and_collect() {
        let t = sample();
        let p = t.search(TreeIndex::ROOT, ".RS", SortKey::Name, false, 0, 10);
        assert_eq!(p.total, 2);
        let src = t.find("src").unwrap();
        assert_eq!(t.collect_entries(&[src]), vec![0, 1, 2]);
    }

    #[test]
    fn smart_root() {
        let t = TreeIndex::build(&[file(0, "proj/a", 1), file(1, "proj/b", 1)]);
        assert!(!t.needs_wrapper_folder());
        assert_eq!(t.single_top_level(), Some("proj"));
        assert!(sample().needs_wrapper_folder());
    }

    #[test]
    fn natural_order() {
        assert_eq!(natural_cmp("a2", "a10"), Ordering::Less);
        assert_eq!(
            natural_cmp("A", "a"),
            Ordering::Less,
            "ties broken byte-wise"
        );
        assert_eq!(
            natural_cmp("img007", "img7"),
            Ordering::Less,
            "equal values tie-break"
        );
        assert_eq!(natural_cmp("img8", "img007"), Ordering::Greater);
        assert_eq!(natural_cmp("b", "A"), Ordering::Greater);
    }
}
