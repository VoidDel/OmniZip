use std::{env, fs, path::PathBuf};

fn main() {
    // tauri-build embeds the icon as the executable's icon resource (which the windows' taskbar
    // icons are loaded from, src/win_icon.rs) but does not ask to rerun when it changes.
    println!("cargo:rerun-if-changed=icons/icon.ico");
    embed_test_cert();
    mark_localizations();
    tauri_build::build()
}

/// The interface languages (locales/), as macOS names them.
const LANGUAGES: [&str; 4] = ["en", "zh-Hans", "zh-Hant", "ja"];

/// Development builds on macOS run without an app bundle: the system takes the folder of the
/// executable for one and its `<language>.lproj` folders for the languages the app has. Without
/// any, it draws its own parts of the app (the open and save panels, the application menu) in
/// English whatever the system language. So empty ones go next to the executable. Bundles
/// declare the languages in their Info.plist instead (platform/macos/Info.plist).
fn mark_localizations() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    // OUT_DIR is <target>/[<triple>/]<profile>/build/<package>-<hash>/out.
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let Some(bin_dir) = out.ancestors().nth(3) else {
        return;
    };
    for lang in LANGUAGES {
        let _ = fs::create_dir_all(bin_dir.join(format!("{lang}.lproj")));
    }
}

/// Builds in the project's test certificate (platform/windows/msix/omnizip-test.cer, or
/// `OMNIZIP_TEST_CERT`), the public half of the key scripts/prepare-bundle.mjs signs the
/// context-menu package with until there is a real certificate, so that registering the package
/// can make the computer trust it (src/integration.rs). Builds signed with a real certificate
/// (`OMNIZIP_SIGN_PFX`), which Windows trusts already, get an empty file.
fn embed_test_cert() {
    println!("cargo:rerun-if-env-changed=OMNIZIP_SIGN_PFX");
    println!("cargo:rerun-if-env-changed=OMNIZIP_TEST_CERT");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("menu-test-cert.cer");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let cert = env::var_os("OMNIZIP_TEST_CERT")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../../platform/windows/msix/omnizip-test.cer"));
    let cert = Some(cert).filter(|p| env::var_os("OMNIZIP_SIGN_PFX").is_none() && p.is_file());
    match cert {
        Some(p) => {
            // Only an existing file is watched: a missing one would rebuild every time.
            println!("cargo:rerun-if-changed={}", p.display());
            fs::copy(&p, &out).expect("copy the test certificate");
        }
        None => fs::write(&out, []).expect("write the empty certificate"),
    }
}
