//! Terminal output: human / JSON / porcelain modes, progress bars, prompts (`docs/08-cli.md` §5, §7).

use std::{
    io::{self, BufRead, IsTerminal, Write},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime},
};

use console::Style;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use omnizip_core::{
    CancelToken, Confirm, Conflict, ConflictChoice, ConflictDecision, Error, ErrorCode,
    Interaction, JobCtx, PasswordRequest, Phase, Progress, ProgressSink, SecurityPolicy, Warning,
    timeutil,
};
use omnizip_i18n::{Localizer, tr};
use serde_json::{Value, json};

use crate::cli::{ColorMode, Global, PasswordArgs, ProgressMode};

/// Output settings resolved from global flags and the environment.
pub struct Ui {
    pub l: Localizer,
    pub json: bool,
    pub porcelain: bool,
    pub quiet: bool,
    pub verbose: u8,
    pub progress: ProgressMode,
    /// Prompts are allowed (stdin is a terminal and `--no-input` was not given).
    pub interactive: bool,
    pub yes: bool,
    pub threads: u32,
}

/// The process-wide cancel token, set by Ctrl+C.
pub fn cancel_token() -> &'static CancelToken {
    static T: OnceLock<CancelToken> = OnceLock::new();
    T.get_or_init(|| {
        let token = CancelToken::new();
        let t = token.clone();
        let _ = ctrlc::set_handler(move || {
            if t.is_cancelled() {
                // Second Ctrl+C: leave immediately.
                std::process::exit(ErrorCode::Cancelled.exit_code());
            }
            t.cancel();
        });
        token
    })
}

impl Ui {
    pub fn new(g: &Global, l: Localizer) -> Self {
        let stderr_tty = io::stderr().is_terminal();
        let color = match g.color {
            ColorMode::Always => true,
            ColorMode::Never => false,
            ColorMode::Auto => {
                stderr_tty && std::env::var_os("NO_COLOR").is_none()
                    || std::env::var_os("CLICOLOR_FORCE").is_some()
            }
        };
        console::set_colors_enabled(color);
        console::set_colors_enabled_stderr(color);
        let progress = match g.progress {
            ProgressMode::Auto if g.quiet || g.json || g.porcelain => ProgressMode::None,
            ProgressMode::Auto if stderr_tty => ProgressMode::Bar,
            ProgressMode::Auto => ProgressMode::None,
            other => other,
        };
        Self {
            l,
            json: g.json,
            porcelain: g.porcelain,
            quiet: g.quiet,
            verbose: g.verbose,
            progress,
            interactive: io::stdin().is_terminal() && !g.no_input,
            yes: g.yes,
            threads: g.threads,
        }
    }

    pub fn style_ok(&self) -> Style {
        Style::new().green()
    }
    pub fn style_dim(&self) -> Style {
        Style::new().dim()
    }
    pub fn style_bold(&self) -> Style {
        Style::new().bold()
    }

    /// A line on stdout (always, unless JSON mode).
    pub fn out(&self, line: impl AsRef<str>) {
        let mut o = io::stdout().lock();
        let _ = writeln!(o, "{}", line.as_ref());
    }

    /// A human-readable status line on stdout (suppressed by `-q`, `--json`, `--porcelain`).
    pub fn info(&self, line: impl AsRef<str>) {
        if !self.quiet && !self.json && !self.porcelain {
            self.out(line);
        }
    }

    /// A JSON document on stdout.
    pub fn emit(&self, v: &Value) {
        self.out(v.to_string());
    }

    /// A warning on stderr.
    pub fn warn_text(&self, text: &str) {
        if self.json {
            eprintln!("{}", json!({"type": "warning", "message": text}));
        } else if !self.quiet {
            eprintln!(
                "{}: {text}",
                Style::new()
                    .yellow()
                    .bold()
                    .apply_to(self.l.get("warn-prefix"))
            );
        }
    }

    /// A per-entry warning from a report.
    pub fn warning(&self, w: &Warning) {
        let e = {
            let mut e = Error::new(w.code);
            if let Some(d) = &w.detail {
                e = e.with("detail", d);
            }
            e
        };
        if self.json {
            eprintln!(
                "{}",
                json!({"type": "warning", "code": w.code.id(), "key": w.code.key(), "path": w.path, "detail": w.detail})
            );
            return;
        }
        let msg = self.l.error(&e);
        self.warn_text(&tr!(
            self.l,
            "warn-entry",
            path = w.path.as_str(),
            message = msg
        ));
    }

    /// An error on stderr: `omnizip: error[E1203]: …`.
    pub fn error(&self, e: &Error) {
        if self.json {
            eprintln!("{}", json!({"type": "error", "error": e.to_report()}));
            return;
        }
        let red = Style::new().red().bold();
        let mut line = format!(
            "omnizip: {}: {}",
            red.apply_to(format!("error[{}]", e.code().id())),
            self.l.error(e)
        );
        if self.verbose > 0
            && let Some(d) = e.param("detail")
        {
            line.push_str(&format!(" ({d})"));
        }
        eprintln!("{line}");
    }

    /// A job context with progress display and interaction wired to this terminal.
    pub fn job(
        &self,
        archive: &str,
        preset_password: Option<String>,
        overwrite_all: Option<bool>,
    ) -> (JobCtx, Display) {
        let display = Display::new(self.progress, self.json);
        let interaction = Arc::new(CliInteraction {
            l: self.l,
            interactive: self.interactive,
            yes: self.yes,
            preset: Mutex::new(preset_password),
            archive: archive.to_owned(),
            bar: display.bar.clone(),
            overwrite_all,
        });
        let sink: Arc<dyn ProgressSink> = Arc::new(display.clone());
        let ctx = JobCtx::new(sink, interaction).with_cancel(cancel_token().clone());
        (ctx, display)
    }

    /// Resolves the password sources of a command.
    pub fn password(&self, p: &PasswordArgs, confirm: bool) -> Result<Option<String>, Error> {
        if let Some(file) = &p.password_file {
            let s = std::fs::read_to_string(file).map_err(|e| Error::io(e, file))?;
            return Ok(Some(s.trim_end_matches(['\r', '\n']).to_owned()));
        }
        if let Some(var) = &p.password_env {
            let s = std::env::var(var)
                .map_err(|_| Error::invalid_argument(format!("${var} is not set")))?;
            return Ok(Some(s));
        }
        match p.password.as_deref() {
            None => Ok(None),
            Some("") => {
                if !self.interactive {
                    return Err(Error::new(ErrorCode::PasswordRequired));
                }
                let first = prompt_hidden(&tr!(self.l, "prompt-password", archive = "-"))?;
                if confirm {
                    let again = prompt_hidden(&tr!(self.l, "prompt-password", archive = "↻"))?;
                    if again != first {
                        return Err(Error::invalid_argument("passwords do not match"));
                    }
                }
                Ok(Some(first))
            }
            Some(inline) => {
                self.warn_text(&self.l.get("warn-password-argument"));
                Ok(Some(inline.to_owned()))
            }
        }
    }

    /// Security policy from flags.
    pub fn policy(&self, allow_high_ratio: bool) -> SecurityPolicy {
        SecurityPolicy {
            allow_high_ratio,
            ..SecurityPolicy::default()
        }
    }
}

fn prompt_hidden(prompt: &str) -> Result<String, Error> {
    rpassword::prompt_password(format!("{prompt} ")).map_err(Error::from)
}

/// Progress display shared with the job context.
#[derive(Clone)]
pub struct Display {
    bar: Option<ProgressBar>,
    mode: ProgressMode,
    json: bool,
    last_plain: Arc<Mutex<Option<Instant>>>,
}

impl Display {
    fn new(mode: ProgressMode, json: bool) -> Self {
        let bar = (mode == ProgressMode::Bar).then(|| {
            let b = ProgressBar::with_draw_target(Some(0), ProgressDrawTarget::stderr_with_hz(15));
            b.set_style(
                ProgressStyle::with_template(
                    "{spinner:.cyan} {bar:32.cyan/blue} {percent:>3}%  {bytes:>10}/{total_bytes:<10} {bytes_per_sec:>12}  ETA {eta:<5} {wide_msg:.dim}",
                )
                .expect("valid template")
                .progress_chars("━╸─"),
            );
            b
        });
        Self {
            bar,
            mode,
            json,
            last_plain: Arc::new(Mutex::new(None)),
        }
    }

    /// Removes the bar before printing the summary.
    pub fn finish(&self) {
        if let Some(b) = &self.bar {
            b.finish_and_clear();
        }
    }
}

impl ProgressSink for Display {
    fn progress(&self, p: &Progress) {
        match self.mode {
            ProgressMode::Bar => {
                if let Some(b) = &self.bar {
                    if p.bytes_total > 0 {
                        b.set_length(p.bytes_total);
                    }
                    b.set_position(p.bytes_done);
                    if let Some(c) = &p.current {
                        b.set_message(c.clone());
                    }
                }
            }
            ProgressMode::Plain => {
                let mut last = self.last_plain.lock().unwrap_or_else(|e| e.into_inner());
                let due = last.is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
                    || p.phase == Phase::Finalizing;
                if due {
                    *last = Some(Instant::now());
                    if self.json {
                        eprintln!("{}", json!({"type": "progress", "progress": p}));
                    } else {
                        eprintln!(
                            "{:>3}% {}/{} {}",
                            (p.fraction() * 100.0).round() as u32,
                            fmt_size(p.bytes_done),
                            fmt_size(p.bytes_total),
                            p.current.as_deref().unwrap_or("")
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

/// Answers the core's questions on the terminal.
struct CliInteraction {
    l: Localizer,
    interactive: bool,
    yes: bool,
    preset: Mutex<Option<String>>,
    archive: String,
    bar: Option<ProgressBar>,
    /// `Some(true)` overwrite everything, `Some(false)` skip everything, `None` ask.
    overwrite_all: Option<bool>,
}

impl CliInteraction {
    fn suspend<T>(&self, f: impl FnOnce() -> T) -> T {
        match &self.bar {
            Some(b) => b.suspend(f),
            None => f(),
        }
    }

    fn read_line(&self, prompt: &str) -> String {
        self.suspend(|| {
            eprint!("{prompt} ");
            let _ = io::stderr().flush();
            let mut s = String::new();
            let _ = io::stdin().lock().read_line(&mut s);
            s.trim().to_owned()
        })
    }
}

impl Interaction for CliInteraction {
    fn password(&self, req: &PasswordRequest) -> Option<String> {
        if req.attempt == 0
            && let Some(p) = self.preset.lock().ok()?.take()
        {
            return Some(p);
        }
        if !self.interactive {
            return None;
        }
        let id = if req.attempt == 0 {
            "prompt-password"
        } else {
            "prompt-password-retry"
        };
        let prompt = tr!(self.l, id, archive = self.archive.as_str());
        self.suspend(|| prompt_hidden(&prompt).ok())
            .filter(|p| !p.is_empty())
    }

    fn conflict(&self, c: &Conflict) -> ConflictDecision {
        let all = |choice| ConflictDecision {
            choice,
            apply_to_all: true,
        };
        match self.overwrite_all {
            Some(true) => return all(ConflictChoice::Overwrite),
            Some(false) => return all(ConflictChoice::Skip),
            None => {}
        }
        if self.yes {
            return all(ConflictChoice::Overwrite);
        }
        if !self.interactive {
            return all(ConflictChoice::Skip);
        }
        let prompt = tr!(
            self.l,
            "prompt-conflict",
            path = c.path.display().to_string(),
            existing = fmt_size(c.existing_size),
            incoming = fmt_size(c.incoming_size)
        );
        loop {
            let answer = self.read_line(&prompt);
            let one = |choice| ConflictDecision {
                choice,
                apply_to_all: false,
            };
            return match answer.as_str() {
                "o" => one(ConflictChoice::Overwrite),
                "s" | "" => one(ConflictChoice::Skip),
                "r" => one(ConflictChoice::Rename),
                "O" => all(ConflictChoice::Overwrite),
                "S" => all(ConflictChoice::Skip),
                "q" | "Q" => all(ConflictChoice::Abort),
                _ => continue,
            };
        }
    }

    fn confirm(&self, c: &Confirm) -> bool {
        if self.yes {
            return true;
        }
        if !self.interactive {
            return false;
        }
        match c {
            Confirm::HighRatio { ratio, unpacked } => {
                let prompt = tr!(
                    self.l,
                    "prompt-high-ratio",
                    archive = self.archive.as_str(),
                    ratio = *ratio,
                    size = fmt_size(*unpacked)
                );
                matches!(self.read_line(&prompt).as_str(), "y" | "Y" | "yes")
            }
        }
    }
}

/// `1.5 KiB`, `42.0 MiB` (binary units, as on the command line everywhere).
pub fn fmt_size(b: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if b < 1024 {
        return format!("{b} B");
    }
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v < 10.0 {
        format!("{v:.2} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// `0.42 s`, `12 s`, `3 min 05 s`, `1 h 02 min`.
pub fn fmt_elapsed(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 10.0 {
        format!("{s:.2} s")
    } else if s < 60.0 {
        format!("{s:.0} s")
    } else if s < 3600.0 {
        format!("{} min {:02} s", (s / 60.0) as u64, (s % 60.0) as u64)
    } else {
        format!(
            "{} h {:02} min",
            (s / 3600.0) as u64,
            ((s % 3600.0) / 60.0) as u64
        )
    }
}

/// Local `YYYY-MM-DD HH:MM`.
pub fn fmt_time(t: Option<SystemTime>) -> String {
    match t {
        Some(t) => {
            let (y, mo, d, h, mi, _) = timeutil::system_to_local_fields(t);
            format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}")
        }
        None => " ".repeat(16),
    }
}

/// ISO-8601 UTC for machine output.
pub fn iso_time(t: Option<SystemTime>) -> Value {
    match t {
        Some(t) => Value::from(omnizip_core::entry::to_millis(t)),
        None => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(fmt_size(0), "0 B");
        assert_eq!(fmt_size(1023), "1023 B");
        assert_eq!(fmt_size(1536), "1.50 KiB");
        assert_eq!(fmt_size(42 << 20), "42.0 MiB");
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_elapsed(Duration::from_millis(420)), "0.42 s");
        assert_eq!(fmt_elapsed(Duration::from_secs(185)), "3 min 05 s");
    }
}
