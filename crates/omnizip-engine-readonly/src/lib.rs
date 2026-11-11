//! Readers for the formats OmniZip only reads (`docs/03-formats.md` §3), in pure Rust: no C
//! parser runs on archive structure in the process (§5, rule 1).

use std::path::Path;

use omnizip_core::{
    ArchiveReader, Capabilities, Engine, Error, ErrorCode, FormatId, JobCtx, OpenOptions, Result,
};

mod ar;
mod arj;
mod cab;
mod common;
mod cpio;
mod iso;
mod lha;
mod lzh;
mod rpm;
mod split;
#[cfg(test)]
mod tests;
mod xar;

const FORMATS: &[FormatId] = &[
    FormatId::Ar,
    FormatId::Deb,
    FormatId::Cpio,
    FormatId::Rpm,
    FormatId::Iso,
    FormatId::Xar,
    FormatId::Cab,
    FormatId::Lzh,
    FormatId::Arj,
    FormatId::Split,
];

/// The engine for every read-only format of this crate.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReadonlyEngine;

impl Engine for ReadonlyEngine {
    fn name(&self) -> &'static str {
        "omnizip-readonly"
    }

    fn formats(&self) -> &'static [FormatId] {
        FORMATS
    }

    fn capabilities(&self, format: FormatId) -> Capabilities {
        let c = Capabilities::READ | Capabilities::MOUNT;
        match format {
            FormatId::Split => c | Capabilities::MULTI_VOLUME | Capabilities::SINGLE_FILE,
            _ => c,
        }
    }

    fn open(
        &self,
        path: &Path,
        format: FormatId,
        opts: &OpenOptions,
        ctx: &JobCtx,
    ) -> Result<Box<dyn ArchiveReader>> {
        match format {
            FormatId::Ar => Ok(Box::new(ar::open_ar(path, opts, ctx)?)),
            FormatId::Deb => Ok(Box::new(ar::open_deb(path, opts, ctx)?)),
            FormatId::Cpio => Ok(Box::new(cpio::open(path, opts, ctx)?)),
            FormatId::Rpm => Ok(Box::new(rpm::open(path, opts, ctx)?)),
            FormatId::Iso => Ok(Box::new(iso::open(path, opts, ctx)?)),
            FormatId::Xar => Ok(Box::new(xar::open(path, opts, ctx)?)),
            FormatId::Cab => Ok(Box::new(cab::open(path, opts, ctx)?)),
            FormatId::Lzh => Ok(Box::new(lzh::open(path, opts, ctx)?)),
            FormatId::Arj => Ok(Box::new(arj::open(path, opts, ctx)?)),
            FormatId::Split => Ok(Box::new(split::SplitReader::open(path)?)),
            other => Err(Error::new(ErrorCode::UnsupportedFormat).with("format", other)),
        }
    }
}
