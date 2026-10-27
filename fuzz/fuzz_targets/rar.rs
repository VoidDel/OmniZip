#![no_main]
//! RAR listing and decoding (unrar).

use libfuzzer_sys::fuzz_target;
use omnizip_core::FormatId;

fuzz_target!(|data: &[u8]| omnizip_fuzz::open_and_test(data, Some(FormatId::Rar)));
