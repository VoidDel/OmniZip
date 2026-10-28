use std::{env, fs, path::PathBuf};

fn main() {
    embed_test_cert();
    tauri_build::build()
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
