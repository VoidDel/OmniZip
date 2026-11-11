//! Readers for the formats OmniZip only reads (`docs/03-formats.md` §3), in pure Rust: no C
//! parser runs on archive structure in the process (§5, rule 1).

use std::path::Path;

use omnizip_core::{
    ArchiveReader, Capabilities, Engine, Error, ErrorCode, FormatId, JobCtx, OpenOptions, Result,
};

mod split;

const FORMATS: &[FormatId] = &[FormatId::Split];

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
        _opts: &OpenOptions,
        _ctx: &JobCtx,
    ) -> Result<Box<dyn ArchiveReader>> {
        match format {
            FormatId::Split => Ok(Box::new(split::SplitReader::open(path)?)),
            other => Err(Error::new(ErrorCode::UnsupportedFormat).with("format", other)),
        }
    }
}
