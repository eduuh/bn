//! Attention snapshot cache — the prefix-J doctor picker (`bn doctor --pick`) recomputes the whole
//! finding list on every open *and* on every in-popup action-reload, and `note_findings` makes that
//! I/O-heavy (one `store::read` YAML parse per branch note — hundreds on a busy machine, seconds of
//! wall time). This mirrors the `next` attention cache: a short-lived on-disk snapshot of the
//! global (`repo = None`) finding list so the picker opens instantly and its reloads stay snappy.
//! Kept warm alongside the `next` cache by the `attention-sync` cron, self-heals on a miss/stale
//! read, and ctrl-r forces a live recompute + rewrite.
//!
//! Split out of `doctor.rs` to keep it under the 1000-line ratchet (see `.file-size-baseline`).
//! Everything here is re-exported flat from `doctor`, so `doctor::cache_path()`,
//! `doctor::cached_or_compute(..)` and friends keep resolving unchanged.

use super::{doctor, Finding};
use crate::resolver::Config;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Maximum age for the precomputed doctor snapshot. Matches the `next` cache: the `attention-sync`
/// warmer runs every five minutes, and this ceiling keeps a stuck warmer from serving a stale list
/// forever — the first read past it recomputes live and rewrites.
const CACHE_MAX_AGE_SECS: i64 = 10 * 60;

/// Path to the precomputed doctor snapshot — the global attention list the prefix-J picker serves.
pub fn cache_path() -> PathBuf {
    crate::resolver::state_dir().join("doctor.json")
}

/// Borrowed view for writing (no `Clone` on `Finding` required).
#[derive(Serialize)]
struct DoctorCacheWrite<'a> {
    generated_at_epoch: i64,
    stale_days: i64,
    findings: &'a [Finding],
}

/// Owned view for reading back the snapshot.
#[derive(Deserialize)]
struct DoctorCacheRead {
    #[serde(default)]
    generated_at_epoch: i64,
    #[serde(default)]
    stale_days: i64,
    findings: Vec<Finding>,
}

fn write_cache_at(findings: &[Finding], stale_days: i64, now: i64) -> std::io::Result<()> {
    let json = serde_json::to_string(&DoctorCacheWrite {
        generated_at_epoch: now,
        stale_days,
        findings,
    })
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let path = cache_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)
}

/// Persist an already-computed global finding list, stamped now. The picker calls this after
/// trimming a just-acted row so the reload serves the trimmed list without a recompute.
pub fn write_cache(findings: &[Finding], stale_days: i64) -> std::io::Result<()> {
    write_cache_at(findings, stale_days, crate::util::now_epoch_secs())
}

/// Raw read of the cached list regardless of age — used by the picker to trim a just-acted row.
/// `None` when the snapshot is absent or unparseable.
pub fn read_cache() -> Option<Vec<Finding>> {
    let text = std::fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str::<DoctorCacheRead>(&text)
        .ok()
        .map(|c| c.findings)
}

/// Read the cached list only when it is fresh *and* was computed under the same `stale_days`.
/// Older, mismatched, or future-dated snapshots read as a miss so the caller recomputes live and
/// rewrites (a future stamp — clock skew / corruption — would otherwise freeze a stale list).
pub fn read_fresh_cache(stale_days: i64, now: i64) -> Option<Vec<Finding>> {
    let text = std::fs::read_to_string(cache_path()).ok()?;
    let cache = serde_json::from_str::<DoctorCacheRead>(&text).ok()?;
    let ts = cache.generated_at_epoch;
    if cache.stale_days != stale_days
        || ts <= 0
        || ts > now
        || now.saturating_sub(ts) > CACHE_MAX_AGE_SECS
    {
        return None;
    }
    Some(cache.findings)
}

/// Remove the precomputed doctor snapshot. `Ok(false)` when there was nothing to clear.
pub fn clear_cache() -> std::io::Result<bool> {
    match std::fs::remove_file(cache_path()) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Recompute the global finding list and rewrite the snapshot, returning it. The forced-refresh
/// path (ctrl-r) and the self-heal on a cache miss.
pub fn recompute_and_cache(cfg: &Config, today: &str, stale_days: i64, now: i64) -> Vec<Finding> {
    let findings = doctor(cfg, today, stale_days, None);
    crate::util::best_effort(
        write_cache_at(&findings, stale_days, now),
        "doctor: warm attention cache",
    );
    findings
}

/// Warm the snapshot without returning it — the write-only path the `attention-sync` cron calls.
pub fn refresh_cache(cfg: &Config, today: &str, stale_days: i64, now: i64) -> std::io::Result<()> {
    write_cache_at(&doctor(cfg, today, stale_days, None), stale_days, now)
}

/// Serve a fresh snapshot when available, else compute live and warm the cache — the picker's
/// instant-open fast path.
pub fn cached_or_compute(cfg: &Config, today: &str, stale_days: i64, now: i64) -> Vec<Finding> {
    match read_fresh_cache(stale_days, now) {
        Some(f) => f,
        None => recompute_and_cache(cfg, today, stale_days, now),
    }
}
