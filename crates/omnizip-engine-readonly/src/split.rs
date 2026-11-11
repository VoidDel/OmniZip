//! A file cut into numbered parts with no container (`movie.mkv.001`, `.002`, …;
//! `docs/03-formats.md` §3): read as an archive holding that one file, so that extracting it
//! joins the parts. Parts of an archive (`x.7z.001`) are read by that archive's engine instead.

use std::{
    fs,
    path::{Path, PathBuf},
};

use omnizip_core::{
    ArchiveInfo, ArchiveReader, Entry, EntryData, EntryId, EntryKind, Error, ErrorCode,
    ExtractSink, FormatId, JobCtx, Result, volume,
};

/// The first part of the set `path` belongs to: `x.001` for `x.007`.
fn first_part(path: &Path) -> PathBuf {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return path.to_owned();
    };
    match name.rsplit_once('.') {
        Some((stem, ext)) if ext.len() == 3 && ext.bytes().all(|b| b.is_ascii_digit()) => {
            path.with_file_name(format!("{stem}.001"))
        }
        _ => path.to_owned(),
    }
}

/// An opened split file.
#[derive(Clone)]
pub struct SplitReader {
    info: ArchiveInfo,
    entries: Vec<Entry>,
    first: PathBuf,
}

impl SplitReader {
    pub fn open(path: &Path) -> Result<Self> {
        let first = first_part(path);
        if !first.is_file() {
            return Err(Error::new(ErrorCode::MissingVolume).with("path", first.display()));
        }
        let parts = volume::split_volumes(&first).unwrap_or_else(|| vec![first.clone()]);
        let size = volume::input_size(&first)?;
        let mut info = ArchiveInfo::new(&first, FormatId::Split, size);
        info.volumes = parts.len() as u32;
        let name = first
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "file".to_owned());
        let mut e = Entry::new(0, name, EntryKind::File);
        e.size = size;
        e.packed_size = Some(size);
        e.mtime = fs::metadata(&first).and_then(|m| m.modified()).ok();
        Ok(Self {
            info,
            entries: vec![e],
            first,
        })
    }
}

impl ArchiveReader for SplitReader {
    fn info(&self) -> &ArchiveInfo {
        &self.info
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn stored_range(&mut self, _id: EntryId) -> Option<(u64, u64)> {
        // In place only with one part: a mount reads one file at an offset.
        (self.info.volumes == 1).then_some((0, self.entries[0].size))
    }

    fn try_clone(&self) -> Option<Box<dyn ArchiveReader>> {
        Some(Box::new(self.clone()))
    }

    fn extract_to(&mut self, sink: &mut dyn ExtractSink, ctx: &JobCtx) -> Result<()> {
        let e = &self.entries[0];
        if !sink.wants(e) {
            return Ok(());
        }
        let mut input = volume::open_input(&self.first)?;
        sink.entry(e, EntryData::Stream(&mut input), ctx)
    }
}

#[cfg(test)]
mod tests {
    use omnizip_core::{Engine, ExtractOptions, OpenOptions, Selection, extract};

    use super::*;
    use crate::ReadonlyEngine;

    #[test]
    fn parts_join_on_extraction() {
        let d = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..250_000u32).map(|i| (i * 31 % 253) as u8).collect();
        for (i, chunk) in data.chunks(100_000).enumerate() {
            fs::write(d.path().join(format!("电影.mkv.{:03}", i + 1)), chunk).unwrap();
        }
        let ctx = JobCtx::default();
        // Detected as a split file by name: the content is no archive.
        let det = omnizip_core::detect::detect_path(&d.path().join("电影.mkv.001")).unwrap();
        assert_eq!(det.format, FormatId::Split);
        // Opened by any part.
        let third = d.path().join("电影.mkv.003");
        let mut r = ReadonlyEngine
            .open(&third, FormatId::Split, &OpenOptions::default(), &ctx)
            .unwrap();
        assert_eq!(r.info().volumes, 3);
        assert_eq!(r.entries()[0].path, "电影.mkv");
        assert_eq!(r.entries()[0].size, data.len() as u64);
        assert_eq!(r.stored_range(0), None);
        let out = d.path().join("out");
        let xo = ExtractOptions {
            destination: out.clone(),
            ..Default::default()
        };
        let report = extract(r.as_mut(), &xo, Selection::all(), &ctx).unwrap();
        assert_eq!(report.files, 1);
        // One file: extracted as it is, no folder around it.
        assert_eq!(fs::read(out.join("电影.mkv")).unwrap(), data);

        // Without the first part, opening says which one is missing.
        fs::remove_file(d.path().join("电影.mkv.001")).unwrap();
        let err = ReadonlyEngine
            .open(&third, FormatId::Split, &OpenOptions::default(), &ctx)
            .err()
            .unwrap();
        assert_eq!(err.code(), ErrorCode::MissingVolume);
    }

    #[test]
    fn names_of_parts() {
        assert_eq!(
            FormatId::from_file_name("movie.MKV.001"),
            Some(FormatId::Split)
        );
        assert_eq!(
            FormatId::from_file_name("backup.002"),
            Some(FormatId::Split)
        );
        assert_eq!(FormatId::from_file_name("x.7z.001"), Some(FormatId::SevenZ));
        assert_eq!(FormatId::from_file_name("x.zip.003"), Some(FormatId::Zip));
        assert_eq!(FormatId::from_file_name(".001"), None);
        assert_eq!(
            first_part(Path::new("/a/b.iso.012")),
            Path::new("/a/b.iso.001")
        );
    }
}
