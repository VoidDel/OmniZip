//! Job context: cancellation, throttled progress, and user interaction callbacks.
//!
//! The core is UI-agnostic: when it needs a password, a conflict decision or a confirmation it
//! asks the [`Interaction`] implementation of the front end (CLI prompt, GUI dialog, or a fixed
//! answer for scripts).

use std::{
    io::{self, Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use serde::{Deserialize, Serialize};

use crate::{Error, ErrorCode, Result, policy::SecurityPolicy};

/// Cooperative cancellation flag shared between a job and its controller.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A fresh, non-cancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Returns `Err(Cancelled)` when cancellation was requested.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::cancelled())
        } else {
            Ok(())
        }
    }
}

/// What a job is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    #[default]
    Preparing,
    Scanning,
    Extracting,
    Compressing,
    Testing,
    Updating,
    Finalizing,
}

/// A progress snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub phase: Phase,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
    pub current: Option<String>,
}

impl Progress {
    /// Completion ratio in `0.0..=1.0` (by bytes, falling back to files).
    pub fn fraction(&self) -> f64 {
        if self.bytes_total > 0 {
            (self.bytes_done as f64 / self.bytes_total as f64).min(1.0)
        } else if self.files_total > 0 {
            (self.files_done as f64 / self.files_total as f64).min(1.0)
        } else {
            0.0
        }
    }
}

/// Receives progress snapshots (already throttled to at most ~30 per second).
pub trait ProgressSink: Send + Sync {
    fn progress(&self, p: &Progress);
}

impl<F: Fn(&Progress) + Send + Sync> ProgressSink for F {
    fn progress(&self, p: &Progress) {
        self(p)
    }
}

/// Discards progress.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullProgress;

impl ProgressSink for NullProgress {
    fn progress(&self, _: &Progress) {}
}

/// A password prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordRequest {
    /// File name of the archive.
    pub archive: String,
    /// Entry that needs the password, when known.
    pub entry: Option<String>,
    /// 0 for the first prompt; higher after wrong passwords.
    pub attempt: u32,
}

/// A destination file that already exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    pub path: PathBuf,
    pub existing_size: u64,
    #[serde(with = "crate::entry::opt_millis")]
    pub existing_mtime: Option<SystemTime>,
    pub incoming_size: u64,
    #[serde(with = "crate::entry::opt_millis")]
    pub incoming_mtime: Option<SystemTime>,
}

/// Answer to a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictChoice {
    Overwrite,
    Skip,
    /// Keep both: write the new file under a numbered name.
    Rename,
    /// Overwrite only when the incoming file is newer.
    OverwriteOlder,
    /// Stop the whole job.
    Abort,
}

/// A conflict answer plus whether it applies to all remaining conflicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictDecision {
    pub choice: ConflictChoice,
    pub apply_to_all: bool,
}

/// A yes/no question.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Confirm {
    /// The archive expands suspiciously (possible decompression bomb).
    HighRatio { ratio: u64, unpacked: u64 },
}

/// Callbacks into the front end. Every method has a safe, non-interactive default.
pub trait Interaction: Send + Sync {
    /// Asks for a password. `None` gives up.
    fn password(&self, _req: &PasswordRequest) -> Option<String> {
        None
    }

    /// Resolves an existing destination file.
    fn conflict(&self, _c: &Conflict) -> ConflictDecision {
        ConflictDecision {
            choice: ConflictChoice::Skip,
            apply_to_all: true,
        }
    }

    /// Asks for confirmation. Defaults to "no".
    fn confirm(&self, _c: &Confirm) -> bool {
        false
    }
}

/// Never asks; uses the defaults of [`Interaction`].
#[derive(Debug, Default, Clone, Copy)]
pub struct NoInteraction;

impl Interaction for NoInteraction {}

/// Provides a fixed password and otherwise behaves like [`NoInteraction`].
#[derive(Debug, Clone)]
pub struct FixedPassword(pub String);

impl Interaction for FixedPassword {
    fn password(&self, req: &PasswordRequest) -> Option<String> {
        (req.attempt == 0).then(|| self.0.clone())
    }
}

#[derive(Debug)]
struct Tracker {
    p: Progress,
    last_emit: Option<Instant>,
}

/// Everything a running job needs from its environment.
pub struct JobCtx {
    pub cancel: CancelToken,
    pub progress: Arc<dyn ProgressSink>,
    pub interaction: Arc<dyn Interaction>,
    pub policy: SecurityPolicy,
    tracker: Mutex<Tracker>,
    /// Minimum delay between two progress callbacks.
    pub throttle: Duration,
}

impl Default for JobCtx {
    fn default() -> Self {
        Self::new(Arc::new(NullProgress), Arc::new(NoInteraction))
    }
}

impl std::fmt::Debug for JobCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobCtx")
            .field("cancelled", &self.cancel.is_cancelled())
            .finish()
    }
}

impl JobCtx {
    /// A context with the given sinks, a new cancel token and the default policy.
    pub fn new(progress: Arc<dyn ProgressSink>, interaction: Arc<dyn Interaction>) -> Self {
        Self {
            cancel: CancelToken::new(),
            progress,
            interaction,
            policy: SecurityPolicy::default(),
            tracker: Mutex::new(Tracker {
                p: Progress::default(),
                last_emit: None,
            }),
            throttle: Duration::from_millis(33),
        }
    }

    /// Replaces the cancel token (so a controller can keep a handle).
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Replaces the security policy.
    #[must_use]
    pub fn with_policy(mut self, policy: SecurityPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Fails with `Cancelled` when cancellation was requested.
    pub fn check(&self) -> Result<()> {
        self.cancel.check()
    }

    fn update(&self, force: bool, f: impl FnOnce(&mut Progress)) {
        let mut t = self.tracker.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut t.p);
        let now = Instant::now();
        if force
            || t.last_emit
                .is_none_or(|last| now.duration_since(last) >= self.throttle)
        {
            t.last_emit = Some(now);
            let snapshot = t.p.clone();
            drop(t);
            self.progress.progress(&snapshot);
        }
    }

    /// Starts a phase with known totals (0 = unknown).
    pub fn begin(&self, phase: Phase, bytes_total: u64, files_total: u64) {
        self.update(true, |p| {
            *p = Progress {
                phase,
                bytes_total,
                files_total,
                ..Progress::default()
            };
        });
    }

    /// Adjusts the byte total once it becomes known.
    pub fn set_bytes_total(&self, total: u64) {
        self.update(false, |p| p.bytes_total = total);
    }

    /// Reports the file being processed.
    pub fn begin_file(&self, name: &str) {
        self.update(false, |p| p.current = Some(name.to_owned()));
    }

    /// Reports one finished file.
    pub fn file_done(&self) {
        self.update(false, |p| p.files_done += 1);
    }

    /// Reports processed bytes.
    pub fn add_bytes(&self, n: u64) {
        self.update(false, |p| p.bytes_done += n);
    }

    /// Emits the final state unconditionally.
    pub fn finish(&self) {
        self.update(true, |p| {
            p.phase = Phase::Finalizing;
            if p.bytes_total > 0 {
                p.bytes_done = p.bytes_done.max(p.bytes_total);
            }
        });
    }

    /// Current snapshot.
    pub fn snapshot(&self) -> Progress {
        self.tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .p
            .clone()
    }

    /// Asks for a password; `Err(PasswordRequired)` when the front end gives up.
    pub fn ask_password(&self, archive: &str, entry: Option<&str>, attempt: u32) -> Result<String> {
        self.check()?;
        let req = PasswordRequest {
            archive: archive.to_owned(),
            entry: entry.map(str::to_owned),
            attempt,
        };
        match self.interaction.password(&req) {
            Some(p) => Ok(p),
            None if attempt == 0 => {
                Err(Error::new(ErrorCode::PasswordRequired).with("path", archive))
            }
            None => Err(Error::new(ErrorCode::WrongPassword).with("path", archive)),
        }
    }
}

/// Number of attempts before a wrong password becomes a hard error.
pub const MAX_PASSWORD_ATTEMPTS: u32 = 5;

/// Wraps a reader: reports bytes to the job and stops on cancellation.
pub struct ProgressReader<'a, R> {
    inner: R,
    ctx: &'a JobCtx,
    pending: u64,
}

impl<'a, R> ProgressReader<'a, R> {
    pub fn new(inner: R, ctx: &'a JobCtx) -> Self {
        Self {
            inner,
            ctx,
            pending: 0,
        }
    }

    fn flush_pending(&mut self) {
        if self.pending > 0 {
            self.ctx.add_bytes(self.pending);
            self.pending = 0;
        }
    }
}

impl<R: Read> Read for ProgressReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.ctx.cancel.is_cancelled() {
            return Err(Error::cancelled().into());
        }
        let n = self.inner.read(buf)?;
        self.pending += n as u64;
        if self.pending >= 256 * 1024 {
            self.flush_pending();
        }
        Ok(n)
    }
}

impl<R> Drop for ProgressReader<'_, R> {
    fn drop(&mut self) {
        self.flush_pending();
    }
}

/// Copies with a 1 MiB buffer, reporting progress and honouring cancellation. Returns the
/// number of bytes copied. Fails with `CompressionBomb` when more than `limit` bytes arrive.
pub fn copy_with_progress(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
    ctx: &JobCtx,
    limit: Option<u64>,
) -> Result<u64> {
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        ctx.check()?;
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted && !ctx.cancel.is_cancelled() => {
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        total += n as u64;
        if let Some(limit) = limit
            && total > limit
        {
            return Err(Error::new(ErrorCode::CompressionBomb).with("size", total));
        }
        writer.write_all(&buf[..n])?;
        ctx.add_bytes(n as u64);
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttling_and_totals() {
        let seen = Arc::new(Mutex::new(Vec::<Progress>::new()));
        let s2 = seen.clone();
        let ctx = JobCtx::new(
            Arc::new(move |p: &Progress| s2.lock().unwrap().push(p.clone())),
            Arc::new(NoInteraction),
        );
        ctx.begin(Phase::Extracting, 100, 2);
        for _ in 0..1000 {
            ctx.add_bytes(1);
        }
        ctx.finish();
        let seen = seen.lock().unwrap();
        assert!(seen.len() < 50, "throttled, got {}", seen.len());
        assert_eq!(seen.last().unwrap().bytes_done, 1000);
    }

    #[test]
    fn cancel_stops_copy() {
        let ctx = JobCtx::default();
        ctx.cancel.cancel();
        let mut src: &[u8] = b"data";
        let err = copy_with_progress(&mut src, &mut Vec::new(), &ctx, None).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Cancelled);
    }

    #[test]
    fn copy_limit() {
        let ctx = JobCtx::default();
        let mut src: &[u8] = &[0u8; 100];
        let err = copy_with_progress(&mut src, &mut Vec::new(), &ctx, Some(10)).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CompressionBomb);
    }

    #[test]
    fn password_prompt_errors() {
        let ctx = JobCtx::default();
        assert_eq!(
            ctx.ask_password("a.7z", None, 0).unwrap_err().code(),
            ErrorCode::PasswordRequired
        );
        assert_eq!(
            ctx.ask_password("a.7z", None, 1).unwrap_err().code(),
            ErrorCode::WrongPassword
        );
        let ctx = JobCtx::new(Arc::new(NullProgress), Arc::new(FixedPassword("pw".into())));
        assert_eq!(ctx.ask_password("a.7z", None, 0).unwrap(), "pw");
        assert_eq!(
            ctx.ask_password("a.7z", None, 1).unwrap_err().code(),
            ErrorCode::WrongPassword
        );
    }
}
