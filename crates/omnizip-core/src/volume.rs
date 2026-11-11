//! Input and output files, including raw-split multi-volume sets (`name.7z.001`, `.002`, …).
//!
//! Output is written to a temporary file next to the destination and only renamed into place by
//! [`Output::commit`], so a failed or cancelled job never leaves a half-written archive behind.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{Error, ErrorCode, Result};

/// The process umask: the permission bits new files and directories never get. It cannot be
/// read without setting it, which would race other threads creating files, so it is measured
/// once on a probe file.
#[cfg(unix)]
pub fn umask() -> u32 {
    use std::{os::unix::fs::PermissionsExt, sync::OnceLock};
    static MASK: OnceLock<u32> = OnceLock::new();
    *MASK.get_or_init(|| {
        tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o777))
            .tempfile()
            .and_then(|f| f.as_file().metadata())
            .map_or(0o022, |m| !m.permissions().mode() & 0o777)
    })
}

/// Permission bits `File::create` gives a new file here: `0o666` less the process umask. Files
/// written through a private temporary file (`0o600`) get them once in place.
#[cfg(unix)]
pub fn new_file_mode() -> u32 {
    0o666 & !umask()
}

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
        volume_at(&self.starts, pos)
    }
}

/// The volume holding joined offset `pos`, given each volume's start offset.
fn volume_at(starts: &[u64], pos: u64) -> usize {
    match starts.binary_search(&pos) {
        Ok(i) => {
            // Skip empty volumes that share a start offset.
            let mut i = i;
            while i + 1 < starts.len() && starts[i + 1] == pos {
                i += 1;
            }
            i
        }
        Err(i) => i - 1,
    }
}

/// Reads at `offset` of `f` without moving a shared cursor: safe from several threads at once.
#[cfg(windows)]
fn read_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(f, buf, offset)
}

#[cfg(unix)]
fn read_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(f, buf, offset)
}

/// One piece of an archive input.
#[derive(Debug)]
enum Part {
    /// A file from offset `base` on (an archive inside an executable starts after the program).
    File { file: File, base: u64 },
    /// Bytes made up in memory (the rewritten central directory of a spanned ZIP).
    Bytes(Arc<[u8]>),
}

/// The pieces of an archive input, one after another: one file, the volumes of a split set, or
/// a window of a file.
#[derive(Debug)]
struct Volumes {
    parts: Vec<Part>,
    starts: Vec<u64>,
    len: u64,
}

impl Volumes {
    fn new() -> Self {
        Self {
            parts: Vec::new(),
            starts: Vec::new(),
            len: 0,
        }
    }

    /// Adds `path` from offset `base` to its end.
    fn push_file(&mut self, path: &Path, base: u64) -> Result<()> {
        let file = File::open(path).map_err(|e| Error::io(e, path))?;
        let size = file.metadata().map_err(|e| Error::io(e, path))?.len();
        self.starts.push(self.len);
        self.len += size.saturating_sub(base);
        self.parts.push(Part::File { file, base });
        Ok(())
    }

    fn push_bytes(&mut self, bytes: Vec<u8>) {
        self.starts.push(self.len);
        self.len += bytes.len() as u64;
        self.parts.push(Part::Bytes(bytes.into()));
    }

    /// Reads at joined offset `pos`, within one part.
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> io::Result<usize> {
        if pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let v = volume_at(&self.starts, pos);
        let end = self.starts.get(v + 1).copied().unwrap_or(self.len);
        let want = buf.len().min((end - pos) as usize);
        let within = pos - self.starts[v];
        match &self.parts[v] {
            Part::File { file, base } => loop {
                match read_at(file, &mut buf[..want], base + within) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    r => return r,
                }
            },
            Part::Bytes(b) => {
                let at = within as usize;
                buf[..want].copy_from_slice(&b[at..at + want]);
                Ok(want)
            }
        }
    }
}

/// Read buffer of a [`SharedInput`]. Kept small: an entry of a ZIP often needs only a few KiB,
/// and the buffer is filled from wherever the decoder seeks to.
const INPUT_BUFFER: usize = 64 << 10;

/// An archive input (split volumes joined) that is cheap to clone: clones share the open files
/// and each keeps its own position and buffer, reading at positions rather than through a
/// shared cursor. Several threads can so decode one archive at once
/// ([`ArchiveReader::try_clone`](crate::ArchiveReader::try_clone)), on the very files the
/// archive was parsed from. A seek within the buffered range keeps the buffer.
#[derive(Debug)]
pub struct SharedInput {
    volumes: Arc<Volumes>,
    pos: u64,
    buf: Box<[u8]>,
    /// Joined offset of `buf[0]`, and how much of `buf` holds data.
    buf_start: u64,
    filled: usize,
}

impl SharedInput {
    /// Opens `path`, or every volume when it is the first of a split set.
    pub fn open(path: &Path) -> Result<Self> {
        let paths = split_volumes(path)
            .filter(|v| v.len() > 1)
            .unwrap_or_else(|| vec![path.to_owned()]);
        Self::join(&paths, Vec::new())
    }

    /// `path` from `offset` on, as if the file started there: the archive of a self-extracting
    /// executable, which the archive's own offsets count from.
    pub fn open_from(path: &Path, offset: u64) -> Result<Self> {
        let mut v = Volumes::new();
        v.push_file(path, offset)?;
        Ok(Self::from_volumes(v))
    }

    /// `paths` one after another, then `tail` (bytes made up in memory, empty for none).
    pub fn join(paths: &[PathBuf], tail: Vec<u8>) -> Result<Self> {
        let mut v = Volumes::new();
        for p in paths {
            v.push_file(p, 0)?;
        }
        if !tail.is_empty() {
            v.push_bytes(tail);
        }
        Ok(Self::from_volumes(v))
    }

    fn from_volumes(volumes: Volumes) -> Self {
        Self {
            volumes: Arc::new(volumes),
            pos: 0,
            buf: Box::default(),
            buf_start: 0,
            filled: 0,
        }
    }

    /// Total length of the input.
    pub fn len(&self) -> u64 {
        self.volumes.len
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Clone for SharedInput {
    /// The same files, at the same position, with a buffer of its own.
    fn clone(&self) -> Self {
        Self {
            volumes: self.volumes.clone(),
            pos: self.pos,
            buf: Box::default(),
            buf_start: 0,
            filled: 0,
        }
    }
}

impl Read for SharedInput {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.pos >= self.volumes.len {
            return Ok(0);
        }
        let buffered = self.pos >= self.buf_start && self.pos < self.buf_start + self.filled as u64;
        if !buffered {
            if out.len() >= INPUT_BUFFER {
                let n = self.volumes.read_at(self.pos, out)?;
                self.pos += n as u64;
                return Ok(n);
            }
            if self.buf.is_empty() {
                self.buf = vec![0; INPUT_BUFFER].into_boxed_slice();
            }
            self.buf_start = self.pos;
            self.filled = 0;
            self.filled = self.volumes.read_at(self.pos, &mut self.buf)?;
            if self.filled == 0 {
                return Ok(0);
            }
        }
        let at = (self.pos - self.buf_start) as usize;
        let n = out.len().min(self.filled - at);
        out[..n].copy_from_slice(&self.buf[at..at + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for SharedInput {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::End(d) => self.volumes.len as i128 + d as i128,
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

    fn stream_position(&mut self) -> io::Result<u64> {
        Ok(self.pos)
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
    /// Buffered: tar headers and the many small writes of the zip crate would otherwise each
    /// be a system call.
    Single(io::BufWriter<File>, tempfile::TempPath),
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
            None => {
                let (file, path) = tempfile::Builder::new()
                    .prefix(".omnizip-")
                    .suffix(".tmp")
                    .tempfile_in(dir)
                    .map_err(|e| Error::io(e, dir))?
                    .into_parts();
                OutputKind::Single(io::BufWriter::with_capacity(1 << 20, file), path)
            }
        };
        Ok(Self {
            kind: Some(kind),
            dest: dest.to_owned(),
        })
    }

    /// Moves the finished archive into place and returns the written paths.
    pub fn commit(mut self) -> Result<Vec<PathBuf>> {
        match self.kind.take().expect("commit called once") {
            OutputKind::Single(w, path) => {
                let file = w
                    .into_inner()
                    .map_err(|e| Error::io(e.into_error(), &self.dest))?;
                // The temporary file is private: the archive gets the permissions of the one it
                // replaces (an archive updated in place), or those of a new file.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let perms = fs::metadata(&self.dest)
                        .map(|m| m.permissions())
                        .unwrap_or_else(|_| fs::Permissions::from_mode(new_file_mode()));
                    file.set_permissions(perms)
                        .map_err(|e| Error::io(e, &self.dest))?;
                }
                file.sync_all().map_err(|e| Error::io(e, &self.dest))?;
                drop(file);
                path.persist(&self.dest)
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
            OutputKind::Single(w, _) => w,
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
        // A TempPath deletes itself.
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
    fn shared_input_clones_read_on_their_own() {
        let dir = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let mut out = Output::create(&dir.path().join("data.bin"), Some(100_000)).unwrap();
        out.write_all(&data).unwrap();
        out.commit().unwrap();
        let single = dir.path().join("single.bin");
        fs::write(&single, &data).unwrap();
        for path in [dir.path().join("data.bin.001"), single] {
            let mut a = SharedInput::open(&path).unwrap();
            assert_eq!(a.len(), 300_000);
            let mut back = Vec::new();
            a.read_to_end(&mut back).unwrap();
            assert_eq!(back, data);
            // Across a volume boundary, after seeking back within the buffer and out of it.
            a.seek(SeekFrom::Start(99_990)).unwrap();
            let mut b = [0u8; 20];
            a.read_exact(&mut b).unwrap();
            assert_eq!(&b, &data[99_990..100_010]);
            a.seek(SeekFrom::Current(-15)).unwrap();
            a.read_exact(&mut b).unwrap();
            assert_eq!(&b, &data[99_995..100_015]);
            // A clone starts where the original is and moves on its own; both keep reading
            // the same files from other threads.
            let mut c = a.clone();
            a.seek(SeekFrom::Start(0)).unwrap();
            let t = std::thread::spawn(move || {
                let mut b = [0u8; 10];
                c.read_exact(&mut b).unwrap();
                b
            });
            let mut x = [0u8; 10];
            a.read_exact(&mut x).unwrap();
            assert_eq!(&x, &data[..10]);
            assert_eq!(&t.join().unwrap(), &data[100_015..100_025]);
            // Large reads bypass the buffer; reading at the end gives nothing.
            a.seek(SeekFrom::End(-(INPUT_BUFFER as i64) - 5)).unwrap();
            let mut big = vec![0u8; INPUT_BUFFER + 10];
            let mut n = 0;
            while let Ok(k @ 1..) = a.read(&mut big[n..]) {
                n += k;
            }
            assert_eq!(&big[..n], &data[data.len() - INPUT_BUFFER - 5..]);
            assert_eq!(a.read(&mut x).unwrap(), 0);
        }
    }

    #[test]
    fn shared_input_windows_and_tails() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        fs::write(&a, b"MZstub|archive").unwrap();
        fs::write(&b, b"0123456789").unwrap();
        let mut w = SharedInput::open_from(&a, 7).unwrap();
        let mut back = Vec::new();
        w.read_to_end(&mut back).unwrap();
        assert_eq!(back, b"archive");
        assert_eq!(w.len(), 7);

        let mut j = SharedInput::join(&[a.clone(), b.clone()], b"TAIL".to_vec()).unwrap();
        assert_eq!(j.len(), 14 + 10 + 4);
        j.seek(SeekFrom::Start(12)).unwrap();
        let mut x = [0u8; 16];
        j.read_exact(&mut x).unwrap();
        assert_eq!(&x, b"ve0123456789TAIL");
        // Past an offset larger than the file: nothing, not an underflow.
        assert_eq!(SharedInput::open_from(&b, 100).unwrap().len(), 0);
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

    #[cfg(unix)]
    #[test]
    fn output_gets_new_file_or_replaced_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.zip");
        // Like `File::create`: not the temporary file's 0600.
        let mut out = Output::create(&dest, None).unwrap();
        out.write_all(b"new").unwrap();
        out.commit().unwrap();
        let created = mode(&dest);
        let probe = dir.path().join("probe");
        File::create(&probe).unwrap();
        assert_eq!(created, mode(&probe));
        assert_eq!(created, new_file_mode());
        // The measured umask covers the execute bits too.
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        assert_eq!(mode(&sub), 0o777 & !umask());
        // An archive updated in place keeps its permissions.
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o640)).unwrap();
        let mut out = Output::create(&dest, None).unwrap();
        out.write_all(b"updated").unwrap();
        out.commit().unwrap();
        assert_eq!(mode(&dest), 0o640);
        assert_eq!(fs::read(&dest).unwrap(), b"updated");
    }
}
