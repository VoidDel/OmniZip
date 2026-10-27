//! The registry maps formats to engines and is the single entry point front ends use.

use std::{path::Path, sync::Arc};

use serde::Serialize;

use crate::{
    Capabilities, Error, ErrorCode, FormatId, JobCtx, Result,
    archive::{ArchiveReader, Engine},
    detect::{Detection, detect_path},
    format::Tier,
    options::{OpenOptions, UpdateOp, WriteOptions},
    sink::Report,
    source::Source,
};

/// What this build can do with one format.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatSupport {
    pub format: FormatId,
    pub label: String,
    pub engine: Option<&'static str>,
    pub capabilities: Vec<&'static str>,
    #[serde(skip)]
    pub caps: Capabilities,
    pub tier: u8,
    pub extensions: Vec<&'static str>,
}

/// Registry of engines.
#[derive(Default, Clone)]
pub struct FormatRegistry {
    engines: Vec<Arc<dyn Engine>>,
}

impl std::fmt::Debug for FormatRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.engines.iter().map(|e| e.name()))
            .finish()
    }
}

impl FormatRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an engine. Later registrations win for formats handled twice.
    pub fn register(&mut self, engine: Arc<dyn Engine>) {
        self.engines.insert(0, engine);
    }

    /// The engine for a format.
    pub fn engine(&self, format: FormatId) -> Option<&Arc<dyn Engine>> {
        self.engines.iter().find(|e| e.formats().contains(&format))
    }

    /// Capabilities of a format in this build.
    pub fn capabilities(&self, format: FormatId) -> Capabilities {
        self.engine(format)
            .map_or(Capabilities::empty(), |e| e.capabilities(format))
    }

    /// Detects the format of a file, letting the engine refine compound formats.
    pub fn detect(&self, path: &Path) -> Result<Detection> {
        let mut d = detect_path(path)?;
        if let Some(e) = self.engine(d.format) {
            d.format = e.refine(path, d.format);
        }
        Ok(d)
    }

    /// Detects and opens an archive.
    pub fn open(
        &self,
        path: &Path,
        opts: &OpenOptions,
        ctx: &JobCtx,
    ) -> Result<Box<dyn ArchiveReader>> {
        let d = self.detect(path)?;
        self.open_as(path, d.format, opts, ctx)
    }

    /// Opens an archive as a specific format.
    pub fn open_as(
        &self,
        path: &Path,
        format: FormatId,
        opts: &OpenOptions,
        ctx: &JobCtx,
    ) -> Result<Box<dyn ArchiveReader>> {
        let engine = self.engine(format).ok_or_else(|| {
            Error::new(ErrorCode::UnsupportedFormat)
                .with("format", format)
                .with("path", path.display())
        })?;
        engine.open(path, format, opts, ctx)
    }

    /// Creates an archive.
    pub fn create(
        &self,
        dest: &Path,
        sources: &[Source],
        opts: &WriteOptions,
        ctx: &JobCtx,
    ) -> Result<Report> {
        let engine = self
            .engine(opts.format)
            .filter(|e| e.capabilities(opts.format).contains(Capabilities::WRITE));
        let engine = engine.ok_or_else(|| {
            Error::new(ErrorCode::UnsupportedOperation)
                .with("format", opts.format)
                .with("operation", "create")
        })?;
        if opts.password.is_some()
            && !engine
                .capabilities(opts.format)
                .contains(Capabilities::ENCRYPT)
        {
            return Err(Error::new(ErrorCode::UnsupportedOperation)
                .with("format", opts.format)
                .with("operation", "encrypt"));
        }
        if sources.is_empty() {
            return Err(Error::invalid_argument("nothing to compress"));
        }
        engine.create(dest, sources, opts, ctx)
    }

    /// Updates an archive in place.
    pub fn update(
        &self,
        path: &Path,
        ops: &[UpdateOp],
        opts: &WriteOptions,
        ctx: &JobCtx,
    ) -> Result<Report> {
        let d = self.detect(path)?;
        let engine = self
            .engine(d.format)
            .filter(|e| e.capabilities(d.format).contains(Capabilities::UPDATE))
            .ok_or_else(|| {
                Error::new(ErrorCode::UnsupportedOperation)
                    .with("format", d.format)
                    .with("operation", "update")
            })?;
        let opts = WriteOptions {
            format: d.format,
            ..opts.clone()
        };
        engine.update(path, d.format, ops, &opts, ctx)
    }

    /// Support table for every known format.
    pub fn support(&self) -> Vec<FormatSupport> {
        FormatId::all()
            .iter()
            .map(|&f| {
                let engine = self.engine(f);
                let caps = engine.map_or(Capabilities::empty(), |e| e.capabilities(f));
                FormatSupport {
                    format: f,
                    label: f.label(),
                    engine: engine.map(|e| e.name()),
                    capabilities: caps.names(),
                    caps,
                    tier: match f.tier() {
                        Tier::One => 1,
                        Tier::Two => 2,
                        Tier::Three => 3,
                    },
                    extensions: f.extensions().to_vec(),
                }
            })
            .collect()
    }
}
