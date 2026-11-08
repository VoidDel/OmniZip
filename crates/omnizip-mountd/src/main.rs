//! `omnizip-mountd`: see the library documentation and `daemon`.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        std::process::exit(omnizip_mountd::daemon::run(&args));
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        eprintln!("omnizip-mountd: no mount backend on this platform yet");
        std::process::exit(2);
    }
}
