#![no_main]
//! Path sanitization: never yields `.`, `..` or empty components, and is idempotent.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| omnizip_fuzz::sanitize_props(data));
