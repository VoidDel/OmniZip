//! Background jobs (`docs/02-architecture.md` §4.4): each job runs on its own thread with a
//! [`JobCtx`] whose progress and interaction callbacks are bridged to the UI through events:
//!
//! * `job://update` — a [`JobInfo`] snapshot (throttled to ~10 per second per job);
//! * `job://prompt` — the job needs a password, a conflict decision or a confirmation; the
//!   worker blocks until the UI calls `job_answer` (or the job is cancelled).

use std::{
    collections::HashMap,
    panic::AssertUnwindSafe,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime},
};

use omnizip_core::{
    CancelToken, Confirm, Conflict, ConflictChoice, ConflictDecision, Error, ErrorCode,
    ErrorReport, Interaction, JobCtx, PasswordRequest, Phase, Progress, ProgressSink, Report,
    SecurityPolicy, entry::to_millis,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Running,
    Paused,
    Done,
    Failed,
    Cancelled,
}

/// Snapshot sent to the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    pub id: u32,
    /// `extract`, `compress`, `test`, `add`, `delete`, `rename`.
    pub kind: String,
    /// Usually the archive name.
    pub title: String,
    pub state: JobState,
    pub progress: Progress,
    /// Bytes per second (smoothed).
    pub speed: f64,
    /// Seconds left, when known.
    pub eta: Option<f64>,
    pub started: i64,
    pub finished: Option<i64>,
    pub report: Option<Report>,
    pub error: Option<ErrorReport>,
    /// Folder or archive produced by the job.
    pub output: Option<String>,
    /// Label of the window that owns the job (mini progress windows).
    pub window: Option<String>,
    /// Archive id to refresh when an update job finishes.
    pub archive_id: Option<u32>,
}

/// Answer to a prompt.
#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PromptAnswer {
    Password {
        value: Option<String>,
    },
    Conflict {
        choice: ConflictChoice,
        apply_to_all: bool,
    },
    Confirm {
        yes: bool,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptEvent {
    job_id: u32,
    /// Window that should show the prompt (`None` = main window).
    window: Option<String>,
    prompt_id: u32,
    kind: &'static str,
    data: serde_json::Value,
}

/// One job.
pub struct Job {
    info: Mutex<JobInfo>,
    cancel: CancelToken,
    paused: AtomicBool,
    pending: Mutex<HashMap<u32, mpsc::Sender<PromptAnswer>>>,
    /// Unanswered prompts, so a window that opens late can still show them.
    open_prompts: Mutex<Vec<PromptEvent>>,
    next_prompt: AtomicU32,
}

impl Job {
    pub fn snapshot(&self) -> JobInfo {
        self.info.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// All jobs of the process.
#[derive(Default)]
pub struct JobManager {
    jobs: Mutex<HashMap<u32, Arc<Job>>>,
    next: AtomicU32,
}

/// How a job is described when it starts.
pub struct JobSpec {
    /// `extract`, `compress`, `test`, `add`, `delete`, `rename`.
    pub kind: &'static str,
    pub title: String,
    /// Owning window; `"mini"` becomes `mini-<id>`.
    pub window: Option<String>,
    pub archive_id: Option<u32>,
    pub policy: SecurityPolicy,
    /// Answer to the first password prompt (typed in the dialog that started the job).
    pub password: Option<String>,
}

impl JobSpec {
    pub fn new(kind: &'static str, title: String, policy: SecurityPolicy) -> Self {
        Self {
            kind,
            title,
            window: None,
            archive_id: None,
            policy,
            password: None,
        }
    }
}

/// Work done by a job: returns the report and the produced output path.
pub type Work =
    Box<dyn FnOnce(&JobCtx) -> Result<(Report, Option<String>), Error> + Send + 'static>;

/// Called on the job thread after the work finished (successfully or not).
pub type Finish = Box<dyn FnOnce(&AppHandle, &JobInfo) + Send + 'static>;

fn now_ms() -> i64 {
    to_millis(SystemTime::now())
}

fn emit(app: &AppHandle, info: &JobInfo) {
    let _ = app.emit("job://update", info);
    crate::tray::jobs_changed(app);
}

fn set_taskbar(app: &AppHandle, info: &JobInfo) {
    use tauri::window::{ProgressBarState, ProgressBarStatus};
    let label = info.window.clone().unwrap_or_else(|| "main".to_owned());
    let Some(w) = app.get_webview_window(&label) else {
        return;
    };
    let state = match info.state {
        JobState::Running => ProgressBarState {
            status: Some(ProgressBarStatus::Normal),
            progress: Some((info.progress.fraction() * 100.0).round() as u64),
        },
        JobState::Paused => ProgressBarState {
            status: Some(ProgressBarStatus::Paused),
            progress: Some((info.progress.fraction() * 100.0).round() as u64),
        },
        JobState::Failed => ProgressBarState {
            status: Some(ProgressBarStatus::Error),
            progress: Some(100),
        },
        _ => ProgressBarState {
            status: Some(ProgressBarStatus::None),
            progress: None,
        },
    };
    let _ = w.set_progress_bar(state);
}

impl JobManager {
    /// Starts a job on a new thread and returns its id.
    pub fn spawn(&self, app: &AppHandle, spec: JobSpec, work: Work, finish: Option<Finish>) -> u32 {
        let JobSpec {
            kind,
            title,
            window,
            archive_id,
            policy,
            password,
        } = spec;
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let window = window.map(|w| if w == "mini" { format!("mini-{id}") } else { w });
        let job = Arc::new(Job {
            info: Mutex::new(JobInfo {
                id,
                kind: kind.to_owned(),
                title,
                state: JobState::Running,
                progress: Progress::default(),
                speed: 0.0,
                eta: None,
                started: now_ms(),
                finished: None,
                report: None,
                error: None,
                output: None,
                window,
                archive_id,
            }),
            cancel: CancelToken::new(),
            paused: AtomicBool::new(false),
            pending: Mutex::new(HashMap::new()),
            open_prompts: Mutex::new(Vec::new()),
            next_prompt: AtomicU32::new(1),
        });
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, job.clone());
        emit(app, &job.snapshot());

        let app = app.clone();
        std::thread::Builder::new()
            .name(format!("omnizip-job-{id}"))
            .spawn(move || {
                let sink = Arc::new(JobProgress {
                    app: app.clone(),
                    job: job.clone(),
                    state: Mutex::new(SpeedState {
                        last: None,
                        last_bytes: 0,
                        speed: 0.0,
                        last_emit: None,
                    }),
                });
                let interaction = Arc::new(JobInteraction {
                    app: app.clone(),
                    job: job.clone(),
                    preset: Mutex::new(password.filter(|p| !p.is_empty())),
                });
                let ctx = JobCtx::new(sink, interaction)
                    .with_cancel(job.cancel.clone())
                    .with_policy(policy);
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| work(&ctx)))
                    .unwrap_or_else(|_| Err(Error::internal("job panicked")));
                let snapshot = {
                    let mut info = job.info.lock().unwrap_or_else(|e| e.into_inner());
                    info.finished = Some(now_ms());
                    info.eta = None;
                    info.progress = ctx.snapshot();
                    match result {
                        Ok((report, output)) => {
                            info.state = JobState::Done;
                            info.report = Some(report);
                            info.output = output;
                        }
                        Err(e) if e.code() == ErrorCode::Cancelled => {
                            info.state = JobState::Cancelled
                        }
                        Err(e) => {
                            info.state = JobState::Failed;
                            info.error = Some(e.to_report());
                        }
                    }
                    info.clone()
                };
                emit(&app, &snapshot);
                set_taskbar(&app, &snapshot);
                if let Some(f) = finish {
                    f(&app, &snapshot);
                }
            })
            .expect("spawn job thread");
        id
    }

    fn get(&self, id: u32) -> Option<Arc<Job>> {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned()
    }

    /// Requests cancellation (also releases a paused or prompting job).
    pub fn cancel(&self, id: u32) {
        if let Some(j) = self.get(id) {
            j.cancel.cancel();
            j.paused.store(false, Ordering::Relaxed);
            j.pending.lock().unwrap_or_else(|e| e.into_inner()).clear();
            j.open_prompts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }
    }

    /// Pauses at the next progress checkpoint.
    pub fn pause(&self, app: &AppHandle, id: u32) {
        if let Some(j) = self.get(id) {
            j.paused.store(true, Ordering::Relaxed);
            let snap = {
                let mut i = j.info.lock().unwrap_or_else(|e| e.into_inner());
                if i.state == JobState::Running {
                    i.state = JobState::Paused;
                }
                i.clone()
            };
            emit(app, &snap);
            set_taskbar(app, &snap);
        }
    }

    pub fn resume(&self, app: &AppHandle, id: u32) {
        if let Some(j) = self.get(id) {
            j.paused.store(false, Ordering::Relaxed);
            let snap = {
                let mut i = j.info.lock().unwrap_or_else(|e| e.into_inner());
                if i.state == JobState::Paused {
                    i.state = JobState::Running;
                }
                i.clone()
            };
            emit(app, &snap);
        }
    }

    /// Delivers the UI's answer to a prompt.
    pub fn answer(&self, job_id: u32, prompt_id: u32, answer: PromptAnswer) {
        if let Some(j) = self.get(job_id) {
            j.open_prompts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|p| p.prompt_id != prompt_id);
            if let Some(tx) = j
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&prompt_id)
            {
                let _ = tx.send(answer);
            }
        }
    }

    /// Prompts still waiting for an answer.
    pub fn open_prompts(&self) -> Vec<PromptEvent> {
        let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.values()
            .flat_map(|j| {
                j.open_prompts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
            })
            .collect()
    }

    pub fn list(&self) -> Vec<JobInfo> {
        let mut v: Vec<JobInfo> = self
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|j| j.snapshot())
            .collect();
        v.sort_by_key(|j| std::cmp::Reverse(j.id));
        v
    }

    /// Removes finished jobs.
    pub fn clear_finished(&self) {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, j| matches!(j.snapshot().state, JobState::Running | JobState::Paused));
    }

    /// Whether any job is still running.
    pub fn busy(&self) -> bool {
        self.list()
            .iter()
            .any(|j| matches!(j.state, JobState::Running | JobState::Paused))
    }
}

struct SpeedState {
    last: Option<Instant>,
    last_bytes: u64,
    speed: f64,
    last_emit: Option<Instant>,
}

struct JobProgress {
    app: AppHandle,
    job: Arc<Job>,
    state: Mutex<SpeedState>,
}

impl ProgressSink for JobProgress {
    fn progress(&self, p: &Progress) {
        // Cooperative pause: the worker sleeps here while paused.
        while self.job.paused.load(Ordering::Relaxed) && !self.job.cancel.is_cancelled() {
            std::thread::sleep(Duration::from_millis(100));
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.last = None;
        }
        let now = Instant::now();
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(last) = s.last {
            let dt = now.duration_since(last).as_secs_f64();
            if dt > 0.05 {
                let inst = p.bytes_done.saturating_sub(s.last_bytes) as f64 / dt;
                s.speed = if s.speed == 0.0 {
                    inst
                } else {
                    s.speed * 0.8 + inst * 0.2
                };
                s.last = Some(now);
                s.last_bytes = p.bytes_done;
            }
        } else {
            s.last = Some(now);
            s.last_bytes = p.bytes_done;
        }
        let due = s
            .last_emit
            .is_none_or(|t| now.duration_since(t) >= Duration::from_millis(100))
            || p.phase == Phase::Finalizing;
        if !due {
            return;
        }
        s.last_emit = Some(now);
        let speed = s.speed;
        drop(s);
        let snap = {
            let mut i = self.job.info.lock().unwrap_or_else(|e| e.into_inner());
            i.progress = p.clone();
            i.speed = speed;
            i.eta = (speed > 1.0 && p.bytes_total > p.bytes_done)
                .then(|| (p.bytes_total - p.bytes_done) as f64 / speed);
            if i.state == JobState::Paused && !self.job.paused.load(Ordering::Relaxed) {
                i.state = JobState::Running;
            }
            i.clone()
        };
        emit(&self.app, &snap);
        set_taskbar(&self.app, &snap);
    }
}

struct JobInteraction {
    app: AppHandle,
    job: Arc<Job>,
    preset: Mutex<Option<String>>,
}

impl JobInteraction {
    fn ask(&self, kind: &'static str, data: serde_json::Value) -> Option<PromptAnswer> {
        let prompt_id = self.job.next_prompt.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.job
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(prompt_id, tx);
        let snap = self.job.snapshot();
        let event = PromptEvent {
            job_id: snap.id,
            window: snap.window,
            prompt_id,
            kind,
            data,
        };
        self.job
            .open_prompts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(event.clone());
        let _ = self.app.emit("job://prompt", event);
        let answer = loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(a) => break Some(a),
                Err(mpsc::RecvTimeoutError::Timeout) if !self.job.cancel.is_cancelled() => {}
                _ => break None,
            }
        };
        self.job
            .open_prompts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|p| p.prompt_id != prompt_id);
        answer
    }
}

impl Interaction for JobInteraction {
    fn password(&self, req: &PasswordRequest) -> Option<String> {
        if req.attempt == 0
            && let Some(p) = self.preset.lock().unwrap_or_else(|e| e.into_inner()).take()
        {
            return Some(p);
        }
        match self.ask("password", serde_json::to_value(req).ok()?) {
            Some(PromptAnswer::Password { value }) => value.filter(|v| !v.is_empty()),
            _ => None,
        }
    }

    fn conflict(&self, c: &Conflict) -> ConflictDecision {
        let data = serde_json::to_value(c).unwrap_or_default();
        match self.ask("conflict", data) {
            Some(PromptAnswer::Conflict {
                choice,
                apply_to_all,
            }) => ConflictDecision {
                choice,
                apply_to_all,
            },
            _ => ConflictDecision {
                choice: ConflictChoice::Abort,
                apply_to_all: true,
            },
        }
    }

    fn confirm(&self, c: &Confirm) -> bool {
        let data = serde_json::to_value(c).unwrap_or_default();
        matches!(
            self.ask("confirm", data),
            Some(PromptAnswer::Confirm { yes: true })
        )
    }
}
