//! The writable layer over a mounted archive: what is written stays in the layer, the archive
//! below is never touched, and `changes` says what to save.

use std::{
    ffi::OsStr,
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime},
};

use omnizip_core::{CollectOptions, FormatId, JobCtx, Level, OpenOptions, WriteOptions, source};
use omnizip_vfs::{
    ArchiveFs, Changes, FsError, FsOptions, OpenFlags, OverlayFs, ROOT, VirtualFs, read_all,
    resolve,
};

const README: &str = "read me\n";

/// `docs/readme.txt`, `docs/notes.txt`, `media/big.bin` (1 MiB), `keep/a.txt`, as a ZIP.
fn archive(dir: &Path) -> (std::path::PathBuf, Vec<u8>) {
    let root = dir.join("src");
    for d in ["docs", "media", "keep"] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    fs::write(root.join("docs/readme.txt"), README.repeat(100)).unwrap();
    fs::write(root.join("docs/notes.txt"), "notes").unwrap();
    let big: Vec<u8> = (0..1u32 << 20).map(|i| (i * 7 % 251) as u8).collect();
    fs::write(root.join("media/big.bin"), &big).unwrap();
    fs::write(root.join("keep/a.txt"), "alpha").unwrap();
    let ctx = JobCtx::default();
    let roots: Vec<_> = ["docs", "media", "keep"]
        .iter()
        .map(|d| root.join(d))
        .collect();
    let sources = source::collect(&roots, &CollectOptions::default(), &ctx).unwrap();
    let dest = dir.join("a.zip");
    let wo = WriteOptions {
        format: FormatId::Zip,
        level: Level::Fast,
        ..Default::default()
    };
    omnizip_formats::shared()
        .create(&dest, &sources, &wo, &ctx)
        .unwrap();
    (dest, big)
}

struct Setup {
    _dir: tempfile::TempDir,
    lower: Arc<ArchiveFs>,
    fs: OverlayFs,
    big: Vec<u8>,
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let (zip, big) = archive(dir.path());
    let reader = omnizip_formats::shared()
        .open(&zip, &OpenOptions::default(), &JobCtx::default())
        .unwrap();
    let lower = Arc::new(ArchiveFs::new(reader, FsOptions::default()).unwrap());
    let fs = OverlayFs::new(lower.clone(), dir.path().join("layer")).unwrap();
    Setup {
        _dir: dir,
        lower,
        fs,
        big,
    }
}

fn node(fs: &dyn VirtualFs, path: &str) -> u32 {
    resolve(fs, path).unwrap().node
}

fn text(fs: &dyn VirtualFs, path: &str) -> String {
    String::from_utf8(read_all(fs, node(fs, path), 4096).unwrap()).unwrap()
}

fn names(fs: &dyn VirtualFs, path: &str) -> Vec<String> {
    let mut n: Vec<String> = fs
        .readdir(node(fs, path), 0, usize::MAX)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    n.sort();
    n
}

fn write(fs: &dyn VirtualFs, path: &str, offset: u64, data: &[u8]) {
    let fh = fs.open(node(fs, path), OpenFlags { write: true }).unwrap();
    assert_eq!(fs.write(fh, offset, data).unwrap(), data.len() as u32);
    fs.flush(fh).unwrap();
    fs.release(fh);
}

fn layer_files(fs: &OverlayFs) -> usize {
    fn walk(p: &Path) -> usize {
        fs::read_dir(p)
            .unwrap()
            .flatten()
            .map(|e| {
                if e.file_type().unwrap().is_dir() {
                    walk(&e.path())
                } else {
                    1
                }
            })
            .sum()
    }
    walk(fs.layer_dir())
}

#[test]
fn reads_pass_through_until_something_changes() {
    let s = setup();
    assert!(s.fs.writable());
    assert_eq!(text(&s.fs, "docs/readme.txt"), README.repeat(100));
    assert_eq!(
        read_all(&s.fs, node(&s.fs, "media/big.bin"), 65536).unwrap(),
        s.big
    );
    assert_eq!(names(&s.fs, ""), ["docs", "keep", "media"]);
    assert!(s.fs.changes().is_empty());
    assert!(!s.fs.is_dirty());
    assert_eq!(layer_files(&s.fs), 0);
}

#[test]
fn new_files_and_directories_live_in_the_layer() {
    let s = setup();
    let a = s.fs.create(ROOT, OsStr::new("new.txt"), false).unwrap();
    assert_eq!(a.size, 0);
    assert_eq!(
        s.fs.create(ROOT, OsStr::new("new.txt"), false),
        Err(FsError::Exists)
    );
    assert_eq!(
        s.fs.create(ROOT, OsStr::new("docs"), true),
        Err(FsError::Exists)
    );
    write(&s.fs, "new.txt", 0, b"hello");
    write(&s.fs, "new.txt", 8, b"!");
    assert_eq!(text(&s.fs, "new.txt"), "hello\0\0\0!");
    assert_eq!(s.fs.getattr(a.node).unwrap().size, 9);

    let d =
        s.fs.create(node(&s.fs, "docs"), OsStr::new("sub"), true)
            .unwrap();
    assert!(d.is_dir());
    s.fs.create(d.node, OsStr::new("x.txt"), false).unwrap();
    write(&s.fs, "docs/sub/x.txt", 0, b"x");
    assert_eq!(names(&s.fs, "docs"), ["notes.txt", "readme.txt", "sub"]);
    assert_eq!(names(&s.fs, ""), ["docs", "keep", "media", "new.txt"]);
    assert_eq!(
        s.fs.changes(),
        Changes {
            files: vec!["docs/sub/x.txt".into(), "new.txt".into()],
            dirs: vec!["docs/sub".into()],
            deleted: vec![],
            entries: vec![],
        }
    );
    // The archive below is untouched.
    assert_eq!(resolve(&*s.lower, "new.txt"), Err(FsError::NotFound));
}

#[test]
fn a_changed_file_is_copied_up_and_every_handle_sees_the_copy() {
    let s = setup();
    let id = node(&s.fs, "docs/readme.txt");
    let reader = s.fs.open(id, OpenFlags::default()).unwrap();
    assert_eq!(s.fs.read(reader, 0, 4).unwrap(), b"read");
    write(&s.fs, "docs/readme.txt", 0, b"READ");
    assert_eq!(s.fs.read(reader, 0, 9).unwrap(), b"READ me\nr");
    s.fs.release(reader);
    assert_eq!(s.fs.getattr(id).unwrap().size, README.len() as u64 * 100);
    assert_eq!(s.fs.changes().files, ["docs/readme.txt"]);
    assert_eq!(layer_files(&s.fs), 1);
    assert_eq!(text(&*s.lower, "docs/readme.txt"), README.repeat(100));

    // Truncating to nothing needs no copy of the old content.
    let big = node(&s.fs, "media/big.bin");
    s.fs.set_size(big, 0).unwrap();
    assert_eq!(s.fs.getattr(big).unwrap().size, 0);
    s.fs.set_size(big, 3).unwrap();
    assert_eq!(read_all(&s.fs, big, 16).unwrap(), [0, 0, 0]);

    let when = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let notes = node(&s.fs, "docs/notes.txt");
    s.fs.set_mtime(notes, when).unwrap();
    assert_eq!(s.fs.getattr(notes).unwrap().mtime, when);
    assert_eq!(text(&s.fs, "docs/notes.txt"), "notes");
    let c = s.fs.changes();
    assert_eq!(
        c.files,
        ["docs/notes.txt", "docs/readme.txt", "media/big.bin"]
    );
    // Each replaces the entry of its own path.
    assert!(c.entries.is_empty());
}

#[test]
fn deleted_entries_stay_hidden_and_a_new_directory_starts_empty() {
    let s = setup();
    let docs = node(&s.fs, "docs");
    s.fs.remove(docs, OsStr::new("notes.txt")).unwrap();
    assert_eq!(resolve(&s.fs, "docs/notes.txt"), Err(FsError::NotFound));
    assert_eq!(names(&s.fs, "docs"), ["readme.txt"]);
    assert_eq!(s.fs.changes().deleted, ["docs/notes.txt"]);

    assert_eq!(
        s.fs.remove(ROOT, OsStr::new("docs")),
        Err(FsError::NotEmpty)
    );
    s.fs.remove(docs, OsStr::new("readme.txt")).unwrap();
    s.fs.remove(ROOT, OsStr::new("docs")).unwrap();
    assert_eq!(names(&s.fs, ""), ["keep", "media"]);
    // The files went with their directory.
    assert_eq!(s.fs.changes().deleted, ["docs"]);
    assert_eq!(s.fs.changes().entries, ["docs"]);
    assert!(s.fs.is_dirty());

    let again = s.fs.create(ROOT, OsStr::new("docs"), true).unwrap();
    assert!(names(&s.fs, "docs").is_empty());
    assert_eq!(resolve(&s.fs, "docs/readme.txt"), Err(FsError::NotFound));
    s.fs.create(again.node, OsStr::new("readme.txt"), false)
        .unwrap();
    assert_eq!(text(&s.fs, "docs/readme.txt"), "");
    let c = s.fs.changes();
    assert_eq!(c.dirs, ["docs"]);
    assert_eq!(c.files, ["docs/readme.txt"]);
    assert_eq!(c.deleted, ["docs"]);
}

#[test]
fn renames_move_files_and_whole_directories() {
    let s = setup();
    let keep = node(&s.fs, "keep");
    s.fs.rename(keep, OsStr::new("a.txt"), ROOT, OsStr::new("b.txt"), false)
        .unwrap();
    assert_eq!(resolve(&s.fs, "keep/a.txt"), Err(FsError::NotFound));
    assert_eq!(text(&s.fs, "b.txt"), "alpha");
    assert!(names(&s.fs, "keep").is_empty());

    let docs = node(&s.fs, "docs");
    assert_eq!(
        s.fs.rename(
            docs,
            OsStr::new("notes.txt"),
            ROOT,
            OsStr::new("b.txt"),
            false
        ),
        Err(FsError::Exists)
    );
    assert_eq!(
        s.fs.rename(ROOT, OsStr::new("b.txt"), ROOT, OsStr::new("media"), true),
        Err(FsError::Access)
    );
    s.fs.rename(
        docs,
        OsStr::new("notes.txt"),
        ROOT,
        OsStr::new("b.txt"),
        true,
    )
    .unwrap();
    assert_eq!(text(&s.fs, "b.txt"), "notes");

    assert_eq!(
        s.fs.rename(ROOT, OsStr::new("media"), docs, OsStr::new("m"), false)
            .and_then(|()| s.fs.rename(
                docs,
                OsStr::new("m"),
                node(&s.fs, "docs/m"),
                OsStr::new("x"),
                false
            )),
        Err(FsError::Invalid)
    );
    s.fs.rename(docs, OsStr::new("m"), ROOT, OsStr::new("videos"), false)
        .unwrap();
    assert_eq!(resolve(&s.fs, "media"), Err(FsError::NotFound));
    assert_eq!(
        read_all(&s.fs, node(&s.fs, "videos/big.bin"), 65536).unwrap(),
        s.big
    );
    assert_eq!(names(&s.fs, ""), ["b.txt", "docs", "keep", "videos"]);
    assert_eq!(
        s.fs.changes(),
        Changes {
            files: vec!["b.txt".into(), "videos/big.bin".into()],
            dirs: vec!["videos".into()],
            deleted: vec!["docs/notes.txt".into(), "keep/a.txt".into(), "media".into()],
            entries: vec!["docs/notes.txt".into(), "keep/a.txt".into(), "media".into()],
        }
    );
}

#[cfg(windows)]
#[test]
fn names_are_case_insensitive_like_the_volume() {
    let s = setup();
    assert_eq!(text(&s.fs, "DOCS/README.TXT"), README.repeat(100));
    let docs = node(&s.fs, "docs");
    assert_eq!(
        s.fs.create(docs, OsStr::new("NOTES.TXT"), false),
        Err(FsError::Exists)
    );
    // Only the case changes.
    s.fs.rename(
        docs,
        OsStr::new("notes.txt"),
        docs,
        OsStr::new("Notes.txt"),
        false,
    )
    .unwrap();
    assert_eq!(names(&s.fs, "docs"), ["Notes.txt", "readme.txt"]);
    let c = s.fs.changes();
    assert_eq!(c.files, ["docs/Notes.txt"]);
    assert_eq!(c.deleted, ["docs/notes.txt"]);
}

/// An entry shown under another name (`:` is not allowed on Windows) is still the one a save
/// removes.
#[cfg(windows)]
#[test]
fn entries_keep_their_archive_names() {
    let dir = tempfile::tempdir().unwrap();
    let tar_path = dir.path().join("odd.tar");
    let mut b = tar::Builder::new(fs::File::create(&tar_path).unwrap());
    for (name, data) in [("x/a:b.txt", b"colon".as_slice()), ("x/c:d.txt", b"other")] {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, name, data).unwrap();
    }
    b.finish().unwrap();
    drop(b);
    let reader = omnizip_formats::shared()
        .open(&tar_path, &OpenOptions::default(), &JobCtx::default())
        .unwrap();
    let lower = Arc::new(ArchiveFs::new(reader, FsOptions::default()).unwrap());
    let fs = OverlayFs::new(lower, dir.path().join("layer")).unwrap();
    assert_eq!(names(&fs, "x"), ["a_b.txt", "c_d.txt"]);
    write(&fs, "x/a_b.txt", 0, b"C");
    fs.remove(node(&fs, "x"), OsStr::new("c_d.txt")).unwrap();
    let c = fs.changes();
    assert_eq!(c.files, ["x/a_b.txt"]);
    assert_eq!(c.deleted, ["x/c_d.txt"]);
    assert_eq!(c.entries, ["x/a:b.txt", "x/c:d.txt"]);
}
