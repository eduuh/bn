//! How a finished run is classified: [`RunStatus`] and [`classify_run`].
//!
//! One process finished — did it work, is the login dead, did the shared account hit its usage
//! window, or did it genuinely break? Those four answers need different responses, and only the
//! first is "nothing to do". Guessing from an exit code alone cannot tell an expired token from a
//! real bug, so `classify_run` reads the captured output for the markers that distinguish them.
//!
//! Auth markers win over rate-limit markers: a 401 that happens to mention limits is still a login
//! problem, and telling someone to wait when they need to run `/login` wastes the whole window.
//!
//! Lived in the staff roster while that was its only caller; the CI-run reader
//! ([`crate::remote`]) uses the same taxonomy, so it is not staff-specific.

use serde::{Deserialize, Serialize};

/// The outcome of one scheduled staff run. Serialized into run records with kebab-case names
/// (`ok` / `fail` / `auth` / `rate-limited` / `skipped` / `off`) — the taxonomy #271 settled on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    /// The run completed and its output validated.
    Ok,
    /// The run genuinely broke — the only status that should raise a warn attention item.
    Fail,
    /// Auth is dead (expired / revoked / not logged in). No retry can fix this; it needs a
    /// human to run `/login`, so surface it distinctly and don't burn retries on it.
    Auth,
    /// The shared account hit its usage window. Not a failure — retry later or skip.
    RateLimited,
    /// Policy chose not to run (gate, or retry-later budget spent). Assigned by the
    /// scheduler, never by [`classify_run`].
    Skipped,
    /// The agent is switched **off** by configuration — its opt-in flag is unset, or its repo
    /// allowlist is empty. The scheduler fired it and it correctly did nothing.
    ///
    /// Distinct from [`Skipped`](RunStatus::Skipped), which is a *temporary* refusal a person can
    /// clear (backpressure, a retry budget). `Off` is a standing choice, so it must never be
    /// dressed up as a problem: an agent you deliberately turned off has not missed anything, and
    /// painting it red trains you to ignore the colour that means a real failure.
    Off,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Ok => "ok",
            RunStatus::Fail => "fail",
            RunStatus::Auth => "auth",
            RunStatus::RateLimited => "rate-limited",
            RunStatus::Skipped => "skipped",
            RunStatus::Off => "off",
        }
    }

    /// Parse the kebab-case wire name back into a status. `None` for anything unrecognized, so a
    /// record written by a newer version degrades to "unknown" rather than a wrong status.
    pub fn parse(s: &str) -> Option<RunStatus> {
        Some(match s {
            "ok" => RunStatus::Ok,
            "fail" => RunStatus::Fail,
            "auth" => RunStatus::Auth,
            "rate-limited" => RunStatus::RateLimited,
            "skipped" => RunStatus::Skipped,
            "off" => RunStatus::Off,
            _ => return None,
        })
    }
}

/// Auth-family markers in the claude CLI's error output. Every login-shaped failure the CLI
/// emits carries one of these ("Not logged in · Please run /login", "API Error: 401 Invalid
/// API key · Please run /login", "OAuth token revoked · Please run /login", "Session expired.
/// Please run /login to sign in again."). Matched case-insensitively.
const AUTH_MARKERS: &[&str] = &[
    "please run /login",
    "not logged in",
    "oauth token revoked",
    "oauth token has expired",
    "invalid api key",
    "authentication_error",
];

/// Rate-limit-family markers: the subscription window ("usage limit reached", legacy
/// "Claude AI usage limit reached|<epoch>") and the API's 429/529 error types. Overload is
/// grouped here because the remedy is identical — wait, don't page.
const RATE_LIMIT_MARKERS: &[&str] = &[
    "usage limit reached",
    "rate_limit_error",
    "overloaded_error",
];

/// Classify one finished harness run into the taxonomy. `exit_success` is the process exit;
/// `output` is the run's captured stdout+stderr (the CLI prints auth errors to stdout, so
/// feed it both). Success is `ok` no matter what the text says; on failure, auth markers win
/// over rate-limit markers (a 401 mentioning limits is still a login problem), and anything
/// unrecognized is an honest `fail`.
pub fn classify_run(exit_success: bool, output: &str) -> RunStatus {
    if exit_success {
        return RunStatus::Ok;
    }
    let lower = output.to_lowercase();
    if AUTH_MARKERS.iter().any(|m| lower.contains(m)) {
        return RunStatus::Auth;
    }
    if RATE_LIMIT_MARKERS.iter().any(|m| lower.contains(m)) {
        return RunStatus::RateLimited;
    }
    RunStatus::Fail
}

pub fn failure_detail(output: &str) -> String {
    let mut tail: Vec<&str> = output
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(3)
        .collect();
    tail.reverse();
    sanitize_line(&tail.join(" · "))
}

fn sanitize_line(s: &str) -> String {
    truncate_chars(&strip_terminal_escapes(s, false), MAX_TITLE_CHARS)
}

const MAX_TITLE_CHARS: usize = 300;

fn strip_terminal_escapes(s: &str, keep_newlines: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                // CSI: params/intermediates up to and including a final byte 0x40..=0x7E.
                Some('[') => {
                    chars.next();
                    while let Some(&n) = chars.peek() {
                        chars.next();
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC: everything up to a BEL or a String Terminator (ESC \).
                Some(']') => {
                    chars.next();
                    while let Some(&n) = chars.peek() {
                        if n == '\u{07}' {
                            chars.next();
                            break;
                        }
                        if n == '\u{1b}' {
                            chars.next();
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                        chars.next();
                    }
                }
                // Any other escape (e.g. `ESC c` reset, `ESC (B`): drop the single next byte.
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            '\n' | '\t' if keep_newlines => out.push(c),
            c if (c as u32) < 0x20 => {} // other C0 controls (incl. lone CR)
            '\u{7f}' => {}               // DEL
            c if ('\u{80}'..='\u{9f}').contains(&c) => {} // C1 controls (alt escape introducers)
            c => out.push(c),
        }
    }
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}
