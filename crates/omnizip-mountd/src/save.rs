//! Saving the changes of a writable mount into its archive (`docs/12-mount.md` §6). A ZIP is
//! updated in place: entries that did not change are copied as they are. Other formats that
//! OmniZip writes are packed again from the archive's content with the changes applied, and the
//! new archive replaces the old one only once it is complete. RAR cannot be written.

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use omnizip_core::{
    ArchiveInfo, Capabilities, CollectOptions, Error, ErrorCode, ExtractOptions, FormatId, JobCtx,
    OpenOptions, OverwriteMode, Report, Result, Selection, Source, SymlinkPolicy, UpdateOp,
    WriteOptions, encoding::NameEncoding, extract, source,
};
use omnizip_formats::shared;

use crate::proto::SavedLayer;

/// Whether changes can be saved into an archive like this one: its format is updated in place
/// or written, it is one file (not split into volumes), and not a self-extracting program.
pub fn can_save(info: &ArchiveInfo) -> bool {
    can_write(info.format) && info.volumes <= 1 && !info.sfx
}

fn can_write(format: FormatId) -> bool {
    let caps = shared().capabilities(format);
    caps.contains(Capabilities::UPDATE)
        || caps.contains(Capabilities::WRITE) && !caps.contains(Capabilities::SINGLE_FILE)
}

/// `base` joined with a `/`-separated volume path, which must stay inside it.
fn inside(base: &Path, path: &str) -> Result<PathBuf> {
    let rel = Path::new(path);
    if path.is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(Error::invalid_argument(format!(
            "not a volume path: {path}"
        )));
    }
    Ok(base.join(rel))
}

/// The layer's new directories and changed files, as sources at their volume paths.
fn layer_sources(saved: &SavedLayer) -> Result<Vec<Source>> {
    let c = &saved.changes;
    c.dirs
        .iter()
        .chain(&c.files)
        .map(|p| Source::file(inside(&saved.layer, p)?, p.as_str()))
        .collect()
}

/// Writes the changes into the archive, then removes the layer. When saving fails, the
/// archive is as it was and the layer stays.
pub fn save(saved: &SavedLayer, threads: u32, ctx: &JobCtx) -> Result<Report> {
    let reg = shared();
    let path = &saved.archive;
    let format = reg.detect(path)?.format;
    let report = if reg.capabilities(format).contains(Capabilities::UPDATE) {
        let mut ops = Vec::new();
        if !saved.changes.entries.is_empty() {
            ops.push(UpdateOp::Delete {
                paths: saved.changes.entries.clone(),
            });
        }
        let sources = layer_sources(saved)?;
        if !sources.is_empty() {
            ops.push(UpdateOp::Add { sources });
        }
        let wo = WriteOptions {
            password: saved.password.clone(),
            threads,
            ..Default::default()
        };
        reg.update(path, &ops, &wo, ctx)?
    } else if can_write(format) {
        rebuild(saved, threads, ctx)?
    } else {
        return Err(Error::new(ErrorCode::UnsupportedOperation)
            .with("format", format)
            .with("operation", "update"));
    };
    let _ = fs::remove_dir_all(&saved.layer);
    Ok(report)
}

/// Unpacks the archive next to it, applies the changes there, and packs it again with the
/// settings it had.
fn rebuild(saved: &SavedLayer, threads: u32, ctx: &JobCtx) -> Result<Report> {
    let reg = shared();
    let path = &saved.archive;
    let oo = OpenOptions {
        password: saved.password.clone(),
        encoding: saved.encoding.as_deref().and_then(NameEncoding::from_label),
        ..Default::default()
    };
    let mut reader = reg.open(path, &oo, ctx)?;
    let info = reader.info().clone();
    if !can_save(&info) {
        return Err(Error::new(ErrorCode::UnsupportedOperation)
            .with("format", info.format)
            .with("operation", "update"));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let staging = tempfile::Builder::new()
        .prefix(".omnizip-save-")
        .tempdir_in(parent)
        .map_err(|e| Error::io(e, parent))?;
    let root = staging.path();
    let xo = ExtractOptions {
        destination: root.to_owned(),
        smart_folder: false,
        overwrite: OverwriteMode::Overwrite,
        symlinks: SymlinkPolicy::CreateIfInside,
        propagate_origin: false,
        ..Default::default()
    };
    let extracted = extract(reader.as_mut(), &xo, Selection::all(), ctx)?;
    drop(reader);

    let c = &saved.changes;
    for p in &c.deleted {
        let target = inside(root, p)?;
        let gone = match fs::symlink_metadata(&target) {
            Ok(m) if m.is_dir() => fs::remove_dir_all(&target),
            Ok(_) => fs::remove_file(&target),
            Err(_) => Ok(()),
        };
        gone.map_err(|e| Error::io(e, &target))?;
    }
    for p in &c.dirs {
        let d = inside(root, p)?;
        fs::create_dir_all(&d).map_err(|e| Error::io(e, &d))?;
    }
    for p in &c.files {
        let (from, to) = (inside(&saved.layer, p)?, inside(root, p)?);
        if let Some(d) = to.parent() {
            fs::create_dir_all(d).map_err(|e| Error::io(e, d))?;
        }
        fs::copy(&from, &to).map_err(|e| Error::io(e, &from))?;
        let mtime = fs::metadata(&from).and_then(|m| m.modified());
        if let Ok(t) = mtime {
            let _ = fs::File::options()
                .write(true)
                .open(&to)
                .and_then(|f| f.set_modified(t));
        }
    }

    let tops: Vec<PathBuf> = fs::read_dir(root)
        .map_err(|e| Error::io(e, root))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    let sources = source::collect(&tops, &CollectOptions::default(), ctx)?;
    let wo = WriteOptions {
        format: info.format,
        solid: info.solid,
        password: saved.password.clone(),
        encrypt_names: info.header_encrypted,
        comment: info.comment.clone(),
        threads,
        ..Default::default()
    };
    let mut report = reg.create(path, &sources, &wo, ctx)?;
    report.warnings.extend(extracted.warnings);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_paths_stay_inside() {
        let base = Path::new("base");
        assert_eq!(
            inside(base, "a/b.txt").unwrap(),
            base.join("a").join("b.txt")
        );
        for bad in ["", "../x", "a/../../x", "/abs"] {
            assert!(inside(base, bad).is_err(), "{bad}");
        }
        // A drive prefix is a path only on Windows; elsewhere it is a file name.
        assert_eq!(inside(base, "C:x").is_err(), cfg!(windows));
    }
}
