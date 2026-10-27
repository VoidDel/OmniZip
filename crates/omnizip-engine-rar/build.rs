//! The bundled UnRAR C++ sources call registry, token and CryptoAPI functions from advapi32,
//! which `unrar_sys` does not declare. Link it explicitly on Windows.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-lib=advapi32");
    }
}
