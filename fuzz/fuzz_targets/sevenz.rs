#![no_main]
//! 7z listing and decoding.

use libfuzzer_sys::fuzz_target;
use omnizip_core::FormatId;

fuzz_target!(|data: &[u8]| omnizip_fuzz::open_and_test(data, Some(FormatId::SevenZ)));
