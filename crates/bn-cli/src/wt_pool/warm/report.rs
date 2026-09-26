//! What a warm did to each slot, and whether that counts as ok.
//!
//! "Consistent" build worktrees means every slot a warm touches ends on the one commit fetched at
//! the start of the run, built. A summary count ("Warmed 2/4") can't say which slot is on what or
//! why one wasn't built, and it let a skip read as success. So each slot gets a line: the commit it
//! is on and what happened. The run is ok only when nothing went wrong — a slot somebody is using
//! is left alone and says so, but a dirty slot, a build that was skipped because another build held
//! the toolchain, a failed build, or a failed fetch all make the run not ok.

use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Outcome {
    /// `bn build` finished.
    Built,
    /// No build script: a live checkout at the base is all there is to warm.
    CheckedOut,
    /// Left alone on purpose, and that is fine: leased, in use, or excluded by `--new`.
    Skipped(String),
    /// Not built, and that is a problem: dirty, or its build declined to run.
    NotBuilt(String),
    /// The checkout or the build failed.
    Failed(String),
}

impl Outcome {
    fn ok(&self) -> bool {
        matches!(
            self,
            Outcome::Built | Outcome::CheckedOut | Outcome::Skipped(_)
        )
    }

    fn label(&self) -> (&'static str, Option<&str>) {
        match self {
            Outcome::Built => ("built", None),
            Outcome::CheckedOut => ("checked out", None),
            Outcome::Skipped(why) => ("skipped", Some(why)),
            Outcome::NotBuilt(why) => ("not built", Some(why)),
            Outcome::Failed(why) => ("FAILED", Some(why)),
        }
    }
}

pub(super) struct SlotReport {
    pub n: u32,
    /// The commit the slot is on after the warm, when git could say.
    pub commit: Option<String>,
    pub outcome: Outcome,
}

pub(super) struct WarmReport {
    pub base: String,
    pub fetch_error: Option<String>,
    pub slots: Vec<SlotReport>,
}

impl WarmReport {
    pub fn record(&mut self, n: u32, commit: Option<String>, outcome: Outcome) {
        self.slots.retain(|s| s.n != n);
        self.slots.push(SlotReport { n, commit, outcome });
        self.slots.sort_by_key(|s| s.n);
    }

    pub fn ok(&self) -> bool {
        self.fetch_error.is_none() && self.slots.iter().all(|s| s.outcome.ok())
    }

    pub fn count(&self, pick: impl Fn(&Outcome) -> bool) -> u32 {
        self.slots.iter().filter(|s| pick(&s.outcome)).count() as u32
    }

    /// One line per slot, for the job log: `[3] built @ 1a2b3c4d5e` or
    /// `[2] skipped @ 1a2b3c4d5e: leased by agent-7 (feature/x)`.
    pub fn lines(&self) -> Vec<String> {
        self.slots
            .iter()
            .map(|s| {
                let (what, why) = s.outcome.label();
                let at = s.commit.as_deref().map(short).unwrap_or("?");
                match why {
                    Some(why) => format!("[{}] {what} @ {at}: {why}", s.n),
                    None => format!("[{}] {what} @ {at}", s.n),
                }
            })
            .collect()
    }

    /// The per-slot detail for the `pool_warm` event.
    pub fn slots_json(&self) -> Value {
        self.slots
            .iter()
            .map(|s| {
                let (what, why) = s.outcome.label();
                json!({ "slot": s.n, "commit": s.commit, "outcome": what, "reason": why })
            })
            .collect()
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(outcomes: Vec<Outcome>) -> WarmReport {
        let mut r = WarmReport {
            base: "c0ffee".into(),
            fetch_error: None,
            slots: Vec::new(),
        };
        for (i, o) in outcomes.into_iter().enumerate() {
            r.record(i as u32 + 1, Some("c0ffee".into()), o);
        }
        r
    }

    #[test]
    fn a_leased_slot_is_reported_but_does_not_fail_the_run() {
        let r = report(vec![
            Outcome::Built,
            Outcome::Skipped("leased by a (b)".into()),
        ]);
        assert!(r.ok());
        assert_eq!(r.lines()[1], "[2] skipped @ c0ffee: leased by a (b)");
    }

    #[test]
    fn a_declined_build_a_dirty_slot_or_a_failure_is_never_ok() {
        for bad in [
            Outcome::NotBuilt("another lage build was running".into()),
            Outcome::NotBuilt("dirty".into()),
            Outcome::Failed("build failed".into()),
        ] {
            assert!(!report(vec![Outcome::Built, bad.clone()]).ok(), "{bad:?}");
        }
    }

    #[test]
    fn a_failed_fetch_is_never_ok_even_when_every_slot_built() {
        let mut r = report(vec![Outcome::Built]);
        r.fetch_error = Some("curl 56".into());
        assert!(!r.ok());
    }

    #[test]
    fn a_retried_slot_keeps_only_its_last_outcome() {
        let mut r = report(vec![Outcome::NotBuilt("busy".into())]);
        r.record(1, Some("c0ffee".into()), Outcome::Built);
        assert_eq!(r.slots.len(), 1);
        assert!(r.ok());
    }
}
