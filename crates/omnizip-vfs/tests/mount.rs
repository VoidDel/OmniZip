//! Drives `ArchiveFs` the way a kernel driver would: lookups, paged listings, random reads.

use std::{
    ffi::OsStr,
    fs::{self, File},
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

use omnizip_core::{
    ArchiveInfo, ArchiveReader, CollectOptions, Entry, EntryData, EntryKind, ExtractSink, FormatId,
    JobCtx, Level, MountClass, OpenOptions, WriteOptions, source,
};
use omnizip_vfs::{
    ArchiveFs, CacheLimits, FileType, FsError, FsOptions, LinkMode, NameRules, OpenFlags, ROOT,
    VirtualFs, read_all, resolve,
};

/// Deterministic, poorly compressible bytes.
fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

fn small_cache() -> CacheLimits {
    CacheLimits {
        memory: 1 << 20,
        disk: 64 << 20,
        inline_max: 64 << 10,
    }
}

fn opts() -> FsOptions {
    FsOptions {
        cache: small_cache(),
        ..Default::default()
    }
}

/// `root/docs/readme.txt`, `root/media/big.bin` (3 MiB), `root/相册/a.txt`, `root/empty/`.
fn source_tree(dir: &Path) -> (PathBuf, Vec<u8>) {
    let root = dir.join("photos");
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::create_dir_all(root.join("media")).unwrap();
    fs::create_dir_all(root.join("相册")).unwrap();
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::write(root.join("docs/readme.txt"), "read me\n".repeat(100)).unwrap();
    let big = noise(3 << 20, 7);
    fs::write(root.join("media/big.bin"), &big).unwrap();
    fs::write(root.join("相册/a.txt"), "alpha").unwrap();
    (root, big)
}

fn create(dir: &Path, root: &Path, name: &str, wo: WriteOptions) -> PathBuf {
    let ctx = JobCtx::default();
    let sources = source::collect(&[root.to_owned()], &CollectOptions::default(), &ctx).unwrap();
    let dest = dir.join(name);
    omnizip_formats::shared()
        .create(&dest, &sources, &wo, &ctx)
        .unwrap();
    dest
}

fn mount(path: &Path, o: FsOptions) -> ArchiveFs {
    let reader = omnizip_formats::shared()
        .open(path, &OpenOptions::default(), &JobCtx::default())
        .unwrap();
    ArchiveFs::new(reader, o).unwrap()
}

fn names(fs: &dyn VirtualFs, dir: u32) -> Vec<String> {
    fs.readdir(dir, 0, usize::MAX)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect()
}

/// Files stored as they are (plain tar, ZIP entries without compression) are read in place:
/// nothing is decoded into the cache, and random reads land where they should.
#[test]
fn stored_files_are_read_in_place() {
    let d = tempfile::tempdir().unwrap();
    let (root, big) = source_tree(d.path());
    for (name, format, level) in [
        ("photos.tar", FormatId::Tar, Level::Normal),
        ("stored.zip", FormatId::Zip, Level::Store),
    ] {
        let archive = create(
            d.path(),
            &root,
            name,
            WriteOptions {
                format,
                level,
                ..Default::default()
            },
        );
        let fs = mount(&archive, opts());
        let b = resolve(&fs, "photos/media/big.bin").unwrap();
        assert_eq!(read_all(&fs, b.node, 65536).unwrap(), big, "{name}");
        let fh = fs.open(b.node, OpenFlags::default()).unwrap();
        assert_eq!(
            fs.read(fh, 2_000_000, 8).unwrap(),
            &big[2_000_000..2_000_008],
            "{name}"
        );
        assert!(
            fs.read(fh, 3 << 20, 8).unwrap().is_empty(),
            "{name}: at the end"
        );
        fs.release(fh);
        let a = resolve(&fs, "photos/相册/a.txt").unwrap();
        assert_eq!(read_all(&fs, a.node, 2).unwrap(), b"alpha", "{name}");
        assert_eq!(fs.cache().len(), 0, "{name}: nothing decoded");
    }
}

#[test]
fn zip_volume_reads_like_a_disk() {
    let d = tempfile::tempdir().unwrap();
    let (root, big) = source_tree(d.path());
    let zip = create(
        d.path(),
        &root,
        "photos.zip",
        WriteOptions {
            format: FormatId::Zip,
            level: Level::Fast,
            ..Default::default()
        },
    );
    let fs = mount(&zip, opts());
    assert_eq!(fs.mount_class(), MountClass::B);

    let stats = fs.statfs().unwrap();
    assert_eq!(stats.label, "photos");
    assert_eq!(stats.fs_name, "OmniZip");
    assert_eq!(stats.files, 3);
    assert_eq!(stats.total_bytes, 800 + (3 << 20) + 5);

    // Paged listing: one item per call, following cursors.
    let top = resolve(&fs, "photos").unwrap();
    assert!(top.is_dir());
    let mut paged = Vec::new();
    let mut cursor = 0;
    loop {
        let page = fs.readdir(top.node, cursor, 1).unwrap();
        let Some(last) = page.last() else { break };
        cursor = last.cursor;
        paged.extend(page.into_iter().map(|e| e.name));
    }
    let mut sorted = paged.clone();
    sorted.sort();
    assert_eq!(sorted, ["docs", "empty", "media", "相册"]);
    assert_eq!(paged, names(&fs, top.node));

    // Unicode names and case-folded lookups (Windows and macOS rules).
    let a = resolve(&fs, "photos/相册/a.txt").unwrap();
    assert_eq!(read_all(&fs, a.node, 3).unwrap(), b"alpha");
    if NameRules::current().case_insensitive {
        assert_eq!(resolve(&fs, "PHOTOS\\Docs\\README.TXT").unwrap().size, 800);
        // The name as listings spell it, whatever the case asked for.
        let (found, name) = fs.lookup_named(ROOT, OsStr::new("PHOTOS")).unwrap();
        assert_eq!((found.node, name.as_str()), (top.node, "photos"));
    } else {
        assert_eq!(
            resolve(&fs, "PHOTOS/docs/readme.txt"),
            Err(FsError::NotFound)
        );
    }
    assert_eq!(resolve(&fs, "photos/../etc"), Err(FsError::Invalid));
    assert_eq!(
        resolve(&fs, "photos/docs/readme.txt/x"),
        Err(FsError::NotDir)
    );
    assert_eq!(
        fs.lookup(ROOT, OsStr::new("missing")),
        Err(FsError::NotFound)
    );

    // Random reads of a large entry, served from a spool file after one decode.
    let b = resolve(&fs, "photos/media/big.bin").unwrap();
    assert_eq!(b.kind, FileType::File);
    assert_eq!(b.size, big.len() as u64);
    assert_eq!(b.mode & 0o222, 0, "never writable");
    let fh = fs.open(b.node, OpenFlags::default()).unwrap();
    for &(off, len) in &[
        (3_000_000u64, 4096u32),
        (0, 100),
        (1_234_567, 65_536),
        (big.len() as u64 - 10, 4096),
        (17, 1),
    ] {
        let got = fs.read(fh, off, len).unwrap();
        let end = (off as usize + len as usize).min(big.len());
        assert_eq!(got, &big[off as usize..end], "offset {off}");
    }
    assert!(fs.read(fh, big.len() as u64, 10).unwrap().is_empty());
    let (_, disk) = fs.cache().usage();
    assert_eq!(disk, big.len() as u64, "big entry spooled to disk");
    fs.release(fh);
    assert_eq!(fs.read(fh, 0, 1), Err(FsError::BadHandle));

    // Read-only volume, directories are not files.
    assert_eq!(
        fs.open(b.node, OpenFlags { write: true }),
        Err(FsError::ReadOnly)
    );
    let dh = fs.open(top.node, OpenFlags::default()).unwrap();
    assert_eq!(fs.read(dh, 0, 1), Err(FsError::IsDir));
    fs.release(dh);
    assert_eq!(fs.open_handles(), 0);
    assert!(names(&fs, resolve(&fs, "photos/empty").unwrap().node).is_empty());
}

#[test]
fn handle_limit_is_enforced() {
    let d = tempfile::tempdir().unwrap();
    let (root, _) = source_tree(d.path());
    let zip = create(d.path(), &root, "h.zip", WriteOptions::default());
    let fs = mount(
        &zip,
        FsOptions {
            max_handles: 2,
            ..opts()
        },
    );
    let n = resolve(&fs, "photos/docs/readme.txt").unwrap().node;
    let a = fs.open(n, OpenFlags::default()).unwrap();
    let _b = fs.open(n, OpenFlags::default()).unwrap();
    assert_eq!(fs.open(n, OpenFlags::default()), Err(FsError::TooManyOpen));
    fs.release(a);
    assert!(fs.open(n, OpenFlags::default()).is_ok());
}

#[test]
fn encrypted_zip_needs_the_password() {
    let d = tempfile::tempdir().unwrap();
    let (root, _) = source_tree(d.path());
    let zip = create(
        d.path(),
        &root,
        "secret.zip",
        WriteOptions {
            password: Some("pw-2026".into()),
            ..Default::default()
        },
    );
    let locked = mount(&zip, opts());
    assert_eq!(locked.check_password(), Err(FsError::Access));
    let a = resolve(&locked, "photos/相册/a.txt").unwrap();
    assert_eq!(read_all(&locked, a.node, 64), Err(FsError::Access));

    let wrong = mount(
        &zip,
        FsOptions {
            password: Some("nope".into()),
            ..opts()
        },
    );
    assert_eq!(wrong.check_password(), Err(FsError::Access));

    let open = mount(
        &zip,
        FsOptions {
            password: Some("pw-2026".into()),
            ..opts()
        },
    );
    assert_eq!(open.check_password(), Ok(()));
    // The smallest encrypted file was decoded to check: it is cached now.
    assert_eq!(open.cache().len(), 1);
    let a = resolve(&open, "photos/相册/a.txt").unwrap();
    assert_eq!(read_all(&open, a.node, 64).unwrap(), b"alpha");

    // Nothing to check in an archive without encryption.
    let plain = create(d.path(), &root, "plain.zip", WriteOptions::default());
    assert_eq!(mount(&plain, opts()).check_password(), Ok(()));
}

#[test]
fn solid_7z_is_decoded_in_one_pass() {
    let d = tempfile::tempdir().unwrap();
    let (root, big) = source_tree(d.path());
    let archive = create(
        d.path(),
        &root,
        "photos.7z",
        WriteOptions {
            format: FormatId::SevenZ,
            level: Level::Fast,
            solid: true,
            ..Default::default()
        },
    );
    let fs = mount(&archive, opts());
    assert_eq!(fs.mount_class(), MountClass::C);
    let a = resolve(&fs, "photos/相册/a.txt").unwrap();
    assert_eq!(read_all(&fs, a.node, 2).unwrap(), b"alpha");
    assert_eq!(
        fs.cache().len(),
        3,
        "whole solid archive cached by the first read"
    );
    let b = resolve(&fs, "photos/media/big.bin").unwrap();
    let fh = fs.open(b.node, OpenFlags::default()).unwrap();
    assert_eq!(
        fs.read(fh, 2_000_000, 8).unwrap(),
        &big[2_000_000..2_000_008]
    );
    fs.release(fh);

    // Without warm-up only the entry that was read is cached.
    let lazy = mount(
        &archive,
        FsOptions {
            warm_solid: false,
            ..opts()
        },
    );
    let a = resolve(&lazy, "photos/相册/a.txt").unwrap();
    assert_eq!(read_all(&lazy, a.node, 64).unwrap(), b"alpha");
    assert_eq!(lazy.cache().len(), 1);
    assert_eq!(lazy.warm().unwrap(), 3);
}

fn tar_with_links(path: &Path) {
    let mut b = tar::Builder::new(File::create(path).unwrap());
    let mut file = |name: &str, data: &[u8], mode: u32| {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Regular);
        h.set_size(data.len() as u64);
        h.set_mode(mode);
        h.set_mtime(1_700_000_000);
        b.append_data(&mut h, name, data).unwrap();
    };
    file("dir/Target.txt", b"upper", 0o4755);
    file("dir/target.TXT", b"lower", 0o600);
    file("dir/bad:name?.txt", b"odd", 0o644);
    file("dir/CON.txt", b"device", 0o644);
    let mut link = |name: &str, target: &str, kind: tar::EntryType| {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(kind);
        h.set_size(0);
        h.set_mode(0o777);
        h.set_mtime(1_700_000_000);
        b.append_link(&mut h, name, target).unwrap();
    };
    link("dir/link-in", "Target.txt", tar::EntryType::Symlink);
    link("dir/link-out", "../../etc/passwd", tar::EntryType::Symlink);
    link("dir/hard", "dir/Target.txt", tar::EntryType::Link);
    b.into_inner().unwrap();
}

#[test]
fn tar_names_links_and_collisions() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("links.tar");
    tar_with_links(&path);
    let windows_rules = NameRules {
        windows: true,
        case_insensitive: true,
    };
    let fs = mount(
        &path,
        FsOptions {
            names: windows_rules,
            links: LinkMode::Native,
            ..opts()
        },
    );
    assert_eq!(fs.mount_class(), MountClass::A);
    let dir = resolve(&fs, "dir").unwrap().node;
    assert_eq!(
        names(&fs, dir),
        [
            "Target.txt",
            "target (2).TXT",
            "bad_name_.txt",
            "CON_.txt",
            "link-in",
            "link-out",
            "hard"
        ]
    );
    let read = |p: &str| read_all(&fs, resolve(&fs, p).unwrap().node, 3).unwrap();
    assert_eq!(
        read("dir/TARGET.TXT"),
        b"upper",
        "case-insensitive lookup hits the first"
    );
    assert_eq!(read("dir/target (2).txt"), b"lower");
    assert_eq!(read("dir/bad_name_.txt"), b"odd");
    assert_eq!(read("dir/con_.txt"), b"device");

    let target = resolve(&fs, "dir/Target.txt").unwrap();
    assert_eq!(target.mode, 0o555, "setuid and write bits dropped");
    assert_eq!(resolve(&fs, "dir/target (2).TXT").unwrap().mode, 0o444);

    // A link that stays inside the volume is a link…
    let inside = resolve(&fs, "dir/link-in").unwrap();
    assert_eq!(inside.kind, FileType::Symlink);
    assert_eq!(fs.readlink(inside.node).unwrap(), Path::new("Target.txt"));
    // …one that leaves it is a small file holding the target text.
    let outside = resolve(&fs, "dir/link-out").unwrap();
    assert_eq!(outside.kind, FileType::File);
    assert_eq!(read("dir/link-out"), b"../../etc/passwd");
    assert_eq!(fs.readlink(outside.node), Err(FsError::Invalid));
    // Hard links are independent files with the target's content.
    let hard = resolve(&fs, "dir/hard").unwrap();
    assert_eq!((hard.kind, hard.size), (FileType::File, 5));
    assert_eq!(read("dir/hard"), b"upper");

    // Windows shows every link as a file.
    let as_files = mount(
        &path,
        FsOptions {
            links: LinkMode::AsFiles,
            ..opts()
        },
    );
    let n = resolve(&as_files, "dir/link-in").unwrap();
    assert_eq!(n.kind, FileType::File);
    assert_eq!(read_all(&as_files, n.node, 64).unwrap(), b"Target.txt");
}

/// A reader that serves scripted entries and counts decoding passes.
struct Scripted {
    info: ArchiveInfo,
    entries: Vec<Entry>,
    data: Vec<Vec<u8>>,
    passes: Arc<AtomicUsize>,
}

impl Scripted {
    fn new(files: Vec<(&str, Vec<u8>, Option<u64>)>) -> (Self, Arc<AtomicUsize>) {
        let passes = Arc::new(AtomicUsize::new(0));
        let mut entries = Vec::new();
        let mut data = Vec::new();
        for (i, (name, bytes, declared)) in files.into_iter().enumerate() {
            let mut e = Entry::new(i as u32, name.into(), EntryKind::File);
            e.size = declared.unwrap_or(0);
            e.size_known = declared.is_some();
            entries.push(e);
            data.push(bytes);
        }
        let info = ArchiveInfo::new(Path::new("scripted.zip"), FormatId::Zip, 0);
        (
            Self {
                info,
                entries,
                data,
                passes: passes.clone(),
            },
            passes,
        )
    }
}

impl ArchiveReader for Scripted {
    fn info(&self) -> &ArchiveInfo {
        &self.info
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn extract_to(&mut self, sink: &mut dyn ExtractSink, ctx: &JobCtx) -> omnizip_core::Result<()> {
        self.passes.fetch_add(1, Ordering::SeqCst);
        for (e, d) in self.entries.iter().zip(&self.data) {
            if sink.done() {
                break;
            }
            if sink.wants(e) {
                let mut r = Cursor::new(d.as_slice());
                sink.entry(e, EntryData::Stream(&mut r), ctx)?;
            }
        }
        Ok(())
    }
}

/// An archive that is one compressed stream (tar.gz) decodes from its start whichever entry is
/// wanted: reading a whole folder decodes it once, not once per file.
#[test]
fn single_stream_archives_are_decoded_in_one_pass() {
    let names: Vec<String> = (0..50).map(|i| format!("dir/f{i:02}.txt")).collect();
    for warm_solid in [true, false] {
        let (mut reader, passes) = Scripted::new(
            names
                .iter()
                .map(|n| (n.as_str(), n.as_bytes().to_vec(), Some(n.len() as u64)))
                .collect(),
        );
        reader.info.format = FormatId::TarGz;
        let fs = ArchiveFs::new(
            Box::new(reader),
            FsOptions {
                warm_solid,
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(fs.mount_class(), MountClass::D);
        for n in names.iter().rev() {
            let a = resolve(&fs, n).unwrap();
            assert_eq!(read_all(&fs, a.node, 64).unwrap(), n.as_bytes());
        }
        assert_eq!(
            passes.load(Ordering::SeqCst),
            1,
            "warm_solid = {warm_solid}"
        );
    }
}

#[test]
fn entries_may_not_outgrow_their_declared_size() {
    let (reader, _) = Scripted::new(vec![
        ("liar.bin", vec![0; 100_000], Some(10)),
        ("unknown.bin", b"no size recorded".to_vec(), None),
    ]);
    let fs = ArchiveFs::new(Box::new(reader), opts()).unwrap();
    let liar = resolve(&fs, "liar.bin").unwrap();
    assert_eq!(liar.size, 10);
    assert_eq!(read_all(&fs, liar.node, 4), Err(FsError::Io));

    let unknown = resolve(&fs, "unknown.bin").unwrap();
    assert_eq!(unknown.size, 0, "not decoded yet");
    assert_eq!(read_all(&fs, unknown.node, 5).unwrap(), b"no size recorded");
    assert_eq!(
        fs.getattr(unknown.node).unwrap().size,
        16,
        "known once decoded"
    );
}

#[test]
fn concurrent_readers_share_one_decode() {
    let big = noise(4 << 20, 3);
    let (reader, passes) = Scripted::new(vec![("big.bin", big.clone(), Some(big.len() as u64))]);
    let fs = Arc::new(ArchiveFs::new(Box::new(reader), opts()).unwrap());
    let node = resolve(fs.as_ref(), "big.bin").unwrap().node;
    let threads: Vec<_> = (0..8u64)
        .map(|t| {
            let fs = fs.clone();
            let big = big.clone();
            std::thread::spawn(move || {
                let fh = fs.open(node, OpenFlags::default()).unwrap();
                for i in 0..50u64 {
                    let off = (t * 524_287 + i * 77_773) % big.len() as u64;
                    let got = fs.read(fh, off, 8192).unwrap();
                    let end = (off as usize + 8192).min(big.len());
                    assert_eq!(got, &big[off as usize..end]);
                }
                fs.release(fh);
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(passes.load(Ordering::SeqCst), 1);
}

#[test]
fn large_archives_mount_and_list_quickly() {
    // 100 000 entries; one directory holds 10 000 of them (docs/12-mount.md §10).
    let files: Vec<(String, Vec<u8>, Option<u64>)> = (0..100_000)
        .map(|i| {
            let name = if i < 10_000 {
                format!("flat/file-{i:05}.txt")
            } else {
                format!("tree/{:03}/file-{i}.txt", i % 900)
            };
            (name, Vec::new(), Some(0))
        })
        .collect();
    let (reader, _) = Scripted::new(
        files
            .iter()
            .map(|(n, d, s)| (n.as_str(), d.clone(), *s))
            .collect(),
    );
    let started = Instant::now();
    let fs = ArchiveFs::new(Box::new(reader), opts()).unwrap();
    let mounted = started.elapsed();
    let flat = resolve(&fs, "flat").unwrap().node;
    let started = Instant::now();
    let listed = fs.readdir(flat, 0, usize::MAX).unwrap();
    let listing = started.elapsed();
    assert_eq!(listed.len(), 10_000);
    assert_eq!(fs.statfs().unwrap().files, 100_000);
    eprintln!("mount 100k entries: {mounted:?}; list 10k: {listing:?}");
    // Generous bounds: this runs unoptimized in debug builds.
    assert!(mounted.as_secs() < 10 && listing.as_secs() < 5);
}
