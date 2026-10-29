//! Mounts an archive until Enter is pressed: `cargo run --example mount -- <archive> [Z:|dir]`.
//! Windows only.

#[cfg(windows)]
fn main() {
    use std::{path::PathBuf, sync::Arc};

    use omnizip_core::{JobCtx, OpenOptions};
    use omnizip_vfs::{ArchiveFs, FsOptions, MountBackend, MountOptions};

    let mut args = std::env::args_os().skip(1);
    let archive = PathBuf::from(args.next().expect("usage: mount <archive> [point]"));
    let reader = omnizip_formats::shared()
        .open(&archive, &OpenOptions::default(), &JobCtx::default())
        .expect("open");
    let fs = Arc::new(ArchiveFs::new(reader, FsOptions::default()).expect("volume"));
    let opts = MountOptions {
        mount_point: args.next().map(PathBuf::from),
        ..Default::default()
    };
    let m = omnizip_mount_winfsp::WinFsp
        .mount(fs, &opts)
        .expect("mount");
    println!("mounted at {}", m.mount_point().display());
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("WinFsp mounts are Windows only");
}
