//! What a due watchdog should do, given what is true about its seat right now. Pure, so every
//! kind's rule is a table test.

use super::watchdog::{Kind, Watchdog};

/// Default quiet time before `keepalive` counts a seat as idle.
pub const DEFAULT_IDLE_SECS: i64 = 10 * 60;
/// Default `context` threshold, in percent of the window.
pub const DEFAULT_CONTEXT_PCT: u8 = 80;

/// What the caller learned about the watchdog's seat before deciding.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// Seconds since the seat's session last did anything; `None` when no session was found.
    pub idle_secs: Option<i64>,
    /// Context-window use, when it can be measured. Never guessed: unknown is `None`, not 0.
    pub context_pct: Option<u8>,
}

/// The decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Act: deliver this text.
    Fire(String),
    /// Nothing to do this time; the condition doesn't hold.
    Skip(String),
    /// Couldn't tell — a fact it needs is missing. Fail open: never fire on a guess.
    Unavailable(String),
}

pub fn decide(wd: &Watchdog, f: &Facts) -> Decision {
    let text = |default: &str| wd.text.clone().unwrap_or_else(|| default.to_string());
    let idle_need = wd.spec.idle_secs.unwrap_or(DEFAULT_IDLE_SECS);
    match wd.kind() {
        Some(Kind::Reminder) => Decision::Fire(text("reminder")),
        Some(Kind::Keepalive) => match f.idle_secs {
            None => Decision::Unavailable("no running session for this seat".into()),
            Some(idle) if idle >= idle_need => Decision::Fire(text(
                "keepalive: you have been quiet a while — still on it? If you are blocked, say so \
                 with `bn add blocker`.",
            )),
            Some(idle) => Decision::Skip(format!("active {idle}s ago")),
        },
        Some(Kind::Context) => {
            let need = wd.spec.threshold_pct.unwrap_or(DEFAULT_CONTEXT_PCT);
            match f.context_pct {
                None => Decision::Unavailable("context usage unknown".into()),
                Some(pct) if pct >= need => Decision::Fire(text(&format!(
                    "Context at {pct}% (threshold {need}%). Compaction is not an unblock: write a \
                     recap and plan it first — bn compact plan"
                ))),
                Some(pct) => Decision::Skip(format!("context at {pct}%")),
            }
        }
        None => Decision::Unavailable(format!("unknown watchdog kind '{}'", wd.kind)),
    }
}
