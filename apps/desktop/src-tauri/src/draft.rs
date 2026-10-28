//! New archives put together before they are saved (docs/04-ui-design.md §4.2, "New archive").
//!
//! A draft is the list of items the archive will contain, each with its place in the archive
//! and the file it comes from. Adding, deleting, renaming and creating folders only change this
//! list, and saving compresses exactly that layout. While it is being edited, a draft is
//! browsed like an opened archive: its reader serves the items straight from disk, so listing,
//! previews and "open with" need nothing special.

use std::{fs::File, path::PathBuf, sync::Mutex, time::SystemTime};

use omnizip_core::{
    ArchiveInfo, ArchiveReader, CollectOptions, Entry, EntryData, EntryKind, Error, ErrorCode,
    ExtractSink, FormatId, JobCtx, Result, Source, SourceKind, TreeIndex,
};

use crate::state::OpenedArchive;

/// A new archive being edited.
pub struct Draft {
    /// Shown as the archive name until it is saved.
    pub name: String,
    pub sources: Vec<Source>,
}

/// Whether `path` is `dir` or lies below it.
fn under(path: &str, dir: &str) -> bool {
    path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

impl Draft {
    pub fn new(name: String) -> Self {
        Self {
            name,
            sources: Vec::new(),
        }
    }

    /// Adds files and folders from disk into the folder `dir`. A file replaces an item of the
    /// same name; a folder merges into a folder of the same name.
    pub fn add(&mut self, paths: &[PathBuf], dir: &str) -> Result<()> {
        let opts = CollectOptions {
            prefix: dir.to_owned(),
            ..Default::default()
        };
        let added = omnizip_core::source::collect(paths, &opts, &JobCtx::default())?;
        for item in added {
            match self
                .sources
                .iter()
                .position(|s| s.archive_path == item.archive_path)
            {
                Some(i)
                    if self.sources[i].kind == SourceKind::Dir && item.kind == SourceKind::Dir => {}
                Some(_) => {
                    self.delete(&item.archive_path);
                    self.sources.push(item);
                }
                None => self.sources.push(item),
            }
        }
        Ok(())
    }

    /// Removes an item, with everything below it for a folder.
    pub fn delete(&mut self, path: &str) {
        self.sources.retain(|s| !under(&s.archive_path, path));
    }

    /// Renames the item at `from` (and moves what is below it) to `to`.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        if from == to {
            return Ok(());
        }
        if self.sources.iter().any(|s| under(&s.archive_path, to)) {
            return Err(Error::new(ErrorCode::AlreadyExists).with("path", to));
        }
        for s in self
            .sources
            .iter_mut()
            .filter(|s| under(&s.archive_path, from))
        {
            s.archive_path = format!("{to}{}", &s.archive_path[from.len()..]);
        }
        Ok(())
    }

    /// Creates an empty folder `name` in the folder `dir`.
    pub fn mkdir(&mut self, dir: &str, name: &str) -> Result<()> {
        let path = join(dir, name);
        if self.sources.iter().any(|s| under(&s.archive_path, &path)) {
            return Err(Error::new(ErrorCode::AlreadyExists).with("path", &path));
        }
        self.sources.push(Source {
            // Writers take nothing but the name from a folder.
            fs_path: PathBuf::new(),
            archive_path: path,
            kind: SourceKind::Dir,
            size: 0,
            mtime: Some(SystemTime::now()),
            mode: None,
            readonly: false,
        });
        Ok(())
    }

    /// The draft as an archive to browse, under the archive id `id`.
    pub fn open(&self, id: u32) -> OpenedArchive {
        let entries: Vec<Entry> = self
            .sources
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let kind = match s.kind {
                    SourceKind::File => EntryKind::File,
                    SourceKind::Dir => EntryKind::Dir,
                    SourceKind::Symlink { .. } => EntryKind::Symlink,
                };
                let mut e = Entry::new(i as u32, s.archive_path.clone(), kind);
                e.size = s.size;
                e.mtime = s.mtime;
                e.unix_mode = s.mode;
                if let SourceKind::Symlink { target } = &s.kind {
                    e.link_target = Some(target.clone());
                }
                e
            })
            .collect();
        let path = PathBuf::from(&self.name);
        let info = ArchiveInfo::new(&path, FormatId::Zip, 0);
        let reader = DraftReader {
            info: info.clone(),
            files: self.sources.iter().map(|s| s.fs_path.clone()).collect(),
            entries,
        };
        OpenedArchive {
            id,
            path,
            info,
            tree: TreeIndex::build(&reader.entries),
            kinds: reader.entries.iter().map(|e| e.kind).collect(),
            reader: Mutex::new(Box::new(reader)),
            password: Mutex::new(None),
            encoding: None,
            draft: true,
        }
    }
}

/// Reads the items of a draft from where they are on disk.
struct DraftReader {
    info: ArchiveInfo,
    entries: Vec<Entry>,
    /// Source file of each entry.
    files: Vec<PathBuf>,
}

impl ArchiveReader for DraftReader {
    fn info(&self) -> &ArchiveInfo {
        &self.info
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn extract_to(&mut self, sink: &mut dyn ExtractSink, ctx: &JobCtx) -> Result<()> {
        for (e, path) in self.entries.iter().zip(&self.files) {
            ctx.check()?;
            if !sink.wants(e) {
                continue;
            }
            if e.kind == EntryKind::File {
                let mut f = File::open(path).map_err(|err| Error::io(err, path))?;
                sink.entry(e, EntryData::Stream(&mut f), ctx)?;
            } else {
                sink.entry(e, EntryData::None, ctx)?;
            }
            if sink.done() {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(d: &Draft) -> Vec<&str> {
        let mut v: Vec<&str> = d.sources.iter().map(|s| s.archive_path.as_str()).collect();
        v.sort_unstable();
        v
    }

    /// A scratch folder: `docs/a.txt`, `docs/sub/b.txt`, `c.txt`.
    fn scratch() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root =
            std::env::temp_dir().join(format!("omnizip-draft-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(root.join("docs/sub")).unwrap();
        std::fs::write(root.join("docs/a.txt"), b"a").unwrap();
        std::fs::write(root.join("docs/sub/b.txt"), b"bb").unwrap();
        std::fs::write(root.join("c.txt"), b"ccc").unwrap();
        root
    }

    #[test]
    fn add_places_items_in_the_folder_and_merges_folders() {
        let root = scratch();
        let mut d = Draft::new("New archive".into());
        d.mkdir("", "out").unwrap();
        d.add(&[root.join("docs"), root.join("c.txt")], "out")
            .unwrap();
        assert_eq!(
            paths(&d),
            [
                "out",
                "out/c.txt",
                "out/docs",
                "out/docs/a.txt",
                "out/docs/sub",
                "out/docs/sub/b.txt"
            ]
        );
        // Adding the same folder again merges instead of duplicating.
        d.add(&[root.join("docs")], "out").unwrap();
        assert_eq!(d.sources.len(), 6);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delete_rename_and_mkdir() {
        let root = scratch();
        let mut d = Draft::new("New archive".into());
        d.add(&[root.join("docs"), root.join("c.txt")], "").unwrap();

        d.rename("docs", "notes").unwrap();
        assert_eq!(
            paths(&d),
            [
                "c.txt",
                "notes",
                "notes/a.txt",
                "notes/sub",
                "notes/sub/b.txt"
            ]
        );
        assert!(
            d.rename("c.txt", "notes").is_err(),
            "renaming onto an existing item"
        );

        d.delete("notes/sub");
        assert_eq!(paths(&d), ["c.txt", "notes", "notes/a.txt"]);

        d.mkdir("notes", "empty").unwrap();
        assert!(d.mkdir("notes", "empty").is_err(), "folder exists");
        assert!(d.open(1).tree.find("notes/empty").is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn saving_writes_the_layout_of_the_draft() {
        let root = scratch();
        let mut d = Draft::new("New archive".into());
        d.add(&[root.join("docs")], "").unwrap();
        d.mkdir("", "empty").unwrap();
        d.rename("docs", "notes").unwrap();
        d.add(&[root.join("c.txt")], "notes").unwrap();
        d.delete("notes/sub");

        let dest = root.join("out.zip");
        let opts = omnizip_core::WriteOptions {
            format: FormatId::Zip,
            ..Default::default()
        };
        let ctx = JobCtx::default();
        omnizip_formats::shared()
            .create(&dest, &d.sources, &opts, &ctx)
            .unwrap();
        let mut r = omnizip_formats::shared()
            .open(&dest, &Default::default(), &ctx)
            .unwrap();
        let mut names: Vec<String> = r
            .entries()
            .iter()
            .map(|e| e.path.trim_end_matches('/').to_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["empty", "notes", "notes/a.txt", "notes/c.txt"]);
        let c = r
            .entries()
            .iter()
            .position(|e| e.path == "notes/c.txt")
            .unwrap();
        let (data, _) = omnizip_core::read_entry(r.as_mut(), c as u32, 1024, &ctx).unwrap();
        assert_eq!(data, b"ccc");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_name_prefix_is_not_a_parent() {
        assert!(under("docs/a", "docs"));
        assert!(under("docs", "docs"));
        assert!(!under("docs2/a", "docs"));
    }
}
