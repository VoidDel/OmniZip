//! Mounts real archives through FUSE and reads them with the ordinary file APIs, as file
//! managers and other programs do. Skipped when FUSE is not available (containers), unless
//! `OMNIZIP_REQUIRE_MOUNT=1` (CI).
#![cfg(target_os = "linux")]

use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use omnizip_core::{CollectOptions, FormatId, JobCtx, Level, OpenOptions, WriteOptions, source};
use omnizip_mount_fuse::{FS_TYPE, Fuse};
use omnizip_vfs::{ArchiveFs, FsOptions, MountBackend, MountError, MountOptions, OverlayFs};

fn ready() -> bool {
    let ok = omnizip_vfs::probe(true).can_mount();
    if !ok {
        // CI installs the driver and sets this: the tests must not pass by skipping there.
        assert!(
            std::env::var("OMNIZIP_REQUIRE_MOUNT").as_deref() != Ok("1"),
            "FUSE is not available (OMNIZIP_REQUIRE_MOUNT=1)"
        );
        eprintln!("FUSE is not available: skipped");
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

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// The file system type the system lists for the mount on `point`.
fn fs_type(point: &Path) -> Option<String> {
    let point = point.to_str()?;
    fs::read_to_string("/proc/self/mountinfo")
        .ok()?
        .lines()
        .find_map(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            let sep = f.iter().position(|x| *x == "-")?;
            (unescape(f.get(4)?) == point).then(|| f[sep + 1].to_owned())
        })
}

/// A path as `mountinfo` writes it: space, tab, newline and backslash as octal escapes.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(u8::is_ascii_digit) {
            out.push(u8::from_str_radix(&s[i + 1..i + 4], 8).unwrap_or(b'?'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
    // Case-sensitive, like the system's own file systems.
    assert!(fs::metadata(photos.join("相册").join("A.TXT")).is_err());

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
    // `ls -a` shows the usual entries too.
    let mut raw = vec![0u8; 4096];
    let dir = fs::File::open(photos.join("empty")).unwrap();
    assert!(getdents_names(&dir, &mut raw).iter().any(|n| n == ".."));

    assert!(fs::metadata(photos.join("missing.txt")).is_err());
    check_read_only(&photos);
}

/// The names `getdents64` lists, `.` and `..` included.
fn getdents_names(dir: &fs::File, buf: &mut [u8]) -> Vec<String> {
    use std::os::fd::AsRawFd;
    // SAFETY: an open directory and a buffer of the length given.
    let n = unsafe {
        libc::syscall(
            libc::SYS_getdents64,
            dir.as_raw_fd(),
            buf.as_mut_ptr(),
            buf.len(),
        )
    };
    assert!(n >= 0);
    let mut out = Vec::new();
    let mut at = 0;
    while at < n as usize {
        let reclen = u16::from_ne_bytes([buf[at + 16], buf[at + 17]]) as usize;
        let name = &buf[at + 19..at + reclen];
        let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
        out.push(String::from_utf8_lossy(&name[..end]).into_owned());
        at += reclen;
    }
    out
}

/// Every way of changing something is refused with "read-only file system".
fn check_read_only(photos: &Path) {
    let readme = photos.join("docs").join("readme.txt");
    let erofs =
        |r: std::io::Result<()>, what: &str| assert_eq!(errno(r, what), libc::EROFS, "{what}");
    erofs(fs::write(photos.join("new.txt"), "x"), "new file");
    erofs(fs::write(&readme, "x"), "overwrite");
    erofs(
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&readme)
            .map(drop),
        "open for writing",
    );
    erofs(fs::remove_file(&readme), "delete");
    erofs(fs::rename(&readme, photos.join("moved.txt")), "rename");
    erofs(fs::create_dir(photos.join("newdir")), "new directory");
    let mut ro = fs::metadata(&readme).unwrap().permissions();
    ro.set_readonly(true);
    erofs(fs::set_permissions(&readme, ro), "change permissions");
}

#[test]
fn zip_on_the_default_folder() {
    if !ready() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (zip, big) = archive(d.path(), "photos.zip", FormatId::Zip);
    let m = Fuse
        .mount(
            volume(&zip),
            &MountOptions {
                label: Some("photos, 2026".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let point = m.mount_point().to_owned();
    assert!(
        point.starts_with(omnizip_mount_fuse::mount_root()),
        "{point:?}"
    );
    assert!(point.ends_with("photos, 2026"), "{point:?}");
    assert_eq!(fs_type(&point).as_deref(), Some(FS_TYPE));
    assert!(m.is_alive());
    check_contents(&point, &big);

    // The same label again gets a folder of its own.
    let again = Fuse
        .mount(
            volume(&zip),
            &MountOptions {
                label: Some("photos, 2026".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        again.mount_point(),
        point.with_file_name("photos, 2026 (2)")
    );
    drop(again);

    drop(m);
    // The folder made for the mount goes with it.
    assert!(!point.exists());
    assert_eq!(fs_type(&point), None);
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
    let m = Fuse
        .mount(
            volume(&archive),
            &MountOptions {
                mount_point: Some(point.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(m.mount_point(), point);
    check_contents(&point, &big);
    drop(m);
    // A folder of the user's stays, empty.
    assert!(point.is_dir() && names(&point).is_empty());

    // A folder with something in it is refused.
    fs::write(point.join("x"), "x").unwrap();
    let e = Fuse
        .mount(
            volume(&archive),
            &MountOptions {
                mount_point: Some(point.clone()),
                ..Default::default()
            },
        )
        .err()
        .unwrap();
    assert_eq!(e, MountError::InUse(point));
}

/// Unmounting while a file is open still ends the volume; the file fails from then on.
#[test]
fn unmounting_with_files_open() {
    if !ready() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (zip, big) = archive(d.path(), "photos.zip", FormatId::Zip);
    let m = Fuse.mount(volume(&zip), &MountOptions::default()).unwrap();
    let point = m.mount_point().to_owned();
    let mut f = fs::File::open(point.join("photos/media/big.bin")).unwrap();
    let mut head = vec![0; 4096];
    f.read_exact(&mut head).unwrap();
    assert_eq!(head, &big[..4096]);
    let t = Instant::now();
    drop(m);
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    assert_eq!(fs_type(&point), None);
    assert!(!point.exists());
    let mut rest = vec![0; 8 << 20];
    assert!(f.read_exact(&mut rest).is_err());
}

/// A volume unmounted from outside (a file manager's "Unmount") reports itself gone.
#[test]
fn unmounted_from_outside() {
    if !ready() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (zip, _) = archive(d.path(), "photos.zip", FormatId::Zip);
    let m = Fuse.mount(volume(&zip), &MountOptions::default()).unwrap();
    let point = m.mount_point().to_owned();
    assert!(names(&point).contains(&"photos".to_owned()));
    let st = std::process::Command::new("fusermount3")
        .arg("-u")
        .arg(&point)
        .status()
        .unwrap();
    assert!(st.success());
    let deadline = Instant::now() + Duration::from_secs(5);
    while m.is_alive() {
        assert!(Instant::now() < deadline, "still alive");
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(m);
    assert!(!point.exists());
}

#[test]
fn writable_layer_through_the_file_apis() {
    if !ready() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (zip, big) = archive(d.path(), "photos.zip", FormatId::Zip);
    let lower = volume(&zip);
    let overlay = Arc::new(OverlayFs::new(lower.clone(), d.path().join("layer")).unwrap());
    let point = d.path().join("mnt");
    let m = Fuse
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
        fs::read_to_string(photos.join("相册").join("a.txt")).unwrap(),
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
    let when = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    f.set_modified(when).unwrap();
    drop(f);
    let meta = fs::metadata(photos.join("copy.bin")).unwrap();
    assert_eq!((meta.len(), meta.modified().unwrap()), (10, when));
    // Modes are not kept: changing them succeeds without effect.
    let mut ro = meta.permissions();
    ro.set_readonly(true);
    fs::set_permissions(photos.join("copy.bin"), ro).unwrap();

    assert_eq!(
        errno(fs::remove_dir(photos.join("docs")), "not empty"),
        libc::ENOTEMPTY
    );
    assert_eq!(errno(fs::remove_dir(&readme), "a file"), libc::ENOTDIR);
    assert_eq!(
        errno(fs::remove_file(photos.join("docs")), "a folder"),
        libc::EISDIR
    );
    fs::remove_file(&readme).unwrap();
    fs::remove_dir(photos.join("docs")).unwrap();
    fs::remove_dir(photos.join("empty")).unwrap();
    assert!(!readme.exists());
    assert_eq!(
        names(&photos),
        ["copy.bin", "new.txt", "notes", "videos", "相册"]
    );
    // The layer's disk shows as free space.
    assert!(statvfs_free(&point) > 0);

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

fn statvfs_free(p: &Path) -> u64 {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
    // SAFETY: all-zero is a valid statvfs; `c` is NUL-terminated.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::statvfs(c.as_ptr(), &mut st) }, 0);
    st.f_bavail as u64 * st.f_frsize as u64
}
