//! Entries compressed on several threads. Small files are compressed one per thread into
//! memory and appended in order; a large file is deflated block by block on every thread
//! ([`ParDeflate`]) into a one-entry archive in a temporary file, which is merged in. A large
//! file stored as it is (compressed already) goes straight through the archive writer.

use std::{
    collections::HashMap,
    fs::File,
    io::{self, BufReader, BufWriter, Seek, SeekFrom, Write},
    path::Path,
    sync::{Mutex, mpsc},
};

use omnizip_core::{
    Error, ErrorCode, JobCtx, Report, Result, Source, SourceKind,
    deflate::ParDeflate,
    job::{ProgressReader, copy_with_buffer},
};
use zip::{
    CompressionMethod, DateTime, ZipArchive, ZipWriter,
    write::{PreparedZipFile, ZipFileBuilder},
};

use crate::{Methods, UTF8_FLAG, file_options, map_err, write_one, zip_time};

/// Files at least this large are deflated block by block on every thread.
const BIG_FILE: u64 = 16 << 20;
/// Uncompressed bytes of small files compressed ahead of the archive writer.
const AHEAD_BYTES: u64 = 256 << 20;
/// From this size a large file's entry gets ZIP64 fields: deflate can make incompressible data
/// slightly larger, and the header is written before the sizes are known.
const ZIP64_FROM: u64 = 0xF000_0000;

pub(crate) fn write_sources<W: Write + Seek>(
    zw: &mut ZipWriter<W>,
    sources: &[Source],
    methods: Methods,
    threads: usize,
    spill: &Path,
    ctx: &JobCtx,
    report: &mut Report,
) -> Result<()> {
    let big = |s: &Source| s.kind == SourceKind::File && s.size >= BIG_FILE;
    let mut rest = sources;
    while !rest.is_empty() {
        let n = rest.iter().position(big).unwrap_or(rest.len());
        write_small(zw, &rest[..n], methods, threads, ctx, report)?;
        rest = &rest[n..];
        if let Some((s, tail)) = rest.split_first() {
            let (method, level) = methods.of(s);
            if method == CompressionMethod::Deflated {
                let level = level.unwrap_or(6) as u32;
                write_big_deflate(zw, s, level, threads, spill, ctx, report)?;
            } else {
                write_one(zw, s, method, level, None, ctx, report)?;
            }
            rest = tail;
        }
    }
    Ok(())
}

type Prepared = Result<(PreparedZipFile, u64)>;

/// Entries without large files: each file compressed on a worker thread, everything appended
/// in order. At most [`AHEAD_BYTES`] wait in memory.
fn write_small<W: Write + Seek>(
    zw: &mut ZipWriter<W>,
    run: &[Source],
    methods: Methods,
    threads: usize,
    ctx: &JobCtx,
    report: &mut Report,
) -> Result<()> {
    let files: Vec<usize> = (0..run.len())
        .filter(|&i| run[i].kind == SourceKind::File)
        .collect();
    if files.len() < 2 {
        return run.iter().try_for_each(|s| {
            let (method, level) = methods.of(s);
            write_one(zw, s, method, level, None, ctx, report)
        });
    }
    let (jobs, job_rx) = mpsc::channel::<usize>();
    let job_rx = Mutex::new(job_rx);
    let (done_tx, done) = mpsc::channel::<(usize, Prepared)>();
    std::thread::scope(|scope| {
        for _ in 0..threads.min(files.len()) {
            let (job_rx, done_tx) = (&job_rx, done_tx.clone());
            scope.spawn(move || {
                loop {
                    let job = job_rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                    let Ok(i) = job else { break };
                    if done_tx.send((i, prepare(&run[i], methods, ctx))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(done_tx);
        // Dropped on every way out, which lets the workers finish.
        let jobs = jobs;
        let mut next = 0;
        let mut ahead = 0u64;
        let mut ready = HashMap::new();
        for (i, s) in run.iter().enumerate() {
            ctx.check()?;
            while let Some(&j) = files.get(next)
                && (ahead == 0 || ahead + run[j].size <= AHEAD_BYTES)
            {
                ahead += run[j].size;
                let _ = jobs.send(j);
                next += 1;
            }
            if s.kind != SourceKind::File {
                let (method, level) = methods.of(s);
                write_one(zw, s, method, level, None, ctx, report)?;
                continue;
            }
            let (prepared, n) = loop {
                if let Some(r) = ready.remove(&i) {
                    break r;
                }
                let (j, r) = done
                    .recv()
                    .map_err(|_| Error::internal("compression threads stopped"))?;
                ready.insert(j, r);
            }?;
            ahead -= s.size;
            zw.add_prepared_file(prepared)
                .map_err(|e| map_err(e, &s.fs_path))?;
            report.files += 1;
            report.bytes += n;
            ctx.file_done();
        }
        Ok(())
    })
}

/// Compresses one file into memory (on a worker thread, which also looks at it to decide
/// whether to store it as it is).
fn prepare(s: &Source, methods: Methods, ctx: &JobCtx) -> Prepared {
    ctx.check()?;
    ctx.begin_file(&s.archive_path);
    let (method, level) = methods.of(s);
    let name = s.archive_path.trim_end_matches('/');
    let mut b = ZipFileBuilder::new(name, file_options(s, method, level))
        .map_err(|e| map_err(e, &s.fs_path))?;
    let f = File::open(&s.fs_path).map_err(|e| Error::io(e, &s.fs_path))?;
    let n =
        io::copy(&mut ProgressReader::new(f, ctx), &mut b).map_err(|e| Error::io(e, &s.fs_path))?;
    let p = b.finish().map_err(|e| map_err(e, &s.fs_path))?;
    Ok((p, n))
}

/// Deflates a large file on every thread into a one-entry archive in a temporary file under
/// `spill`, then merges that entry in.
fn write_big_deflate<W: Write + Seek>(
    zw: &mut ZipWriter<W>,
    s: &Source,
    level: u32,
    threads: usize,
    spill: &Path,
    ctx: &JobCtx,
    report: &mut Report,
) -> Result<()> {
    ctx.check()?;
    ctx.begin_file(&s.archive_path);
    let io_err = |e: io::Error| Error::io(e, &s.fs_path);
    let mut entry = OneEntry {
        name: s.archive_path.trim_end_matches('/'),
        crc32: 0,
        size: 0,
        packed: 0,
        zip64: s.size >= ZIP64_FROM,
        time: zip_time(s.mtime),
        mode: s.mode,
    };
    let data_start = entry.local_header().len() as u64;
    let mut tmp = tempfile::tempfile_in(spill).map_err(|e| Error::io(e, spill))?;
    tmp.seek(SeekFrom::Start(data_start)).map_err(io_err)?;
    let mut src = File::open(&s.fs_path).map_err(io_err)?;
    let mut w = ParDeflate::new(BufWriter::with_capacity(1 << 20, &mut tmp), level, threads);
    copy_with_buffer(&mut src, &mut w, &mut vec![0; 1 << 20], ctx, None)?;
    let (out, sum) = w.finish().map_err(io_err)?;
    out.into_inner().map_err(|e| io_err(e.into_error()))?;
    if !entry.zip64 && sum.input_size.max(sum.output_size) >= u64::from(u32::MAX) {
        return Err(Error::new(ErrorCode::Io)
            .with("path", s.fs_path.display())
            .detail("the file grew while it was being read"));
    }
    (entry.crc32, entry.size, entry.packed) = (sum.crc32, sum.input_size, sum.output_size);
    tmp.seek(SeekFrom::Start(0)).map_err(io_err)?;
    tmp.write_all(&entry.local_header()).map_err(io_err)?;
    tmp.seek(SeekFrom::Start(data_start + entry.packed))
        .map_err(io_err)?;
    tmp.write_all(&entry.directory(data_start + entry.packed))
        .map_err(io_err)?;
    tmp.seek(SeekFrom::Start(0)).map_err(io_err)?;
    let one = ZipArchive::new(BufReader::with_capacity(1 << 20, tmp))
        .map_err(|e| map_err(e, &s.fs_path))?;
    zw.merge_archive(one).map_err(|e| map_err(e, &s.fs_path))?;
    report.files += 1;
    report.bytes += entry.size;
    ctx.file_done();
    Ok(())
}

/// A deflated file entry, laid out as the zip crate writes one (APPNOTE 4.3).
struct OneEntry<'a> {
    name: &'a str,
    crc32: u32,
    size: u64,
    packed: u64,
    zip64: bool,
    time: DateTime,
    mode: Option<u32>,
}

impl OneEntry<'_> {
    fn version(&self) -> u16 {
        if self.zip64 { 45 } else { 20 }
    }

    /// Fields shared by the local and the central header, from "version needed" to the
    /// name length.
    fn common(&self, h: &mut Vec<u8>, extra_len: u16) {
        let (packed, size) = if self.zip64 {
            (u32::MAX, u32::MAX)
        } else {
            (self.packed as u32, self.size as u32)
        };
        let flags = if self.name.is_ascii() { 0 } else { UTF8_FLAG };
        h.extend(self.version().to_le_bytes());
        h.extend(flags.to_le_bytes());
        h.extend(8u16.to_le_bytes()); // deflate
        h.extend(self.time.timepart().to_le_bytes());
        h.extend(self.time.datepart().to_le_bytes());
        h.extend(self.crc32.to_le_bytes());
        h.extend(packed.to_le_bytes());
        h.extend(size.to_le_bytes());
        h.extend((self.name.len() as u16).to_le_bytes());
        h.extend(extra_len.to_le_bytes());
    }

    fn zip64_extra(&self) -> Vec<u8> {
        if !self.zip64 {
            return Vec::new();
        }
        let mut x = Vec::with_capacity(20);
        x.extend(1u16.to_le_bytes());
        x.extend(16u16.to_le_bytes());
        x.extend(self.size.to_le_bytes());
        x.extend(self.packed.to_le_bytes());
        x
    }

    fn local_header(&self) -> Vec<u8> {
        let extra = self.zip64_extra();
        let mut h = 0x0403_4B50u32.to_le_bytes().to_vec();
        self.common(&mut h, extra.len() as u16);
        h.extend(self.name.as_bytes());
        h.extend(extra);
        h
    }

    /// Central directory and end records, for data ending at `start`.
    fn directory(&self, start: u64) -> Vec<u8> {
        // Like the zip crate: DOS on Windows (where sources have no mode), Unix elsewhere.
        let system: u16 = if cfg!(windows) && self.mode.is_none() {
            0
        } else {
            3
        };
        let made_by = (system << 8) | self.version();
        let attrs = (0o100_000 | self.mode.unwrap_or(0o644)) << 16;
        let extra = self.zip64_extra();
        let mut d = 0x0201_4B50u32.to_le_bytes().to_vec();
        d.extend(made_by.to_le_bytes());
        self.common(&mut d, extra.len() as u16);
        d.extend(0u16.to_le_bytes()); // comment
        d.extend(0u16.to_le_bytes()); // disk
        d.extend(0u16.to_le_bytes()); // internal attributes
        d.extend(attrs.to_le_bytes());
        d.extend(0u32.to_le_bytes()); // local header offset
        d.extend(self.name.as_bytes());
        d.extend(extra);
        let len = d.len() as u64;
        let zip64_end = self.zip64 || start >= u64::from(u32::MAX);
        if zip64_end {
            let at = start + len;
            d.extend(0x0606_4B50u32.to_le_bytes());
            d.extend(44u64.to_le_bytes());
            d.extend(made_by.to_le_bytes());
            d.extend(45u16.to_le_bytes());
            d.extend(0u32.to_le_bytes());
            d.extend(0u32.to_le_bytes());
            d.extend(1u64.to_le_bytes());
            d.extend(1u64.to_le_bytes());
            d.extend(len.to_le_bytes());
            d.extend(start.to_le_bytes());
            d.extend(0x0706_4B50u32.to_le_bytes());
            d.extend(0u32.to_le_bytes());
            d.extend(at.to_le_bytes());
            d.extend(1u32.to_le_bytes());
        }
        d.extend(0x0605_4B50u32.to_le_bytes());
        d.extend(0u16.to_le_bytes());
        d.extend(0u16.to_le_bytes());
        d.extend(1u16.to_le_bytes());
        d.extend(1u16.to_le_bytes());
        d.extend((len as u32).to_le_bytes());
        let start32 = if zip64_end { u32::MAX } else { start as u32 };
        d.extend(start32.to_le_bytes());
        d.extend(0u16.to_le_bytes());
        d
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};

    use super::*;

    /// The one-entry archive as `write_big_deflate` lays it out.
    fn one_entry(name: &str, data: &[u8], zip64: bool) -> Vec<u8> {
        let mut w = ParDeflate::new(Vec::new(), 6, 3);
        w.write_all(data).unwrap();
        let (deflated, sum) = w.finish().unwrap();
        let entry = OneEntry {
            name,
            crc32: sum.crc32,
            size: sum.input_size,
            packed: sum.output_size,
            zip64,
            time: DateTime::from_date_and_time(2026, 10, 29, 12, 34, 56).unwrap(),
            mode: Some(0o640),
        };
        let mut out = entry.local_header();
        out.extend(&deflated);
        out.extend(entry.directory(out.len() as u64));
        out
    }

    #[test]
    fn one_entry_archives_read_back_and_merge() {
        let data: Vec<u8> = b"ZIP64 layout check ".repeat(300_000);
        for zip64 in [false, true] {
            let raw = one_entry("目录/大.bin", &data, zip64);
            let mut one = ZipArchive::new(Cursor::new(raw)).unwrap();
            {
                let mut f = one.by_index(0).unwrap();
                assert_eq!(f.name().unwrap(), "目录/大.bin");
                assert_eq!(f.compression(), CompressionMethod::Deflated);
                assert_eq!(f.unix_mode().map(|m| m & 0o777), Some(0o640));
                assert_eq!(f.last_modified().unwrap().second(), 56);
                let mut back = Vec::new();
                f.read_to_end(&mut back).unwrap();
                assert_eq!(back, data, "zip64={zip64}");
            }
            // Merged after another entry, read back from the final archive.
            let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
            zw.start_file("first.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            zw.write_all(b"first").unwrap();
            zw.merge_archive(one).unwrap();
            let mut z = ZipArchive::new(zw.finish().unwrap()).unwrap();
            let mut back = Vec::new();
            z.by_name("目录/大.bin")
                .unwrap()
                .read_to_end(&mut back)
                .unwrap();
            assert_eq!(back, data, "merged, zip64={zip64}");
        }
    }
}
