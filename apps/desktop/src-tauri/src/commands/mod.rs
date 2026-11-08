//! IPC commands (`docs/02-architecture.md` §5). Every command returns `ErrorReport` on failure so
//! the UI can localize the message from its code.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
};

use base64::Engine as _;
use omnizip_core::{
    Capabilities, EntryKind, Error, ErrorCode, ErrorReport, FormatId, FormatSupport, NodeId,
    SortKey, UpdateOp,
    encoding::{self, NameEncoding},
    entry::to_millis,
};
use omnizip_formats::shared;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use crate::{
    clipboard,
    draft::Draft,
    dragout,
    integration::{self, ShellStatus},
    jobs::{Finish, JobInfo, JobSpec, JobState, PromptAnswer},
    launch::LaunchRequest,
    mounts::{self, MountInfo, MountRequest},
    ops::{self, CompressRequest, ConvertRequest, ExtractRequest},
    preview::{self, Preview},
    settings::{self, Settings},
    state::{AppState, OpenedArchive, RecentItem},
    system::{self, SystemInfo},
    theme::{self, ResolveOptions, ResolvedTheme, ThemeSummary},
    windows,
};

mod app;
mod archive;
mod files;
mod jobs;
mod mount;

pub use self::{app::*, archive::*, files::*, jobs::*, mount::*};

type CmdResult<T> = Result<T, ErrorReport>;

fn err(e: Error) -> ErrorReport {
    e.to_report()
}

fn archive(state: &AppState, id: u32) -> CmdResult<Arc<OpenedArchive>> {
    state
        .archive(id)
        .ok_or_else(|| err(Error::invalid_argument(format!("archive {id} is not open"))))
}

fn threads(state: &AppState) -> u32 {
    state.settings().advanced.threads
}

fn policy(state: &AppState) -> omnizip_core::SecurityPolicy {
    omnizip_core::SecurityPolicy {
        max_ratio: state.settings().security.max_ratio.max(10),
        ..Default::default()
    }
}
