//! Application state shared by all commands.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};

use omnizip_core::{
    ArchiveInfo, ArchiveReader, EntryKind, TreeIndex, appdirs, encoding::NameEncoding,
};
use serde::{Deserialize, Serialize};

use crate::{
    draft::Draft, jobs::JobManager, launch::LaunchRequest, mounts::MountManager,
    settings::Settings, theme::ThemeFile,
};

/// An archive open in the browser.
pub struct OpenedArchive {
    pub id: u32,
    pub path: PathBuf,
    pub info: ArchiveInfo,
    pub tree: TreeIndex,
    /// Kind of every entry, so listing never waits for the reader lock.
    pub kinds: Vec<EntryKind>,
    /// Locked by jobs and previews; listing only needs `tree`.
    pub reader: Mutex<Box<dyn ArchiveReader>>,
    pub password: Mutex<Option<String>>,
    pub encoding: Option<NameEncoding>,
    /// A new archive not saved yet (`crate::draft`): edits change memory, not a file.
    pub draft: bool,
}

/// A recently opened archive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecentItem {
    pub path: String,
    pub name: String,
    pub format: String,
    pub size: u64,
    pub opened: i64,
}

pub struct AppState {
    pub archives: Mutex<HashMap<u32, Arc<OpenedArchive>>>,
    /// New archives being edited, by archive id (also in `archives`, for browsing).
    pub drafts: Mutex<HashMap<u32, Draft>>,
    next_archive: AtomicU32,
    pub jobs: JobManager,
    pub settings: Mutex<Settings>,
    pub themes: Mutex<Vec<(ThemeFile, bool)>>,
    pub launch: Mutex<Option<LaunchRequest>>,
    /// Whether the main window listens for launch requests (it has called `app_take_launch`);
    /// until then a request waits in `launch`. Changed only while `launch` is locked.
    pub launch_ready: AtomicBool,
    /// Requests of standalone dialog windows (`shell-<n>`), by window label.
    pub shell_requests: Mutex<HashMap<String, LaunchRequest>>,
    next_shell: AtomicU32,
    pub recent: Mutex<Vec<RecentItem>>,
    pub config_file: Option<PathBuf>,
    pub themes_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
    /// Items copied or cut from an open archive (`crate::clipboard`).
    pub clip: Mutex<Option<crate::clipboard::Clip>>,
    pub mounts: MountManager,
}

impl AppState {
    pub fn new() -> Self {
        let config_file = appdirs::config_file();
        let themes_dir = appdirs::config_dir().map(|d| d.join("themes"));
        let data_dir = appdirs::data_dir();
        let settings = config_file
            .as_deref()
            .map(crate::settings::load)
            .unwrap_or_default();
        let themes = crate::theme::load_all(themes_dir.as_deref());
        let recent = data_dir
            .as_ref()
            .and_then(|d| std::fs::read_to_string(d.join("recent.json")).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self {
            archives: Mutex::new(HashMap::new()),
            drafts: Mutex::new(HashMap::new()),
            next_archive: AtomicU32::new(1),
            jobs: JobManager::default(),
            settings: Mutex::new(settings),
            themes: Mutex::new(themes),
            launch: Mutex::new(None),
            launch_ready: AtomicBool::new(false),
            shell_requests: Mutex::new(HashMap::new()),
            next_shell: AtomicU32::new(1),
            recent: Mutex::new(recent),
            config_file,
            themes_dir,
            data_dir,
            cache_dir: appdirs::cache_dir(),
            clip: Mutex::new(None),
            mounts: MountManager::default(),
        }
    }

    /// Label for a new standalone dialog window.
    pub fn next_shell_label(&self) -> String {
        format!("shell-{}", self.next_shell.fetch_add(1, Ordering::Relaxed))
    }

    pub fn next_archive_id(&self) -> u32 {
        self.next_archive.fetch_add(1, Ordering::Relaxed)
    }

    pub fn archive(&self, id: u32) -> Option<Arc<OpenedArchive>> {
        self.archives
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned()
    }

    pub fn settings(&self) -> Settings {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Records an opened archive in the recent list (most recent first, at most 20).
    pub fn touch_recent(&self, item: RecentItem) {
        let mut r = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        r.retain(|x| !x.path.eq_ignore_ascii_case(&item.path));
        r.insert(0, item);
        r.truncate(20);
        self.save_recent(&r);
    }

    pub fn save_recent(&self, r: &[RecentItem]) {
        if let Some(d) = &self.data_dir {
            let _ = std::fs::create_dir_all(d);
            if let Ok(text) = serde_json::to_string_pretty(r) {
                let _ = std::fs::write(d.join("recent.json"), text);
            }
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
