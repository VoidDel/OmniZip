//! Runs the daemon binary under its own pipe names and directories, and mounts through it as
//! OmniZip does. Skipped when WinFsp is not installed (CI runners).
#![cfg(windows)]

use std::{
    fs,
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

use omnizip_core::{CollectOptions, ErrorCode, FormatId, JobCtx, WriteOptions, source};
use omnizip_mountd::{Level, MountSpec, Request, Response, SavedMount, client, saved_list_path};

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
            let _ = client::call(Level::User, &Request::UnmountAll);
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
    let saved: Vec<SavedMount> =
        serde_json::from_str(&fs::read_to_string(saved_list_path().unwrap()).unwrap()).unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].mount_point, mount.mount_point.to_string_lossy());

    // Errors come back with their codes.
    let missing = MountSpec {
        archive: d.path().join("missing.zip"),
        ..Default::default()
    };
    let e = client::call(Level::User, &Request::Mount { spec: missing }).unwrap_err();
    assert_eq!(e.code(), ErrorCode::NotFound, "{e:?}");
    assert!(!client::running(Level::Admin));

    client::call(Level::User, &Request::Unmount { id: mount.id }).unwrap();
    rx.recv_timeout(Duration::from_secs(5))
        .expect("watchers hear of the unmount");
    assert!(!Path::new(&root).exists());

    // With no mount left, the daemon exits once its idle time is up.
    let deadline = Instant::now() + Duration::from_secs(20);
    while client::running(Level::User) {
        assert!(Instant::now() < deadline, "the daemon did not exit");
        std::thread::sleep(Duration::from_millis(200));
    }
}
