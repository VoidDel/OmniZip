#![no_main]
//! File-name encoding detection and decoding of arbitrary bytes.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| omnizip_fuzz::encoding_props(data));
