//! Overcommit protection for spawned agents (#720, epic #707 M1). Every admitted agent is wrapped
//! in a cgroup memory cap so a runaway `cargo`/Node build is killed by the kernel rather than
//! dragging the whole host into swap OOM.
//!
//! Preferred: `systemd-run --scope -p MemoryMax=${hard}M -p MemoryHigh=${soft}M -- <cmd>` (cgroup
//! v2 — a hard ceiling with a soft throttle band). Degraded fallback where `systemd-run` is
//! absent (e.g. a non-systemd container): `sh -c 'ulimit -v <hardKB>; exec <cmd>'` — a per-process
//! address-space cap (coarser: no soft band, per-process not per-cgroup), which is why it is only
//! the fallback.
//!
//! The builders are pure (they take a `has_systemd_run` flag rather than probing the host) so the
//! wrapper shape is unit-testable without systemd; the CLI passes the real detection result.

/// The two cgroup memory thresholds for a wrapped agent, in MB. `hard` is the kernel OOM ceiling
/// (`MemoryMax`); `soft` is the reclaim-pressure band (`MemoryHigh`) — the kernel throttles the
/// cgroup and reclaims aggressively above it before the hard kill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryCap {
    pub hard_mb: u64,
    pub soft_mb: u64,
}

impl MemoryCap {
    /// Derive a cap from an agent's memory estimate: the estimate is the hard ceiling and the soft
    /// band sits at 80% of it (leaving a reclaim runway before the OOM kill). A zero estimate
    /// yields a zero cap, which [`wrap`] treats as "no cap".
    pub fn from_estimate_mb(estimate_mb: u64) -> MemoryCap {
        MemoryCap {
            hard_mb: estimate_mb,
            soft_mb: estimate_mb * 4 / 5,
        }
    }
}

/// Build the `systemd-run --scope` argv wrapping `inner`, applying `cap`. Transient scope
/// (`--scope`), non-interactive/quiet (`--collect`/`--quiet`), with the cgroup-v2 memory
/// properties. Pure.
pub fn systemd_run_scope(cap: MemoryCap, inner: &[String]) -> Vec<String> {
    let mut argv = vec![
        "systemd-run".to_string(),
        "--user".to_string(),
        "--scope".to_string(),
        "--quiet".to_string(),
        "--collect".to_string(),
        format!("-pMemoryMax={}M", cap.hard_mb),
        format!("-pMemoryHigh={}M", cap.soft_mb),
        "--".to_string(),
    ];
    argv.extend_from_slice(inner);
    argv
}

/// Build the degraded `ulimit -v` fallback: `sh -c 'ulimit -v <hardKB>; exec "$@"' bn-worker
/// <inner...>`. `-v` is an address-space (virtual memory) cap in KB — coarser than a cgroup
/// (per-process, no soft band) but universally available. Pure.
pub fn ulimit_v_wrapper(cap: MemoryCap, inner: &[String]) -> Vec<String> {
    let hard_kb = cap.hard_mb.saturating_mul(1024);
    let mut argv = vec![
        "sh".to_string(),
        "-c".to_string(),
        format!("ulimit -v {hard_kb}; exec \"$@\""),
        "bn-worker".to_string(),
    ];
    argv.extend_from_slice(inner);
    argv
}

/// Wrap `inner` in the best available memory cap. With `cap.hard_mb == 0` (no estimate) the command
/// is returned unwrapped. Otherwise prefers [`systemd_run_scope`] when `has_systemd_run`, falling
/// back to [`ulimit_v_wrapper`]. Pure — the CLI supplies `has_systemd_run` from a real probe.
pub fn wrap(cap: MemoryCap, has_systemd_run: bool, inner: &[String]) -> Vec<String> {
    if cap.hard_mb == 0 {
        return inner.to_vec();
    }
    if has_systemd_run {
        systemd_run_scope(cap, inner)
    } else {
        ulimit_v_wrapper(cap, inner)
    }
}
