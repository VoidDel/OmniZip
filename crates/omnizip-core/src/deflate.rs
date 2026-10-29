//! Deflate on several threads, as pigz does: the input is cut into blocks compressed in
//! parallel, each primed with the 32 KiB before it, ended with a sync flush and joined into one
//! raw deflate stream. Any inflater reads it; the ratio is within a fraction of a percent of a
//! single-threaded stream.

use std::{
    collections::BTreeMap,
    io::{self, Write},
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
};

use flate2::{Compress, Compression, FlushCompress, Status};

/// Input per block.
const BLOCK: usize = 1 << 20;
/// History a block is primed with (the deflate window).
const WINDOW: usize = 32 * 1024;

struct Job {
    seq: u64,
    dict: Vec<u8>,
    data: Vec<u8>,
    last: bool,
}

type Done = (u64, io::Result<Vec<u8>>);

/// What a finished stream was made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeflateSummary {
    /// CRC-32 of the input.
    pub crc32: u32,
    pub input_size: u64,
    pub output_size: u64,
}

/// A writer that deflates what it is given on `threads` worker threads and writes the raw
/// stream to `inner` in order. [`ParDeflate::finish`] ends the stream.
pub struct ParDeflate<W: Write> {
    /// Taken by `finish`.
    inner: Option<W>,
    block: Vec<u8>,
    /// The end of the previous block, priming the next one.
    dict: Vec<u8>,
    jobs: Option<mpsc::Sender<Job>>,
    done: mpsc::Receiver<Done>,
    workers: Vec<JoinHandle<()>>,
    next_seq: u64,
    next_write: u64,
    ready: BTreeMap<u64, Vec<u8>>,
    max_ahead: u64,
    crc: crc32fast::Hasher,
    input_size: u64,
    output_size: u64,
}

impl<W: Write> ParDeflate<W> {
    /// `level` is a zlib level (1–9).
    pub fn new(inner: W, level: u32, threads: usize) -> Self {
        let threads = threads.max(1);
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let (done_tx, done) = mpsc::channel::<Done>();
        let workers = (0..threads)
            .map(|_| {
                let rx = job_rx.clone();
                let tx = done_tx.clone();
                std::thread::spawn(move || {
                    loop {
                        let job = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                        let Ok(job) = job else { break };
                        let out = compress_block(level, &job.dict, &job.data, job.last);
                        if tx.send((job.seq, out)).is_err() {
                            break;
                        }
                    }
                })
            })
            .collect();
        Self {
            inner: Some(inner),
            block: Vec::with_capacity(BLOCK),
            dict: Vec::new(),
            jobs: Some(jobs),
            done,
            workers,
            next_seq: 0,
            next_write: 0,
            ready: BTreeMap::new(),
            max_ahead: 2 * threads as u64,
            crc: crc32fast::Hasher::new(),
            input_size: 0,
            output_size: 0,
        }
    }

    /// Hands the current block to the workers.
    fn dispatch(&mut self, last: bool) -> io::Result<()> {
        while self.next_seq - self.next_write >= self.max_ahead {
            self.collect_one()?;
        }
        let data = std::mem::replace(&mut self.block, Vec::with_capacity(BLOCK));
        let keep = data.len().min(WINDOW);
        let dict = std::mem::replace(&mut self.dict, data[data.len() - keep..].to_vec());
        let job = Job {
            seq: self.next_seq,
            dict,
            data,
            last,
        };
        self.next_seq += 1;
        self.jobs
            .as_ref()
            .and_then(|j| j.send(job).ok())
            .ok_or_else(|| io::Error::other("deflate workers stopped"))
    }

    /// Waits for one compressed block and writes every block that is now next in order.
    fn collect_one(&mut self) -> io::Result<()> {
        let (seq, out) = self
            .done
            .recv()
            .map_err(|_| io::Error::other("deflate workers stopped"))?;
        self.ready.insert(seq, out?);
        while let Some(out) = self.ready.remove(&self.next_write) {
            if let Some(w) = self.inner.as_mut() {
                w.write_all(&out)?;
            }
            self.output_size += out.len() as u64;
            self.next_write += 1;
        }
        Ok(())
    }

    /// Ends the stream and returns the inner writer with what was written.
    pub fn finish(mut self) -> io::Result<(W, DeflateSummary)> {
        self.dispatch(true)?;
        while self.next_write < self.next_seq {
            self.collect_one()?;
        }
        self.stop();
        let summary = DeflateSummary {
            crc32: self.crc.clone().finalize(),
            input_size: self.input_size,
            output_size: self.output_size,
        };
        let inner = self.inner.take().expect("finish is called once");
        Ok((inner, summary))
    }

    fn stop(&mut self) {
        self.jobs = None;
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

impl<W: Write> Write for ParDeflate<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len().min(BLOCK - self.block.len());
        self.block.extend_from_slice(&buf[..n]);
        self.crc.update(&buf[..n]);
        self.input_size += n as u64;
        if self.block.len() == BLOCK {
            self.dispatch(false)?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.as_mut().map_or(Ok(()), Write::flush)
    }
}

impl<W: Write> Drop for ParDeflate<W> {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Deflates one block primed with `dict`. All but the last end with a sync flush, so the next
/// block starts on a byte boundary; the last one ends the stream.
fn compress_block(level: u32, dict: &[u8], data: &[u8], last: bool) -> io::Result<Vec<u8>> {
    let mut c = Compress::new(Compression::new(level), false);
    if !dict.is_empty() {
        c.set_dictionary(dict).map_err(io::Error::other)?;
    }
    let flush = if last {
        FlushCompress::Finish
    } else {
        FlushCompress::Sync
    };
    let mut out = Vec::with_capacity(data.len() + data.len() / 8 + 1024);
    let mut input = data;
    loop {
        if out.len() == out.capacity() {
            out.reserve(64 * 1024);
        }
        let before = c.total_in();
        let status = c
            .compress_vec(input, &mut out, flush)
            .map_err(io::Error::other)?;
        input = &input[(c.total_in() - before) as usize..];
        let flushed = input.is_empty() && out.len() < out.capacity();
        match status {
            Status::StreamEnd => break,
            Status::Ok | Status::BufError if !last && flushed => break,
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn round_trip(data: &[u8], threads: usize) -> (Vec<u8>, DeflateSummary) {
        let mut w = ParDeflate::new(Vec::new(), 6, threads);
        // Uneven writes, so blocks fill across calls.
        for chunk in data.chunks(77_777) {
            w.write_all(chunk).unwrap();
        }
        let (out, summary) = w.finish().unwrap();
        let mut back = Vec::new();
        flate2::read::DeflateDecoder::new(&out[..])
            .read_to_end(&mut back)
            .unwrap();
        assert_eq!(back, data);
        (out, summary)
    }

    fn sample(len: usize) -> Vec<u8> {
        // Compressible text with some noise.
        let mut x = 0x2545_f491_u32;
        (0..len)
            .map(|i| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                if i % 7 == 0 {
                    (x & 0xFF) as u8
                } else {
                    b"omnizip "[i % 8]
                }
            })
            .collect()
    }

    #[test]
    fn round_trips_at_block_edges() {
        for len in [0, 1, BLOCK - 1, BLOCK, BLOCK + 1, 3 * BLOCK + 12_345] {
            let data = sample(len);
            let (out, s) = round_trip(&data, 4);
            assert_eq!(s.crc32, crc32fast::hash(&data), "len {len}");
            assert_eq!(s.input_size, len as u64);
            assert_eq!(s.output_size, out.len() as u64);
        }
    }

    #[test]
    fn ratio_close_to_one_stream() {
        let data = sample(6 * BLOCK);
        let (par, _) = round_trip(&data, 8);
        let mut single = flate2::write::DeflateEncoder::new(Vec::new(), Compression::new(6));
        single.write_all(&data).unwrap();
        let single = single.finish().unwrap();
        assert!(
            par.len() as f64 <= single.len() as f64 * 1.01,
            "{} vs {}",
            par.len(),
            single.len()
        );
    }

    #[test]
    fn dropping_unfinished_stops_the_workers() {
        let mut w = ParDeflate::new(Vec::new(), 6, 4);
        w.write_all(&sample(3 * BLOCK)).unwrap();
        drop(w);
    }
}
