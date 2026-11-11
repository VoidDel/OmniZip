#![no_main]
//! The read-only formats (`omnizip-engine-readonly`): the first byte picks the parser, the rest
//! is the archive, so each parser is reached without a matching signature.

use libfuzzer_sys::fuzz_target;
use omnizip_core::FormatId;

const FORMATS: [FormatId; 9] = [
    FormatId::Ar,
    FormatId::Deb,
    FormatId::Cpio,
    FormatId::Rpm,
    FormatId::Iso,
    FormatId::Xar,
    FormatId::Cab,
    FormatId::Lzh,
    FormatId::Arj,
];

fuzz_target!(|data: &[u8]| {
    if let Some((&pick, archive)) = data.split_first() {
        let format = FORMATS[usize::from(pick) % FORMATS.len()];
        omnizip_fuzz::open_and_test(archive, Some(format));
    }
});
