#![no_main]
//! Mounting: names, lookups, cache and reads over arbitrary archives.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| omnizip_fuzz::mount_and_walk(data));
