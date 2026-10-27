//! Collecting files from disk for compression.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::{Error, JobCtx, Result};

/// Kind of a source item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    File,
    Dir,
    Symlink { target: String },
}

/// One file system item to put into an archive.
#[derive(Debug, Clone)]
pub struct Source {
    /// Where to read it from.
    pub fs_path: PathBuf,
    /// Path inside the archive, `/`-separated, without a trailing slash.
    pub archive_path: String,
    pub kind: SourceKind,
    pub size: u64,
    pub mtime: Option<SystemTime>,
    /// Unix permission bits (Unix only).
    pub mode: Option<u32>,
    pub readonly: bool,
}

impl Source {
    /// A source whose content is a file on disk, placed at `archive_path`.
    pub fn file(fs_path: impl Into<PathBuf>, archive_path: impl Into<String>) -> Result<Self> {
        let fs_path = fs_path.into();
        let meta = fs::metadata(&fs_path).map_err(|e| Error::io(e, &fs_path))?;
        Ok(Self {
            archive_path: archive_path.into(),
            kind: if meta.is_dir() {
                SourceKind::Dir
            } else {
                SourceKind::File
            },
            size: if meta.is_dir() { 0 } else { meta.len() },
            mtime: meta.modified().ok(),
            mode: mode_of(&meta),
            readonly: meta.permissions().readonly(),
            fs_path,
        })
    }
}

/// Options for [`collect`].
#[derive(Debug, Clone, Default)]
pub struct CollectOptions {
    /// Only files matching one of these globs are kept (empty = everything).
    pub include: Vec<String>,
    /// Files and directories matching these globs are skipped.
    pub exclude: Vec<String>,
    /// Archive the targets of symbolic links instead of the links.
    pub follow_symlinks: bool,
    /// Skip hidden files and directories.
    pub skip_hidden: bool,
    /// Directory inside the archive to place everything under (`""` = root).
    pub prefix: String,
    /// Files never to include (typically the archive being written).
    pub never: Vec<PathBuf>,
}

fn build_globs(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let g = Glob::new(p).map_err(|e| Error::invalid_argument(format!("{p}: {e}")))?;
        b.add(g);
        // A pattern without a slash also matches at any depth ("node_modules" or "*.log").
        if !p.contains('/') {
            let deep = format!("**/{p}");
            b.add(Glob::new(&deep).map_err(|e| Error::invalid_argument(format!("{p}: {e}")))?);
        }
    }
    b.build()
        .map(Some)
        .map_err(|e| Error::invalid_argument(e.to_string()))
}

#[cfg(unix)]
fn mode_of(meta: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(meta.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_of(_meta: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(windows)]
fn is_hidden(path: &Path, meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
        || path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

#[cfg(not(windows))]
fn is_hidden(path: &Path, _meta: &fs::Metadata) -> bool {
    path.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

struct Walker<'a> {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
    opts: &'a CollectOptions,
    never: HashSet<PathBuf>,
    visited: HashSet<PathBuf>,
    out: Vec<Source>,
    ctx: &'a JobCtx,
}

/// Walks `paths` (files or directories) and returns everything to archive, depth-first in name
/// order. Each top-level path appears in the archive under its own name.
pub fn collect(paths: &[PathBuf], opts: &CollectOptions, ctx: &JobCtx) -> Result<Vec<Source>> {
    let never = opts
        .never
        .iter()
        .filter_map(|p| fs::canonicalize(p).ok())
        .collect();
    let mut w = Walker {
        include: build_globs(&opts.include)?,
        exclude: build_globs(&opts.exclude)?,
        opts,
        never,
        visited: HashSet::new(),
        out: Vec::new(),
        ctx,
    };
    let prefix = opts.prefix.trim_matches('/');
    for p in paths {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .or_else(|| {
                fs::canonicalize(p)
                    .ok()?
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .ok_or_else(|| Error::invalid_argument(format!("cannot archive {}", p.display())))?;
        let archive_path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        w.visit(p, archive_path, true)?;
    }
    Ok(w.out)
}

impl Walker<'_> {
    fn visit(&mut self, path: &Path, archive_path: String, top: bool) -> Result<()> {
        self.ctx.check()?;
        let lmeta = fs::symlink_metadata(path).map_err(|e| Error::io(e, path))?;
        let is_link = lmeta.file_type().is_symlink();
        let meta = if is_link && self.opts.follow_symlinks {
            match fs::metadata(path) {
                Ok(m) => m,
                Err(_) => lmeta.clone(), // dangling link: keep as link
            }
        } else {
            lmeta.clone()
        };
        if !top && self.opts.skip_hidden && is_hidden(path, &meta) {
            return Ok(());
        }
        if let Some(ex) = &self.exclude
            && ex.is_match(&archive_path)
        {
            return Ok(());
        }
        if let Ok(canon) = fs::canonicalize(path)
            && self.never.contains(&canon)
        {
            return Ok(());
        }

        if is_link && !meta.is_dir() && !(self.opts.follow_symlinks && meta.is_file()) {
            let target = fs::read_link(path).map_err(|e| Error::io(e, path))?;
            if self.included(&archive_path) {
                self.out.push(Source {
                    fs_path: path.to_owned(),
                    archive_path,
                    kind: SourceKind::Symlink {
                        target: target.to_string_lossy().replace('\\', "/"),
                    },
                    size: 0,
                    mtime: lmeta.modified().ok(),
                    mode: mode_of(&lmeta),
                    readonly: false,
                });
            }
            return Ok(());
        }

        if meta.is_dir() {
            // Cycle protection when following links.
            if let Ok(canon) = fs::canonicalize(path)
                && !self.visited.insert(canon)
            {
                return Ok(());
            }
            let dir_index = self.out.len();
            self.out.push(Source {
                fs_path: path.to_owned(),
                archive_path: archive_path.clone(),
                kind: SourceKind::Dir,
                size: 0,
                mtime: meta.modified().ok(),
                mode: mode_of(&meta),
                readonly: false,
            });
            let mut children: Vec<_> = fs::read_dir(path)
                .map_err(|e| Error::io(e, path))?
                .filter_map(|e| e.ok())
                .collect();
            children.sort_by_key(|e| e.file_name());
            for child in children {
                let name = child.file_name().to_string_lossy().into_owned();
                self.visit(&child.path(), format!("{archive_path}/{name}"), false)?;
            }
            // With an include filter, drop directories that ended up empty because of it.
            if self.include.is_some() && self.out.len() == dir_index + 1 && !top {
                self.out.pop();
            }
            return Ok(());
        }

        if self.included(&archive_path) {
            self.out.push(Source {
                fs_path: path.to_owned(),
                archive_path,
                kind: SourceKind::File,
                size: meta.len(),
                mtime: meta.modified().ok(),
                mode: mode_of(&meta),
                readonly: meta.permissions().readonly(),
            });
        }
        Ok(())
    }

    fn included(&self, archive_path: &str) -> bool {
        self.include
            .as_ref()
            .is_none_or(|g| g.is_match(archive_path))
    }
}

/// Sum of file sizes.
pub fn total_size(sources: &[Source]) -> u64 {
    sources
        .iter()
        .filter(|s| s.kind == SourceKind::File)
        .map(|s| s.size)
        .sum()
}

/// Number of regular files.
pub fn file_count(sources: &[Source]) -> u64 {
    sources
        .iter()
        .filter(|s| s.kind == SourceKind::File)
        .count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("proj");
        fs::create_dir_all(p.join("src/deep")).unwrap();
        fs::create_dir_all(p.join("node_modules/x")).unwrap();
        fs::write(p.join("src/main.rs"), "fn main(){}").unwrap();
        fs::write(p.join("src/deep/b.txt"), "b").unwrap();
        fs::write(p.join("node_modules/x/i.js"), "x").unwrap();
        fs::write(p.join("debug.log"), "log").unwrap();
        fs::write(p.join(".hidden"), "h").unwrap();
        d
    }

    fn paths(v: &[Source]) -> Vec<&str> {
        v.iter().map(|s| s.archive_path.as_str()).collect()
    }

    #[test]
    fn walks_in_order() {
        let d = tree();
        let ctx = JobCtx::default();
        let v = collect(&[d.path().join("proj")], &CollectOptions::default(), &ctx).unwrap();
        assert_eq!(
            paths(&v),
            [
                "proj",
                "proj/.hidden",
                "proj/debug.log",
                "proj/node_modules",
                "proj/node_modules/x",
                "proj/node_modules/x/i.js",
                "proj/src",
                "proj/src/deep",
                "proj/src/deep/b.txt",
                "proj/src/main.rs"
            ]
        );
        assert_eq!(total_size(&v), 11 + 1 + 1 + 3 + 1);
    }

    #[test]
    fn filters() {
        let d = tree();
        let ctx = JobCtx::default();
        let opts = CollectOptions {
            exclude: vec!["node_modules".into(), "*.log".into()],
            skip_hidden: true,
            prefix: "backup".into(),
            ..Default::default()
        };
        let v = collect(&[d.path().join("proj")], &opts, &ctx).unwrap();
        assert_eq!(
            paths(&v),
            [
                "backup/proj",
                "backup/proj/src",
                "backup/proj/src/deep",
                "backup/proj/src/deep/b.txt",
                "backup/proj/src/main.rs"
            ]
        );
        let opts = CollectOptions {
            include: vec!["*.rs".into()],
            ..Default::default()
        };
        let v = collect(&[d.path().join("proj")], &opts, &ctx).unwrap();
        assert_eq!(paths(&v), ["proj", "proj/src", "proj/src/main.rs"]);
    }

    #[test]
    fn never_list_excludes_output() {
        let d = tree();
        let ctx = JobCtx::default();
        let out = d.path().join("proj/debug.log");
        let opts = CollectOptions {
            never: vec![out],
            ..Default::default()
        };
        let v = collect(&[d.path().join("proj")], &opts, &ctx).unwrap();
        assert!(!paths(&v).contains(&"proj/debug.log"));
    }
}
