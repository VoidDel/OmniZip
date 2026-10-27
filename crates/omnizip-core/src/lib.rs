//! OmniZip core library.
//!
//! UI-free building blocks shared by the desktop app, the `omnizip` CLI, the shell integration
//! and the mount daemon:
//!
//! * [`FormatId`], [`Capabilities`] and detection by content ([`detect`]);
//! * the [`Engine`] / [`ArchiveReader`] traits that format engines implement, and the
//!   [`FormatRegistry`] that wires them together;
//! * safe extraction ([`sink::FsSink`]): path sanitization, conflict handling, link checks,
//!   decompression-bomb limits, origin-mark propagation;
//! * file-name encoding repair for legacy archives ([`encoding`]);
//! * the job context: cancellation, throttled progress and UI callbacks ([`JobCtx`]).
//!
//! The core never produces user-facing text: errors are [`ErrorCode`]s with parameters.

pub mod appdirs;
pub mod archive;
pub mod detect;
pub mod encoding;
pub mod entry;
pub mod error;
pub mod format;
pub mod job;
pub mod options;
pub mod origin;
pub mod path;
pub mod policy;
pub mod registry;
pub mod sink;
pub mod source;
pub mod timeutil;
pub mod tree;
pub mod volume;

pub use archive::{ArchiveInfo, ArchiveReader, Engine, extract, read_entry, test};
pub use entry::{Entry, EntryId, EntryKind};
pub use error::{Error, ErrorCode, ErrorReport, Result};
pub use format::{Capabilities, Codec, FormatId, MountClass, Tier};
pub use job::{
    CancelToken, Confirm, Conflict, ConflictChoice, ConflictDecision, FixedPassword, Interaction,
    JobCtx, MAX_PASSWORD_ATTEMPTS, NoInteraction, NullProgress, PasswordRequest, Phase, Progress,
    ProgressSink,
};
pub use options::{
    ExtractOptions, Level, OpenOptions, OverwriteMode, PathMode, SymlinkPolicy, UpdateOp,
    WriteOptions,
};
pub use policy::SecurityPolicy;
pub use registry::{FormatRegistry, FormatSupport};
pub use sink::{EntryData, ExtractSink, Report, Selection, Warning};
pub use source::{CollectOptions, Source, SourceKind};
pub use tree::{NodeId, SortKey, TreeIndex};

/// Version of the core library.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
