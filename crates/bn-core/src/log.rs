//! Opt-in structured **action log** (verbose/debug mode) — a debugging substrate distinct from
//! the cron/metrics [`crate::events`] log. When enabled, bn appends one JSONL record per
//! high-value action (command dispatch, external `git`/`gh`/`ssh` calls, orchestrate/dispatch
//! decisions, review gates) to a rotating daily file under the resolved state dir, so a whole
//! session can be inspected after the fact.
//!
//! Design (issue: verbose/debug action logging):
//! * **Not the `tracing` crate.** bn has no `tracing`/`log` facade wired; this reuses the proven
//!   JSONL-append style of [`crate::events`] with a tiny process-global logger and zero new deps.
//! * **Separate file.** Never writes to `events.jsonl` (the cron/metrics substrate) — it writes to
//!   `<log_dir>/bn-YYYY-MM-DD.jsonl` so metrics/cron reads stay clean.
//! * **Off by default, strict no-op.** Until [`init`] flips it on, every [`record`] / [`action`]
//!   call is a cheap early return — so the hot runner seam and all tests stay silent unless the
//!   operator opts in via `--verbose/--debug`, `BN_VERBOSE`/`BN_LOG`, or `[log]` in `config.toml`.
//! * **Redacted.** Args/results are scrubbed of tokens/secrets before serialization.

use crate::util::{local_date_of, now_epoch_secs, now_rfc3339_utc};
use serde_json::{json, Map, Value};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Severity of an action-log record. Ordered so a configured threshold can suppress lower levels.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    #[default]
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    /// Parse a case-insensitive level name (`debug`/`info`/`warn`|`warning`/`error`). `None` for
    /// anything else so callers can fall back to a default.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "debug" | "trace" | "verbose" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" | "warning" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// The live logger. `None` (or an unset [`LOGGER`]) means disabled — the common case.
struct Logger {
    /// Minimum level that is written; records below it are dropped.
    threshold: LogLevel,
    /// Directory holding the daily `bn-YYYY-MM-DD.jsonl` files.
    dir: PathBuf,
    /// Days of daily files to retain; older files are pruned on rotation. `0` disables pruning.
    retain_days: i64,
    /// Cached open file keyed by its date, for daily rotation.
    open: Mutex<Option<(String, File)>>,
}

static LOGGER: OnceLock<Option<Logger>> = OnceLock::new();

/// Enable the action log for this process. Idempotent: only the first call wins (subsequent calls
/// are ignored), matching a once-per-process CLI init. Passing `enabled == false` installs the
/// disabled state so later [`record`] calls remain a no-op even if something probes the logger.
///
/// `dir` is the resolved log directory (see [`crate::resolver::log_dir`]); `threshold` is the
/// minimum level written; `retain_days` bounds retained daily files (`0` = keep all).
pub fn init(enabled: bool, threshold: LogLevel, dir: PathBuf, retain_days: i64) {
    let _ = LOGGER.set(enabled.then(|| Logger {
        threshold,
        dir,
        retain_days,
        open: Mutex::new(None),
    }));
}

/// Whether the action log is enabled. A strict no-op guard for callers that want to skip building
/// an expensive record when logging is off.
pub fn enabled() -> bool {
    matches!(LOGGER.get(), Some(Some(_)))
}

/// Append one structured record. Cheap early return when disabled or below the threshold. Failures
/// (dir/file/serialization) are swallowed — the action log must never mask the user-facing path.
pub fn record(
    level: LogLevel,
    subsystem: &str,
    action: &str,
    args: Value,
    result: Value,
    duration_ms: Option<u64>,
) {
    let Some(Some(logger)) = LOGGER.get() else {
        return;
    };
    if level < logger.threshold {
        return;
    }
    logger.write_record(level, subsystem, action, args, result, duration_ms);
}

/// Convenience for the common `info`-level action with string args and a boolean-ish result. Use
/// [`record`] directly for richer payloads. No-op when disabled.
pub fn action(subsystem: &str, action_name: &str, args: Value) {
    record(
        LogLevel::Info,
        subsystem,
        action_name,
        args,
        Value::Null,
        None,
    );
}

impl Logger {
    /// Build the redacted JSON record and append it. Split out from [`record`] so tests can drive
    /// a standalone [`Logger`] (temp dir) without touching the process-global [`LOGGER`].
    fn write_record(
        &self,
        level: LogLevel,
        subsystem: &str,
        action: &str,
        mut args: Value,
        mut result: Value,
        duration_ms: Option<u64>,
    ) {
        redact_value(&mut args);
        redact_value(&mut result);
        let mut obj = Map::new();
        obj.insert("ts".into(), json!(now_epoch_secs()));
        obj.insert("time".into(), json!(now_rfc3339_utc()));
        obj.insert("level".into(), json!(level.as_str()));
        obj.insert("subsystem".into(), json!(subsystem));
        obj.insert("action".into(), json!(action));
        if !args.is_null() {
            obj.insert("args".into(), args);
        }
        if !result.is_null() {
            obj.insert("result".into(), result);
        }
        if let Some(ms) = duration_ms {
            obj.insert("duration_ms".into(), json!(ms));
        }
        let rec = Value::Object(obj);
        if let Ok(line) = serde_json::to_string(&rec) {
            self.write_line(&line);
        }
    }

    /// Append `line` (already-serialized JSON, no trailing newline) to today's file, rotating the
    /// open handle on a date change and pruning stale daily files. Best-effort: errors are
    /// dropped.
    fn write_line(&self, line: &str) {
        let today = local_date_of(now_epoch_secs());
        let Ok(mut guard) = self.open.lock() else {
            return;
        };
        let need_open = !matches!(&*guard, Some((d, _)) if *d == today);
        if need_open {
            if std::fs::create_dir_all(&self.dir).is_err() {
                return;
            }
            self.prune(&today);
            let path = self.dir.join(format!("bn-{today}.jsonl"));
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                Ok(f) => *guard = Some((today, f)),
                Err(_) => return,
            }
        }
        if let Some((_, f)) = guard.as_mut() {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    }

    /// Delete `bn-*.jsonl` daily files older than `retain_days` before today. No-op when
    /// `retain_days <= 0`.
    fn prune(&self, today: &str) {
        if self.retain_days <= 0 {
            return;
        }
        let Some(cutoff) = crate::util::epoch_day(today).map(|d| d - self.retain_days) else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(date) = name
                .strip_prefix("bn-")
                .and_then(|s| s.strip_suffix(".jsonl"))
            else {
                continue;
            };
            if let Some(day) = crate::util::epoch_day(date) {
                if day < cutoff {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

/// Case-insensitive substrings that mark a JSON key (or `--flag`) as sensitive; its value is
/// masked in the log.
const SENSITIVE_KEYS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "authorization",
    "api_key",
    "apikey",
    "access_key",
    "private_key",
    "credential",
];

/// True if `key` names a sensitive value that must be redacted.
fn is_sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    SENSITIVE_KEYS.iter().any(|s| k.contains(s))
}

/// True if a bare string value looks like a credential (GitHub/OpenAI/AWS token prefixes or a long
/// opaque blob), so it can be masked even when it isn't behind a sensitive key.
fn looks_secret(v: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "sk-",
        "xoxb-",
        "xoxp-",
        "AKIA",
        "AIza",
    ];
    PREFIXES.iter().any(|p| v.starts_with(p))
}

/// Recursively mask sensitive values in a JSON payload: any object entry whose key is sensitive,
/// or any string that looks like a credential, becomes `"***"`.
pub fn redact_value(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if is_sensitive_key(k) {
                    *val = Value::String("***".into());
                } else {
                    redact_value(val);
                }
            }
        }
        Value::Array(arr) => {
            for val in arr.iter_mut() {
                redact_value(val);
            }
        }
        Value::String(s) if looks_secret(s) => {
            *s = "***".into();
        }
        _ => {}
    }
}

/// Redact a single argv token: mask a `--flag=value` when the flag is sensitive, mask a bare
/// credential-looking token, and pass everything else through. Flag/value *pairs* (`--token v`)
/// are handled by [`redact_args`], which has the neighbor context.
fn redact_token(tok: &str) -> String {
    if let Some((flag, val)) = tok.split_once('=') {
        if is_sensitive_key(flag.trim_start_matches('-')) && !val.is_empty() {
            return format!("{flag}=***");
        }
    }
    if looks_secret(tok) {
        return "***".into();
    }
    tok.to_string()
}

/// Redact a command's argv for logging: masks credential-looking tokens, `--flag=secret`, and the
/// value following a sensitive `--flag`. Returns owned strings safe to serialize.
pub fn redact_args<S: AsRef<str>>(args: &[S]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut mask_next = false;
    for a in args {
        let a = a.as_ref();
        if mask_next {
            out.push("***".to_string());
            mask_next = false;
            continue;
        }
        // `--token secret` / `--password secret`: mask the following value.
        if a.starts_with('-') && !a.contains('=') && is_sensitive_key(a.trim_start_matches('-')) {
            out.push(a.to_string());
            mask_next = true;
            continue;
        }
        out.push(redact_token(a));
    }
    out
}

/// A running timer for [`record`]'s `duration_ms`. `start()` at the beginning of an action,
/// `elapsed_ms()` when logging its result.
pub struct Timer(Instant);

impl Timer {
    pub fn start() -> Self {
        Timer(Instant::now())
    }
    pub fn elapsed_ms(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }
}
