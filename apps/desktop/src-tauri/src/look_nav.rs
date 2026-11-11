//! OmniZip's side of Omni Look's protocol 2 (OmniLook `docs/development-plan.md` §6.3): a preview
//! of an archive's entry asks, with the arrow keys, for the previous or next file. The answer is
//! the file beside it in its folder, in the order the list shows, unpacked into the cache like the
//! first; the list's selection follows.
//!
//! One pipe per OmniZip process, `\\.\pipe\omnizip-nav-<pid>-<user SID>`, open to the user only.
//! One JSON line each way per connection:
//!
//! ```text
//! → {"op":"next","path":"<the cache file shown>"}
//! ← {"path":"<the next file's cache file>","title":"<archive> › <path inside>"}  or  {"end":true}
//! ```

use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use omnizip_core::{NodeId, SortKey, TreeIndex};
use omnizip_mountd::pipe::{self, Access};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;

/// What a cache file handed to Omni Look is: an entry, and the order its folder was listed in.
#[derive(Debug, Clone, Copy)]
pub struct Served {
    pub archive: u32,
    pub node: NodeId,
    pub key: SortKey,
    pub desc: bool,
}

struct Nav {
    name: String,
    served: Mutex<HashMap<PathBuf, Served>>,
}

static NAV: OnceLock<Nav> = OnceLock::new();

#[derive(Deserialize)]
struct Ask {
    op: String,
    path: PathBuf,
}

#[derive(Serialize, Default)]
struct Answer {
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    end: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// The selection follows the preview (the browser selects and shows the entry).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Moved {
    archive: u32,
    node: NodeId,
}

/// Notes what `file` is, for later questions about its neighbours.
pub fn record(file: PathBuf, served: Served) {
    if let Some(n) = NAV.get()
        && let Ok(mut m) = n.served.lock()
    {
        m.insert(file, served);
    }
}

/// The pipe's name, serving it from the first call on.
pub fn pipe(app: &AppHandle) -> Option<String> {
    if let Some(n) = NAV.get() {
        return Some(n.name.clone());
    }
    let name = format!(
        r"\\.\pipe\omnizip-nav-{}-{}",
        std::process::id(),
        pipe::user_sid().ok()?
    );
    let first = pipe::server(&name, true, Access::User).ok()?;
    let nav = NAV.get_or_init(|| Nav {
        name: name.clone(),
        served: Mutex::new(HashMap::new()),
    });
    let app = app.clone();
    std::thread::Builder::new()
        .name("look-nav".into())
        .spawn(move || {
            let mut next = first;
            loop {
                if pipe::accept(&next).is_ok() {
                    let conn = File::from(next);
                    let app = app.clone();
                    std::thread::spawn(move || connection(&app, conn));
                }
                next = match pipe::server(&name, false, Access::User) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("look navigation pipe: {e}");
                        return;
                    }
                };
            }
        })
        .ok()?;
    Some(nav.name.clone())
}

fn connection(app: &AppHandle, conn: File) {
    let mut line = String::new();
    if BufReader::new(&conn).read_line(&mut line).is_err() {
        return;
    }
    let answer = match serde_json::from_str::<Ask>(&line) {
        Ok(ask) => answer(app, &ask),
        Err(e) => Answer {
            error: Some(e.to_string()),
            ..Default::default()
        },
    };
    let mut out = serde_json::to_string(&answer).unwrap_or_default();
    out.push('\n');
    let _ = (&conn).write_all(out.as_bytes());
}

/// The file before (`back`) or after `node` in its folder, as the list orders it: folders come
/// first there and are skipped. `None` at either end.
pub fn neighbour(
    tree: &TreeIndex,
    node: NodeId,
    key: SortKey,
    desc: bool,
    back: bool,
) -> Option<NodeId> {
    let parent = tree.node(node)?.parent?;
    let files: Vec<NodeId> = tree
        .list(parent, key, desc, 0, usize::MAX)
        .items
        .into_iter()
        .filter(|&id| tree.node(id).is_some_and(|n| !n.is_dir))
        .collect();
    let i = files.iter().position(|&id| id == node)?;
    let j = if back { i.checked_sub(1)? } else { i + 1 };
    files.get(j).copied()
}

fn answer(app: &AppHandle, ask: &Ask) -> Answer {
    let fail = |e: String| Answer {
        error: Some(e),
        ..Default::default()
    };
    let Some(nav) = NAV.get() else {
        return fail("not serving".into());
    };
    let Some(cur) = nav
        .served
        .lock()
        .ok()
        .and_then(|m| m.get(&ask.path).copied())
    else {
        return fail("not a file OmniZip gave".into());
    };
    let state = app.state::<AppState>();
    let Some(a) = state.archive(cur.archive) else {
        return fail("the archive was closed".into());
    };
    let Some(cache) = state.cache_dir.clone() else {
        return fail("no cache".into());
    };
    let Some(node) = neighbour(&a.tree, cur.node, cur.key, cur.desc, ask.op == "prev") else {
        return Answer {
            end: true,
            ..Default::default()
        };
    };
    match crate::commands::cache_entry(&a, node, &cache) {
        Ok(file) => {
            record(file.clone(), Served { node, ..cur });
            let _ = app.emit(
                "look://moved",
                Moved {
                    archive: cur.archive,
                    node,
                },
            );
            Answer {
                title: Some(format!(
                    "{} › {}",
                    crate::ops::file_name(&a.path),
                    a.tree.path(node)
                )),
                path: Some(file),
                ..Default::default()
            }
        }
        Err(e) => fail(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use omnizip_core::{Entry, EntryKind};

    use super::*;

    fn tree(paths: &[(&str, u64)]) -> TreeIndex {
        let entries: Vec<Entry> = paths
            .iter()
            .enumerate()
            .map(|(i, (p, size))| {
                let kind = if p.ends_with('/') {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                };
                let mut e = Entry::new(i as u32, (*p).into(), kind);
                e.size = *size;
                e
            })
            .collect();
        TreeIndex::build(&entries)
    }

    #[test]
    fn neighbours_in_list_order_skipping_folders() {
        let t = tree(&[
            ("d/sub/", 0),
            ("d/img10.png", 1),
            ("d/img2.png", 30),
            ("d/a.txt", 20),
            ("d/sub/deep.txt", 5),
        ]);
        let id = |p: &str| t.find(p).unwrap();
        let name = |n: Option<NodeId>| n.map(|n| t.node(n).unwrap().name.clone());
        // By name, naturally: a.txt, img2.png, img10.png (the folder "sub" is skipped).
        assert_eq!(
            name(neighbour(&t, id("d/a.txt"), SortKey::Name, false, false)),
            Some("img2.png".into())
        );
        assert_eq!(
            name(neighbour(&t, id("d/img2.png"), SortKey::Name, false, false)),
            Some("img10.png".into())
        );
        assert_eq!(
            neighbour(&t, id("d/img10.png"), SortKey::Name, false, false),
            None
        );
        assert_eq!(
            neighbour(&t, id("d/a.txt"), SortKey::Name, false, true),
            None
        );
        // The list sorted by size, largest first: img2.png (30), a.txt (20), img10.png (1).
        assert_eq!(
            name(neighbour(&t, id("d/img2.png"), SortKey::Size, true, false)),
            Some("a.txt".into())
        );
        assert_eq!(
            name(neighbour(&t, id("d/img10.png"), SortKey::Size, true, true)),
            Some("a.txt".into())
        );
    }
}
