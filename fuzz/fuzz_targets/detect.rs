#![no_main]
//! Format detection and every engine, chosen by content.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| omnizip_fuzz::open_and_test(data, None));
