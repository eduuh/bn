//! The auto-review ledger: which PRs the idle-triggered review loop is tracking, their
//! lifecycle stage, and the one-at-a-time lock. Pure state — no network, no git (the north
//! star: scripts fetch, bn renders). The cron picks the next PR, bn-cli does the worktree/az
//! work, and the verdict + lifecycle are recorded back here.
//!
//! Lifecycle: `pick_next` chooses an unreviewed (or force-pushed) PR → `begin` takes the lock
//! as the single `InFlight` entry → the review agent runs → `record` moves it to `Reviewed`
//! (lock released, worktree kept). The poll pass calls `needs_rereview` when new commits land
//! and `remove` once the PR completes or is abandoned. A crashed agent can't deadlock the loop:
//! an `InFlight` entry older than the lock TTL is treated as stale and superseded.

use crate::resolver::state_dir;
use crate::reviews::ReviewPr;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;

/// Minutes an `InFlight` entry may hold the lock before it's considered a crashed/abandoned
/// review and may be superseded. Generous: a fan-out review legitimately runs many minutes.
pub const LOCK_TTL_MINS: i64 = 60;

/// Where a tracked PR sits in its lifecycle. Terminal PRs (completed / abandoned) are removed
/// by the cleanup pass, so only live entries ever persist here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Setup done, the review agent is running. At most one entry is `InFlight` — it *is* the
    /// one-at-a-time lock.
    InFlight,
    /// A recommendation exists; the PR is still active. The worktree persists and the poll job
    /// keeps it fresh until the PR completes or is abandoned.
    Reviewed,
}

/// One PR the auto-review loop is tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Bare-repo name — resolves the worktree's `~/projects/bare/<repo>.git`.
    pub repo: String,
    pub number: i64,
    pub url: String,
    /// Source branch (resolved at setup): the worktree, and the ref the poll job re-fetches.
    pub branch: String,
    pub stage: Stage,
    /// Commit sha last reviewed; a newer branch tip ⇒ re-review. `None` until the first verdict.
    #[serde(default)]
    pub reviewed_sha: Option<String>,
    /// The recommendation: `approve` / `reject`, or `None` while in flight.
    #[serde(default)]
    pub verdict: Option<String>,
    /// The PR's `updated_at` from the queue when picked — dedup so a force-push re-triggers.
    #[serde(default)]
    pub updated_at: String,
    /// Epoch secs the entry was created (also the lock-acquired time for `InFlight`).
    pub first_seen: i64,
}

impl Entry {
    /// Stable identity for dedup and lookup: `repo#number`.
    pub fn key(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Ledger {
    #[serde(default)]
    pub entries: Vec<Entry>,
}

pub fn path() -> PathBuf {
    state_dir().join("auto-review.json")
}

/// Read the ledger. Missing or unparseable → empty (the loop just hasn't run yet), never an error.
pub fn read() -> Ledger {
    match std::fs::read_to_string(path()) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Ledger::default(),
    }
}

/// Persist the ledger, creating the state dir on demand.
pub fn write(ledger: &Ledger) -> io::Result<()> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(ledger).map_err(io::Error::other)?;
    std::fs::write(p, json)
}

impl Ledger {
    pub fn find(&self, repo: &str, number: i64) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.repo == repo && e.number == number)
    }

    fn find_mut(&mut self, repo: &str, number: i64) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .find(|e| e.repo == repo && e.number == number)
    }

    /// The current lock holder: the single `InFlight` entry, if any.
    pub fn in_flight(&self) -> Option<&Entry> {
        self.entries.iter().find(|e| e.stage == Stage::InFlight)
    }

    /// Whether a review is in flight and still within the lock TTL at `now`. A stale in-flight
    /// entry (crashed agent) does *not* count as locked, so the loop can recover.
    pub fn is_locked(&self, now: i64) -> bool {
        self.in_flight()
            .is_some_and(|e| now - e.first_seen < LOCK_TTL_MINS * 60)
    }

    /// The next PR to review from `queue`, or `None` if the lock is held or nothing is eligible.
    /// Eligible = tagged `review`, still open (a merged/closed/abandoned PR is history, skipped),
    /// and either not yet tracked or force-pushed since (its `updated_at` advanced past the tracked
    /// entry's). Tiebreak: longest review wait first.
    pub fn pick_next<'a>(&self, queue: &'a [ReviewPr], now: i64) -> Option<&'a ReviewPr> {
        if self.is_locked(now) {
            return None;
        }
        queue
            .iter()
            .filter(|pr| pr.tag == "review")
            .filter(|pr| !pr.is_dead())
            .filter(|pr| match self.find(&pr.repo, pr.number) {
                None => true,
                Some(e) => !pr.updated_at.is_empty() && pr.updated_at != e.updated_at,
            })
            .max_by(|a, b| {
                let wa = a.wait_hours(now).unwrap_or(0.0);
                let wb = b.wait_hours(now).unwrap_or(0.0);
                wa.partial_cmp(&wb).unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// Take the lock for `pr`: insert (or reset) its entry as `InFlight`. Returns `Err` if a
    /// non-stale review is already in flight (one-at-a-time). On a force-push re-trigger this
    /// replaces the existing entry, clearing the prior verdict.
    pub fn begin(&mut self, pr: &ReviewPr, branch: &str, now: i64) -> Result<(), String> {
        if let Some(holder) = self.in_flight() {
            if now - holder.first_seen < LOCK_TTL_MINS * 60 {
                return Err(format!("review already in flight: {}", holder.key()));
            }
        }
        self.entries
            .retain(|e| !(e.repo == pr.repo && e.number == pr.number));
        self.entries.push(Entry {
            repo: pr.repo.clone(),
            number: pr.number,
            url: pr.url.clone(),
            branch: branch.to_string(),
            stage: Stage::InFlight,
            reviewed_sha: None,
            verdict: None,
            updated_at: pr.updated_at.clone(),
            first_seen: now,
        });
        Ok(())
    }

    /// Record the recommendation and release the lock: `InFlight` → `Reviewed`, storing the
    /// verdict and the reviewed sha. No-op if the entry is gone (PR reclaimed mid-review).
    pub fn record(&mut self, repo: &str, number: i64, verdict: &str, sha: &str) {
        if let Some(e) = self.find_mut(repo, number) {
            e.stage = Stage::Reviewed;
            e.verdict = Some(verdict.to_string());
            e.reviewed_sha = Some(sha.to_string());
        }
    }

    /// Whether a reviewed PR has new commits since its last review (`current_sha` differs from
    /// the reviewed sha). False for in-flight or unknown PRs.
    pub fn needs_rereview(&self, repo: &str, number: i64, current_sha: &str) -> bool {
        self.find(repo, number)
            .filter(|e| e.stage == Stage::Reviewed)
            .and_then(|e| e.reviewed_sha.as_deref())
            .is_some_and(|reviewed| reviewed != current_sha)
    }

    /// Drop a PR from the ledger (its PR completed or was abandoned and the worktree reclaimed).
    pub fn remove(&mut self, repo: &str, number: i64) {
        self.entries
            .retain(|e| !(e.repo == repo && e.number == number));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(
        repo: &str,
        number: i64,
        tag: &str,
        updated: &str,
        created_ago_h: i64,
        now: i64,
    ) -> ReviewPr {
        // created_at = now - created_ago_h hours, as RFC-3339 (the shape parse_ts reads).
        let created = chrono::DateTime::from_timestamp(now - created_ago_h * 3600, 0)
            .unwrap()
            .to_rfc3339();
        ReviewPr {
            repo: repo.into(),
            number,
            title: format!("PR {number}"),
            author: "someone".into(),
            url: format!("https://example/{repo}/{number}"),
            updated_at: updated.into(),
            created_at: created,
            tag: tag.into(),
            state: "open".into(),
            review_decision: None,
        }
    }

    const NOW: i64 = 1_750_000_000;

    #[test]
    fn picks_longest_waiting_review_skips_learning() {
        let led = Ledger::default();
        let q = vec![
            pr("a", 1, "review", "u1", 2, NOW),
            pr("b", 2, "learning", "u2", 10, NOW), // ignored: not a review
            pr("c", 3, "review", "u3", 5, NOW),    // waited longest
        ];
        let chosen = led.pick_next(&q, NOW).expect("a review is eligible");
        assert_eq!((chosen.repo.as_str(), chosen.number), ("c", 3));
    }

    #[test]
    fn skips_dead_pr_even_when_longest_waiting() {
        let led = Ledger::default();
        // The dead PR waited longest, so it would win the tiebreak if it were eligible.
        let mut dead = pr("a", 1, "review", "u1", 10, NOW);
        dead.state = "merged".into();
        let live = pr("b", 2, "review", "u2", 2, NOW);
        let q = vec![dead, live];
        let chosen = led.pick_next(&q, NOW).expect("the live review is eligible");
        assert_eq!(
            (chosen.repo.as_str(), chosen.number),
            ("b", 2),
            "a merged/closed/abandoned PR is never auto-picked"
        );
        // With only the dead PR left in the queue, nothing is eligible.
        let mut only_dead = pr("a", 1, "review", "u1", 10, NOW);
        only_dead.state = "abandoned".into();
        assert!(
            led.pick_next(&[only_dead], NOW).is_none(),
            "a dead PR alone leaves nothing to pick"
        );
    }

    #[test]
    fn lock_blocks_until_ttl_then_recovers() {
        let mut led = Ledger::default();
        let q = vec![pr("a", 1, "review", "u1", 2, NOW)];
        led.begin(&q[0], "feat/x", NOW).unwrap();
        assert!(led.is_locked(NOW), "fresh in-flight entry holds the lock");
        assert!(led.pick_next(&q, NOW).is_none(), "locked → nothing picked");
        // A second begin while locked is rejected.
        assert!(led
            .begin(&pr("z", 9, "review", "u9", 1, NOW), "feat/z", NOW)
            .is_err());
        // Past the TTL the stale lock is ignored.
        let later = NOW + (LOCK_TTL_MINS + 1) * 60;
        assert!(!led.is_locked(later), "stale in-flight no longer locks");
    }

    #[test]
    fn skips_reviewed_pr_until_force_push() {
        let mut led = Ledger::default();
        led.begin(&pr("a", 1, "review", "u1", 2, NOW), "feat/x", NOW)
            .unwrap();
        led.record("a", 1, "approve", "sha1");
        // Same updated_at → already reviewed, not re-picked.
        let q_same = vec![pr("a", 1, "review", "u1", 2, NOW)];
        assert!(led.pick_next(&q_same, NOW).is_none());
        // Force-push bumps updated_at → eligible again.
        let q_pushed = vec![pr("a", 1, "review", "u2", 2, NOW)];
        assert!(led.pick_next(&q_pushed, NOW).is_some());
    }

    #[test]
    fn rereview_when_tip_moves_and_remove_clears() {
        let mut led = Ledger::default();
        led.begin(&pr("a", 1, "review", "u1", 2, NOW), "feat/x", NOW)
            .unwrap();
        led.record("a", 1, "approve", "sha1");
        assert!(
            !led.needs_rereview("a", 1, "sha1"),
            "same tip → no re-review"
        );
        assert!(led.needs_rereview("a", 1, "sha2"), "moved tip → re-review");
        led.remove("a", 1);
        assert!(led.find("a", 1).is_none());
    }

    #[test]
    fn roundtrips_through_disk() {
        let _g = crate::test_support::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _state_env = crate::test_support::EnvGuard::set("BN_STATE_DIR", dir.path());
        let mut led = Ledger::default();
        led.begin(&pr("a", 1, "review", "u1", 2, NOW), "feat/x", NOW)
            .unwrap();
        led.record("a", 1, "reject", "deadbeef");
        write(&led).unwrap();
        let back = read();
        let e = back.find("a", 1).expect("entry persisted");
        assert_eq!(e.stage, Stage::Reviewed);
        assert_eq!(e.verdict.as_deref(), Some("reject"));
        assert_eq!(e.reviewed_sha.as_deref(), Some("deadbeef"));
    }
}
