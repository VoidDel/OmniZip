//! Application state shared by all commands.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, TryLockError,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

use omnizip_core::{
    ArchiveInfo, ArchiveReader, EntryKind, Error, JobCtx, TreeIndex, appdirs,
    encoding::NameEncoding,
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
    /// One rewrite of an archive file at a time (`ops::rewrite_locked`).
    pub archive_locks: ArchiveLocks,
}

/// A lock per archive file, for the jobs that rewrite it (add, delete, rename, saving a mount's
/// changes). Two at once would each start from the original archive, and the one finishing
/// last would replace the other's result: its changes would be lost.
#[derive(Default)]
pub struct ArchiveLocks(Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>);

impl ArchiveLocks {
    /// Runs `f` once no other rewrite of `path` is running. Cancelling the job ends the wait.
    pub fn run<T>(&self, path: &Path, ctx: &JobCtx, f: impl FnOnce() -> T) -> Result<T, Error> {
        // The same file under another spelling (Windows paths ignore case).
        let key = if cfg!(windows) {
            PathBuf::from(path.to_string_lossy().replace('/', "\\").to_lowercase())
        } else {
            path.to_owned()
        };
        let lock = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key)
            .or_default()
            .clone();
        let _guard = loop {
            match lock.try_lock() {
                Ok(g) => break g,
                Err(TryLockError::Poisoned(e)) => break e.into_inner(),
                Err(TryLockError::WouldBlock) => {
                    ctx.check()?;
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        };
        Ok(f())
    }
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
            archive_locks: ArchiveLocks::default(),
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

#[cfg(test)]
mod tests {
    use std::sync::{atomic::AtomicU32, mpsc};

    use omnizip_core::{
        CancelToken, CollectOptions, ErrorCode, FormatId, OpenOptions, UpdateOp, WriteOptions,
        source,
    };
    use omnizip_formats::shared;

    use super::*;

    /// An add and a rename started together both end up in the archive.
    #[test]
    fn rewrites_of_one_archive_take_turns() {
        let d = tempfile::tempdir().unwrap();
        let ctx = JobCtx::default();
        let collect = |name: &str| {
            let p = d.path().join(name);
            std::fs::write(&p, name).unwrap();
            source::collect(&[p], &CollectOptions::default(), &ctx).unwrap()
        };
        let zip = d.path().join("t.zip");
        let wo = WriteOptions {
            format: FormatId::Zip,
            ..Default::default()
        };
        shared().create(&zip, &collect("a.txt"), &wo, &ctx).unwrap();
        let edits = [
            vec![UpdateOp::Add {
                sources: collect("b.txt"),
            }],
            vec![UpdateOp::Rename {
                from: "a.txt".into(),
                to: "c.txt".into(),
            }],
        ];
        let (locks, inside) = (ArchiveLocks::default(), AtomicU32::new(0));
        std::thread::scope(|s| {
            for ops in edits {
                let (locks, inside, zip) = (&locks, &inside, &zip);
                s.spawn(move || {
                    let ctx = JobCtx::default();
                    locks
                        .run(zip, &ctx, || {
                            assert_eq!(inside.fetch_add(1, Ordering::SeqCst), 0, "alone");
                            std::thread::sleep(Duration::from_millis(50));
                            let r = shared().update(zip, &ops, &WriteOptions::default(), &ctx);
                            inside.fetch_sub(1, Ordering::SeqCst);
                            r
                        })
                        .unwrap()
                        .unwrap();
                });
            }
        });
        let r = shared().open(&zip, &OpenOptions::default(), &ctx).unwrap();
        let mut names: Vec<_> = r.entries().iter().map(|e| e.path.clone()).collect();
        names.sort();
        assert_eq!(names, ["b.txt", "c.txt"]);
    }

    /// A job waiting for its turn can be cancelled.
    #[test]
    fn waiting_for_a_turn_can_be_cancelled() {
        let locks = ArchiveLocks::default();
        let path = Path::new("x.zip");
        let (entered, release) = (mpsc::channel(), mpsc::channel::<()>());
        std::thread::scope(|s| {
            let locks = &locks;
            s.spawn(move || {
                locks
                    .run(path, &JobCtx::default(), || {
                        entered.0.send(()).unwrap();
                        release.1.recv().unwrap();
                    })
                    .unwrap();
            });
            entered.1.recv().unwrap();
            let cancel = CancelToken::new();
            cancel.cancel();
            let ctx = JobCtx::default().with_cancel(cancel);
            let e = locks.run(path, &ctx, || ()).unwrap_err();
            assert_eq!(e.code(), ErrorCode::Cancelled);
            release.0.send(()).unwrap();
        });
    }
}
