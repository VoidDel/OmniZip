//! Mounts real archives through WinFsp and reads them with the ordinary file APIs, as Explorer
//! and other programs do. Skipped when WinFsp is not installed (CI runners).
#![cfg(windows)]

use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
};

use omnizip_core::{CollectOptions, FormatId, JobCtx, Level, OpenOptions, WriteOptions, source};
use omnizip_mount_winfsp::{WinFsp, free_drive_letters};
use omnizip_vfs::{ArchiveFs, FsOptions, Mount, MountBackend, MountError, MountOptions, OverlayFs};

fn ready() -> bool {
    let ok = omnizip_vfs::probe(true).can_mount();
    if !ok {
        eprintln!("WinFsp is not installed: skipped");
    }
    ok
}

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

/// `photos/docs/readme.txt`, `photos/media/big.bin` (3 MiB), `photos/相册/a.txt`,
/// `photos/empty/`, packed as `name` in `format`.
fn archive(dir: &Path, name: &str, format: FormatId) -> (PathBuf, Vec<u8>) {
    let root = dir.join("photos");
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::create_dir_all(root.join("media")).unwrap();
    fs::create_dir_all(root.join("相册")).unwrap();
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::write(root.join("docs/readme.txt"), "read me\n".repeat(100)).unwrap();
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

/// `Z:` → `Z:\`, so that joining stays on the volume root.
fn root(m: &dyn Mount) -> PathBuf {
    let p = m.mount_point().to_owned();
    if p.as_os_str().len() == 2 {
        PathBuf::from(format!("{}\\", p.display()))
    } else {
        p
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

fn check_contents(r: &Path, big: &[u8]) {
    assert_eq!(names(r), ["photos"]);
    let photos = r.join("photos");
    assert_eq!(names(&photos), ["docs", "empty", "media", "相册"]);
    assert!(names(&photos.join("empty")).is_empty());
    assert_eq!(
        fs::read_to_string(photos.join("docs").join("readme.txt")).unwrap(),
        "read me\n".repeat(100)
    );
    // Case-insensitive, like NTFS.
    assert_eq!(
        fs::read_to_string(photos.join("相册").join("A.TXT")).unwrap(),
        "alpha"
    );

    let big_path = photos.join("media").join("big.bin");
    let meta = fs::metadata(&big_path).unwrap();
    assert_eq!(meta.len(), big.len() as u64);
    // The volume is read-only, not its files: copies taken out are ordinary files.
    assert!(!meta.permissions().readonly());
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
    check_write_protected(&photos);
    check_normalized_name(&photos);
}

/// `ERROR_WRITE_PROTECT`, as on read-only media.
fn write_protected<T: std::fmt::Debug>(r: std::io::Result<T>, what: &str) {
    let e = r.expect_err(what);
    assert_eq!(e.raw_os_error(), Some(19), "{what}: {e}");
}

/// Every way of changing something is refused with "write protected", not "access denied";
/// an open asking for whatever is allowed still reads.
fn check_write_protected(photos: &Path) {
    let readme = photos.join("docs").join("readme.txt");
    write_protected(fs::write(photos.join("new.txt"), "x"), "new file");
    write_protected(fs::write(&readme, "x"), "overwrite");
    write_protected(
        fs::OpenOptions::new().read(true).write(true).open(&readme),
        "open for writing",
    );
    write_protected(fs::remove_file(&readme), "delete");
    write_protected(fs::rename(&readme, photos.join("moved.txt")), "rename");
    write_protected(fs::create_dir(photos.join("newdir")), "new directory");
    let mut ro = fs::metadata(&readme).unwrap().permissions();
    ro.set_readonly(true);
    write_protected(fs::set_permissions(&readme, ro), "change attributes");

    use std::{io::Write, os::windows::fs::OpenOptionsExt};
    const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
    let mut f = fs::OpenOptions::new()
        .access_mode(MAXIMUM_ALLOWED)
        .open(&readme)
        .expect("MAXIMUM_ALLOWED opens read-only");
    let mut head = [0; 7];
    f.read_exact(&mut head).unwrap();
    assert_eq!(&head, b"read me");
    write_protected(f.write_all(b"x"), "write through MAXIMUM_ALLOWED");
}

/// Opened in another case, a file reports its name as the volume spells it.
fn check_normalized_name(photos: &Path) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
    const VOLUME_NAME_NT: u32 = 2;
    let f = fs::File::open(photos.join("相册").join("A.TXT")).unwrap();
    let mut buf = [0u16; 1024];
    // SAFETY: an open handle and a buffer of the length given.
    let n = unsafe {
        GetFinalPathNameByHandleW(
            f.as_raw_handle(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            VOLUME_NAME_NT,
        )
    } as usize;
    assert!(n > 0 && n < buf.len());
    let name = String::from_utf16_lossy(&buf[..n]);
    assert!(name.ends_with(r"\photos\相册\a.txt"), "{name}");
}

#[test]
fn zip_on_a_drive_letter() {
    if !ready() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (zip, big) = archive(d.path(), "photos.zip", FormatId::Zip);
    let m = WinFsp
        .mount(volume(&zip), &MountOptions::default())
        .unwrap();
    let r = root(m.as_ref());
    assert!(m.is_alive());
    assert_eq!(
        m.mount_point().as_os_str().len(),
        2,
        "{}",
        m.mount_point().display()
    );
    check_contents(&r, &big);

    // The letter is taken now.
    let letter = m.mount_point().to_owned();
    let again = WinFsp.mount(
        volume(&zip),
        &MountOptions {
            mount_point: Some(letter.clone()),
            ..Default::default()
        },
    );
    assert!(matches!(again, Err(MountError::InUse(p)) if p == letter));

    drop(m);
    assert!(!r.exists());
}

#[test]
fn solid_7z_on_an_empty_folder() {
    if !ready() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (archive, big) = archive(d.path(), "photos.7z", FormatId::SevenZ);
    let point = d.path().join("mnt");
    fs::create_dir(&point).unwrap();
    let m = WinFsp
        .mount(
            volume(&archive),
            &MountOptions {
                mount_point: Some(point.clone()),
                label: Some("Fotos".into()),
                ..Default::default()
            },
        )
        .unwrap();
    check_contents(&point, &big);
    drop(m);
    // The empty folder is back.
    assert!(point.is_dir());
    assert!(names(&point).is_empty());

    // A folder with something in it is refused.
    fs::write(point.join("keep.txt"), "x").unwrap();
    let busy = WinFsp.mount(
        volume(&archive),
        &MountOptions {
            mount_point: Some(point.clone()),
            ..Default::default()
        },
    );
    assert!(matches!(busy, Err(MountError::InUse(_))));
    assert!(point.join("keep.txt").is_file());
}

#[test]
fn free_letters_shrink_while_mounted() {
    if !ready() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (zip, _) = archive(d.path(), "photos.zip", FormatId::Zip);
    let m = WinFsp
        .mount(volume(&zip), &MountOptions::default())
        .unwrap();
    let letter = m.mount_point().to_string_lossy().chars().next().unwrap();
    assert!(!free_drive_letters().contains(&letter));
    drop(m);
    assert!(free_drive_letters().contains(&letter));
}

/// The context menu's "Eject" shows on the drive OmniZip mounted (its file system reports
/// "OmniZip"), not on a folder inside it, nor on the system drive.
#[test]
fn eject_shows_on_the_mounted_drive() {
    use omnizip_shell::menu::{Verb, applies, classify};
    if !ready() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (zip, _) = archive(d.path(), "photos.zip", FormatId::Zip);
    let m = WinFsp
        .mount(volume(&zip), &MountOptions::default())
        .unwrap();
    let r = root(m.as_ref());
    let drive = classify(std::slice::from_ref(&r));
    assert!(drive.mount_root, "{}", r.display());
    assert!(applies(Verb::Unmount, &drive));
    let inside = classify(&[r.join("photos")]);
    assert!(!inside.mount_root && !applies(Verb::Unmount, &inside));
    let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    assert!(!classify(&[PathBuf::from(format!("{system}\\"))]).mount_root);
}

/// A writable layer over the archive, changed through the ordinary file APIs: new files and
/// folders, overwrite, append, truncate, rename (also over an existing file, as editors save),
/// delete, copy, times. The archive stays as it was; the layer lists what changed.
#[test]
fn writable_layer_through_the_file_apis() {
    if !ready() {
        return;
    }
    use std::io::Write;
    let d = tempfile::tempdir().unwrap();
    let (zip, big) = archive(d.path(), "photos.zip", FormatId::Zip);
    let lower = volume(&zip);
    let overlay = Arc::new(OverlayFs::new(lower.clone(), d.path().join("layer")).unwrap());
    let point = d.path().join("mnt");
    let m = WinFsp
        .mount(
            overlay.clone(),
            &MountOptions {
                mount_point: Some(point.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    let photos = point.join("photos");
    let readme = photos.join("docs").join("readme.txt");

    fs::write(photos.join("new.txt"), "fresh").unwrap();
    assert_eq!(fs::read_to_string(photos.join("new.txt")).unwrap(), "fresh");
    fs::OpenOptions::new()
        .append(true)
        .open(&readme)
        .unwrap()
        .write_all(b"more\n")
        .unwrap();
    assert_eq!(
        fs::read_to_string(&readme).unwrap(),
        "read me\n".repeat(100) + "more\n"
    );
    fs::write(photos.join("相册").join("a.txt"), "beta").unwrap();
    assert_eq!(
        fs::read_to_string(photos.join("相册").join("A.TXT")).unwrap(),
        "beta"
    );

    fs::create_dir(photos.join("notes")).unwrap();
    fs::write(photos.join("notes").join("n.txt"), "n").unwrap();
    assert_eq!(
        names(&photos),
        ["docs", "empty", "media", "new.txt", "notes", "相册"]
    );

    // An editor's save: write a temporary file, then move it over the original.
    fs::write(photos.join("notes").join("n.tmp"), "n2").unwrap();
    fs::rename(
        photos.join("notes").join("n.tmp"),
        photos.join("notes").join("n.txt"),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(photos.join("notes").join("n.txt")).unwrap(),
        "n2"
    );
    assert_eq!(names(&photos.join("notes")), ["n.txt"]);

    fs::rename(photos.join("media"), photos.join("videos")).unwrap();
    assert_eq!(
        fs::read(photos.join("videos").join("big.bin")).unwrap(),
        big
    );
    fs::copy(
        photos.join("videos").join("big.bin"),
        photos.join("copy.bin"),
    )
    .unwrap();
    assert_eq!(fs::read(photos.join("copy.bin")).unwrap(), big);
    let f = fs::OpenOptions::new()
        .write(true)
        .open(photos.join("copy.bin"))
        .unwrap();
    f.set_len(10).unwrap();
    let when = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    f.set_modified(when).unwrap();
    drop(f);
    let meta = fs::metadata(photos.join("copy.bin")).unwrap();
    assert_eq!((meta.len(), meta.modified().unwrap()), (10, when));

    let e = fs::remove_dir(photos.join("docs")).expect_err("not empty");
    assert_eq!(e.raw_os_error(), Some(145), "{e}"); // ERROR_DIR_NOT_EMPTY
    fs::remove_file(&readme).unwrap();
    fs::remove_dir(photos.join("docs")).unwrap();
    fs::remove_dir(photos.join("empty")).unwrap();
    assert!(!readme.exists());
    assert_eq!(
        names(&photos),
        ["copy.bin", "new.txt", "notes", "videos", "相册"]
    );

    drop(m);
    let c = overlay.changes();
    assert_eq!(
        c.files,
        [
            "photos/copy.bin",
            "photos/new.txt",
            "photos/notes/n.txt",
            "photos/videos/big.bin",
            "photos/相册/a.txt"
        ]
    );
    assert_eq!(c.dirs, ["photos/notes", "photos/videos"]);
    assert_eq!(c.deleted, ["photos/docs", "photos/empty", "photos/media"]);
    // The archive below is untouched.
    let original = omnizip_vfs::resolve(&*lower, "photos/docs/readme.txt").unwrap();
    assert_eq!(
        omnizip_vfs::read_all(&*lower, original.node, 4096).unwrap(),
        "read me\n".repeat(100).as_bytes()
    );
}
