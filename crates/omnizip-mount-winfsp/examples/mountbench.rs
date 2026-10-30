//! Measures a mounted archive through the ordinary file APIs, the way Explorer and programs use
//! it: listing, the first bytes of the largest file, reading every file (first and second
//! time), random reads, and with `--writable` writes into the temporary layer. Extracting the
//! same archive and the same writes on NTFS are the reference points. Windows only; needs
//! WinFsp.
//!
//! ```text
//! cargo run --release -p omnizip-mount-winfsp --example mountbench -- <archive> [--writable] [--threads N]
//! ```
//!
//! Run the binary from outside the project directory: Defender makes file-heavy work there
//! about 1.5 times slower.

#[cfg(not(windows))]
fn main() {
    eprintln!("mountbench: Windows only");
}

#[cfg(windows)]
fn main() {
    bench::main();
}

#[cfg(windows)]
mod bench {
    use std::{
        fs::{self, File},
        io::{Read, Seek, SeekFrom, Write},
        path::{Path, PathBuf},
        sync::Arc,
        time::{Duration, Instant},
    };

    use omnizip_core::{
        ExtractOptions, JobCtx, OpenOptions, OverwriteMode, Selection, SymlinkPolicy, extract,
    };
    use omnizip_mount_winfsp::WinFsp;
    use omnizip_vfs::{
        ArchiveFs, FsOptions, Mount, MountBackend, MountOptions, OverlayFs, VirtualFs,
    };

    struct Args {
        archive: PathBuf,
        writable: bool,
        threads: usize,
    }

    fn args() -> Args {
        let mut a = Args {
            archive: PathBuf::new(),
            writable: false,
            threads: 1,
        };
        let mut it = std::env::args().skip(1);
        while let Some(s) = it.next() {
            match s.as_str() {
                "--writable" => a.writable = true,
                "--threads" => a.threads = it.next().and_then(|n| n.parse().ok()).unwrap_or(1),
                _ => a.archive = PathBuf::from(s),
            }
        }
        if a.archive.as_os_str().is_empty() {
            eprintln!("usage: mountbench <archive> [--writable] [--threads N]");
            std::process::exit(2);
        }
        a
    }

    fn row(what: &str, t: Duration, extra: impl std::fmt::Display) {
        println!("{what:<34} {:>9.3} s  {extra}", t.as_secs_f64());
    }

    fn rate(bytes: u64, t: Duration) -> String {
        format!(
            "{:.0} MB/s",
            bytes as f64 / 1048576.0 / t.as_secs_f64().max(1e-9)
        )
    }

    fn open_fs(archive: &Path, scratch: &Path) -> Arc<ArchiveFs> {
        let reader = omnizip_formats::shared()
            .open(archive, &OpenOptions::default(), &JobCtx::default())
            .expect("open");
        let cache = scratch.join("cache");
        fs::create_dir_all(&cache).unwrap();
        Arc::new(
            ArchiveFs::new(
                reader,
                FsOptions {
                    cache_dir: Some(cache),
                    ..Default::default()
                },
            )
            .unwrap(),
        )
    }

    /// Reads every file below `dir` through the interface; files and bytes.
    fn read_vfs(fs: &ArchiveFs, dir: u32) -> (u64, u64) {
        let (mut files, mut bytes) = (0, 0);
        for e in fs.readdir(dir, 0, usize::MAX).unwrap() {
            if e.attr.is_dir() {
                let (f, b) = read_vfs(fs, e.attr.node);
                files += f;
                bytes += b;
            } else {
                files += 1;
                bytes += omnizip_vfs::read_all(fs, e.attr.node, 1 << 20)
                    .unwrap()
                    .len() as u64;
            }
        }
        (files, bytes)
    }

    /// A mount of `archive`, writable over a layer in `scratch` when asked.
    fn mount(archive: &Path, scratch: &Path, writable: bool) -> (Box<dyn Mount>, PathBuf) {
        let fs = open_fs(archive, scratch);
        let volume: Arc<dyn VirtualFs> = if writable {
            let layer = scratch.join("layer");
            let _ = fs::remove_dir_all(&layer);
            Arc::new(OverlayFs::new(fs, layer).unwrap())
        } else {
            fs
        };
        let m = WinFsp
            .mount(
                volume,
                &MountOptions {
                    read_only: !writable,
                    ..Default::default()
                },
            )
            .expect("mount");
        let root = PathBuf::from(format!("{}\\", m.mount_point().display()));
        (m, root)
    }

    fn walk(dir: &Path, out: &mut Vec<(PathBuf, u64)>) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let m = e.metadata().unwrap();
            if m.is_dir() {
                walk(&e.path(), out);
            } else {
                out.push((e.path(), m.len()));
            }
        }
    }

    fn read_file(p: &Path, buf: &mut [u8]) -> u64 {
        let mut f = File::open(p).unwrap();
        let mut n = 0;
        loop {
            match f.read(buf).unwrap() {
                0 => return n,
                k => n += k as u64,
            }
        }
    }

    /// Reads every file, on `threads` threads; bytes read.
    fn read_all(files: &[(PathBuf, u64)], threads: usize) -> u64 {
        let chunk = files.len().div_ceil(threads.max(1)).max(1);
        std::thread::scope(|s| {
            files
                .chunks(chunk)
                .map(|part| {
                    s.spawn(move || {
                        let mut buf = vec![0; 1 << 20];
                        part.iter()
                            .map(|(p, _)| read_file(p, &mut buf))
                            .sum::<u64>()
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|h| h.join().unwrap())
                .sum()
        })
    }

    /// Creates `n` files of `size` bytes in `dir`, writes one file of `big` bytes in 1 MiB
    /// writes, reads it back, and deletes everything; the time of each step.
    fn writes(dir: &Path, n: usize, size: usize, big: u64) -> [Duration; 4] {
        fs::create_dir_all(dir).unwrap();
        let data = vec![0x5a; size];
        let t = Instant::now();
        for i in 0..n {
            fs::write(dir.join(format!("f{i:05}.bin")), &data).unwrap();
        }
        let small = t.elapsed();
        let chunk = vec![0xa5; 1 << 20];
        let t = Instant::now();
        let mut f = File::create(dir.join("big.bin")).unwrap();
        for _ in 0..big >> 20 {
            f.write_all(&chunk).unwrap();
        }
        f.flush().unwrap();
        drop(f);
        let large = t.elapsed();
        let t = Instant::now();
        let mut buf = vec![0; 1 << 20];
        assert_eq!(read_file(&dir.join("big.bin"), &mut buf), big);
        let back = t.elapsed();
        let t = Instant::now();
        fs::remove_dir_all(dir).unwrap();
        [small, large, back, t.elapsed()]
    }

    pub fn main() {
        let a = args();
        let scratch = tempfile::Builder::new()
            .prefix("mountbench-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        println!("{}", a.archive.display());

        // Reference: extracting the whole archive.
        let out = scratch.path().join("extracted");
        let t = Instant::now();
        let mut reader = omnizip_formats::shared()
            .open(&a.archive, &OpenOptions::default(), &JobCtx::default())
            .unwrap();
        let xo = ExtractOptions {
            destination: out.clone(),
            smart_folder: false,
            overwrite: OverwriteMode::Overwrite,
            symlinks: SymlinkPolicy::CreateIfInside,
            propagate_origin: false,
            ..Default::default()
        };
        extract(reader.as_mut(), &xo, Selection::all(), &JobCtx::default()).unwrap();
        drop(reader);
        let extract_time = t.elapsed();
        let mut on_disk = Vec::new();
        walk(&out, &mut on_disk);
        let total: u64 = on_disk.iter().map(|f| f.1).sum();
        row(
            "extract (reference)",
            extract_time,
            format!("{} files, {}", on_disk.len(), rate(total, extract_time)),
        );
        let _ = fs::remove_dir_all(&out);

        // The same files read through the file system interface in this process, without the
        // driver: what decoding costs.
        let fs = open_fs(&a.archive, &scratch.path().join("direct"));
        let t = Instant::now();
        let (count, bytes) = read_vfs(&fs, omnizip_vfs::ROOT);
        let direct = t.elapsed();
        row(
            "read all through VirtualFs",
            direct,
            format!("{count} files, {}", rate(bytes, direct)),
        );
        drop(fs);

        // Mount and list.
        let t = Instant::now();
        let (m, root) = mount(&a.archive, &scratch.path().join("m1"), false);
        row("open + mount", t.elapsed(), m.mount_point().display());
        let t = Instant::now();
        let mut files = Vec::new();
        walk(&root, &mut files);
        row(
            "list everything",
            t.elapsed(),
            format!("{} files", files.len()),
        );
        let (largest, largest_len) = files.iter().max_by_key(|f| f.1).cloned().unwrap();

        // First bytes of the largest file, on a fresh volume.
        let t = Instant::now();
        let mut f = File::open(&largest).unwrap();
        let mut head = vec![0; 64 << 10];
        let _ = f.read(&mut head).unwrap();
        row(
            "first 64 KiB of the largest file",
            t.elapsed(),
            format!("{} MB", largest_len >> 20),
        );
        drop(f);
        drop(m);

        // Every file, first and second time, then random reads.
        let (m, root) = mount(&a.archive, &scratch.path().join("m2"), false);
        let mut files = Vec::new();
        walk(&root, &mut files);
        let t = Instant::now();
        let bytes = read_all(&files, a.threads);
        let cold = t.elapsed();
        row(
            &format!("read all, first time ({} thr)", a.threads),
            cold,
            format!(
                "{} ({:.0} files/s)",
                rate(bytes, cold),
                files.len() as f64 / cold.as_secs_f64()
            ),
        );
        let t = Instant::now();
        let bytes = read_all(&files, a.threads);
        let warm = t.elapsed();
        row(
            "read all, second time",
            warm,
            format!(
                "{} ({:.0} files/s)",
                rate(bytes, warm),
                files.len() as f64 / warm.as_secs_f64()
            ),
        );
        let mut f = File::open(&largest).unwrap();
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut buf = vec![0; 4096];
        let n = 2000;
        let t = Instant::now();
        for _ in 0..n {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            f.seek(SeekFrom::Start(x % largest_len.saturating_sub(4096).max(1)))
                .unwrap();
            let _ = f.read(&mut buf).unwrap();
        }
        let rnd = t.elapsed();
        row(
            "random 4 KiB reads (cached)",
            rnd,
            format!("{:.1} µs each", rnd.as_secs_f64() * 1e6 / n as f64),
        );
        drop(f);
        drop(m);

        if !a.writable {
            return;
        }
        // Writes: on NTFS, then into the layer of a writable mount.
        let (n, size, big) = (2000, 16 << 10, 512 << 20);
        let names = [
            "create 2000 × 16 KiB",
            "write 512 MiB (1 MiB writes)",
            "read the 512 MiB back",
            "delete it all",
        ];
        let base = writes(&scratch.path().join("ntfs"), n, size, big);
        let (m, root) = mount(&a.archive, &scratch.path().join("m3"), true);
        let layer = writes(&root.join("bench-new"), n, size, big);
        for ((name, b), l) in names.iter().zip(base).zip(layer) {
            row(
                &format!("{name}: mount"),
                l,
                format!("NTFS {:.3} s", b.as_secs_f64()),
            );
        }
        // The first change of a file of the archive copies it into the layer.
        let t = Instant::now();
        fs::OpenOptions::new()
            .append(true)
            .open(largest_in(&root, &largest))
            .unwrap()
            .write_all(b"x")
            .unwrap();
        row(
            "append 1 byte to the largest file",
            t.elapsed(),
            format!("{} MB copied into the layer", largest_len >> 20),
        );
        drop(m);
    }

    /// The same file on another mount of the same archive.
    fn largest_in(root: &Path, on_first: &Path) -> PathBuf {
        let rel: PathBuf = on_first.components().skip(2).collect();
        root.join(rel)
    }
}
