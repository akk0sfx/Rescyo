//! Output: localized text for people, stable JSON for machines, and exit codes.

use std::io::{IsTerminal, Write};
use std::path::Path;

use rescyo_core::{Problem, Progress, RecoveryError, RecoveryProgress, ScanError, SessionError};
use rescyo_fs::Phase;
use rescyo_i18n::{Arg, Localizer, size};
use rescyo_io::GuardError;
use serde::Serialize;

use crate::dto::{Envelope, ErrorBody, SCHEMA_VERSION, StreamEvent};

pub struct Out {
    command: &'static str,
    json: bool,
    stream: bool,
    quiet: bool,
    tty: bool,
    l10n: Localizer,
    last_progress: Option<std::time::Instant>,
}

/// An error on its way to the user: stable code, English message, localized text, exit code.
#[derive(Debug)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
    pub text_id: &'static str,
    pub args: Vec<(&'static str, String)>,
    pub details: serde_json::Value,
    pub exit: u8,
    /// Extra lines for the human (preflight problems, hints).
    pub lines: Vec<String>,
}

impl Failure {
    fn new(
        code: &'static str,
        message: String,
        text_id: &'static str,
        args: Vec<(&'static str, String)>,
        exit: u8,
    ) -> Self {
        Self {
            code,
            message,
            text_id,
            args,
            details: serde_json::Value::Null,
            exit,
            lines: Vec::new(),
        }
    }

    pub fn usage(message: String) -> Self {
        Self::new(
            "usage",
            message.clone(),
            "error-internal",
            vec![("reason", message)],
            2,
        )
    }

    pub fn internal(reason: &str) -> Self {
        Self::new(
            "internal",
            format!("internal error: {reason}"),
            "error-internal",
            vec![("reason", reason.into())],
            1,
        )
    }

    pub fn source_open(source: &str, e: &std::io::Error) -> Self {
        let mut f = Self::new(
            "source_open_failed",
            format!("cannot open {source}: {e}"),
            "error-source-open",
            vec![("source", source.into()), ("reason", e.to_string())],
            3,
        );
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            f.text_id = "error-source-open";
            f.lines.push("hint-admin".into());
        }
        f
    }

    pub fn source_mismatch(source: &str, reason: &str) -> Self {
        Self::new(
            "source_mismatch",
            format!("{source} does not match the session's source: {reason}"),
            "error-source-mismatch",
            vec![("source", source.into()), ("reason", reason.into())],
            3,
        )
    }

    pub fn session(path: &Path, e: SessionError) -> Self {
        let p = path.display().to_string();
        match e {
            SessionError::Exists(_) => Self::new(
                "session_exists",
                format!("{p} already exists"),
                "error-session-exists",
                vec![("path", p)],
                1,
            ),
            other => Self::new(
                "session_invalid",
                format!("{p}: {other}"),
                "error-session-invalid",
                vec![("path", p), ("reason", other.to_string())],
                1,
            ),
        }
    }

    pub fn session_on_source(p: &Problem) -> Self {
        let mut f = match p {
            Problem::TargetOnSourceDevice { devices } => Self::new(
                "session_on_source_device",
                format!(
                    "the session file would be on the source disk ({})",
                    devices.join(", ")
                ),
                "error-session-on-source",
                vec![("devices", devices.join(", "))],
                4,
            ),
            other => problem_failure(other),
        };
        f.details = serde_json::to_value(p).unwrap_or_default();
        f
    }

    pub fn from_guard(e: GuardError) -> Self {
        match e {
            GuardError::DegradationAlarm { errors } => Self::new(
                "source_degraded",
                format!("the source degraded: {errors} unreadable sectors"),
                "error-degraded",
                vec![("count", errors.to_string())],
                3,
            ),
            GuardError::Gone(e) => Self::new(
                "source_gone",
                format!("the source disappeared: {e}"),
                "error-gone",
                vec![("reason", e.to_string())],
                3,
            ),
            other => Self::internal(&other.to_string()),
        }
    }

    pub fn from_scan(e: ScanError) -> Self {
        match e {
            ScanError::Degraded { errors } => Self::from_guard(GuardError::DegradationAlarm { errors }),
            ScanError::Gone(e) => Self::from_guard(GuardError::Gone(e)),
            ScanError::Cancelled => {
                Self::new("cancelled", "cancelled".into(), "error-cancelled", vec![], 130)
            }
            ScanError::Session(s) => Self::new(
                "session_invalid",
                s.to_string(),
                "error-session-invalid",
                vec![("path", String::new()), ("reason", s.to_string())],
                1,
            ),
            other => Self::internal(&other.to_string()),
        }
    }

    pub fn from_recovery(e: RecoveryError) -> Self {
        match e {
            RecoveryError::Degraded { errors } => Self::from_guard(GuardError::DegradationAlarm { errors }),
            RecoveryError::Gone(e) => Self::from_guard(GuardError::Gone(e)),
            RecoveryError::Cancelled => {
                Self::new("cancelled", "cancelled".into(), "error-cancelled", vec![], 130)
            }
            RecoveryError::Blocked(p) => {
                let mut f = Self::new(
                    "preflight_failed",
                    "preflight failed".into(),
                    "error-preflight",
                    vec![],
                    4,
                );
                f.details = serde_json::to_value(&p).unwrap_or_default();
                f
            }
            RecoveryError::Io(e) => Self::internal(&format!("cannot write to the target: {e}")),
            RecoveryError::Session(s) => Self::internal(&s.to_string()),
        }
    }

    pub fn preflight(problems: &[Problem], out: &Out) -> Self {
        let mut f = Self::new(
            "preflight_failed",
            "preflight failed: nothing was written".into(),
            "error-preflight",
            vec![],
            4,
        );
        f.details = serde_json::to_value(problems).unwrap_or_default();
        for p in problems {
            let pf = problem_failure(p);
            let args: Vec<(&str, Arg<'_>)> =
                pf.args.iter().map(|(k, v)| (*k, Arg::from(v.as_str()))).collect();
            f.lines.push(format!("  - {}", out.l10n.t(pf.text_id, &args)));
        }
        f
    }

    pub fn dmde_parse(reason: &str) -> Self {
        Self::new(
            "dmde_parse_failed",
            format!("cannot read the DMDE log: {reason}"),
            "error-dmde-parse",
            vec![("reason", reason.into())],
            1,
        )
    }

    pub fn dmde_geometry(log: &str, source: &str) -> Self {
        Self::new(
            "dmde_geometry_mismatch",
            format!("the DMDE log describes another disk ({log}); the source is {source}"),
            "error-dmde-geometry",
            vec![("log", log.into()), ("source", source.into())],
            3,
        )
    }
}

fn problem_failure(p: &Problem) -> Failure {
    let (code, id, args): (&'static str, &'static str, Vec<(&'static str, String)>) = match p {
        Problem::TargetOnSourceDevice { devices } => (
            "target_on_source_device",
            "problem-target-on-source-device",
            vec![("devices", devices.join(", "))],
        ),
        Problem::UnverifiedTarget { reason } => (
            "unverified_target",
            "problem-unverified-target",
            vec![("reason", reason.clone())],
        ),
        Problem::SourceDevicesUnknown { reason } => (
            "source_devices_unknown",
            "problem-source-devices-unknown",
            vec![("reason", reason.clone())],
        ),
        Problem::InsufficientSpace { needed, free } => (
            "insufficient_space",
            "problem-insufficient-space",
            vec![("needed", size(*needed)), ("free", size(*free))],
        ),
        Problem::FileTooLarge { path, size: s, limit } => (
            "file_too_large",
            "problem-file-too-large",
            vec![
                ("path", path.clone()),
                ("size", size(*s)),
                ("limit", size(*limit)),
            ],
        ),
        Problem::NothingSelected => ("nothing_selected", "problem-nothing-selected", vec![]),
    };
    Failure::new(code, code.replace('_', " "), id, args, 4)
}

impl Out {
    pub fn new(command: &'static str, json: bool, stream: bool, quiet: bool, lang: Option<&str>) -> Self {
        Self {
            command,
            json: json || stream,
            stream,
            quiet,
            tty: std::io::stderr().is_terminal(),
            l10n: Localizer::new(lang),
            last_progress: None,
        }
    }

    pub fn machine(&self) -> bool {
        self.json
    }

    pub fn lang(&self) -> &'static str {
        self.l10n.lang()
    }

    pub fn t(&self, id: &str, args: &[(&str, Arg<'_>)]) -> String {
        self.l10n.t(id, args)
    }

    /// A line for people (stdout); silent in JSON mode.
    pub fn say(&self, line: &str) {
        if !self.json {
            println!("{line}");
        }
    }

    fn emit(&self, ev: &StreamEvent) {
        if let Ok(s) = serde_json::to_string(ev) {
            println!("{s}");
        }
    }

    pub fn progress(&mut self, p: Progress) {
        let phase = match p.phase {
            Phase::Probe => "probe",
            Phase::DeepScan => "deep_scan",
            Phase::Walk => "walk",
            Phase::LostFolders => "lost_folders",
        };
        if self.stream {
            if self.throttle(250) {
                self.emit(&StreamEvent::Progress {
                    phase: phase.into(),
                    done: p.done,
                    total: p.total,
                });
            }
            return;
        }
        if self.json || self.quiet || !self.tty || !self.throttle(200) {
            return;
        }
        let label = self.t(
            match p.phase {
                Phase::Probe => "phase-probe",
                Phase::DeepScan => "phase-deep-scan",
                Phase::Walk => "phase-walk",
                Phase::LostFolders => "phase-lost-folders",
            },
            &[],
        );
        let line = match (p.phase, p.total) {
            (Phase::DeepScan, Some(t)) if t > 0 => {
                format!(
                    "{label}: {:5.1}%  {} / {}",
                    p.done as f64 * 100.0 / t as f64,
                    size(p.done),
                    size(t)
                )
            }
            (_, Some(t)) if t > 0 => format!("{label}: {} / {t}", p.done),
            _ => format!("{label}: {}", p.done),
        };
        eprint!("\r{line:<78}");
        let _ = std::io::stderr().flush();
    }

    pub fn recovery_progress(&mut self, p: RecoveryProgress) {
        if self.stream {
            if self.throttle(250) {
                self.emit(&StreamEvent::Progress {
                    phase: "recover".into(),
                    done: p.bytes_done,
                    total: Some(p.bytes_total),
                });
            }
            return;
        }
        if self.json || self.quiet || !self.tty || !self.throttle(200) {
            return;
        }
        let pct = if p.bytes_total > 0 {
            p.bytes_done as f64 * 100.0 / p.bytes_total as f64
        } else {
            100.0
        };
        eprint!(
            "\r{pct:5.1}%  {} / {}  {} / {}",
            size(p.bytes_done),
            size(p.bytes_total),
            p.files_done,
            p.files_total
        );
        let _ = std::io::stderr().flush();
    }

    fn throttle(&mut self, ms: u64) -> bool {
        let now = std::time::Instant::now();
        if self
            .last_progress
            .is_some_and(|t| now.duration_since(t).as_millis() < u128::from(ms))
        {
            return false;
        }
        self.last_progress = Some(now);
        true
    }

    pub fn end_progress(&mut self) {
        if !self.json && !self.quiet && self.tty && self.last_progress.is_some() {
            eprint!("\r{:<78}\r", "");
        }
        self.last_progress = None;
    }

    /// The final envelope in JSON mode.
    pub fn done<T: Serialize>(&self, data: &T) {
        if self.json {
            let env = Envelope {
                schema_version: SCHEMA_VERSION,
                command: self.command,
                ok: true,
                data: Some(data),
                error: None,
            };
            if let Ok(s) = serde_json::to_string(&env) {
                println!("{s}");
            }
        }
    }

    /// Reports a failure and returns the exit code.
    pub fn fail(&mut self, f: Failure) -> u8 {
        self.end_progress();
        if self.json {
            let env: Envelope<()> = Envelope {
                schema_version: SCHEMA_VERSION,
                command: self.command,
                ok: false,
                data: None,
                error: Some(ErrorBody {
                    code: f.code.into(),
                    message: f.message.clone(),
                    details: f.details.clone(),
                }),
            };
            if let Ok(s) = serde_json::to_string(&env) {
                println!("{s}");
            }
        } else {
            let args: Vec<(&str, Arg<'_>)> =
                f.args.iter().map(|(k, v)| (*k, Arg::from(v.as_str()))).collect();
            eprintln!("rescyo: {}", self.t(f.text_id, &args));
            for l in &f.lines {
                if l.starts_with("hint-") {
                    eprintln!("{}", self.t(l, &[]));
                } else {
                    eprintln!("{l}");
                }
            }
        }
        f.exit
    }
}
