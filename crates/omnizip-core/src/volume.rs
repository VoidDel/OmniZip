//! Input and output files, including raw-split multi-volume sets (`name.7z.001`, `.002`, …).
//!
//! Output is written to a temporary file next to the destination and only renamed into place by
//! [`Output::commit`], so a failed or cancelled job never leaves a half-written archive behind.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use crate::{Error, ErrorCode, Result};

/// `Read + Seek + Send`, boxed by engines that accept any input.
pub trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

/// `Write + Seek + Send`.
pub trait WriteSeek: Write + Seek + Send {}
impl<T: Write + Seek + Send> WriteSeek for T {}

/// Paths of all volumes when `path` is the first part of a raw split set (`*.001`).
pub fn split_volumes(path: &Path) -> Option<Vec<PathBuf>> {
    let name = path.file_name()?.to_str()?;
    let (stem, ext) = name.rsplit_once('.')?;
    if ext.len() != 3 || !ext.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let first: u32 = ext.parse().ok()?;
    if first != 1 {
        return None;
    }
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut out = Vec::new();
    for n in 1..=9999u32 {
        let p = dir.join(format!("{stem}.{n:03}"));
        if !p.is_file() {
            break;
        }
        out.push(p);
    }
    (!out.is_empty()).then_some(out)
}

/// Opens an archive for reading, transparently joining split volumes.
pub fn open_input(path: &Path) -> Result<Box<dyn ReadSeek>> {
    if let Some(vols) = split_volumes(path)
        && vols.len() > 1
    {
        return Ok(Box::new(MultiVolumeReader::open(&vols)?));
    }
    let f = File::open(path).map_err(|e| Error::io(e, path))?;
    Ok(Box::new(io::BufReader::with_capacity(256 * 1024, f)))
}

/// Total size of the input (all volumes).
pub fn input_size(path: &Path) -> Result<u64> {
    if let Some(vols) = split_volumes(path) {
        let mut total = 0;
        for v in vols {
            total += fs::metadata(&v).map_err(|e| Error::io(e, &v))?.len();
        }
        return Ok(total);
    }
    Ok(fs::metadata(path).map_err(|e| Error::io(e, path))?.len())
}

/// Concatenation of several files as one seekable stream.
pub struct MultiVolumeReader {
    files: Vec<File>,
    /// Start offset of each volume within the joined stream.
    starts: Vec<u64>,
    len: u64,
    pos: u64,
}

impl MultiVolumeReader {
    /// Opens the volumes in order.
    pub fn open(paths: &[PathBuf]) -> Result<Self> {
        let mut files = Vec::with_capacity(paths.len());
        let mut starts = Vec::with_capacity(paths.len());
        let mut len = 0;
        for p in paths {
            let f = File::open(p).map_err(|e| Error::io(e, p))?;
            starts.push(len);
            len += f.metadata().map_err(|e| Error::io(e, p))?.len();
            files.push(f);
        }
        Ok(Self {
            files,
            starts,
            len,
            pos: 0,
        })
    }

    fn volume_at(&self, pos: u64) -> usize {
        match self.starts.binary_search(&pos) {
            Ok(i) => {
                // Skip empty volumes that share a start offset.
                let mut i = i;
                while i + 1 < self.starts.len() && self.starts[i + 1] == pos {
                    i += 1;
                }
                i
            }
            Err(i) => i - 1,
        }
    }
}

impl Read for MultiVolumeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let v = self.volume_at(self.pos);
        let vol_end = self.starts.get(v + 1).copied().unwrap_or(self.len);
        let within = self.pos - self.starts[v];
        let want = buf.len().min((vol_end - self.pos) as usize);
        let f = &mut self.files[v];
        f.seek(SeekFrom::Start(within))?;
        let n = f.read(&mut buf[..want])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for MultiVolumeReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::End(d) => self.len as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if new < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}

/// Writes fixed-size volumes `base.001`, `base.002`, … and supports seeking across them.
pub struct SplitWriter {
    base: PathBuf,
    volume_size: u64,
    files: Vec<File>,
    pos: u64,
}

impl SplitWriter {
    /// Volumes will be named `<base>.001` onwards.
    pub fn new(base: PathBuf, volume_size: u64) -> Result<Self> {
        if volume_size < 64 * 1024 {
            return Err(Error::invalid_argument(
                "volume size must be at least 64 KiB",
            ));
        }
        Ok(Self {
            base,
            volume_size,
            files: Vec::new(),
            pos: 0,
        })
    }

    /// Path of volume `n` (0-based).
    pub fn volume_path(&self, n: usize) -> PathBuf {
        let mut s = self.base.as_os_str().to_owned();
        s.push(format!(".{:03}", n + 1));
        PathBuf::from(s)
    }

    fn file(&mut self, n: usize) -> io::Result<&mut File> {
        while self.files.len() <= n {
            let p = self.volume_path(self.files.len());
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(p)?;
            self.files.push(f);
        }
        Ok(&mut self.files[n])
    }

    /// Paths of the volumes written so far.
    pub fn paths(&self) -> Vec<PathBuf> {
        (0..self.files.len()).map(|n| self.volume_path(n)).collect()
    }
}

impl Write for SplitWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let n = (self.pos / self.volume_size) as usize;
        let off = self.pos % self.volume_size;
        let room = (self.volume_size - off) as usize;
        let take = buf.len().min(room);
        let f = self.file(n)?;
        f.seek(SeekFrom::Start(off))?;
        let written = f.write(&buf[..take])?;
        self.pos += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        for f in &mut self.files {
            f.flush()?;
        }
        Ok(())
    }
}

impl Seek for SplitWriter {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let end = self
            .files
            .iter()
            .map(|f| f.metadata().map(|m| m.len()).unwrap_or(0))
            .sum::<u64>();
        let new = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::End(d) => end as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if new < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}

enum OutputKind {
    Single(tempfile::NamedTempFile),
    Split(SplitWriter),
}

/// An archive being written. Dropping it without [`Output::commit`] removes everything.
pub struct Output {
    kind: Option<OutputKind>,
    dest: PathBuf,
}

impl Output {
    /// Creates the output for `dest`, split into volumes when `volume_size` is set.
    pub fn create(dest: &Path, volume_size: Option<u64>) -> Result<Self> {
        let dir = dest
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(dir).map_err(|e| Error::io(e, dir))?;
        let kind = match volume_size {
            Some(size) => OutputKind::Split(SplitWriter::new(dest.to_owned(), size)?),
            None => OutputKind::Single(
                tempfile::Builder::new()
                    .prefix(".omnizip-")
                    .suffix(".tmp")
                    .tempfile_in(dir)
                    .map_err(|e| Error::io(e, dir))?,
            ),
        };
        Ok(Self {
            kind: Some(kind),
            dest: dest.to_owned(),
        })
    }

    /// Moves the finished archive into place and returns the written paths.
    pub fn commit(mut self) -> Result<Vec<PathBuf>> {
        match self.kind.take().expect("commit called once") {
            OutputKind::Single(tmp) => {
                tmp.as_file()
                    .sync_all()
                    .map_err(|e| Error::io(e, &self.dest))?;
                tmp.persist(&self.dest)
                    .map_err(|e| Error::io(e.error, &self.dest))?;
                Ok(vec![self.dest.clone()])
            }
            OutputKind::Split(mut w) => {
                w.flush().map_err(|e| Error::io(e, &self.dest))?;
                Ok(w.paths())
            }
        }
    }

    fn inner(&mut self) -> &mut dyn WriteSeek {
        match self.kind.as_mut().expect("output used after commit") {
            OutputKind::Single(t) => t.as_file_mut(),
            OutputKind::Split(w) => w,
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        if let Some(OutputKind::Split(w)) = self.kind.take() {
            for p in w.paths() {
                let _ = fs::remove_file(p);
            }
        }
        // A NamedTempFile deletes itself.
    }
}

impl Write for Output {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner().flush()
    }
}

impl Seek for Output {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner().seek(pos)
    }
}

/// Fails with `AlreadyExists` when `dest` exists (callers decide whether to overwrite).
pub fn ensure_absent(dest: &Path) -> Result<()> {
    if dest.exists() {
        return Err(Error::new(ErrorCode::AlreadyExists).with("path", dest.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_join_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("data.bin");
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let mut out = Output::create(&base, Some(100_000)).unwrap();
        out.write_all(&data[..10]).unwrap();
        out.write_all(&data[10..]).unwrap();
        // Patch the header after the fact, as 7z and ZIP writers do.
        out.seek(SeekFrom::Start(0)).unwrap();
        out.write_all(&data[..10]).unwrap();
        let paths = out.commit().unwrap();
        assert_eq!(paths.len(), 3);
        let first = dir.path().join("data.bin.001");
        assert_eq!(split_volumes(&first).unwrap(), paths);
        let mut r = open_input(&first).unwrap();
        let mut back = Vec::new();
        r.read_to_end(&mut back).unwrap();
        assert_eq!(back, data);
        r.seek(SeekFrom::Start(99_998)).unwrap();
        let mut b = [0u8; 4];
        r.read_exact(&mut b).unwrap();
        assert_eq!(&b, &data[99_998..100_002]);
        assert_eq!(input_size(&first).unwrap(), 300_000);
    }

    #[test]
    fn uncommitted_output_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.zip");
        {
            let mut out = Output::create(&dest, None).unwrap();
            out.write_all(b"partial").unwrap();
        }
        {
            let mut out = Output::create(&dest, Some(65_536)).unwrap();
            out.write_all(&[0u8; 70_000]).unwrap();
        }
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        let mut out = Output::create(&dest, None).unwrap();
        out.write_all(b"done").unwrap();
        out.commit().unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"done");
    }
}
