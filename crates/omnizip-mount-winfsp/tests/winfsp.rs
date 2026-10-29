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
use omnizip_vfs::{ArchiveFs, FsOptions, Mount, MountBackend, MountError, MountOptions};

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
    assert!(meta.permissions().readonly());
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
    assert!(fs::write(photos.join("new.txt"), "x").is_err());
    assert!(fs::remove_file(photos.join("docs").join("readme.txt")).is_err());
    assert!(fs::create_dir(photos.join("newdir")).is_err());
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
