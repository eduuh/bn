//! A TTL cache for the expensive read-only view payloads.
//!
//! # Why
//!
//! The dashboard shell fires ten reads on every page load — the rail's counts, the topbar's
//! machine vitals, plus whatever the route itself needs — and each one recomputes its view from
//! the filesystem. Measured on `/repos`: `/api/cron` 783ms, `/api/actions` 256ms, `/api/repos`
//! 255ms. The assets are already free (the service worker serves them from cache with zero
//! network), so those reads *are* the page load.
//!
//! Two things make that worse than the numbers suggest. The reads are duplicated — every
//! navigation back to a screen recomputes what it computed seconds ago, and a second browser tab
//! doubles it. And they are concurrent — ten heavy synchronous reads land at once, so they queue
//! behind each other instead of overlapping.
//!
//! This fixes both. A hit inside the TTL costs a clone of an `Arc`, and concurrent misses on one
//! key collapse into a single computation that the waiters share rather than each repeating.
//!
//! # What it is not
//!
//! Not a correctness layer, and not a substitute for liveness. The TTLs here are short and chosen
//! per view from how fast that view's source actually changes (see [`CRON`], [`REPOS`],
//! [`ACTIONS`]); the client's
//! `refetchInterval` and the SSE stream still carry liveness exactly as before. A cached view is
//! never served past its TTL, and a write that invalidates a view can drop it immediately with
//! [`ViewCache::invalidate`].
//!
//! # Why there is no eviction
//!
//! Deliberately in-process and never evicted, which is only safe because the key space is finite:
//! every view keys on its `&'static str` name, so no key can come from a query string.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A cacheable view: its key and how long it stays fresh, named together so a call site cannot
/// pair one view's key with another's window.
#[derive(Clone, Copy)]
pub struct View {
    key: &'static str,
    ttl: Duration,
}

/// The cron registry: schedules from `crons.toml` joined with last-run health. Schedules change when
/// you edit a file; health changes when a job finishes. Neither is sub-minute, and at ~780ms this is
/// the most expensive view on the shell's critical path, so it gets the longest window.
pub const CRON: View = View {
    key: "cron",
    ttl: Duration::from_secs(15),
};

/// The repo inventory: clones, worktrees, notes, pool state. Bounded by how fast you can create a
/// worktree or land a note. The pool controls that write it invalidate explicitly, so this window
/// never stands between a click and its effect.
pub const REPOS: View = View {
    key: "repos",
    ttl: Duration::from_secs(10),
};

/// How long the two "something needs you" views stay fresh. Shorter than the rest, because these
/// are the views whose whole job is to surface a signal — staleness here has a cost the inventory
/// views don't have.
const SIGNAL_TTL: Duration = Duration::from_secs(5);

/// The action feed: seven sources folded into one ranked list.
pub const ACTIONS: View = View {
    key: "actions",
    ttl: SIGNAL_TTL,
};

/// The open attention items behind the rail's Attention badge.
pub const ATTN: View = View {
    key: "attn",
    ttl: SIGNAL_TTL,
};

/// The live views, and why they are cached at all.
///
/// `/api/fleet`, `/api/machine` and `/api/nodes` each call `agent::supervise` — a full walk of every
/// live session — and `/api/repos` calls it a fourth time. One page load supervised the fleet four
/// times over, which is the largest piece of duplicated work the shell had.
///
/// These are live data, so the window is set below the fastest thing that reads them: the topbar
/// polls `/api/machine` every 4s and the fleet is patched live by the SSE stream in between polls.
/// At 2s no poll can ever be served a value from before its predecessor, so nothing observable
/// changes — the duplicate work inside a single page load is all that disappears.
const LIVE_TTL: Duration = Duration::from_secs(2);

/// Live agents, their health, and harness availability.
pub const FLEET: View = View {
    key: "fleet",
    ttl: LIVE_TTL,
};

/// Host vitals (CPU, RAM) plus the live agent count.
pub const MACHINE: View = View {
    key: "machine",
    ttl: LIVE_TTL,
};

/// The machines behind the fleet, as a list.
pub const NODES: View = View {
    key: "nodes",
    ttl: LIVE_TTL,
};

/// The completed-work record. Unparameterized, and by definition about work that has already
/// finished, so it tolerates the longest window of any view here.
pub const COMPLETED: View = View {
    key: "completed",
    ttl: Duration::from_secs(30),
};

/// The cross-repo open-issue sweep: one `gh issue list` per repo, fanned out.
///
/// The longest window here, and the one that most needs it. `bn_core::issues` already caches each
/// repo for ten minutes — but only on *success*, deliberately, so one flaky minute cannot become a
/// ten-minute outage. The consequence is that unreachable repos re-pay a 20s timeout on every
/// sweep, which is what made this endpoint take 14 to 70 seconds while sitting on the shell's
/// critical path. Parallel fetches cut the wall clock; this stops the dashboard's poll loop from
/// re-running the sweep at all between windows.
pub const ISSUES: View = View {
    key: "issues",
    ttl: Duration::from_secs(120),
};

/// One cached view: the value and the moment it was computed.
struct Cached {
    at: Instant,
    value: Arc<Value>,
}

/// A key's slot. The async mutex is what makes a miss single-flight: concurrent callers for the
/// same key queue here, and all but the first find a fresh value already in place when they get in.
#[derive(Default)]
struct Slot {
    cached: tokio::sync::Mutex<Option<Cached>>,
}

/// The cache itself — one slot per view name. Cheap to clone (everything is behind an `Arc`).
#[derive(Clone, Default)]
pub struct ViewCache {
    /// The std `Mutex` guards only the slot *lookup*, which never awaits; the per-slot async mutex
    /// is what a slow computation is held across. Keeping those two locks separate is what stops
    /// one expensive view from blocking lookups of every other view.
    slots: Arc<Mutex<HashMap<String, Arc<Slot>>>>,
}

impl ViewCache {
    pub fn new() -> ViewCache {
        ViewCache::default()
    }

    /// The slot for `key`, created on first use.
    fn slot(&self, key: &str) -> Arc<Slot> {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = slots.get(key) {
            return Arc::clone(slot);
        }
        Arc::clone(slots.entry(key.to_string()).or_default())
    }

    /// Serve `key` from cache, or compute it and store it.
    ///
    /// `compute` is synchronous, filesystem-heavy work, so it runs on the blocking pool rather than
    /// on a runtime worker — these handlers used to run it inline on the async runtime, which is
    /// why ten concurrent reads serialized instead of overlapping.
    ///
    /// A panic inside `compute` is not cached and not swallowed into a wrong value: the error
    /// surfaces to the caller as `None` so the handler can answer honestly.
    pub async fn get_or_compute<F>(&self, view: View, compute: F) -> Option<Arc<Value>>
    where
        F: FnOnce() -> Value + Send + 'static,
    {
        let ttl = view.ttl;
        let slot = self.slot(view.key);
        let mut guard = slot.cached.lock().await;

        // `elapsed() < ttl` — a value exactly at the TTL is stale, so the window is a real bound.
        if let Some(hit) = guard.as_ref().filter(|c| c.at.elapsed() < ttl) {
            return Some(Arc::clone(&hit.value));
        }

        // Stamp the time from *before* the computation: a view that takes 600ms to build describes
        // the world as it was when the read started, and dating it from the end would let a slow
        // view claim to be fresher than it is.
        let started = Instant::now();
        let computed = tokio::task::spawn_blocking(compute).await.ok()?;
        let value = Arc::new(computed);
        *guard = Some(Cached {
            at: started,
            value: Arc::clone(&value),
        });
        Some(value)
    }

    /// Drop a cached view so the next read recomputes it. For writes whose effect the user expects
    /// to see immediately, rather than at the end of a TTL they cannot observe.
    pub async fn invalidate(&self, view: View) {
        let slot = {
            let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            slots.get(view.key).map(Arc::clone)
        };
        if let Some(slot) = slot {
            *slot.cached.lock().await = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A view with a window long enough that nothing expires mid-test.
    const V: View = View {
        key: "v",
        ttl: Duration::from_secs(60),
    };

    /// A compute closure that counts how many times it actually ran.
    fn counting(calls: &Arc<AtomicUsize>) -> impl FnOnce() -> Value + Send + 'static {
        let calls = Arc::clone(calls);
        move || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            serde_json::json!({ "n": n })
        }
    }

    #[tokio::test]
    async fn a_hit_inside_the_ttl_does_not_recompute() {
        let cache = ViewCache::new();
        let calls = Arc::new(AtomicUsize::new(0));

        let first = cache.get_or_compute(V, counting(&calls)).await;
        let second = cache.get_or_compute(V, counting(&calls)).await;

        assert_eq!(calls.load(Ordering::SeqCst), 1, "second read was a hit");
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn a_zero_ttl_never_serves_a_cached_value() {
        // The boundary is `<`, not `<=`, so a TTL of zero means "always recompute" rather than
        // "cache forever" — the failure mode that would quietly freeze a live view.
        let cache = ViewCache::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let always_stale = View {
            key: "v",
            ttl: Duration::ZERO,
        };

        cache.get_or_compute(always_stale, counting(&calls)).await;
        cache.get_or_compute(always_stale, counting(&calls)).await;

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn keys_do_not_share_a_slot() {
        let cache = ViewCache::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let other = View { key: "b", ..V };

        cache.get_or_compute(V, counting(&calls)).await;
        cache.get_or_compute(other, counting(&calls)).await;

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "each view has its own slot"
        );
    }

    #[tokio::test]
    async fn invalidate_forces_the_next_read_to_recompute() {
        let cache = ViewCache::new();
        let calls = Arc::new(AtomicUsize::new(0));

        cache.get_or_compute(V, counting(&calls)).await;
        cache.invalidate(V).await;
        cache.get_or_compute(V, counting(&calls)).await;

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn concurrent_misses_collapse_into_one_computation() {
        // The point of the whole module: ten tabs asking at once must cost one rollup, not ten.
        let cache = Arc::new(ViewCache::new());
        let calls = Arc::new(AtomicUsize::new(0));

        let racers: Vec<_> = (0..10)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let calls = Arc::clone(&calls);
                tokio::spawn(async move {
                    cache
                        .get_or_compute(V, {
                            let calls = Arc::clone(&calls);
                            move || {
                                calls.fetch_add(1, Ordering::SeqCst);
                                // Long enough that every racer is queued behind the first.
                                std::thread::sleep(Duration::from_millis(50));
                                serde_json::json!("done")
                            }
                        })
                        .await
                })
            })
            .collect();

        let mut results = Vec::new();
        for r in racers {
            results.push(r.await.expect("racer joined"));
        }

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "one computation, nine hits"
        );
        assert!(results.iter().all(|r| r.is_some()));
    }

    #[tokio::test]
    async fn invalidating_one_view_leaves_another_view_alone() {
        let cache = ViewCache::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let other = View { key: "other", ..V };

        cache.get_or_compute(V, counting(&calls)).await;
        cache.get_or_compute(other, counting(&calls)).await;
        cache.invalidate(V).await;
        cache.get_or_compute(other, counting(&calls)).await;

        assert_eq!(calls.load(Ordering::SeqCst), 2, "`other` was still a hit");
    }

    #[tokio::test]
    async fn a_panicking_compute_is_reported_and_not_cached() {
        let cache = ViewCache::new();

        let failed = cache.get_or_compute(V, || panic!("boom")).await;
        assert!(failed.is_none(), "the caller learns the view failed");

        // The panic left nothing behind, so a later good read still populates the slot.
        let ok = cache
            .get_or_compute(V, || serde_json::json!("recovered"))
            .await;
        assert_eq!(ok.as_deref(), Some(&serde_json::json!("recovered")));
    }
}
