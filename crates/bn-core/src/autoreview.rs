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


