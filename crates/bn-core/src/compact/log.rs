//! `compactions.jsonl`: both ends of every compaction, and the restore's honest outcome.

use crate::resolver::{Config, Resolved};
use serde::{Deserialize, Serialize};
use std::io::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Before: recap and handoff written.
    Prep,
    /// After: the seat was re-oriented.
    Restore,
}

/// How a seat came back. Honest by design: a restore is reported, never promised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// Came back onto a recap written just before.
    Resumed,
    /// No recent recap; re-oriented from the note alone.
    Rebuilt,
    /// A new session with no seed.
    Fresh,
    /// A new session seeded from the handoff.
    FreshPrimed,
    /// Prepared, but nothing has come back yet.
    Waiting,
    /// Couldn't re-orient (the note is unreadable).
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Epoch seconds.
    pub ts: i64,
    pub phase: Phase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// `manual` | `auto` | `deliberate` | `relaunch`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_pct: Option<u8>,
    #[serde(default)]
    pub recap: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
}

fn path(cfg: &Config, ctx: &Resolved) -> std::path::PathBuf {
    cfg.note_dir(ctx).join("compactions.jsonl")
}

pub fn append(cfg: &Config, ctx: &Resolved, e: &Entry) -> std::io::Result<()> {
    std::fs::create_dir_all(cfg.note_dir(ctx))?;
    let line = serde_json::to_string(e).map_err(std::io::Error::other)?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path(cfg, ctx))?;
    writeln!(f, "{line}")
}

/// Every entry, oldest first. Unreadable lines are skipped.
pub fn entries(cfg: &Config, ctx: &Resolved) -> Vec<Entry> {
    std::fs::read_to_string(path(cfg, ctx))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// A prep counts for a restore when it is at most this old.
pub const PREP_FRESH_SECS: i64 = 30 * 60;

/// How a restore at `now` went, given the log so far: onto a fresh prep with a recap is
/// `resumed`; anything else was rebuilt from the note.
pub fn outcome_for_restore(log: &[Entry], now: i64, note_readable: bool) -> Outcome {
    if !note_readable {
        return Outcome::Failed;
    }
    // A `waiting` marker (a deliberate compaction that hadn't come back yet) isn't a restore, so
    // it doesn't use up the prep before it.
    let last_prep = log
        .iter()
        .rev()
        .filter(|e| e.outcome != Some(Outcome::Waiting))
        .take_while(|e| e.phase == Phase::Prep)
        .find(|e| e.recap && now - e.ts <= PREP_FRESH_SECS);
    if last_prep.is_some() {
        Outcome::Resumed
    } else {
        Outcome::Rebuilt
    }
}
