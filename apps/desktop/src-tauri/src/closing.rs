//! Closing windows and quitting while jobs run.
//!
//! A job asks its questions (password, conflict, confirmation) in the window that owns it: its
//! progress window, or the main window. Without that window nobody can answer, and the job
//! waits for good. Nor may the process end under a running job: extraction would leave a
//! half-written file, a rewrite of an archive its temporary file. So:
//!
//! * a window owning running jobs, or the last window (the process ends with it), cancels them
//!   and closes once they have ended, as the close button of a progress window does; the main
//!   window first asks (`app://close-requested`, answered by `window_close_cancelling`);
//! * quitting (the notification-area icon) cancels every job and exits once they have ended;
//! * a window gone without closing (destroyed) takes its jobs' questions with it: they are
//!   cancelled.
//!
//! A job that does not stop within [`WAIT`] (a decoder that does not look at cancellation) no
//! longer holds the window or the process up.

use std::{
    sync::{
        Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use tauri::{AppHandle, CloseRequestApi, Emitter, ExitRequestApi, Manager, Window};

use crate::state::AppState;

/// How long closing or quitting waits for cancelled jobs.
const WAIT: Duration = Duration::from_secs(15);

/// Windows whose jobs have been dealt with: their next close request goes through.
static RELEASED: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Windows waiting for their cancelled jobs: they close by themselves.
static CLOSING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Quitting: not asked yet, waiting for cancelled jobs, or free to exit.
static QUIT: AtomicU8 = AtomicU8::new(IDLE);
const IDLE: u8 = 0;
const WAITING: u8 = 1;
const GO: u8 = 2;

fn mark(set: &Mutex<Vec<String>>, label: &str) {
    set.lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(label.to_owned());
}

/// Removes `label` from `set`; whether it was there.
fn unmark(set: &Mutex<Vec<String>>, label: &str) -> bool {
    let mut r = set.lock().unwrap_or_else(|e| e.into_inner());
    let before = r.len();
    r.retain(|l| l != label);
    r.len() != before
}

fn marked(set: &Mutex<Vec<String>>, label: &str) -> bool {
    set.lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|l| l == label)
}

/// Jobs that closing the window `label` would cut short: those it owns, or all of them when it
/// is the last window.
fn affected(app: &AppHandle, label: &str) -> Vec<u32> {
    let last = app.webview_windows().keys().all(|l| l == label);
    app.state::<AppState>().jobs.live((!last).then_some(label))
}

/// `CloseRequested` of a window that is really closing (not hiding to the notification area).
pub fn close_requested(window: &Window, api: &CloseRequestApi) {
    let label = window.label();
    if unmark(&RELEASED, label) {
        return;
    }
    if marked(&CLOSING, label) {
        return api.prevent_close();
    }
    let app = window.app_handle();
    let jobs = affected(app, label);
    if jobs.is_empty() {
        return;
    }
    api.prevent_close();
    if label == "main" {
        let _ = app.emit("app://close-requested", jobs.len());
    } else {
        cancel_then_close(app, label);
    }
}

/// Cancels the jobs closing the window `label` would cut short, then closes it once they have
/// ended.
pub fn cancel_then_close(app: &AppHandle, label: &str) {
    if marked(&CLOSING, label) {
        return;
    }
    mark(&CLOSING, label);
    let jobs = affected(app, label);
    let state = app.state::<AppState>();
    for id in &jobs {
        state.jobs.cancel(*id);
    }
    let app = app.clone();
    let label = label.to_owned();
    std::thread::spawn(move || {
        if !app.state::<AppState>().jobs.wait_ended(&jobs, WAIT) {
            tracing::warn!("closing a window: jobs still running after cancelling");
        }
        unmark(&CLOSING, &label);
        if let Some(w) = app.get_webview_window(&label) {
            mark(&RELEASED, &label);
            let _ = w.close();
        }
    });
}

/// A window was destroyed: nobody can answer its jobs' questions any more.
pub fn destroyed(app: &AppHandle, label: &str) {
    unmark(&RELEASED, label);
    let state = app.state::<AppState>();
    for id in state.jobs.live(Some(label)) {
        state.jobs.cancel(id);
    }
}

/// `RunEvent::ExitRequested`: running jobs are cancelled and the process exits once they have
/// ended.
pub fn exit_requested(app: &AppHandle, api: &ExitRequestApi, code: Option<i32>) {
    match QUIT.load(Ordering::Acquire) {
        GO => return,
        WAITING => return api.prevent_exit(),
        _ => {}
    }
    let state = app.state::<AppState>();
    let jobs = state.jobs.live(None);
    if jobs.is_empty() {
        return;
    }
    api.prevent_exit();
    QUIT.store(WAITING, Ordering::Release);
    for id in &jobs {
        state.jobs.cancel(*id);
    }
    let app = app.clone();
    std::thread::spawn(move || {
        if !app.state::<AppState>().jobs.wait_ended(&jobs, WAIT) {
            tracing::warn!("quitting: jobs still running after cancelling");
        }
        QUIT.store(GO, Ordering::Release);
        app.exit(code.unwrap_or(0));
    });
}
