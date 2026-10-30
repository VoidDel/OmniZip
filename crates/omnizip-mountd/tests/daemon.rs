//! Runs the daemon binary under its own pipe names and directories, and mounts through it as
//! OmniZip does. Skipped when WinFsp is not installed (CI runners).
#![cfg(windows)]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

use omnizip_core::{CollectOptions, ErrorCode, FormatId, JobCtx, WriteOptions, source};
use omnizip_mountd::{
    Level, MountSpec, MountStatus, Request, Response, SavedLayer, client, proto::saving_dir, save,
};

#[test]
fn mounts_through_the_daemon_until_it_is_idle() {
    if !omnizip_vfs::probe(true).can_mount() {
        eprintln!("WinFsp is not installed: skipped");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary; set before any thread reads the environment.
    unsafe {
        std::env::set_var(
            "OMNIZIP_MOUNTD_INSTANCE",
            format!("test-{}", std::process::id()),
        );
        std::env::set_var("OMNIZIP_HOME", d.path().join("home"));
        std::env::set_var("OMNIZIP_MOUNTD_IDLE_SECS", "2");
    }
    let src = d.path().join("docs");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("readme.txt"), "read me").unwrap();
    let zip = d.path().join("docs.zip");
    let ctx = JobCtx::default();
    let sources = source::collect(&[src], &CollectOptions::default(), &ctx).unwrap();
    let wo = WriteOptions {
        format: FormatId::Zip,
        ..Default::default()
    };
    omnizip_formats::shared()
        .create(&zip, &sources, &wo, &ctx)
        .unwrap();

    let exe = Path::new(env!("CARGO_BIN_EXE_omnizip-mountd"));
    assert!(!client::running(Level::User));
    client::ensure_user_daemon(exe).unwrap();
    // Should an assertion fail, the daemon still unmounts and goes idle.
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = client::call(
                Level::User,
                &Request::UnmountAll {
                    keep_changed: false,
                },
            );
        }
    }
    let _cleanup = Cleanup;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = client::watch(Level::User, || {
            let _ = tx.send(());
        });
    });
    // The watcher may connect after the first change: wait until it is there.
    std::thread::sleep(Duration::from_millis(300));

    let spec = MountSpec {
        archive: zip.clone(),
        cache_bytes: 1 << 30,
        ..Default::default()
    };
    let Response::Mounted { mount } =
        client::call(Level::User, &Request::Mount { spec: spec.clone() }).unwrap()
    else {
        panic!("not mounted")
    };
    rx.recv_timeout(Duration::from_secs(5))
        .expect("watchers hear of the mount");
    let root = format!("{}\\", mount.mount_point.display());
    assert_eq!(
        fs::read_to_string(Path::new(&root).join("docs").join("readme.txt")).unwrap(),
        "read me"
    );
    // The same archive again: the same mount.
    let Response::Mounted { mount: again } =
        client::call(Level::User, &Request::Mount { spec }).unwrap()
    else {
        panic!("not mounted")
    };
    assert_eq!(again.id, mount.id);
    let Response::Mounts { mounts } = client::call(Level::User, &Request::List).unwrap() else {
        panic!("no list")
    };
    assert_eq!(mounts.len(), 1);
    assert_eq!(
        (mounts[0].id, &mounts[0].mount_point),
        (mount.id, &mount.mount_point)
    );
    assert_eq!(mounts[0].cache_bytes, 7, "the file read is cached");

    // Errors come back with their codes.
    let missing = MountSpec {
        archive: d.path().join("missing.zip"),
        ..Default::default()
    };
    let e = client::call(Level::User, &Request::Mount { spec: missing }).unwrap_err();
    assert_eq!(e.code(), ErrorCode::NotFound, "{e:?}");
    assert!(!client::running(Level::Admin));

    client::call(
        Level::User,
        &Request::Unmount {
            id: mount.id,
            keep_changes: false,
        },
    )
    .unwrap();
    rx.recv_timeout(Duration::from_secs(5))
        .expect("watchers hear of the unmount");
    assert!(!Path::new(&root).exists());

    writable_mounts(&zip);

    // With no mount left, the daemon exits once its idle time is up.
    let deadline = Instant::now() + Duration::from_secs(20);
    while client::running(Level::User) {
        assert!(Instant::now() < deadline, "the daemon did not exit");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn mount_writable(zip: &Path) -> (MountStatus, PathBuf) {
    let spec = MountSpec {
        archive: zip.to_owned(),
        cache_bytes: 1 << 30,
        writable: true,
        ..Default::default()
    };
    let Response::Mounted { mount } = client::call(Level::User, &Request::Mount { spec }).unwrap()
    else {
        panic!("not mounted")
    };
    assert!(mount.writable && !mount.dirty && mount.saveable);
    let root = PathBuf::from(format!("{}\\", mount.mount_point.display()));
    (mount, root)
}

fn status(id: u64) -> Option<MountStatus> {
    let Response::Mounts { mounts } = client::call(Level::User, &Request::List).unwrap() else {
        panic!("no list")
    };
    mounts.into_iter().find(|m| m.id == id)
}

fn unmount(id: u64, keep_changes: bool) -> Option<SavedLayer> {
    match client::call(Level::User, &Request::Unmount { id, keep_changes }).unwrap() {
        Response::Unmounted { saved } => saved,
        r => panic!("{r:?}"),
    }
}

/// Writes through a writable mount; the changes are kept and saved, or discarded, at unmount.
/// Unmounting everything for OmniZip's exit leaves mounts with changes alone.
fn writable_mounts(zip: &Path) {
    let original = fs::read(zip).unwrap();
    let (mount, root) = mount_writable(zip);
    fs::write(root.join("docs").join("readme.txt"), "changed").unwrap();
    fs::write(root.join("docs").join("new.txt"), "new").unwrap();
    assert!(status(mount.id).unwrap().dirty);
    // The archive is untouched until the changes are saved.
    assert_eq!(fs::read(zip).unwrap(), original);

    let saved = unmount(mount.id, true).expect("changes kept");
    assert!(!root.exists());
    assert!(saved.layer.starts_with(saving_dir().unwrap()));
    assert_eq!(saved.changes.files, ["docs/new.txt", "docs/readme.txt"]);
    save::save(&saved, 0, &JobCtx::default()).unwrap();
    assert!(!saved.layer.exists());
    let (mount, root) = mount_writable(zip);
    assert_eq!(
        fs::read_to_string(root.join("docs").join("readme.txt")).unwrap(),
        "changed"
    );
    assert_eq!(
        fs::read_to_string(root.join("docs").join("new.txt")).unwrap(),
        "new"
    );

    // Discarded: the archive stays as it is.
    let saved_once = fs::read(zip).unwrap();
    fs::remove_file(root.join("docs").join("new.txt")).unwrap();
    assert_eq!(unmount(mount.id, false), None);
    assert_eq!(fs::read(zip).unwrap(), saved_once);

    // Unchanged mounts go; changed ones stay until the user decides.
    let (changed, root) = mount_writable(zip);
    fs::write(root.join("scratch.txt"), "x").unwrap();
    client::call(Level::User, &Request::UnmountAll { keep_changed: true }).unwrap();
    assert!(status(changed.id).is_some_and(|m| m.dirty));
    assert_eq!(unmount(changed.id, false), None);
    assert!(status(changed.id).is_none());
    assert_eq!(fs::read(zip).unwrap(), saved_once);
}
