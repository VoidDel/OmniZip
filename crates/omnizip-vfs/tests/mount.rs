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
    JobCtx, Level, MountClass, OpenOptions, SecurityPolicy, WriteOptions, source,
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
        ..CacheLimits::default()
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

/// Whether `f` holds within a few seconds (work the volume finishes in the background).
fn eventually(f: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    while !f() {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    true
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
            // big.bin is random, which would be stored as it is (and read in place): compressed,
            // it is decoded and spooled to the disk cache, which this test is about.
            skip_compressed: false,
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
    // Decoded in the background: in the cache a moment after the last byte was read.
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while fs.cache().usage().1 == 0 && Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
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
    // The read comes back once its file is decoded; the pass goes on with the rest.
    assert!(
        eventually(|| fs.cache().len() == 3),
        "whole solid archive cached after the first read"
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

/// A link is resolved through the archive's other links, as the system resolves it once shown as
/// a link: with `sub/up -> ..`, the target `../up/..` of `sub/x/esc` reads as `sub` but leads
/// out of the volume, so it is shown as a file holding its target.
#[test]
fn links_leading_out_through_other_links_are_files() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("chain.tar");
    let mut b = tar::Builder::new(File::create(&path).unwrap());
    let mut add = |name: &str, target: Option<&str>| {
        let mut h = tar::Header::new_gnu();
        h.set_mode(0o777);
        h.set_mtime(1_700_000_000);
        h.set_size(0);
        match target {
            Some(t) => {
                h.set_entry_type(tar::EntryType::Symlink);
                b.append_link(&mut h, name, t).unwrap();
            }
            None => {
                h.set_entry_type(tar::EntryType::Regular);
                b.append_data(&mut h, name, &[][..]).unwrap();
            }
        }
    };
    add("sub/x/file.txt", None);
    add("sub/x/esc", Some("../up/.."));
    add("sub/up", Some(".."));
    add("sub/x/ok", Some("../up/sub/x/file.txt"));
    b.into_inner().unwrap();
    let fs = mount(
        &path,
        FsOptions {
            links: LinkMode::Native,
            ..opts()
        },
    );
    assert_eq!(resolve(&fs, "sub/up").unwrap().kind, FileType::Symlink);
    assert_eq!(resolve(&fs, "sub/x/ok").unwrap().kind, FileType::Symlink);
    let esc = resolve(&fs, "sub/x/esc").unwrap();
    assert_eq!(esc.kind, FileType::File);
    assert_eq!(read_all(&fs, esc.node, 64).unwrap(), b"../up/..");
}

/// A reader that serves scripted entries and counts decoding passes.
#[derive(Clone)]
struct Scripted {
    info: ArchiveInfo,
    entries: Vec<Entry>,
    data: Arc<Vec<Vec<u8>>>,
    passes: Arc<AtomicUsize>,
    /// Clones decode side by side ([`ArchiveReader::try_clone`]).
    clones: bool,
    /// The solid block of each entry.
    blocks: Option<Vec<u32>>,
    /// Data comes in pieces of at most 64 KiB, each after this pause.
    pace: Option<std::time::Duration>,
    /// Passes running now, and the most that ran at once.
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
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
                data: Arc::new(data),
                passes: passes.clone(),
                clones: false,
                blocks: None,
                pace: None,
                active: Arc::default(),
                peak: Arc::default(),
            },
            passes,
        )
    }

    /// `n` files `fNN.txt` of `size` bytes each.
    fn files(n: usize, size: usize) -> (Self, Arc<AtomicUsize>) {
        let names: Vec<String> = (0..n).map(|i| format!("f{i:02}.txt")).collect();
        Self::new(
            names
                .iter()
                .enumerate()
                .map(|(i, name)| (name.as_str(), noise(size, i as u32), Some(size as u64)))
                .collect(),
        )
    }
}

/// Reads in paced pieces.
struct Paced<'a> {
    inner: Cursor<&'a [u8]>,
    pace: std::time::Duration,
}

impl std::io::Read for Paced<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::thread::sleep(self.pace);
        let n = buf.len().min(64 << 10);
        self.inner.read(&mut buf[..n])
    }
}

impl ArchiveReader for Scripted {
    fn info(&self) -> &ArchiveInfo {
        &self.info
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn try_clone(&self) -> Option<Box<dyn ArchiveReader>> {
        self.clones
            .then(|| Box::new(self.clone()) as Box<dyn ArchiveReader>)
    }

    fn solid_block(&self, id: omnizip_core::EntryId) -> Option<u32> {
        self.blocks.as_ref()?.get(id as usize).copied()
    }

    fn extract_to(&mut self, sink: &mut dyn ExtractSink, ctx: &JobCtx) -> omnizip_core::Result<()> {
        self.passes.fetch_add(1, Ordering::SeqCst);
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        let mut result = Ok(());
        for (e, d) in self.entries.iter().zip(self.data.iter()) {
            if sink.done() {
                break;
            }
            if sink.wants(e) {
                let mut cursor = Cursor::new(d.as_slice());
                let mut paced;
                let r: &mut dyn std::io::Read = match self.pace {
                    Some(pace) => {
                        paced = Paced {
                            inner: cursor,
                            pace,
                        };
                        &mut paced
                    }
                    None => &mut cursor,
                };
                result = sink.entry(e, EntryData::Stream(r), ctx);
                if result.is_err() {
                    break;
                }
            }
        }
        self.active.fetch_sub(1, Ordering::SeqCst);
        result
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

/// The declared size comes from the archive too: an entry may not unpack past the expansion
/// ratio extraction would ask about, whatever it declares.
#[test]
fn entries_may_not_pass_the_expansion_ratio() {
    let (mut reader, _) = Scripted::new(vec![
        ("bomb.bin", vec![0; 300_000], Some(300_000)),
        ("fine.bin", vec![1; 900], Some(900)),
    ]);
    reader.info.archive_size = 100;
    let fs = ArchiveFs::new(
        Box::new(reader),
        FsOptions {
            policy: SecurityPolicy {
                max_ratio: 10,
                ratio_min_bytes: 1000,
                ..SecurityPolicy::default()
            },
            ..opts()
        },
    )
    .unwrap();
    let bomb = resolve(&fs, "bomb.bin").unwrap();
    assert_eq!(bomb.size, 300_000, "the declared size is still shown");
    assert_eq!(read_all(&fs, bomb.node, 4096), Err(FsError::Io));
    let fine = resolve(&fs, "fine.bin").unwrap();
    assert_eq!(read_all(&fs, fine.node, 4096).unwrap(), vec![1; 900]);
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

/// One file whose data stops after `gate` bytes until the test opens the gate.
struct Gated {
    info: ArchiveInfo,
    entries: Vec<Entry>,
    data: Arc<Vec<u8>>,
    gate: usize,
    open: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

struct GatedRead<'a> {
    g: &'a Gated,
    pos: usize,
}

impl std::io::Read for GatedRead<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.g.gate {
            let (lock, cv) = &*self.g.open;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
        }
        let end = if self.pos < self.g.gate {
            self.g.gate
        } else {
            self.g.data.len()
        };
        let n = buf.len().min(end - self.pos);
        buf[..n].copy_from_slice(&self.g.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl ArchiveReader for Gated {
    fn info(&self) -> &ArchiveInfo {
        &self.info
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn extract_to(&mut self, sink: &mut dyn ExtractSink, ctx: &JobCtx) -> omnizip_core::Result<()> {
        let e = self.entries[0].clone();
        if sink.wants(&e) {
            let mut r = GatedRead { g: self, pos: 0 };
            sink.entry(&e, EntryData::Stream(&mut r), ctx)?;
        }
        Ok(())
    }
}

/// A large file is readable while it is being decoded: the start comes back before the rest
/// is there, and the whole file is right once it is.
#[test]
fn large_files_are_read_while_they_decode() {
    let data = Arc::new(noise(6 << 20, 9));
    let open = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let mut e = Entry::new(0, "video.bin".into(), EntryKind::File);
    e.size = data.len() as u64;
    e.size_known = true;
    let reader = Gated {
        info: ArchiveInfo::new(Path::new("gated.zip"), FormatId::Zip, 0),
        entries: vec![e],
        data: data.clone(),
        gate: 1 << 20,
        open: open.clone(),
    };
    let fs = Arc::new(ArchiveFs::new(Box::new(reader), opts()).unwrap());
    let node = resolve(fs.as_ref(), "video.bin").unwrap().node;
    let fh = fs.open(node, OpenFlags::default()).unwrap();

    // The first megabyte, while the decoder waits at the gate.
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let fs = fs.clone();
        std::thread::spawn(move || {
            let _ = tx.send(fs.read(fh, 1000, 64 << 10));
        });
    }
    let head = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the start is readable before the rest is decoded")
        .unwrap();
    assert_eq!(head, &data[1000..1000 + (64 << 10)]);
    assert_eq!(fs.cache().len(), 0, "still decoding");

    // Beyond the gate a read waits; opening it lets everything through.
    {
        let (lock, cv) = &*open;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    assert_eq!(
        fs.read(fh, (6 << 20) - 10, 64).unwrap(),
        &data[(6 << 20) - 10..]
    );
    fs.release(fh);
    assert_eq!(read_all(fs.as_ref(), node, 1 << 20).unwrap(), *data);
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while fs.cache().is_empty() && Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(fs.cache().len(), 1);
}

/// One large file whose decoder panics after `good` bytes (a decoder choking on corrupt data).
struct Panicky {
    info: ArchiveInfo,
    entries: Vec<Entry>,
    good: usize,
}

struct PanickyRead {
    pos: usize,
    good: usize,
}

impl std::io::Read for PanickyRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        assert!(self.pos < self.good, "corrupt data");
        let n = buf.len().min(self.good - self.pos);
        buf[..n].fill(7);
        self.pos += n;
        Ok(n)
    }
}

impl ArchiveReader for Panicky {
    fn info(&self) -> &ArchiveInfo {
        &self.info
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn extract_to(&mut self, sink: &mut dyn ExtractSink, ctx: &JobCtx) -> omnizip_core::Result<()> {
        let e = self.entries[0].clone();
        if sink.wants(&e) {
            let mut r = PanickyRead {
                pos: 0,
                good: self.good,
            };
            sink.entry(&e, EntryData::Stream(&mut r), ctx)?;
        }
        Ok(())
    }
}

/// Runs `f` on a thread and waits for it at most `secs` seconds.
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(std::time::Duration::from_secs(secs))
        .expect("the read came back")
}

/// A background decoder that panics fails the reads waiting on it, now and later, instead of
/// leaving them (and the program reading) hanging.
#[test]
fn a_panicking_decoder_fails_the_read() {
    let mut e = Entry::new(0, "video.bin".into(), EntryKind::File);
    e.size = 6 << 20;
    e.size_known = true;
    let reader = Panicky {
        info: ArchiveInfo::new(Path::new("panicky.zip"), FormatId::Zip, 0),
        entries: vec![e],
        good: 1 << 20,
    };
    let fs = Arc::new(ArchiveFs::new(Box::new(reader), opts()).unwrap());
    let node = resolve(fs.as_ref(), "video.bin").unwrap().node;
    let fh = fs.open(node, OpenFlags::default()).unwrap();
    for _ in 0..2 {
        let f = fs.clone();
        let got = within(10, move || f.read(fh, 5 << 20, 4096));
        assert_eq!(got, Err(FsError::Io));
    }
    fs.release(fh);
}

/// A decoder stuck without progress fails the read after the stall time; bytes it did
/// produce stay readable.
#[test]
fn a_stuck_decoder_fails_the_read_after_the_stall_time() {
    let data = Arc::new(noise(6 << 20, 11));
    let open = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let mut e = Entry::new(0, "video.bin".into(), EntryKind::File);
    e.size = data.len() as u64;
    e.size_known = true;
    let reader = Gated {
        info: ArchiveInfo::new(Path::new("stuck.zip"), FormatId::Zip, 0),
        entries: vec![e],
        data: data.clone(),
        gate: 1 << 20,
        open: open.clone(),
    };
    let fs = Arc::new(
        ArchiveFs::new(
            Box::new(reader),
            FsOptions {
                stall_timeout: std::time::Duration::from_millis(300),
                ..opts()
            },
        )
        .unwrap(),
    );
    let node = resolve(fs.as_ref(), "video.bin").unwrap().node;
    let fh = fs.open(node, OpenFlags::default()).unwrap();
    let f = fs.clone();
    let started = Instant::now();
    assert_eq!(
        within(10, move || f.read(fh, 3 << 20, 4096)),
        Err(FsError::Io)
    );
    assert!(started.elapsed() >= std::time::Duration::from_millis(300));
    let f = fs.clone();
    assert_eq!(
        within(10, move || f.read(fh, 100, 16)).unwrap(),
        &data[100..116]
    );
    // Let the decoder finish so the thread ends with the test.
    let (lock, cv) = &*open;
    *lock.lock().unwrap() = true;
    cv.notify_all();
    fs.release(fh);
}

/// Reads from several threads of entries that decode on their own run on several workers at
/// once, each with a clone of the reader; an engine without clones gets one worker.
#[test]
fn entries_decode_side_by_side_on_cloned_readers() {
    for clones in [true, false] {
        let (mut reader, passes) = Scripted::files(160, 20_000);
        reader.clones = clones;
        reader.pace = Some(std::time::Duration::from_millis(20));
        let (data, peak) = (reader.data.clone(), reader.peak.clone());
        let fs = Arc::new(
            ArchiveFs::new(
                Box::new(reader),
                FsOptions {
                    workers: 4,
                    ..opts()
                },
            )
            .unwrap(),
        );
        assert_eq!(fs.workers(), if clones { 4 } else { 1 });
        // Entries ten apart, so that read-ahead stays out of it.
        let threads: Vec<_> = (0..8u32)
            .map(|t| {
                let (fs, data) = (fs.clone(), data.clone());
                std::thread::spawn(move || {
                    for i in [t * 20, t * 20 + 10] {
                        let a = resolve(fs.as_ref(), &format!("f{i:02}.txt")).unwrap();
                        assert_eq!(
                            read_all(fs.as_ref(), a.node, 4096).unwrap(),
                            data[i as usize]
                        );
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(passes.load(Ordering::SeqCst), 16);
        let peak = peak.load(Ordering::SeqCst);
        if clones {
            assert!(peak > 1, "decoded one at a time");
        } else {
            assert_eq!(peak, 1);
        }
    }
}

/// Reading files in archive order (a folder copied out) decodes the next small files ahead of
/// the reads; reads elsewhere do not.
#[test]
fn reading_in_archive_order_decodes_ahead() {
    let (mut reader, _) = Scripted::files(40, 1000);
    reader.clones = true;
    let fs = ArchiveFs::new(Box::new(reader), opts()).unwrap();
    for i in 0..3 {
        let a = resolve(&fs, &format!("f{i:02}.txt")).unwrap();
        read_all(&fs, a.node, 4096).unwrap();
    }
    assert!(
        eventually(|| fs.cache().contains(20)),
        "the files after the ones read are decoded"
    );

    let (mut reader, _) = Scripted::files(40, 1000);
    reader.clones = true;
    let fs = ArchiveFs::new(Box::new(reader), opts()).unwrap();
    for i in [30, 3, 15] {
        let a = resolve(&fs, &format!("f{i:02}.txt")).unwrap();
        read_all(&fs, a.node, 4096).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(fs.cache().len(), 3);
}

/// A solid archive whose blocks the engine knows warms up block by block: the block of the
/// entry read first, then the others on the remaining workers.
#[test]
fn solid_blocks_are_decoded_as_separate_passes() {
    let (mut reader, passes) = Scripted::files(30, 5000);
    reader.clones = true;
    reader.info.format = FormatId::SevenZ;
    reader.info.solid = true;
    reader.blocks = Some((0..30).map(|i| i / 10).collect());
    let data = reader.data.clone();
    let fs = ArchiveFs::new(Box::new(reader), opts()).unwrap();
    assert_eq!(fs.mount_class(), MountClass::C);
    let a = resolve(&fs, "f25.txt").unwrap();
    assert_eq!(read_all(&fs, a.node, 4096).unwrap(), data[25]);
    assert!(eventually(|| fs.cache().len() == 30));
    assert_eq!(passes.load(Ordering::SeqCst), 3, "one pass per block");
    let b = resolve(&fs, "f03.txt").unwrap();
    assert_eq!(read_all(&fs, b.node, 4096).unwrap(), data[3]);
    assert_eq!(passes.load(Ordering::SeqCst), 3);
}

/// An entry late in a single-stream pass waits while the pass decodes the entries before it.
/// That is progress, not a stuck decoder, however long it takes.
#[test]
fn a_long_pass_before_an_entry_is_not_a_stall() {
    let (mut reader, _) = Scripted::new(vec![
        ("first.bin", noise(1 << 20, 1), Some(1 << 20)),
        ("second.txt", b"after the first".to_vec(), Some(15)),
    ]);
    reader.info.format = FormatId::TarGz;
    // 16 pieces of 64 KiB, 60 ms apart: about a second before the second entry.
    reader.pace = Some(std::time::Duration::from_millis(60));
    let fs = Arc::new(
        ArchiveFs::new(
            Box::new(reader),
            FsOptions {
                stall_timeout: std::time::Duration::from_millis(300),
                ..opts()
            },
        )
        .unwrap(),
    );
    let node = resolve(fs.as_ref(), "second.txt").unwrap().node;
    let f = fs.clone();
    assert_eq!(
        within(20, move || read_all(f.as_ref(), node, 64)).unwrap(),
        b"after the first"
    );
}

/// The engines clone their readers, so a mounted ZIP, 7z or tar.gz decodes on several workers;
/// what many threads read is right.
#[test]
fn real_archives_decode_on_several_workers() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("many");
    fs::create_dir_all(root.join("sub")).unwrap();
    let mut expected = Vec::new();
    for i in 0..60u32 {
        let rel = if i % 3 == 0 {
            format!("sub/file{i:02}.txt")
        } else {
            format!("file{i:02}.txt")
        };
        let data = format!("{i} ").repeat(500 + i as usize * 37).into_bytes();
        fs::write(root.join(&rel), &data).unwrap();
        expected.push((format!("many/{rel}"), data));
    }
    let expected = Arc::new(expected);
    for (name, format, solid) in [
        ("many.zip", FormatId::Zip, false),
        ("many.7z", FormatId::SevenZ, false),
        ("solid.7z", FormatId::SevenZ, true),
        ("many.tar.gz", FormatId::TarGz, false),
    ] {
        let archive = create(
            d.path(),
            &root,
            name,
            WriteOptions {
                format,
                solid,
                ..Default::default()
            },
        );
        let fs = Arc::new(mount(
            &archive,
            FsOptions {
                workers: 4,
                ..opts()
            },
        ));
        assert_eq!(fs.workers(), 4, "{name}");
        let threads: Vec<_> = (0..6usize)
            .map(|t| {
                let (fs, expected) = (fs.clone(), expected.clone());
                std::thread::spawn(move || {
                    for (path, data) in expected.iter().skip(t).step_by(6) {
                        let a = resolve(fs.as_ref(), path).unwrap();
                        assert_eq!(
                            &read_all(fs.as_ref(), a.node, 1000).unwrap(),
                            data,
                            "{path}"
                        );
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
    }
}

/// RAR readers open a native handle per decode: several decoding at once on different threads
/// (clones, other volumes) each get their own, and every read is right.
#[test]
fn rar_volumes_decode_side_by_side() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rar");
    let names = ["version.rar", "unicode-entry.rar", "solid.rar"];
    let only_file = |fs: &ArchiveFs| -> u32 {
        let mut dir = ROOT;
        loop {
            let e = fs.readdir(dir, 0, 1).unwrap().remove(0);
            if !e.attr.is_dir() {
                return e.attr.node;
            }
            dir = e.attr.node;
        }
    };
    let expected: Vec<Vec<u8>> = names
        .iter()
        .map(|n| {
            let fs = mount(&fixtures.join(n), opts());
            read_all(&fs, only_file(&fs), 1 << 16).unwrap()
        })
        .collect();
    let expected = Arc::new(expected);
    for _ in 0..10 {
        let volumes: Vec<Arc<ArchiveFs>> = names
            .iter()
            .map(|n| {
                Arc::new(mount(
                    &fixtures.join(n),
                    FsOptions {
                        workers: 4,
                        ..opts()
                    },
                ))
            })
            .collect();
        let threads: Vec<_> = (0..12)
            .map(|t| {
                let (fs, expected) = (volumes[t % 3].clone(), expected.clone());
                std::thread::spawn(move || {
                    let node = only_file(&fs);
                    assert_eq!(read_all(fs.as_ref(), node, 777).unwrap(), expected[t % 3]);
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
    }
}
