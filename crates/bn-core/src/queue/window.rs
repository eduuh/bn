//! Priority windows — work that should be preferred at particular times of day.
//!
//! A queue orders work by importance, but importance is not constant. Warming repos, running the
//! heavy nightly sweeps, letting a long scout loose: none of that should compete with you at
//! 10am, and all of it is exactly what the machine should be doing while you are at lunch or after
//! you close the laptop.
//!
//! A window is a recurring span plus a priority delta. While it is open, matching work ranks
//! higher — it is preferred, not forced, so capacity, skills and pauses still apply. Nothing is
//! *scheduled* by a window; that is what a cron's `on_calendar` is for. This answers the different
//! question of what the machine should reach for first when it has a free hand.
//!
//! The window's own opening is an `OnCalendar` spec, so it uses the same schedule vocabulary as
//! everything else in bn rather than inventing a second one.
use crate::backlog::BacklogEntry;
use serde::{Deserialize, Serialize};

/// A recurring span during which some work is preferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorityWindow {
    /// What to call it in `bn queue status` — `lunch`, `evening`, `overnight`.
    pub name: String,
    /// When it opens, as an `OnCalendar` spec: `Mon..Fri *-*-* 12:30:00`.
    pub at: String,
    /// How long it stays open, in seconds.
    pub secs: i64,
    /// How much to raise matching work's priority while open.
    ///
    /// A delta rather than an absolute, so relative order *within* the boosted set is preserved —
    /// a window says "prefer this class of work now", not "make it all equally urgent".
    pub boost: i64,
    /// What it applies to: `kind:<k>`, `repo:<r>`, `source:<prefix>`, or `*` for everything.
    pub matches: Vec<String>,
}

impl PriorityWindow {
    /// Whether the window is open at `now`.
    ///
    /// Open means: its most recent firing is within `secs` of now. Reusing
    /// [`crate::staff_schedule::previous_occurrence`] means a window inherits every schedule shape
    /// bn already understands — weekdays, hour steps, concrete dates — instead of a parallel
    /// grammar that would drift from it.
    pub fn is_open(&self, now: i64) -> bool {
        crate::staff_schedule::previous_occurrence(&self.at, now)
            .is_some_and(|start| now >= start && now < start.saturating_add(self.secs.max(0)))
    }

    /// Whether this window applies to `entry`.
    ///
    /// `source:` matches by prefix, so `source:cron:` covers every scheduled job without listing
    /// them — the common case, since a window usually means "the background stuff", not one job.
    pub fn applies_to(&self, entry: &BacklogEntry) -> bool {
        self.matches.iter().any(|m| {
            let m = m.trim();
            if m == "*" {
                return true;
            }
            match m.split_once(':') {
                Some(("kind", k)) => entry.kind.label().eq_ignore_ascii_case(k),
                Some(("repo", r)) => entry.repo.eq_ignore_ascii_case(r),
                Some(("source", p)) => entry.source.starts_with(p),
                _ => false,
            }
        })
    }

    /// Seconds until this window next closes, for the status line. `None` when it is not open.
    pub fn closes_in(&self, now: i64) -> Option<i64> {
        crate::staff_schedule::previous_occurrence(&self.at, now)
            .map(|start| start.saturating_add(self.secs.max(0)))
            .filter(|end| *end > now)
            .map(|end| end - now)
    }
}

/// The total boost applying to `entry` at `now`.
///
/// Summed rather than maxed: overlapping windows are a deliberate statement — "it is both lunch
/// *and* a Friday" — and the arithmetic should say what the configuration says. Pure, so the
/// effective priority of any entry is reproducible from a captured snapshot.
pub fn boost_for(entry: &BacklogEntry, windows: &[PriorityWindow], now: i64) -> i64 {
    windows
        .iter()
        .filter(|w| w.is_open(now) && w.applies_to(entry))
        .fold(0i64, |acc, w| acc.saturating_add(w.boost))
}

/// The windows open at `now`, for the status surface.
pub fn open(windows: &[PriorityWindow], now: i64) -> Vec<&PriorityWindow> {
    windows.iter().filter(|w| w.is_open(now)).collect()
}
