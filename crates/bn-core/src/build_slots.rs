//! The build pool: admission for work that saturates the machine and whose caller must wait.
//!
//! A verify gate is not dispatched: the process that wants it is already running and cannot
//! continue until it has the verdict. `bn wt merge` must know whether the head is green before it
//! lands; `bn verify` prints an exit code its caller acts on. So the gate takes a **ticket** from a
//! fixed-size pool, runs inline, and gives the ticket back. This is throttling, not scheduling.
//!
//! # Why a pool of its own
//!
//! **A build is not an agent.** An agent is overwhelmingly I/O-bound — it sits waiting on the model,
//! on git, on the network. A `cargo build` saturates every core it is given and peaks several GB in
//! rustc and the linker, so builds are sized by their own arithmetic.
//!
//! **Sharing a slot with agents would deadlock.** The caller of a gate is usually an agent. If the
//! gate needed the same kind of slot the agent already holds, an agent holding the last one while
//! waiting for a gate would be waiting for itself. Keeping the pool disjoint removes the cycle by
//! construction. This is the one property here worth not breaking later.
//!
//! # Failure is bounded in the safe direction
//!
//! A ticket is released by [`Drop`], so an early return, a `?`, or a panic frees it. A holder that
//! dies without unwinding — SIGKILL, a reboot — leaves its row behind, and [`prune`] drops it on the
//! next read: at once if its pid is gone, and on an age bound for the cases a pid cannot answer (a
//! zombie awaiting its parent still reads as alive; so does a reused pid).
//!
//! And a caller that cannot get a slot within its budget runs anyway: the gate is a correctness
//! check, and delaying a land forever to protect throughput would trade the wrong one away. The
//! worst case degrades to every build at once rather than to a merge that never completes.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// RAM held back for the OS, the editor, the browser: everything that is not a build.
pub const RESERVED_RAM_MB: u64 = 4096;

/// RAM one build is assumed to occupy, MB. rustc's peak is per-codegen-unit and the link step is
/// worse; 4 GB is the honest number for a workspace build, against 2 GB for an agent.
pub const BUILD_RAM_MB: u64 = 4096;

/// Cores per build. A build takes every core it is offered, so this is not "how many it uses" but
/// "how few it can be squeezed into before it stops being worth starting".
pub const CORES_PER_BUILD: usize = 4;

/// The ceiling, regardless of machine. Past three concurrent workspace builds the disk and the
/// scheduler are the bottleneck, and the marginal PR landing a minute sooner is not worth making
/// every agent's tooling crawl.
pub const MAX_BUILDS: usize = 3;

/// How many concurrent builds this machine can hold — `min(usable RAM ÷ per-build RAM, cores ÷
/// per-build cores)`, floored at 1.
///
/// Floored rather than allowed to reach zero: a machine that can
/// start no builds cannot land anything, which is broken rather than careful. Pure over its inputs,
/// so a surprising number is answerable instead of being folklore.
pub fn slots(ram_total_mb: u64, cores: usize) -> usize {
    let by_ram = ram_total_mb.saturating_sub(RESERVED_RAM_MB) / BUILD_RAM_MB;
    let by_cpu = cores / CORES_PER_BUILD;
    (by_ram as usize).min(by_cpu).clamp(1, MAX_BUILDS)
}

/// [`slots`] against the live machine.
pub fn observe_slots() -> usize {
    let mem = crate::machine::read_meminfo();
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    slots(mem.total_mb.unwrap_or(0), cores)
}

/// One held ticket. `pid` is what makes a crashed holder recoverable rather than permanent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub id: String,
    pub pid: u32,
    /// What is being built, for the waiting message — "someone else" is not an answer.
    pub label: String,
    pub repo: String,
    pub since: i64,
}

/// The pool file's contents.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pool {
    #[serde(default)]
    pub held: Vec<Held>,
}

/// Where the pool lives. One file per machine — the resource being rationed is this box's CPU, so
/// the scope is the box, not the repo.
pub fn pool_path() -> PathBuf {
    crate::resolver::state_dir().join("build-slots.json")
}

/// How long a row may sit in the pool before it is presumed abandoned regardless of its pid.
///
/// Generous: a workspace build plus `npm ci` is minutes, and reclaiming a slot from a build that is
/// still running would put two of them on the machine — the exact thing this prevents. An hour is
/// well past any honest gate.
pub const SLOT_TTL_SECS: i64 = 3600;

/// Drop rows whose holder is gone.
///
/// Two independent grounds, because neither alone is sufficient:
///
///   1. **Its pid is gone** — the fast path, so a crashed build frees its slot at once instead of
///      after a timeout.
///   2. **It is past [`SLOT_TTL_SECS`]** — the backstop, and not merely belt-and-braces. A pid check
///      has two blind spots that would otherwise strand a slot forever: a killed process stays a
///      zombie until its parent reaps it, and reads as *alive* to `kill(pid, 0)`; and once the OS
///      wraps the pid space, an unrelated process can inherit a dead holder's number and keep its
///      row looking live indefinitely. Only an age bound closes those.
///
/// Pure over the liveness predicate so the caller supplies the platform check and the rule stays
/// inspectable rather than being a background behaviour people discover.
pub fn prune(held: Vec<Held>, now: i64, alive: &impl Fn(u32) -> bool) -> Vec<Held> {
    held.into_iter()
        .filter(|h| alive(h.pid) && now.saturating_sub(h.since) <= SLOT_TTL_SECS)
        .collect()
}

/// What a request for a slot got.
pub enum Admission {
    /// Hold this until the build finishes.
    Granted(Ticket),
    /// The pool is full; these are the holders, so the wait can say who it is waiting for.
    Full(Vec<Held>),
}

/// A held build slot. Released on [`Drop`] — there is deliberately no `release` method, because a
/// slot you can forget to give back is a slot that eventually is not given back.
#[must_use = "hold the ticket for the whole build; dropping it releases the slot"]
pub struct Ticket {
    id: String,
}

impl Ticket {
    /// The row this ticket owns, for logging.
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let _ = release(&self.id);
    }
}

/// Take a slot if the pool has room. Non-blocking: the caller owns the waiting policy, because
/// what to do when full differs between a gate that may fail open and a job that may not.
pub fn try_acquire(
    label: &str,
    repo: &str,
    cap: usize,
    now: i64,
    alive: &impl Fn(u32) -> bool,
) -> std::io::Result<Admission> {
    let path = pool_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _lock = crate::store::FileLock::acquire_required(&path)?;
    let mut held = prune(read_pool(&path).held, now, alive);
    if held.len() >= cap {
        return Ok(Admission::Full(held));
    }
    let id = next_id(now);
    held.push(Held {
        id: id.clone(),
        pid: std::process::id(),
        label: label.to_string(),
        repo: repo.to_string(),
        since: now,
    });
    write_pool(&path, &Pool { held })?;
    Ok(Admission::Granted(Ticket { id }))
}

/// Whether `pid` names a live process.
///
/// Signal 0 checks existence without delivering anything. It reads a zombie as alive and cannot see
/// pid reuse, which is exactly why [`prune`] also bounds by age.
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "libc::kill with signal 0 is the portable existence check; it delivers nothing"
)]
pub fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 performs error checking only — no signal is sent, and the call cannot
    // affect the target process.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(not(unix))]
pub fn pid_alive(_pid: u32) -> bool {
    false
}

/// Give a slot back. Private: [`Ticket`]'s `Drop` is the only caller, so a slot cannot be released
/// twice or released by someone who never held it.
fn release(id: &str) -> std::io::Result<()> {
    let path = pool_path();
    let _lock = crate::store::FileLock::acquire_required(&path)?;
    let mut pool = read_pool(&path);
    pool.held.retain(|h| h.id != id);
    write_pool(&path, &pool)
}

/// A pool file that cannot be read or parsed reads as empty.
///
/// Fail-open on purpose: an unreadable pool file must not wedge every build on the machine. The
/// cost is a moment of over-admission, which is what we had before this existed.
fn read_pool(path: &std::path::Path) -> Pool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_pool(path: &std::path::Path, pool: &Pool) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(pool).map_err(std::io::Error::other)?;
    crate::store::write_atomic(path, json.as_bytes())
}

/// A ticket id unique within this machine: pid, the second, and a per-process counter so one
/// process taking two slots in the same second cannot collide with itself.
fn next_id(now: i64) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{}-{now}-{n}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_are_sized_by_their_own_arithmetic() {
        // 4 GB and 4 cores each: on 16 GB / 10 cores that is 2 builds.
        assert_eq!(slots(16384, 10), 2);
        assert_eq!(slots(1024, 1), 1);
        assert_eq!(slots(1_048_576, 512), MAX_BUILDS);
    }

    fn held(id: &str, pid: u32, since: i64) -> Held {
        Held {
            id: id.to_string(),
            pid,
            label: "test".to_string(),
            repo: "r".to_string(),
            since,
        }
    }

    #[test]
    fn a_dead_holder_frees_its_slot_at_once() {
        let rows = vec![held("a", 1, 1000), held("b", 2, 1000)];
        let kept = prune(rows, 1000, &|pid| pid == 1);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "a");
    }

    #[test]
    fn a_live_pid_past_the_ttl_is_still_reclaimed() {
        // A zombie or a reused pid answers `kill(pid, 0)` as alive; liveness alone would strand
        // the slot.
        let rows = vec![held("a", 1, 1000)];
        assert!(prune(rows, 1000 + SLOT_TTL_SECS + 1, &|_| true).is_empty());
    }

    #[test]
    fn a_holder_exactly_at_the_ttl_is_kept() {
        let rows = vec![held("a", 1, 1000)];
        assert_eq!(prune(rows, 1000 + SLOT_TTL_SECS, &|_| true).len(), 1);
    }
}
