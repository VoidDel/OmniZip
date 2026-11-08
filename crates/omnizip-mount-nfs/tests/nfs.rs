//! Mounts real archives through the loopback NFS server and `mount_nfs`, and reads and writes
//! them with the ordinary file APIs, as the Finder and other programs do.
#![cfg(target_os = "macos")]

use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use omnizip_core::{CollectOptions, FormatId, JobCtx, Level, OpenOptions, WriteOptions, source};
use omnizip_mount_nfs::Nfs;
use omnizip_vfs::{ArchiveFs, FsOptions, Mount, MountBackend, MountError, MountOptions, OverlayFs};

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

/// `photos/docs/readme.txt`, `photos/docs/café.txt`, `photos/media/big.bin` (3 MiB),
/// `photos/相册/a.txt`, `photos/empty/`, packed as `name` in `format`.
fn archive(dir: &Path, name: &str, format: FormatId) -> (PathBuf, Vec<u8>) {
    let root = dir.join("photos");
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::create_dir_all(root.join("media")).unwrap();
    fs::create_dir_all(root.join("相册")).unwrap();
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::write(root.join("docs/readme.txt"), "read me\n".repeat(100)).unwrap();
    fs::write(root.join("docs/caf\u{e9}.txt"), "coffee").unwrap();
    let big = noise(3 << 20, 7);
    fs::write(root.join("media/big.bin"), &big).unwrap();
    fs::write(root.join("相册/a.txt"), "alpha").unwrap();
    let ctx = JobCtx::default();
    let sources = source::collect(&[root], &CollectOptions::default(), &ctx).unwrap();
    let dest = dir.join(name);
    let wo = WriteOptions {
        format,
        level: Level::Fast,
        ..Default::default()
    };
    omnizip_formats::shared()
        .create(&dest, &sources, &wo, &ctx)
        .unwrap();
    (dest, big)
}

fn volume(path: &Path) -> Arc<ArchiveFs> {
    let reader = omnizip_formats::shared()
        .open(path, &OpenOptions::default(), &JobCtx::default())
        .unwrap();
    Arc::new(ArchiveFs::new(reader, FsOptions::default()).unwrap())
}

fn at(point: PathBuf) -> MountOptions {
    MountOptions {
        mount_point: Some(point),
        ..Default::default()
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn errno<T: std::fmt::Debug>(r: std::io::Result<T>, what: &str) -> i32 {
    r.expect_err(what).raw_os_error().unwrap_or(0)
}

fn check_contents(r: &Path, big: &[u8]) {
    assert_eq!(names(r), ["photos"]);
    let photos = r.join("photos");
    assert_eq!(names(&photos), ["docs", "empty", "media", "相册"]);
    assert!(names(&photos.join("empty")).is_empty());
    assert_eq!(
        fs::read_to_string(photos.join("docs").join("readme.txt")).unwrap(),
        "read me\n".repeat(100)
    );
    assert_eq!(
        fs::read_to_string(photos.join("相册").join("a.txt")).unwrap(),
        "alpha"
    );
    // Case-insensitive, like the system's own volumes.
    assert_eq!(
        fs::read_to_string(photos.join("相册").join("A.TXT")).unwrap(),
        "alpha"
    );
    // A name in NFD (as some programs write it) finds the NFC name of the volume.
    assert_eq!(
        fs::read_to_string(photos.join("docs").join("cafe\u{301}.txt")).unwrap(),
        "coffee"
    );
    let big_path = photos.join("media").join("big.bin");
    let meta = fs::metadata(&big_path).unwrap();
    assert_eq!(meta.len(), big.len() as u64);
    assert_eq!(fs::read(&big_path).unwrap(), big);
    // Random access, backwards.
    let mut f = fs::File::open(&big_path).unwrap();
    let mut buf = vec![0; 4096];
    for off in [2_000_000u64, 17, 3_000_000] {
        f.seek(SeekFrom::Start(off)).unwrap();
        f.read_exact(&mut buf).unwrap();
        assert_eq!(buf, &big[off as usize..off as usize + 4096]);
    }
    assert!(fs::metadata(photos.join("missing.txt")).is_err());
}

/// Every way of changing something is refused with "read-only file system".
fn check_read_only(photos: &Path) {
    let readme = photos.join("docs").join("readme.txt");
    let erofs =
        |r: std::io::Result<()>, what: &str| assert_eq!(errno(r, what), libc::EROFS, "{what}");
    erofs(fs::write(photos.join("new.txt"), "x"), "new file");
    erofs(fs::write(&readme, "x"), "overwrite");
    erofs(fs::remove_file(&readme), "delete");
    erofs(fs::rename(&readme, photos.join("moved.txt")), "rename");
    erofs(fs::create_dir(photos.join("newdir")), "new directory");
}

/// The mount on `point` goes away within a few seconds of `done`.
fn wait_gone(m: &dyn Mount) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while m.is_alive() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!m.is_alive());
}

#[test]
fn zip_read_only() {
    let d = tempfile::tempdir().unwrap();
    let (zip, big) = archive(d.path(), "photos.zip", FormatId::Zip);
    let point = d.path().join("mnt");
    let m = Nfs.mount(volume(&zip), &at(point.clone())).unwrap();
    assert!(m.is_alive());
    assert_eq!(m.mount_point(), point);
    check_contents(&point, &big);
    check_read_only(&point.join("photos"));

    // The folder is taken now.
    let again = Nfs.mount(volume(&zip), &at(point.clone()));
    assert!(matches!(again, Err(MountError::InUse(p)) if p == point));

    drop(m);
    // A folder that was created goes with the volume.
    assert!(!point.exists());
}

#[test]
fn solid_7z_on_an_empty_folder() {
    let d = tempfile::tempdir().unwrap();
    let (archive, big) = archive(d.path(), "photos.7z", FormatId::SevenZ);
    let point = d.path().join("mnt");
    fs::create_dir(&point).unwrap();
    let m = Nfs.mount(volume(&archive), &at(point.clone())).unwrap();
    check_contents(&point, &big);
    drop(m);
    // An existing folder stays, empty.
    assert!(point.is_dir());
    assert!(names(&point).is_empty());
}

#[test]
fn unmounted_from_outside() {
    let d = tempfile::tempdir().unwrap();
    let (zip, _) = archive(d.path(), "photos.zip", FormatId::Zip);
    let point = d.path().join("mnt");
    let m = Nfs.mount(volume(&zip), &at(point.clone())).unwrap();
    let out = std::process::Command::new("/sbin/umount")
        .arg(&point)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    wait_gone(m.as_ref());
    drop(m);
    assert!(!point.exists());
}

#[test]
fn writable_layer() {
    let d = tempfile::tempdir().unwrap();
    let (zip, big) = archive(d.path(), "photos.zip", FormatId::Zip);
    let before = fs::read(&zip).unwrap();
    let overlay = Arc::new(OverlayFs::new(volume(&zip), d.path().join("layer")).unwrap());
    let point = d.path().join("mnt");
    let m = Nfs
        .mount(
            overlay.clone(),
            &MountOptions {
                mount_point: Some(point.clone()),
                read_only: false,
                ..Default::default()
            },
        )
        .unwrap();
    let photos = point.join("photos");

    // New file, appended to.
    fs::write(photos.join("new.txt"), "one").unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(photos.join("new.txt"))
        .unwrap()
        .write_all(b" two")
        .unwrap();
    assert_eq!(
        fs::read_to_string(photos.join("new.txt")).unwrap(),
        "one two"
    );
    // A file of the archive overwritten; its neighbour still reads from the archive.
    fs::write(photos.join("docs/readme.txt"), "changed").unwrap();
    assert_eq!(
        fs::read_to_string(photos.join("docs/readme.txt")).unwrap(),
        "changed"
    );
    assert_eq!(fs::read(photos.join("media/big.bin")).unwrap(), big);
    // Directories, moves, an editor's save (write a temporary file, rename it over).
    fs::create_dir(photos.join("new dir")).unwrap();
    fs::rename(photos.join("new.txt"), photos.join("new dir/moved.txt")).unwrap();
    fs::write(photos.join("相册/a.txt.tmp"), "beta").unwrap();
    fs::rename(photos.join("相册/a.txt.tmp"), photos.join("相册/a.txt")).unwrap();
    assert_eq!(
        fs::read_to_string(photos.join("相册/a.txt")).unwrap(),
        "beta"
    );
    // Truncate, delete.
    let f = fs::OpenOptions::new()
        .write(true)
        .open(photos.join("new dir/moved.txt"))
        .unwrap();
    f.set_len(3).unwrap();
    drop(f);
    assert_eq!(
        fs::read_to_string(photos.join("new dir/moved.txt")).unwrap(),
        "one"
    );
    fs::remove_dir(photos.join("empty")).unwrap();
    // macOS keeps extended attributes in `._` files on NFS volumes; they are not changes.
    let listed: Vec<String> = names(&photos)
        .into_iter()
        .filter(|n| !n.starts_with("._"))
        .collect();
    assert_eq!(
        listed,
        ["docs", "media", "new dir", "相册"],
        "listings follow the changes"
    );
    assert_eq!(
        errno(fs::remove_dir(photos.join("docs")), "non-empty"),
        libc::ENOTEMPTY
    );

    drop(m);
    let changes = overlay.changes();
    let mut files = changes.files.clone();
    files.sort();
    assert_eq!(
        files,
        [
            "photos/docs/readme.txt",
            "photos/new dir/moved.txt",
            "photos/相册/a.txt"
        ]
    );
    assert!(changes.deleted.iter().any(|p| p == "photos/empty"));
    // The archive itself is untouched.
    assert_eq!(fs::read(&zip).unwrap(), before);
}
