//! Worker capacity snapshot (#719) — the admission signals a scheduler (#720) reads to decide
//! whether this node can take another attempt: RAM (`MemAvailable`), swap headroom, 1-min load
//! per core, and free disk on the worktree volume.
//!
//! Split bn-style: the fiddly format parsing is pure ([`crate::machine::parse_meminfo`],
//! [`crate::machine::parse_proc_loadavg`], [`parse_df_avail_mb`]) so it is unit-testable without a
//! Linux host; only [`sample`] touches the machine. Every field is best-effort — an unreadable
//! signal reads as `None`/unknown and never fabricates capacity.

use crate::machine;
use serde::{Deserialize, Serialize};

/// Fraction of `swap` that must stay free; below this the node is under memory pressure. Mirrors
/// the epic #707 v4 admission signal (`SwapFree/SwapTotal < 0.2` → pressure).
pub const SWAP_FREE_MIN_FRAC: f64 = 0.2;
/// `loadavg[0] / nproc` at/above which the node is under CPU pressure (epic #707 v4).
pub const LOAD_PER_CORE_MAX: f64 = 0.85;

/// The node's admission status derived from its capacity snapshot. `DEAD` is a *remote* verdict
/// (a peer can't reach the worker at all) and so is never produced by a local `sample`; it exists
/// in the enum for the registry/scheduler that reconcile liveness across SSH (#720).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum NodeStatus {
    /// Comfortably below every pressure threshold — free to admit new attempts.
    Ready,
    /// Above a load/swap pressure threshold — admit conservatively (or not at all).
    Pressure,
    /// Unreachable/unreconcilable (remote verdict only; never from a local sample).
    Dead,
}

/// A point-in-time capacity reading for one worker node. All figures are MB except `loadavg1`
/// (raw 1-min load) and `nproc` (logical core count).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Capacity {
    /// Total physical RAM.
    pub mem_total_mb: Option<u64>,
    /// `MemAvailable` — the kernel's estimate of allocatable RAM (the real admission gate).
    pub mem_available_mb: Option<u64>,
    /// Total configured swap.
    pub swap_total_mb: Option<u64>,
    /// Free swap (the OOM-headroom signal).
    pub swap_free_mb: Option<u64>,
    /// 1-minute load average.
    pub loadavg1: Option<f64>,
    /// Logical core count (the load denominator).
    pub nproc: usize,
    /// Free space on the volume backing the worktree root.
    pub disk_avail_mb: Option<u64>,
}

impl Capacity {
    /// Derive the local admission status (epic #707 v4): PRESSURE when the 1-min load per core is
    /// at/above [`LOAD_PER_CORE_MAX`], or when free swap drops below [`SWAP_FREE_MIN_FRAC`] of
    /// total swap; READY otherwise. Unknown signals never raise pressure (fail-open, matching the
    /// rest of bn's guards). A local sample is never `DEAD` — that is a remote reconcile verdict.
    pub fn status(&self) -> NodeStatus {
        if let (Some(load), n) = (self.loadavg1, self.nproc) {
            if n > 0 && load / n as f64 >= LOAD_PER_CORE_MAX {
                return NodeStatus::Pressure;
            }
        }
        if let (Some(total), Some(free)) = (self.swap_total_mb, self.swap_free_mb) {
            if total > 0 && (free as f64 / total as f64) < SWAP_FREE_MIN_FRAC {
                return NodeStatus::Pressure;
            }
        }
        NodeStatus::Ready
    }
}

/// Parse the available-KB figure out of `df -Pk <path>` output (the POSIX portable form) → MB. The
/// header line is skipped; the 4th column of the data line is "available 1K-blocks". Pure so the
/// parse is unit-testable without shelling out. `None` if the output has no parseable data line.
pub fn parse_df_avail_mb(s: &str) -> Option<u64> {
    s.lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().nth(3))
        .and_then(|kb| kb.parse::<u64>().ok())
        .map(|kb| kb / 1024)
}

/// Sample this node's live capacity. Delegates mem/swap/load to the single cross-platform
/// [`machine`] readers (so Linux and macOS share one platform matrix — the admission gate no
/// longer reads blind on a Mac) and shells out to `df` for the disk figure on the given
/// `worktree_root`. Best-effort: every unreadable signal reads as `None`.
pub fn sample(worktree_root: &std::path::Path) -> Capacity {
    let nproc = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let mem = machine::read_meminfo();
    let loadavg1 = machine::read_loadavg1();
    let disk_avail_mb = read_disk_avail_mb(worktree_root);
    Capacity {
        mem_total_mb: mem.total_mb,
        mem_available_mb: mem.available_mb,
        swap_total_mb: mem.swap_total_mb,
        swap_free_mb: mem.swap_free_mb,
        loadavg1,
        nproc,
        disk_avail_mb,
    }
}

/// Free MB on the volume backing `path` via `df -Pk`. Portable across Linux/macOS/BSD; fail-open
/// (`None`) if `df` is missing or errors.
fn read_disk_avail_mb(path: &std::path::Path) -> Option<u64> {
    let out = std::process::Command::new("df")
        .arg("-Pk")
        .arg(path)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| parse_df_avail_mb(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}
