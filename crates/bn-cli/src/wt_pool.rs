//! `bn wt pool` — a treehouse-style pool of warm, reusable worktrees for monorepos.
//!
//! The problem this solves: spinning up a fresh worktree for every agent task pays a cold
//! `build.sh` / `npm install` each time. A *pool* keeps a small set of checkouts around with their
//! deps and build cache intact; `pool get` leases an idle one and switches your branch into it,
//! `pool return` resets it (without wiping ignored deps) so the next `get` is instant.
//!
//! Model (decided): **named-branch reuse**, not treehouse's anonymous detached pool — slots still
//! hold a real branch, so `bn`'s branch-keyed notes/metrics keep working untouched. Pools are
//! opt-in per repo via `~/.config/wt/pools` (`<repo> <size>` per line); repos not listed behave
//! exactly as before.
//!
//! Layout: slots live at `<root>/worktree/<repo>/_pool/<n>` beside the normal branch worktrees,
//! and lease state in `<root>/worktree/<repo>/_pool/leases.json`. The `_pool` dir is excluded from
//! every `wt` sweep (cleanup/prune/status/go) so the pool is never torn down by housekeeping.

use crate::wt;
use bn_core::resolver::Config;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use wt::{P_BLUE as BLUE, P_GREEN as GREEN, P_NC as NC, P_RED as RED, P_YELLOW as YELLOW};

/// Dispatch from `wt::run`. Returns whether the command succeeded (CLI exit semantics).
pub fn run(cfg: &Config, action: wt::PoolCmd) -> bool {
    match action {
        wt::PoolCmd::Get {
            branch,
            repo,
            holder,
            path,
            no_tmux,
        } => pool_get(cfg, branch, repo, holder, path, no_tmux),
        wt::PoolCmd::Return { target, force } => pool_return(cfg, target, force),
        wt::PoolCmd::Status { repo } => pool_status(cfg, repo),
        wt::PoolCmd::Warm { repo, here, new } => pool_warm(cfg, repo, here, new),
        wt::PoolCmd::Metrics { path, repo } => pool_metrics(cfg, path, repo),
        wt::PoolCmd::Set { size, repo, off } => pool_set(cfg, repo, size, off),
        wt::PoolCmd::Prune {
            repo,
            yes,
            all,
            verbose,
        } => pool_prune(cfg, repo, yes, all, verbose),
        wt::PoolCmd::Destroy {
            slot,
            repo,
            all,
            yes,
            include_leased,
            include_dirty,
            include_in_use,
        } => pool_destroy(
            cfg,
            slot,
            repo,
            all,
            yes,
            DestroyFlags {
                include_leased,
                include_dirty,
                include_in_use,
            },
        ),
    }
}

/// Append a `pool_get` / `pool_return` event to the shared event log, for `pool metrics`.
/// Best-effort — a logging failure never fails the pool operation.
fn log_pool_event(kind: &str, repo: &str, slot: u32, branch: &str, warm: Option<bool>) {
    let mut f = serde_json::Map::new();
    f.insert("repo".to_string(), repo.into());
    f.insert("slot".to_string(), slot.into());
    f.insert("branch".to_string(), branch.into());
    if let Some(w) = warm {
        f.insert("warm".to_string(), w.into());
    }
    let _ = bn_core::events::append(kind, f);
}

// --- config (~/.config/wt/pools) -------------------------------------------

/// Path of the pools config — delegates to [`bn_core::pool::pools_config_path`], the single source
/// of truth shared with `bn-web`'s pool-status read (`$WT_POOLS` else `<xdg>/wt/pools`).
fn pools_config_path() -> PathBuf {
    bn_core::pool::pools_config_path()
}

/// Pool size for `repo` from the config, or `None` if the repo has no explicit/`default` entry.
/// Delegates to [`bn_core::pool::pool_size`] (the shared file read + parse). `pub(crate)` so
/// `bn wt pool` / `bn orchestrate` can ask the configured size; a plain `bn dispatch` instead pools
/// every repo, falling back to [`bn_core::pool::DEFAULT_DISPATCH_POOL_SIZE`] when this is `None`.
pub(crate) fn pool_size(repo: &str) -> Option<usize> {
    bn_core::pool::pool_size(repo)
}

// --- lease store (leases.json) ---------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct Leases {
    slots: BTreeMap<u32, Lease>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Lease {
    branch: String,
    holder: String,
    /// PID of the process that took the lease — the *caller* (parent of the `bn` invocation, e.g.
    /// the agent or shell), not the ephemeral `bn` process which exits immediately. This is what
    /// makes the lease durable yet recoverable: a slot is only reclaimable once this process is
    /// gone. See [`caller_pid`].
    pid: u32,
    /// Epoch seconds when the lease was taken.
    at: i64,
    /// The boot id of the machine when the lease was taken (see [`current_boot_id`]). A lease whose
    /// id differs from the current boot's was taken *before* a restart (WSL shutdown / reboot), so
    /// its holder is provably gone even if the recycled `pid` now reads as alive — this is what lets
    /// the pool recover its slots after WSL dies and comes back. Empty for leases written before
    /// boot-scoping (treated as an unknown boot → falls back to the pid check), hence `default`.
    #[serde(default)]
    boot_id: String,
}

/// This boot's unique id: a value that changes on every (re)boot, so a lease tagged with a
/// *different* id predates the current boot and its holder process cannot still exist — WSL
/// shutdown / a host reboot kills every process, then recycles their pids to new ones. Linux (and
/// WSL) expose a per-boot random UUID at `/proc/sys/kernel/random/boot_id`; if that's unreadable we
/// fall back to `/proc/stat`'s `btime` (the boot epoch-seconds, equally boot-unique). `None`
/// off-Linux (no `/proc`) — there we can't boot-scope, so liveness stays pid-only. `$BN_BOOT_ID`
/// overrides for deterministic tests (simulating a reboot by changing it).
fn current_boot_id() -> Option<String> {
    if let Ok(id) = std::env::var("BN_BOOT_ID") {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return Some(id);
        }
    }
    if let Ok(id) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return Some(id);
        }
    }
    // Fallback: the kernel boot time from /proc/stat (`btime <epoch_secs>`) — also boot-unique.
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    stat.lines()
        .find_map(|l| l.strip_prefix("btime "))
        .map(|s| format!("btime:{}", s.trim()))
        .filter(|s| s != "btime:")
}

/// A stored `boot_id` as an `Option` for [`bn_core::pool::lease_liveness`]: an empty string (a
/// pre-boot-scoping lease) is an *unknown* boot, not a real id to compare.
fn boot_opt(id: &str) -> Option<&str> {
    (!id.is_empty()).then_some(id)
}

/// The PID to record as the lease holder: the parent of this `bn` process (the long-lived
/// agent/shell), so liveness checks mean "is the holder still around?" rather than "is `bn` still
/// running?" (which is always false by the time anyone looks). Falls back to our own PID off-unix.
#[allow(
    unsafe_code,
    reason = "libc::getppid is the portable Unix API for identifying the lease holder"
)]
fn caller_pid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: getppid is always safe and never fails.
        let ppid = unsafe { libc::getppid() };
        if ppid > 0 {
            return ppid as u32;
        }
    }
    std::process::id()
}

fn leases_path(pool_dir: &Path) -> PathBuf {
    pool_dir.join("leases.json")
}

/// Read the lease file, treating a missing or corrupt file as an empty pool (defensive, like
/// `resolver::ConfigFile::load`).
fn read_leases(pool_dir: &Path) -> Leases {
    std::fs::read_to_string(leases_path(pool_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Write the lease file atomically (temp + rename) so a crash mid-write can't corrupt it.
fn write_leases(pool_dir: &Path, leases: &Leases) -> std::io::Result<()> {
    let path = leases_path(pool_dir);
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_string_pretty(leases).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, &path)
}

// --- paths -----------------------------------------------------------------

fn pool_dir(worktree_dir: &Path, repo: &str) -> PathBuf {
    worktree_dir.join(repo).join("_pool")
}

fn slot_path(pool_dir: &Path, n: u32) -> PathBuf {
    pool_dir.join(n.to_string())
}

/// `(repo, slot_n, pool_dir)` for a slot path that ends in `.../_pool/<n>`, else `None`.
fn slot_parts(slot: &Path) -> Option<(String, u32, PathBuf)> {
    let n: u32 = slot.file_name()?.to_string_lossy().parse().ok()?;
    let pdir = slot.parent()?;
    if pdir.file_name()? != "_pool" {
        return None;
    }
    let repo = pdir.parent()?.file_name()?.to_string_lossy().to_string();
    Some((repo, n, pdir.to_path_buf()))
}

/// Walk up from `start` to the enclosing `_pool/<n>` slot dir, if any.
fn find_slot_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|anc| {
            anc.parent()
                .and_then(|p| p.file_name())
                .map(|n| n == "_pool")
                .unwrap_or(false)
        })
        .map(|p| p.to_path_buf())
}

// --- git helpers -----------------------------------------------------------

/// Run git with stdout suppressed (stderr inherited for progress). The pool's stdout contract is
/// "only the slot path / status text" — git's "HEAD is now at …" chatter must not leak into a
/// `pool get --path` capture. Returns whether it succeeded. Thin delegate to the shared
/// [`crate::git::git_run_quiet`] so its stderr progress still reaches the terminal.
fn git_quiet(args: &[&str]) -> bool {
    crate::git::git_run_quiet(args)
}

/// Like `git_quiet` but also suppresses stderr — for calls whose failure is expected and handled
/// (e.g. `worktree remove` on a broken/orphan registration, before the prune+rmdir fallback).
/// Delegates to the runner-backed [`crate::git::git_ok`]: capturing and discarding both streams is
/// observably identical to routing them to `/dev/null`.
fn git_silent(args: &[&str]) -> bool {
    crate::git::git_ok(args)
}

/// A valid base ref to create/reset slots against — the first of origin/main, main, origin/master,
/// master that resolves, else HEAD.
fn default_base(bare_s: &str) -> String {
    for r in ["origin/main", "main", "origin/master", "master"] {
        if wt::git_out(&["--git-dir", bare_s, "rev-parse", "--verify", "--quiet", r]).is_some() {
            return r.to_string();
        }
    }
    "HEAD".to_string()
}

/// Create one warm slot as a detached worktree off the default base.
fn add_detached_slot(bare_s: &str, slot_s: &str) -> bool {
    let base = default_base(bare_s);
    git_quiet(&[
        "--git-dir",
        bare_s,
        "worktree",
        "add",
        "--detach",
        slot_s,
        &base,
    ])
}

/// Switch `branch` into an existing slot checkout (reusing its warm deps). Mirrors the
/// heads / remotes / new three-way logic in `create_worktree`, but via `git switch` in place.
fn switch_into_slot(slot_s: &str, bare_s: &str, branch: &str) -> bool {
    let exists = |kind: &str| {
        wt::git_out(&[
            "--git-dir",
            bare_s,
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/{kind}/{branch}"),
        ])
        .is_some()
    };
    if exists("heads") {
        git_quiet(&["-C", slot_s, "switch", branch])
    } else if exists("remotes/origin") {
        git_quiet(&[
            "-C",
            slot_s,
            "switch",
            "-c",
            branch,
            &format!("origin/{branch}"),
        ])
    } else {
        let base = default_base(bare_s);
        git_quiet(&["-C", slot_s, "switch", "-c", branch, &base])
    }
}

/// Reset a slot back to a clean warm-idle state: discard tracked changes and detach to the base,
/// then drop untracked files — but NEVER `-x`/`-X`, so ignored deps/build cache survive. This
/// no-`-x` rule is the whole point of the pool.
fn warm_reset(slot_s: &str, bare_s: &str) {
    let base = default_base(bare_s);
    let _ = git_quiet(&[
        "-C",
        slot_s,
        "switch",
        "--detach",
        "--discard-changes",
        &base,
    ]);
    let _ = git_quiet(&["-C", slot_s, "clean", "-df"]);
}

/// Whether `slot` is a usable warm checkout — a *real* git worktree, not just a directory left
/// behind by an interrupted `worktree add` (killed mid-checkout leaves the files but no `.git`
/// link). Such a stub passes `is_dir()` yet every git command in it fails, so it must be recreated,
/// not reused. We require its own `.git` entry AND that git accepts it as a work tree.
fn is_live_slot(slot: &Path) -> bool {
    slot.join(".git").exists()
        && git_silent(&[
            "-C",
            &slot.to_string_lossy(),
            "rev-parse",
            "--is-inside-work-tree",
        ])
}

/// Forcibly tear down a slot's worktree registration and on-disk dir, tolerating a broken/orphan
/// state (interrupted `worktree add`, dangling ref): try `worktree remove --force`, else prune the
/// dangling ref and remove the dir. Shared by slot removal and by the recreate path in get/warm.
fn force_remove_worktree(bare_s: &str, slot: &Path) {
    let slot_s = slot.to_string_lossy().to_string();
    if !git_silent(&[
        "--git-dir",
        bare_s,
        "worktree",
        "remove",
        "--force",
        &slot_s,
    ]) {
        let _ = git_silent(&["--git-dir", bare_s, "worktree", "prune"]);
        let _ = std::fs::remove_dir_all(slot);
    }
}

// --- in-use detection ------------------------------------------------------

/// Whether a process with `pid` is alive. Linux reads `/proc/<pid>`; elsewhere we can't cheaply
/// tell, so assume alive (the lease stays authoritative — reclaim needs `--force`).
#[cfg(target_os = "linux")]
fn pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}
#[cfg(not(target_os = "linux"))]
fn pid_alive(_pid: u32) -> bool {
    true
}

/// Whether any running process has its cwd inside `slot` (best-effort, Linux-only).
#[cfg(target_os = "linux")]
fn any_proc_in(slot: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return false;
    };
    for e in rd.flatten() {
        if let Ok(target) = std::fs::read_link(e.path().join("cwd")) {
            if target.starts_with(slot) {
                return true;
            }
        }
    }
    false
}
#[cfg(not(target_os = "linux"))]
fn any_proc_in(_slot: &Path) -> bool {
    false
}

/// How aggressively [`reclaim_stale`] frees abandoned slots.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReclaimScope {
    /// Only leases from a *prior boot* (holder provably dead — WSL shutdown / host reboot). Safe to
    /// run on every access, so the pool self-heals the moment anything touches it after a restart.
    PriorBootOnly,
    /// Any dead-holder lease — prior-boot *or* a same-boot lease whose pid is gone. The last-resort
    /// sweep when the pool is otherwise full, honoring the durable-lease policy for a live boot
    /// (a same-boot lease is only reclaimed here, not pre-emptively).
    AnyDeadHolder,
}

/// What a [`reclaim_stale`] pass freed, split by cause so callers can report the WSL/reboot
/// recovery (`prior_boot`) apart from an ordinary dead-holder reclaim (`this_boot`).
#[derive(Debug, Default, Clone)]
struct ReclaimReport {
    /// Slots reclaimed because their lease predated the current boot (a restart recovery).
    prior_boot: Vec<u32>,
    /// Slots reclaimed because a same-boot holder pid had died.
    this_boot: Vec<u32>,
}

impl ReclaimReport {
    fn changed(&self) -> bool {
        !self.prior_boot.is_empty() || !self.this_boot.is_empty()
    }
}

/// Drop leases whose holder is gone, resetting each reclaimed slot to warm-idle, and persist the
/// lease store when anything changed. A slot is reclaimed only when its holder is gone (per `scope`
/// and the boot-aware [`bn_core::pool::lease_liveness`]) AND nothing is running in it AND it has no
/// uncommitted work to lose — a dirty abandoned slot is left leased (`return --force` frees it).
/// Each prior-boot reclaim logs a `pool_recover` event so `pool metrics` can show restart recoveries.
fn reclaim_stale(ctx: &PoolCtx, leases: &mut Leases, scope: ReclaimScope) -> ReclaimReport {
    use bn_core::pool::LeaseLiveness;
    let bare_s = ctx.bare.to_string_lossy().to_string();
    let current_boot = current_boot_id();
    let mut report = ReclaimReport::default();

    let candidates: Vec<(u32, bool)> = leases
        .slots
        .iter()
        .filter_map(|(n, l)| {
            let slot = slot_path(&ctx.pool_dir, *n);
            let liveness = bn_core::pool::lease_liveness(
                boot_opt(&l.boot_id),
                current_boot.as_deref(),
                pid_alive(l.pid),
            );
            let prior_boot = matches!(liveness, LeaseLiveness::StalePriorBoot);
            let dead_holder = match scope {
                ReclaimScope::PriorBootOnly => prior_boot,
                ReclaimScope::AnyDeadHolder => !matches!(liveness, LeaseLiveness::Live),
            };
            // Never clobber a live process's cwd or uncommitted work, even for a prior-boot lease:
            // the working tree survives a reboot, so a dirty slot is preserved and surfaced by
            // `pool metrics` (commit or `return --force`) rather than silently discarded.
            let reclaimable =
                dead_holder && !any_proc_in(&slot) && wt::dirty_count(&slot.to_string_lossy()) == 0;
            reclaimable.then_some((*n, prior_boot))
        })
        .collect();

    for (n, prior_boot) in candidates {
        let slot = slot_path(&ctx.pool_dir, n);
        let branch = leases
            .slots
            .get(&n)
            .map(|l| l.branch.clone())
            .unwrap_or_default();
        warm_reset(&slot.to_string_lossy(), &bare_s);
        leases.slots.remove(&n);
        if prior_boot {
            log_pool_event("pool_recover", &ctx.repo, n, &branch, None);
            report.prior_boot.push(n);
        } else {
            report.this_boot.push(n);
        }
    }
    if report.changed() {
        let _ = write_leases(&ctx.pool_dir, leases);
    }
    report
}

// --- removal classification (pure, unit-tested) ----------------------------

/// What a slot is, for the removal decision. Filled by `classify_slot` (which does the I/O), then
/// handed to the pure deciders below.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SlotClass {
    /// Held in leases.json.
    leased: bool,
    /// Has uncommitted changes (ignored deps don't count — `wt::dirty_count`).
    dirty: bool,
    /// A live process is working in it.
    in_use: bool,
    /// Slot number exceeds the configured pool size (e.g. after the size was lowered).
    over_size: bool,
    /// On disk but git doesn't know it, or the bare repo is gone.
    orphan: bool,
}

#[derive(Clone, Copy, Default)]
struct DestroyFlags {
    include_leased: bool,
    include_dirty: bool,
    include_in_use: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    Remove,
    Skip(&'static str),
}

/// `prune` removes ONLY genuinely-stale slots — over-size or orphaned — and never anything
/// leased/dirty/in-use. An in-size idle warm slot *is* the pool, so it's kept. Pure → testable.
fn prune_decision(c: SlotClass) -> Action {
    if c.leased {
        return Action::Skip("leased");
    }
    if c.in_use {
        return Action::Skip("in use");
    }
    if c.dirty {
        return Action::Skip("uncommitted changes");
    }
    if c.over_size || c.orphan {
        return Action::Remove;
    }
    Action::Skip("in-size warm slot")
}

/// `destroy` removes a *targeted* slot (the caller already scoped to a slot or `--all`), skipping
/// protected classes unless the matching `--include-*` flag opts in. Pure → testable.
fn destroy_decision(c: SlotClass, f: DestroyFlags) -> Action {
    if c.leased && !f.include_leased {
        return Action::Skip("leased (use --include-leased)");
    }
    if c.in_use && !f.include_in_use {
        return Action::Skip("in use (use --include-in-use)");
    }
    if c.dirty && !f.include_dirty {
        return Action::Skip("uncommitted changes (use --include-dirty)");
    }
    Action::Remove
}

// --- resolution ------------------------------------------------------------

struct PoolCtx {
    bare: PathBuf,
    repo: String,
    pool_dir: PathBuf,
    worktree_dir: PathBuf,
    size: usize,
}

/// Resolve repo → bare path + pool dir + size, or print why not and return `None`.
/// `bn wt pool set [size] [--repo r] [--off]` — the pools config's only writer. Resolves the repo
/// against the bare clones (so a typo is caught here rather than becoming a dead config line) and
/// rewrites its entry via `bn_core::pool`, the same edit `/api/repos/{repo}/pool` performs.
///
/// Deliberately does not build the slots. Warming is a long checkout-plus-build per slot, and the
/// `pool-warm` cron already does it while the box is idle; making the enable command block on a
/// build would be a surprise, and doing it at a keystroke is exactly what that cron exists to avoid.
fn pool_set(cfg: &Config, repo_arg: Option<String>, size: Option<usize>, off: bool) -> bool {
    let (bare_dir, _) = wt::layout(cfg);
    let Some(bare) = (match repo_arg {
        Some(r) => {
            let b = bare_dir.join(format!("{r}.git"));
            if !b.is_dir() {
                eprintln!("{RED}No bare repo '{r}' under {}{NC}", bare_dir.display());
                return false;
            }
            Some(b)
        }
        None => wt::resolve_bare_or_pick(&bare_dir),
    }) else {
        return false;
    };
    let repo = wt::repo_name(&bare);

    let target = if off {
        None
    } else {
        match size {
            Some(n) => Some(n.clamp(bn_core::pool::MIN_POOL_SIZE, bn_core::pool::MAX_POOL_SIZE)),
            None => {
                eprintln!("{RED}Pass a slot count, or --off to stop pooling {repo}.{NC}");
                return false;
            }
        }
    };
    if let Err(e) = bn_core::pool::set_pool_size(&repo, target) {
        eprintln!(
            "{RED}Could not write {}: {e}{NC}",
            pools_config_path().display()
        );
        return false;
    }
    match target {
        Some(n) => {
            println!("{GREEN}Pooling {repo} at {n} slot(s).{NC}");
            println!(
                "{BLUE}The idle pool-warm cron builds them on its next pass; \
                 `bn wt pool warm --repo {repo}` does it now.{NC}"
            );
        }
        // Slots are left on disk on purpose: unpooling is a config change, and silently deleting
        // checkouts (which may hold build caches worth hours) is not what "off" should mean.
        None => println!(
            "{GREEN}Stopped pooling {repo}. Existing slots remain — \
             `bn wt pool destroy --repo {repo} --all` removes them.{NC}"
        ),
    }
    true
}

fn resolve_pool(cfg: &Config, repo_arg: Option<String>) -> Option<PoolCtx> {
    let (bare_dir, worktree_dir) = wt::layout(cfg);
    let bare = match repo_arg {
        Some(r) => {
            let b = bare_dir.join(format!("{r}.git"));
            if !b.is_dir() {
                eprintln!("{RED}No bare repo '{r}' under {}{NC}", bare_dir.display());
                return None;
            }
            b
        }
        None => wt::resolve_bare_or_pick(&bare_dir)?,
    };
    let repo = wt::repo_name(&bare);
    let Some(size) = pool_size(&repo) else {
        eprintln!(
            "{YELLOW}Repo '{repo}' is not pooled. Add it to {} (`<repo> <size>`, or a \
             `default <size>` line to pool any repo).{NC}",
            pools_config_path().display()
        );
        return None;
    };
    let pool_dir = pool_dir(&worktree_dir, &repo);
    Some(PoolCtx {
        bare,
        repo,
        pool_dir,
        worktree_dir,
        size,
    })
}

/// Resolve the repo (canonical name + bare path) a `pool metrics` targets. Precedence: an explicit
/// filesystem `path` (a pool slot, any worktree checkout, or a `<repo>.git` bare dir) — so you can
/// point metrics at any repo path and get its pool health — then the `--repo` name, then the current
/// repo (or an fzf pick). Unlike [`resolve_pool`], this does NOT require the repo to be pooled: the
/// caller reports a clean "not pooled" so `metrics` answers "health of pools *if they exist*". Prints
/// and returns `None` only when nothing resolves to a real repo.
fn resolve_metrics_repo(
    cfg: &Config,
    path: Option<&str>,
    repo: Option<String>,
) -> Option<(String, PathBuf)> {
    let (bare_dir, _) = wt::layout(cfg);
    if let Some(p) = path {
        let bare = bare_from_path(p)?;
        return Some((wt::repo_name(&bare), bare));
    }
    if let Some(r) = repo {
        let bare = bare_dir.join(format!("{r}.git"));
        if !bare.is_dir() {
            eprintln!("{RED}No bare repo '{r}' under {}{NC}", bare_dir.display());
            return None;
        }
        return Some((wt::repo_name(&bare), bare));
    }
    let bare = wt::resolve_bare_or_pick(&bare_dir)?;
    Some((wt::repo_name(&bare), bare))
}

/// Resolve a filesystem path to the bare repo backing it: a `<repo>.git` bare dir is itself the
/// answer; a pool slot or any worktree checkout maps through its `.git` pointer up to the bare (via
/// [`wt::find_bare_repo`]). Prints and returns `None` when the path is neither.
fn bare_from_path(path: &str) -> Option<PathBuf> {
    let raw = Path::new(path);
    let abs = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        wt::cwd().join(raw)
    };
    // A bare repo dir passed directly (`.../<repo>.git` holding a HEAD).
    if abs.extension().map(|e| e == "git").unwrap_or(false) && abs.join("HEAD").is_file() {
        return Some(abs);
    }
    // Inside a worktree or a `_pool/<n>` slot → walk the `.git` pointer up to the bare.
    if let Some(bare) = wt::find_bare_repo(&abs) {
        return Some(bare);
    }
    eprintln!(
        "{RED}'{}' isn't a bare repo or inside a worktree — can't find its pool.{NC}",
        abs.display()
    );
    None
}

fn default_holder() -> String {
    std::env::var("USER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn now_epoch() -> i64 {
    // Delegate to the canonical clock so `$BN_NOW` (the deterministic-test override) is honored — a
    // raw SystemTime read silently ignored it.
    bn_core::util::now_epoch_secs()
}

// --- commands --------------------------------------------------------------

fn pool_get(
    cfg: &Config,
    branch: Option<String>,
    repo: Option<String>,
    holder: Option<String>,
    path_only: bool,
    no_tmux: bool,
) -> bool {
    let Some(ctx) = resolve_pool(cfg, repo) else {
        return false;
    };
    let Some(branch) = branch else {
        eprintln!("{RED}Usage: wt pool get <branch>{NC}");
        return false;
    };
    let Some(leased) = acquire_slot(&ctx, &branch, holder, SlotRole::Worker) else {
        return false;
    };
    finish_get(&ctx, &leased.slot, &branch, path_only, no_tmux)
}

/// The result of leasing a slot: its path, plus whether this was a *fresh* lease of `branch` (a
/// newly taken slot) versus an idempotent hand-back of the slot already leased to it. `bn dispatch`
/// maps `fresh` onto its "created" flag (seed the note / run setup only on a fresh lease).
struct Leased {
    slot: PathBuf,
    fresh: bool,
}

/// Which half of the pool a lease may draw from. Slot [`CONTROLLER_POOL_SLOT`] is `bn control`'s
/// alone; everything else leases from the worker range, so the two can never collide.
///
/// [`CONTROLLER_POOL_SLOT`]: bn_core::agent::CONTROLLER_POOL_SLOT
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotRole {
    Worker,
    Controller,
}

impl SlotRole {
    fn owns(self, n: u32) -> bool {
        let controller = n == bn_core::agent::CONTROLLER_POOL_SLOT;
        match self {
            SlotRole::Worker => !controller,
            SlotRole::Controller => controller,
        }
    }
}

/// Lease a warm slot for `branch` in an already-resolved pool. Reuses the slot already leased to
/// this branch (idempotent warm hit), else picks — and if the pool is full, reclaims an abandoned
/// — idle slot, creates it cold if it isn't warm yet, switches `branch` in, and persists the lease.
/// Returns `None` (message already printed) when the pool is full or a git op failed. Shared by
/// `wt pool get` and `bn dispatch --pool` so both lease identically. `class` decides which slots are
/// even candidates: a worker never sees the controller's slot, and vice versa.
fn acquire_slot(
    ctx: &PoolCtx,
    branch: &str,
    holder: Option<String>,
    class: SlotRole,
) -> Option<Leased> {
    let _ = std::fs::create_dir_all(&ctx.pool_dir);
    let mut leases = read_leases(&ctx.pool_dir);

    // Self-heal first: reclaim any lease left by a *prior boot* (WSL shutdown / host reboot). Its
    // holder is provably gone, so the durable-until-return rule no longer applies — freeing these
    // pre-emptively is what lets the pool come back to life after a restart, and it may reveal the
    // very slot this call needs. Same-boot dead holders are left for the last-resort sweep below.
    reclaim_stale(ctx, &mut leases, ReclaimScope::PriorBootOnly);

    // Already leased to this branch, in a slot of the right class? Hand back the same slot
    // (idempotent, a warm hit).
    if let Some((&n, _)) = leases
        .slots
        .iter()
        .find(|(&n, l)| l.branch == branch && class.owns(n))
    {
        let slot = slot_path(&ctx.pool_dir, n);
        log_pool_event("pool_get", &ctx.repo, n, branch, Some(true));
        return Some(Leased { slot, fresh: false });
    }

    // Pick the lowest idle slot of this class (pure policy in bn-core). Live-boot leases are durable
    // (held until `return`), so we DON'T reclaim them pre-emptively — only as a last resort when
    // every slot is leased, and only for slots whose holder is genuinely gone (dead pid, nothing
    // running in them).
    let leased_set = |l: &Leases| l.slots.keys().copied().collect::<BTreeSet<u32>>();
    let pick = |l: &Leases| match class {
        SlotRole::Worker => bn_core::pool::pick_slot(ctx.size, &leased_set(l)),
        SlotRole::Controller => bn_core::pool::pick_controller_slot(&leased_set(l)),
    };
    let mut n = pick(&leases);
    if n.is_none() && reclaim_stale(ctx, &mut leases, ReclaimScope::AnyDeadHolder).changed() {
        n = pick(&leases);
    }
    let Some(n) = n else {
        match class {
            SlotRole::Controller => eprintln!(
                "{RED}Controller slot {} is already leased.{NC} Stop the running controller with \
                 `bn control stop <id>`, or hand the slot back with `wt pool return`.",
                bn_core::agent::CONTROLLER_POOL_SLOT
            ),
            SlotRole::Worker => eprintln!(
                "{RED}Pool full: {}/{} worker slots leased.{NC} Return one with `wt pool return`.",
                leases.slots.keys().filter(|&&n| class.owns(n)).count(),
                ctx.size.saturating_sub(1)
            ),
        }
        return None;
    };

    let slot = slot_path(&ctx.pool_dir, n);
    let slot_s = slot.to_string_lossy().to_string();
    let bare_s = ctx.bare.to_string_lossy().to_string();

    // Warm hit = the slot already holds a *live* checkout (we skip the cold checkout); cold =
    // created. A bare dir left by an interrupted `worktree add` is NOT warm — tear it down and
    // recreate, else the `git switch` below fails with "not a git repository".
    let warm = is_live_slot(&slot);
    if !warm {
        if slot.exists() {
            eprintln!("{YELLOW}Slot {n} checkout is broken — recreating…{NC}");
            force_remove_worktree(&bare_s, &slot);
        }
        eprintln!("{BLUE}Creating warm slot {n} for {}…{NC}", ctx.repo);
        if !add_detached_slot(&bare_s, &slot_s) {
            eprintln!("{RED}Failed to create slot {n}{NC}");
            return None;
        }
    }
    match class {
        SlotRole::Worker => {
            if !switch_into_slot(&slot_s, &bare_s, branch) {
                eprintln!(
                    "{RED}Failed to switch '{branch}' into slot {n} (branch checked out elsewhere?){NC}"
                );
                return None;
            }
        }
        // The controller coordinates and ships nothing, so its slot stays **detached** at the base
        // rather than checking a branch out: `main` is already checked out in the main worktree, and
        // git refuses the same branch twice. A reset also clears whatever a prior tenant left.
        SlotRole::Controller => warm_reset(&slot_s, &bare_s),
    }

    leases.slots.insert(
        n,
        Lease {
            branch: branch.to_string(),
            holder: holder.unwrap_or_else(default_holder),
            pid: caller_pid(),
            at: now_epoch(),
            boot_id: current_boot_id().unwrap_or_default(),
        },
    );
    if write_leases(&ctx.pool_dir, &leases).is_err() {
        eprintln!("{YELLOW}Warning: failed to persist lease for slot {n}{NC}");
    }
    log_pool_event("pool_get", &ctx.repo, n, branch, Some(warm));
    Some(Leased { slot, fresh: true })
}

/// Lease a warm pool slot for a `bn dispatch` of `branch` in `repo` (#457) — the same primitive as
/// `wt pool get`, minus the tmux-window / stdout side effects: dispatch owns the window (it launches
/// the seeded agent there) and needs the slot path *returned*, not printed. Returns `(slot_path,
/// fresh)` where `fresh` is true for a new lease of `branch` (dispatch seeds the note only then).
/// `None` when the repo has no bn-managed bare clone or the pool is full — the caller falls back to
/// a cold worktree. Leasing is the default dispatch path, so this pools *every* repo (see
/// [`resolve_dispatch_pool`]).
pub fn lease_for_dispatch(cfg: &Config, repo: &str, branch: &str) -> Option<(PathBuf, bool)> {
    let ctx = resolve_dispatch_pool(cfg, repo)?;
    let leased = acquire_slot(&ctx, branch, None, SlotRole::Worker)?;
    Some((leased.slot, leased.fresh))
}

/// The lease label the controller's slot carries in `leases.json` and `wt pool status`. It is not a
/// branch — the slot sits detached at the base — but the lease store is keyed by this string, so a
/// stable label makes re-leasing idempotent.
pub const CONTROLLER_LEASE_LABEL: &str = "control";

/// Lease the controller's reserved slot (`_pool/1`) for `bn control start`. Returns the slot path
/// and whether the lease is fresh, or `None` when the repo has no bn-managed bare clone or the
/// controller slot is already held (message printed). The controller never rotates through the
/// worker range, so this can only ever hand back slot [`bn_core::agent::CONTROLLER_POOL_SLOT`].
pub fn lease_controller_slot(cfg: &Config, repo: &str) -> Option<(PathBuf, bool)> {
    let ctx = resolve_dispatch_pool(cfg, repo)?;
    let leased = acquire_slot(
        &ctx,
        CONTROLLER_LEASE_LABEL,
        Some("bn-control".into()),
        SlotRole::Controller,
    )?;
    Some((leased.slot, leased.fresh))
}

/// Hand the controller's slot back when `bn control stop` tears its window down, so the next
/// `bn control start` can lease it instead of reporting the slot held by a controller that is gone.
/// Quiet and idempotent: a slot that isn't leased (or a repo with no pool) is simply a no-op, since
/// this runs on every stop including controllers that predate slot leasing. Dirty slots are left
/// alone — `wt pool return --force` is the deliberate discard.
pub fn release_controller_slot(cfg: &Config, repo: &str) {
    let Some(ctx) = resolve_dispatch_pool(cfg, repo) else {
        return;
    };
    let n = bn_core::agent::CONTROLLER_POOL_SLOT;
    let mut leases = read_leases(&ctx.pool_dir);
    if !leases.slots.contains_key(&n) {
        return;
    }
    let slot = slot_path(&ctx.pool_dir, n);
    if wt::dirty_count(&slot.to_string_lossy()) > 0 {
        eprintln!(
            "{YELLOW}Controller slot {n} ({}) has uncommitted changes — leaving it leased. \
             Inspect it, then `wt pool return {n} --force`.{NC}",
            ctx.repo
        );
        return;
    }
    warm_reset(&slot.to_string_lossy(), &ctx.bare.to_string_lossy());
    leases.slots.remove(&n);
    let _ = write_leases(&ctx.pool_dir, &leases);
    log_pool_event("pool_return", &ctx.repo, n, CONTROLLER_LEASE_LABEL, None);
}

/// Resolve the pool for a dispatch — pooling *every* repo by default, since leasing is the default
/// dispatch path. An explicit or `default`/`*` config size wins; otherwise the built-in
/// [`bn_core::pool::DEFAULT_DISPATCH_POOL_SIZE`] applies, so a repo need not be pre-registered in
/// `~/.config/wt/pools` to get a warm, reap-safe slot. Returns `None` only when the repo has no
/// bn-managed bare clone — the one case a dispatch genuinely can't pool — so the caller cuts a cold
/// worktree instead. (Contrast [`resolve_pool`], which errors for `wt pool` when a repo is unpooled.)
fn resolve_dispatch_pool(cfg: &Config, repo: &str) -> Option<PoolCtx> {
    let (bare_dir, worktree_dir) = wt::layout(cfg);
    let bare = bare_dir.join(format!("{repo}.git"));
    if !bare.is_dir() {
        return None;
    }
    let size = pool_size(repo).unwrap_or(bn_core::pool::DEFAULT_DISPATCH_POOL_SIZE);
    let pool_dir = pool_dir(&worktree_dir, repo);
    Some(PoolCtx {
        bare,
        repo: repo.to_string(),
        pool_dir,
        worktree_dir,
        size,
    })
}

/// Open the slot's tmux window (unless suppressed) and print its path on stdout for the shell
/// wrapper / agent to `cd` into.
fn finish_get(ctx: &PoolCtx, slot: &Path, branch: &str, path_only: bool, no_tmux: bool) -> bool {
    if !path_only && !no_tmux {
        wt::open_worktree_window(&ctx.repo, &wt::sanitize_branch(branch), slot, false);
    }
    println!("{}", slot.display());
    true
}

fn pool_return(cfg: &Config, target: Option<String>, force: bool) -> bool {
    let (bare_dir, worktree_dir) = wt::layout(cfg);

    let slot = match target {
        Some(t) if Path::new(&t).is_absolute() => PathBuf::from(t),
        Some(t) if t.chars().all(|c| c.is_ascii_digit()) => {
            let Some(bare) = wt::find_bare_repo(&wt::cwd()) else {
                eprintln!("{RED}A slot number needs a repo — run from inside the repo or pass a path.{NC}");
                return false;
            };
            let repo = wt::repo_name(&bare);
            slot_path(&pool_dir(&worktree_dir, &repo), t.parse().unwrap_or(0))
        }
        Some(t) => {
            eprintln!("{RED}Invalid target '{t}' — pass a slot number or an absolute path.{NC}");
            return false;
        }
        None => match find_slot_root(&wt::cwd()) {
            Some(s) => s,
            None => {
                eprintln!("{RED}Not inside a pool slot — pass a slot number or path.{NC}");
                return false;
            }
        },
    };

    let Some((repo, n, pdir)) = slot_parts(&slot) else {
        eprintln!("{RED}Not a pool slot: {}{NC}", slot.display());
        return false;
    };
    let bare = bare_dir.join(format!("{repo}.git"));

    let mut leases = read_leases(&pdir);
    if !leases.slots.contains_key(&n) && !force {
        eprintln!("{YELLOW}Slot {n} ({repo}) isn't leased. Use --force to reset it anyway.{NC}");
        return false;
    }
    // Don't silently discard work: refuse if the slot has uncommitted changes (modified tracked or
    // untracked non-ignored — ignored deps/build cache don't count). `--force` discards anyway.
    if !force {
        let dirty = wt::dirty_count(&slot.to_string_lossy());
        if dirty > 0 {
            eprintln!(
                "{RED}Slot {n} ({repo}) has {dirty} uncommitted change(s).{NC} Commit/stash first, or `wt pool return {n} --force` to discard."
            );
            return false;
        }
    }
    let branch = leases
        .slots
        .get(&n)
        .map(|l| l.branch.clone())
        .unwrap_or_default();
    warm_reset(&slot.to_string_lossy(), &bare.to_string_lossy());
    leases.slots.remove(&n);
    let _ = write_leases(&pdir, &leases);
    log_pool_event("pool_return", &repo, n, &branch, None);
    println!("{GREEN}Returned slot {n} ({repo}) to the pool.{NC}");
    true
}

fn pool_status(cfg: &Config, repo: Option<String>) -> bool {
    let Some(ctx) = resolve_pool(cfg, repo) else {
        return false;
    };
    let mut leases = read_leases(&ctx.pool_dir);
    // Self-heal on read: reclaim any slot left leased by a prior boot (WSL/host restart) so the
    // status reflects a recovered pool, not phantom leases pinned by recycled pids.
    let recovered = reclaim_stale(&ctx, &mut leases, ReclaimScope::PriorBootOnly);
    println!(
        "{BLUE}{} pool — {} slots ({} worker + 1 controller){NC}",
        ctx.repo,
        ctx.size,
        ctx.size.saturating_sub(1)
    );
    if !recovered.prior_boot.is_empty() {
        println!(
            "  {GREEN}recovered {} slot(s) from a prior boot: {}{NC}",
            recovered.prior_boot.len(),
            slot_list(&recovered.prior_boot)
        );
    }
    let now = now_epoch();
    let current_boot = current_boot_id();
    for n in 1..=ctx.size as u32 {
        // Slot 1 is `bn control`'s alone — label it so an idle controller slot doesn't read as
        // spare worker capacity.
        let role = if n == bn_core::agent::CONTROLLER_POOL_SLOT {
            "  (controller)"
        } else {
            ""
        };
        match leases.slots.get(&n) {
            Some(l) => {
                // A same-boot lease whose holder pid has died is abandoned — flag it so a stuck
                // slot is visible even though the durable-lease policy leaves it for `prune`/reuse.
                let alive = matches!(
                    bn_core::pool::lease_liveness(
                        boot_opt(&l.boot_id),
                        current_boot.as_deref(),
                        pid_alive(l.pid),
                    ),
                    bn_core::pool::LeaseLiveness::Live
                );
                let flag = if alive { "" } else { "  (holder gone)" };
                println!(
                    "  {YELLOW}[{n}] leased{NC}{role}  {}  holder={} age={}{flag}",
                    l.branch,
                    l.holder,
                    fmt_age(now - l.at)
                );
            }
            None => {
                let state = if slot_path(&ctx.pool_dir, n).is_dir() {
                    "idle (warm)"
                } else {
                    "—"
                };
                println!("  {GREEN}[{n}] {state}{NC}{role}");
            }
        }
    }
    true
}

/// Render a slot-number list as `[1] [3]` for status/health lines.
fn slot_list(slots: &[u32]) -> String {
    slots
        .iter()
        .map(|n| format!("[{n}]"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn pool_metrics(cfg: &Config, path: Option<String>, repo: Option<String>) -> bool {
    let Some((repo_name, bare)) = resolve_metrics_repo(cfg, path.as_deref(), repo) else {
        return false;
    };
    // "…health of pools if they exist": a real repo without a pool is a clean answer, not an error.
    let Some(size) = pool_size(&repo_name) else {
        println!(
            "{YELLOW}{repo_name}: not pooled — no warm-slot pool to report.{NC}\n  Add it to {} (`<repo> <size>`) to enable one.",
            pools_config_path().display()
        );
        return true;
    };
    let (_, worktree_dir) = wt::layout(cfg);
    let ctx = PoolCtx {
        bare,
        pool_dir: pool_dir(&worktree_dir, &repo_name),
        repo: repo_name,
        worktree_dir,
        size,
    };
    let mut leases = read_leases(&ctx.pool_dir);
    // Self-heal on read: recover slots pinned by a prior boot (WSL/host restart) before reporting,
    // so `metrics` both *shows* and *maintains* the pool's health in one call.
    let recovered = reclaim_stale(&ctx, &mut leases, ReclaimScope::PriorBootOnly);
    let events = bn_core::events::read_all();
    let slot_of = |e: &bn_core::events::Event| e.fields.get("slot").and_then(|v| v.as_u64());

    let (mut gets, mut warm, mut returns, mut recoveries) = (0u64, 0u64, 0u64, 0u64);
    let mut per_slot: BTreeMap<u64, u64> = BTreeMap::new();
    // Pair each return with the most recent prior get on the same slot to estimate lease duration.
    let mut open_get: BTreeMap<u64, i64> = BTreeMap::new();
    let mut durations: Vec<i64> = Vec::new();

    for e in events
        .iter()
        .filter(|e| e.fields.get("repo").and_then(|v| v.as_str()) == Some(ctx.repo.as_str()))
    {
        match e.kind.as_str() {
            "pool_get" => {
                gets += 1;
                if e.fields
                    .get("warm")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    warm += 1;
                }
                if let Some(s) = slot_of(e) {
                    *per_slot.entry(s).or_default() += 1;
                    open_get.insert(s, e.ts);
                }
            }
            "pool_return" => {
                returns += 1;
                if let Some(s) = slot_of(e) {
                    if let Some(g) = open_get.remove(&s) {
                        if e.ts >= g {
                            durations.push(e.ts - g);
                        }
                    }
                }
            }
            // A slot auto-reclaimed after a boot changed under it — the restart-recovery counter.
            "pool_recover" => {
                recoveries += 1;
                // A recover ends an open lease without a matching return; don't leave it dangling.
                if let Some(s) = slot_of(e) {
                    open_get.remove(&s);
                }
            }
            _ => {}
        }
    }

    let leased = leases.slots.len();
    let idle = ctx.size.saturating_sub(leased);
    let cold = gets.saturating_sub(warm);
    let warm_pct = if gets > 0 {
        (warm as f64 / gets as f64 * 100.0).round() as u64
    } else {
        0
    };
    let avg_lease = if durations.is_empty() {
        0
    } else {
        durations.iter().sum::<i64>() / durations.len() as i64
    };

    println!("{BLUE}{} pool metrics — {} slots{NC}", ctx.repo, ctx.size);
    println!("  utilization : {leased}/{} leased, {idle} idle", ctx.size);
    println!("  gets        : {gets}  ({warm} warm / {cold} cold · {warm_pct}% warm-hit)");
    println!("  returns     : {returns}");
    println!("  avg lease   : {}", fmt_age(avg_lease));
    if gets > 0 {
        let churn: Vec<String> = (1..=ctx.size as u64)
            .map(|n| format!("[{n}]{}", per_slot.get(&n).copied().unwrap_or(0)))
            .collect();
        println!("  per-slot    : {}", churn.join(" "));
    }
    print_pool_health(&ctx, &leases, &recovered, recoveries);
    true
}

/// The recovery + health tail of `pool metrics`: what this check just healed (prior-boot slots
/// reclaimed now), the lifetime restart-recovery count, and any leased slot still needing a human
/// (a prior-boot lease left dirty, or a same-boot lease whose holder pid has died). This is the
/// "continuously maintain the health of the leasing worktree" surface — reading it also heals.
fn print_pool_health(
    ctx: &PoolCtx,
    leases: &Leases,
    recovered: &ReclaimReport,
    lifetime_recoveries: u64,
) {
    use bn_core::pool::LeaseLiveness;
    if !recovered.prior_boot.is_empty() {
        println!(
            "  recovered   : {} slot(s) reclaimed from a prior boot now — {}",
            recovered.prior_boot.len(),
            slot_list(&recovered.prior_boot)
        );
    }
    if lifetime_recoveries > 0 {
        println!("  boot recov. : {lifetime_recoveries} (lifetime restart recoveries)");
    }

    let current_boot = current_boot_id();
    let mut attention: Vec<String> = Vec::new();
    for (n, l) in &leases.slots {
        let liveness = bn_core::pool::lease_liveness(
            boot_opt(&l.boot_id),
            current_boot.as_deref(),
            pid_alive(l.pid),
        );
        let dirty = wt::dirty_count(&slot_path(&ctx.pool_dir, *n).to_string_lossy());
        match liveness {
            LeaseLiveness::Live => {}
            LeaseLiveness::StalePriorBoot => attention.push(format!(
                "[{n}] {} — from a prior boot{}; reset with `wt pool return {n} --force`",
                l.branch,
                if dirty > 0 {
                    format!(" ({dirty} uncommitted change(s))")
                } else {
                    String::new()
                }
            )),
            LeaseLiveness::DeadThisBoot => attention.push(format!(
                "[{n}] {} — holder pid {} gone this boot; `wt pool return {n}{}`",
                l.branch,
                l.pid,
                if dirty > 0 { " --force" } else { "" }
            )),
        }
    }
    if attention.is_empty() {
        println!("  health      : ok");
    } else {
        println!("  health      : {} slot(s) need attention", attention.len());
        for line in attention {
            println!("    - {line}");
        }
    }
}

// --- removal: prune + destroy ----------------------------------------------

/// Slot numbers present on disk (`_pool/<n>` dirs) — including over-size ones beyond the config.
fn existing_slot_numbers(pool_dir: &Path) -> Vec<u32> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(pool_dir) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                if let Ok(n) = e.file_name().to_string_lossy().parse::<u32>() {
                    out.push(n);
                }
            }
        }
    }
    out
}

/// Every slot that actually exists for this pool (a `_pool/<n>` dir and/or a lease), sorted.
fn scan_slots(ctx: &PoolCtx, leases: &Leases) -> Vec<u32> {
    let mut set: BTreeMap<u32, ()> = BTreeMap::new();
    for n in existing_slot_numbers(&ctx.pool_dir) {
        set.insert(n, ());
    }
    for n in leases.slots.keys() {
        set.insert(*n, ());
    }
    set.into_keys().collect()
}

/// Slot numbers git has registered for this pool (from `worktree list --porcelain`) — anything on
/// disk but missing here is an orphan.
fn registered_slots(ctx: &PoolCtx) -> std::collections::HashSet<u32> {
    let listing = wt::git_out(&[
        "--git-dir",
        &ctx.bare.to_string_lossy(),
        "worktree",
        "list",
        "--porcelain",
    ])
    .unwrap_or_default();
    let mut out = std::collections::HashSet::new();
    for line in listing.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some((_, n, pdir)) = slot_parts(Path::new(p)) {
                if pdir == ctx.pool_dir {
                    out.insert(n);
                }
            }
        }
    }
    out
}

/// Classify a slot for the removal decision (the I/O; the pure deciders take the result).
fn classify_slot(
    ctx: &PoolCtx,
    leases: &Leases,
    registered: &std::collections::HashSet<u32>,
    n: u32,
) -> SlotClass {
    let slot = slot_path(&ctx.pool_dir, n);
    let exists = slot.is_dir();
    SlotClass {
        leased: leases.slots.contains_key(&n),
        dirty: exists && wt::dirty_count(&slot.to_string_lossy()) > 0,
        in_use: any_proc_in(&slot),
        over_size: n > ctx.size as u32,
        orphan: !ctx.bare.is_dir() || (exists && !registered.contains(&n)),
    }
}

/// SIGTERM any process whose cwd is inside `slot` (graceful — parity with treehouse). Linux only.
#[cfg(target_os = "linux")]
#[allow(
    unsafe_code,
    reason = "libc::kill is required to signal processes discovered through /proc"
)]
fn kill_procs_in(slot: &Path) {
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return;
    };
    for e in rd.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        if let Ok(target) = std::fs::read_link(e.path().join("cwd")) {
            if target.starts_with(slot) {
                // SAFETY: kill(2) with SIGTERM is always safe; an ESRCH race is ignored.
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
        }
    }
}
#[cfg(not(target_os = "linux"))]
fn kill_procs_in(_slot: &Path) {}

/// Remove one slot: SIGTERM its processes, step our cwd out if we're inside it, `git worktree
/// remove --force` (with an orphan fallback), drop its lease (caller persists), tidy the parent.
fn remove_slot(ctx: &PoolCtx, n: u32, leases: &mut Leases) -> bool {
    let slot = slot_path(&ctx.pool_dir, n);
    let bare_s = ctx.bare.to_string_lossy().to_string();
    let branch = leases
        .slots
        .get(&n)
        .map(|l| l.branch.clone())
        .unwrap_or_default();

    kill_procs_in(&slot);
    if wt::cwd().starts_with(&slot) {
        let _ = std::env::set_current_dir(&ctx.pool_dir);
    }
    force_remove_worktree(&bare_s, &slot);
    leases.slots.remove(&n);
    wt::rmdir_if_empty_repo(&slot, &ctx.worktree_dir);
    log_pool_event("pool_destroy", &ctx.repo, n, &branch, None);
    true
}

/// `branch=<x>` for a leased slot, else `idle`.
fn slot_tag(leases: &Leases, n: u32) -> String {
    leases
        .slots
        .get(&n)
        .map(|l| format!("branch={}", l.branch))
        .unwrap_or_else(|| "idle".to_string())
}

/// Print one slot's planned/applied action.
fn print_action(leases: &Leases, n: u32, class: &SlotClass, action: &Action, executed: bool) {
    let tag = slot_tag(leases, n);
    match action {
        Action::Remove => {
            let reason = if class.over_size {
                "over-size"
            } else if class.orphan {
                "orphan"
            } else {
                "targeted"
            };
            let verb = if executed {
                "removed     "
            } else {
                "would remove"
            };
            println!("  {YELLOW}[{n}] {verb}{NC}  {tag}  ({reason})");
        }
        Action::Skip(why) => println!("  {GREEN}[{n}] keep        {NC}  {tag}  ({why})"),
    }
}

/// All repos with a pool configured (for `--all`).
fn pooled_repos() -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(pools_config_path()) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|l| {
            let l = l.split('#').next().unwrap_or("").trim();
            (!l.is_empty())
                .then(|| l.split_whitespace().next().map(|s| s.to_string()))
                .flatten()
        })
        .collect()
}

fn pool_prune(cfg: &Config, repo: Option<String>, yes: bool, all: bool, verbose: bool) -> bool {
    let targets: Vec<Option<String>> = if all {
        let repos = pooled_repos();
        if repos.is_empty() {
            eprintln!(
                "{YELLOW}No pooled repos configured in {}.{NC}",
                pools_config_path().display()
            );
            return true;
        }
        repos.into_iter().map(Some).collect()
    } else {
        vec![repo]
    };
    let mut ok = true;
    for t in targets {
        let Some(ctx) = resolve_pool(cfg, t) else {
            ok = false;
            continue;
        };
        prune_one(&ctx, yes, verbose);
    }
    ok
}

fn prune_one(ctx: &PoolCtx, yes: bool, verbose: bool) {
    let mut leases = read_leases(&ctx.pool_dir);
    let registered = registered_slots(ctx);
    println!(
        "{BLUE}{} pool prune — {} slots configured{NC}",
        ctx.repo, ctx.size
    );
    let mut to_remove = Vec::new();
    for n in scan_slots(ctx, &leases) {
        let class = classify_slot(ctx, &leases, &registered, n);
        let action = prune_decision(class);
        match action {
            Action::Remove => {
                to_remove.push(n);
                print_action(&leases, n, &class, &action, yes);
            }
            // Quietly keep in-size warm slots unless --verbose; always show protected skips.
            Action::Skip("in-size warm slot") if !verbose => {}
            Action::Skip(_) => print_action(&leases, n, &class, &action, yes),
        }
    }
    if yes {
        for n in &to_remove {
            remove_slot(ctx, *n, &mut leases);
        }
        let _ = write_leases(&ctx.pool_dir, &leases);
        println!("{GREEN}Pruned {} slot(s).{NC}", to_remove.len());
    } else {
        println!(
            "{YELLOW}Dry run — {} slot(s) would be pruned. Re-run with --yes.{NC}",
            to_remove.len()
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn pool_destroy(
    cfg: &Config,
    slot: Option<u32>,
    repo: Option<String>,
    all: bool,
    yes: bool,
    flags: DestroyFlags,
) -> bool {
    if flags.include_leased && all {
        eprintln!("{RED}--include-leased can't be combined with --all — target an exact slot.{NC}");
        return false;
    }
    match (slot, all) {
        (None, false) => {
            eprintln!("{RED}Specify a slot number or --all.{NC}");
            return false;
        }
        (Some(_), true) => {
            eprintln!("{RED}Pass either a slot number or --all, not both.{NC}");
            return false;
        }
        _ => {}
    }
    let Some(ctx) = resolve_pool(cfg, repo) else {
        return false;
    };
    let mut leases = read_leases(&ctx.pool_dir);
    let registered = registered_slots(&ctx);
    let targets: Vec<u32> = match slot {
        Some(n) => vec![n],
        None => scan_slots(&ctx, &leases),
    };
    if targets.is_empty() {
        println!(
            "{YELLOW}{} pool is empty — nothing to destroy.{NC}",
            ctx.repo
        );
        return true;
    }
    println!(
        "{BLUE}{} pool destroy — {} slots configured{NC}",
        ctx.repo, ctx.size
    );
    let mut to_remove = Vec::new();
    for n in &targets {
        // A specific slot that doesn't exist at all → report and skip.
        if !slot_path(&ctx.pool_dir, *n).is_dir() && !leases.slots.contains_key(n) {
            println!("  {YELLOW}[{n}] no such slot{NC}");
            continue;
        }
        let class = classify_slot(&ctx, &leases, &registered, *n);
        let action = destroy_decision(class, flags);
        if action == Action::Remove {
            to_remove.push(*n);
        }
        print_action(&leases, *n, &class, &action, yes);
    }
    if yes {
        for n in &to_remove {
            remove_slot(&ctx, *n, &mut leases);
        }
        let _ = write_leases(&ctx.pool_dir, &leases);
        println!("{GREEN}Destroyed {} slot(s).{NC}", to_remove.len());
    } else {
        println!(
            "{YELLOW}Dry run — {} slot(s) would be destroyed. Re-run with --yes.{NC}",
            to_remove.len()
        );
    }
    true
}

/// Template written when a repo has no `build.sh` yet — the user fills in the install/build.
const BUILD_TEMPLATE: &str = "#!/bin/sh
# bn wt pool warm runs this in each pool slot (cwd = the slot) to install deps / build, so the
# slot stays warm across get/return. Keep it idempotent — it re-runs on every `wt pool warm`.
set -eu

# Fill in your repo's install/build, e.g.:
#   yarn install --frozen-lockfile
#   cargo build
true
";

/// Whether we're running inside a tmux client.
fn in_tmux() -> bool {
    std::env::var("TMUX")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// Spawn the warm in a `pool-warm` window in the repo's own tmux session (session-per-repo
/// convention, same as `pool get`), so a long warm is visible. Returns false if tmux is
/// unreachable so the caller falls back to inline.
fn warm_in_window(ctx: &PoolCtx, new: bool) -> bool {
    use std::io::IsTerminal;
    let session = ctx.repo.clone();
    let target = format!("={session}");
    // Create the repo's session if it isn't open yet.
    let has = crate::proc::tmux_run(&["has-session", "-t", &target]);
    if !has && !crate::proc::tmux_run(&["new-session", "-d", "-s", &session]) {
        return false; // tmux unavailable → inline fallback
    }
    let self_exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "bn".to_string());
    let new_flag = if new { " --new" } else { "" };
    let runner = format!(
        "{} wt pool warm --repo {}{new_flag} --here; echo; echo '── warm {} done — press enter to close ──'; read",
        wt::shq(&self_exe),
        wt::shq(&ctx.repo),
        ctx.repo
    );
    if !crate::proc::tmux_run(&["new-window", "-t", &target, "-n", "pool-warm", &runner]) {
        return false;
    }
    println!(
        "{GREEN}Warming {} in tmux window {session}:pool-warm (watch it there).{NC}",
        ctx.repo
    );
    // Follow into it for a human at an interactive terminal.
    if in_tmux() && std::io::stdout().is_terminal() {
        let _ = crate::proc::tmux_run(&["switch-client", "-t", &target]);
        let _ = crate::proc::tmux_run(&["select-window", "-t", &format!("{target}:pool-warm")]);
    }
    true
}

fn pool_warm(cfg: &Config, repo: Option<String>, here: bool, new: bool) -> bool {
    let Some(ctx) = resolve_pool(cfg, repo) else {
        return false;
    };
    // Warming is long (checkout + build per slot), so inside tmux run it in a window in the repo's
    // own session — you watch it live instead of it blocking/hiding. `--here` (and the window
    // itself) run inline.
    if !here && in_tmux() && warm_in_window(&ctx, new) {
        return true;
    }
    let _ = std::fs::create_dir_all(&ctx.pool_dir);
    let bare_s = ctx.bare.to_string_lossy().to_string();

    // Resolve the repo's `build.sh` (same convention as `bn build`); scaffold a template if absent.
    let rctx = bn_core::resolver::Resolved {
        repo: ctx.repo.to_lowercase(),
        branch: "main".to_string(),
    };
    let build = match bn_core::resolver::job_script(cfg, Some(&rctx), "build") {
        Some((_, path)) => Some(path),
        None => {
            let dir = cfg.note_dir(&rctx).join("jobs");
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join("build.sh");
            // Atomic scaffold: write a temp sibling, mark it executable, then rename into place —
            // an atomic swap on the same fs, so a concurrent reader (e.g. `bn verify`/`bn build`
            // resolving this script) never sees a half-written build.sh, only the old file or the
            // complete new one.
            let tmp = dir.join(format!("build.sh.{}.tmp", std::process::id()));
            let mut ok = std::fs::write(&tmp, BUILD_TEMPLATE).is_ok();
            if ok {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
                }
                ok = std::fs::rename(&tmp, &path).is_ok();
            }
            if ok {
                eprintln!("{YELLOW}Scaffolded build script: {}{NC}", path.display());
                eprintln!(
                    "{YELLOW}Fill in the install/build command, then re-run `wt pool warm` to install deps.{NC}"
                );
            } else {
                let _ = std::fs::remove_file(&tmp);
            }
            None // template is a no-op this pass — just create the checkouts
        }
    };

    // Never warm a slot that's in use or holds local work: a leased slot belongs to a running
    // agent, and a dirty slot has uncommitted changes — running build.sh in either is disruptive
    // and slow (a full dep install on top of someone's work, as a stale WSL lease could trigger).
    // So we skip both and only warm idle-clean and brand-new slots. `--new` narrows further to
    // slots with no checkout yet, so a re-warm after growing the pool fills only the freshly-added
    // slots and leaves every existing one (idle included) untouched.
    let leases = read_leases(&ctx.pool_dir);
    println!("{BLUE}Warming {} slots for {}…{NC}", ctx.size, ctx.repo);
    // Phase 1 — ensure every non-skipped slot has a live checkout. Serial on purpose: `git
    // worktree add` takes the bare repo's lock, so parallel adds would only contend. Never warm a
    // slot that's leased (a running agent owns it), dirty (uncommitted local work), or — under
    // `--new` — already checked out; those are counted as skipped and left untouched.
    let mut skipped = 0u32;
    let mut ready: Vec<(u32, PathBuf)> = Vec::new();
    for n in 1..=ctx.size as u32 {
        let slot = slot_path(&ctx.pool_dir, n);
        let slot_s = slot.to_string_lossy().to_string();
        let exists = slot.exists();
        if new && exists {
            eprintln!("{BLUE}[{n}] existing — skipping (--new){NC}");
            skipped += 1;
            continue;
        }
        if let Some(l) = leases.slots.get(&n) {
            eprintln!("{YELLOW}[{n}] leased {} — skipping{NC}", l.branch);
            skipped += 1;
            continue;
        }
        if exists && is_live_slot(&slot) && wt::dirty_count(&slot_s) > 0 {
            eprintln!("{YELLOW}[{n}] dirty — skipping{NC}");
            skipped += 1;
            continue;
        }
        if is_live_slot(&slot) {
            eprintln!("{BLUE}[{n}] checkout present{NC}");
        } else {
            if exists {
                eprintln!("{YELLOW}[{n}] broken checkout — recreating…{NC}");
                force_remove_worktree(&bare_s, &slot);
            } else {
                eprintln!("{BLUE}[{n}] creating checkout…{NC}");
            }
            if !add_detached_slot(&bare_s, &slot.to_string_lossy()) {
                eprintln!("{RED}[{n}] failed to create checkout{NC}");
                continue;
            }
        }
        ready.push((n, slot));
    }

    // Phase 2 — build. A live checkout with no build script is already warm. With one, prime the
    // first slot to seed the shared sccache cache (repo `.cargo/config.toml`), then build the rest
    // in parallel: they compile the *same* base commit, so they land as cache hits instead of
    // repeating the work — collapsing a warm from N cold builds to ~1 build + N-1 cache/link
    // passes.
    let warmed = match &build {
        None => ready.len() as u32,
        Some(script) => build_slots_warm(script, &ready),
    };
    // A ready slot whose build.sh failed is still leasable, just not fully provisioned — count it
    // separately (made) from the fully-warmed slots so the summary can distinguish the two.
    let made = ready.len() as u32 - warmed;
    // Record the refresh so `bn metrics` can surface pool freshness — how many slots warmed, and
    // when the pool was last refreshed (one event per `pool warm`, alongside pool_get/pool_return).
    {
        let mut f = serde_json::Map::new();
        f.insert("repo".to_string(), ctx.repo.clone().into());
        f.insert("warmed".to_string(), warmed.into());
        f.insert("size".to_string(), (ctx.size as u64).into());
        let _ = bn_core::events::append("pool_warm", f);
    }
    let mut summary = format!("Warmed {warmed}/{} slots for {}", ctx.size, ctx.repo);
    if made > 0 {
        summary += &format!(" · {made} checked out but build failed");
    }
    if skipped > 0 {
        summary += &format!(" · {skipped} skipped (leased/dirty/existing)");
    }
    println!("{GREEN}{summary}.{NC}");
    true
}

/// Build every ready slot: prime the first at full parallelism (seeding the sccache cache), then
/// build the rest concurrently sharing a job budget so the box isn't oversubscribed. Returns how
/// many built cleanly.
fn build_slots_warm(script: &Path, ready: &[(u32, PathBuf)]) -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    let Some(((first_n, first_slot), rest)) = ready.split_first() else {
        return 0;
    };
    let mut warmed = 0u32;
    eprintln!("{BLUE}[{first_n}] building (priming cache)…{NC}");
    if run_build_script(script, first_slot, None) {
        warmed += 1;
    } else {
        eprintln!("{RED}[{first_n}] build failed{NC}");
    }
    if rest.is_empty() {
        return warmed;
    }
    // Divide the cores across the concurrent builds (at least one each). sccache hits keep these
    // light, but an unprimed crate still compiles, so bound the fan-out rather than letting each
    // `cargo` grab every core.
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let jobs = (cpus / rest.len()).max(1);
    let ok = AtomicU32::new(0);
    std::thread::scope(|s| {
        for (n, slot) in rest {
            let ok = &ok;
            s.spawn(move || {
                eprintln!("{BLUE}[{n}] building…{NC}");
                if run_build_script(script, slot, Some(jobs)) {
                    ok.fetch_add(1, Ordering::Relaxed);
                } else {
                    eprintln!("{RED}[{n}] build failed{NC}");
                }
            });
        }
    });
    warmed + ok.load(Ordering::Relaxed)
}

/// Run a repo's `build.sh` in `slot` (cwd = slot), with `~/.local/bin:~/.bin` prepended to PATH
/// like the cron job runner, so tools the script shells out to are found. Inherits stdio.
/// `CARGO_INCREMENTAL=0`: a warm builds a fresh checkout with no prior incremental state to reuse,
/// and disabling it lets sccache cache the workspace crates too. `jobs` caps `cargo`'s parallelism
/// (`CARGO_BUILD_JOBS`) so several slots building at once share the cores instead of each grabbing
/// all of them.
fn run_build_script(script: &Path, slot: &Path, jobs: Option<usize>) -> bool {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = std::env::var("PATH").unwrap_or_default();
    let mut cmd = Command::new("/bin/sh");
    cmd.arg(script)
        .current_dir(slot)
        .env("PATH", format!("{home}/.local/bin:{home}/.bin:{path}"))
        .env("CARGO_INCREMENTAL", "0");
    if let Some(j) = jobs {
        cmd.env("CARGO_BUILD_JOBS", j.to_string());
    }
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

/// Compact human age: `42s`, `5m`, `3h`, `2d`.
fn fmt_age(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// How long the box must have been quiet before an idle-time pool refresh runs — a warm rebuilds
/// slots, so we never do it on top of active work. A short lull (the "device idle" intent), not a
/// full night, so refreshes land in the gaps between sessions.
const POOL_REFRESH_IDLE_MINS: i64 = 20;

/// Don't re-warm a repo whose slots were refreshed within this window, even across a long idle
/// stretch — bounds an all-day-idle box to a few builds per repo instead of one every tick.
const POOL_REFRESH_MIN_AGE_SECS: i64 = 4 * 3600;

/// What an idle-time pool refresh should do this tick.
#[derive(Debug, PartialEq, Eq)]
enum PoolRefreshPlan {
    /// Nothing to warm (box active, no pools, or all fresh) — the reason is logged, not an error.
    Skip(String),
    /// Warm these repos (de-duped, config order preserved).
    Warm(Vec<String>),
}

/// Freshest `pool_warm` timestamp per repo from the machine-local event log — pools are per-box,
/// so we fold local events only. Feeds the staleness guard that skips recently-refreshed repos.
fn last_pool_warm_by_repo(evs: &[bn_core::events::Event]) -> BTreeMap<String, i64> {
    let mut m = BTreeMap::new();
    for e in evs.iter().filter(|e| e.kind == "pool_warm") {
        if let Some(repo) = e.fields.get("repo").and_then(|v| v.as_str()) {
            let slot = m.entry(repo.to_string()).or_insert(e.ts);
            if e.ts > *slot {
                *slot = e.ts;
            }
        }
    }
    m
}

/// Decide which pooled repos to warm on an idle tick: none when the box is busy (never build over
/// active work) or nothing is pooled, else every repo not refreshed within `min_age` seconds.
/// De-dupes while preserving config order. Pure so the gating is unit-tested without a real pool.
fn plan_idle_pool_refresh(
    idle: bool,
    repos: &[String],
    last_warm: &BTreeMap<String, i64>,
    now: i64,
    min_age: i64,
) -> PoolRefreshPlan {
    if repos.is_empty() {
        return PoolRefreshPlan::Skip("no pooled repos configured".into());
    }
    if !idle {
        return PoolRefreshPlan::Skip("box is active — deferring pool refresh".into());
    }
    let mut targets = Vec::new();
    let mut seen = BTreeSet::new();
    for r in repos {
        if !seen.insert(r.as_str()) {
            continue;
        }
        let stale = last_warm.get(r).is_none_or(|ts| now - *ts >= min_age);
        if stale {
            targets.push(r.clone());
        }
    }
    if targets.is_empty() {
        return PoolRefreshPlan::Skip("all pools refreshed recently".into());
    }
    PoolRefreshPlan::Warm(targets)
}

/// `pool-warm` cron — refresh every repo's warm-slot pool while the device is idle, so a leased
/// dispatch always lands on a ready checkout. Off-peak by schedule and idle-gated at runtime (never
/// builds on top of active work), with a per-repo staleness guard so a long idle stretch re-warms
/// each repo at most every few hours. Each repo's warm records a `pool_warm` event, so `bn metrics`
/// shows when the pools were last refreshed (the "record when we refresh pools" ask).
pub(crate) fn job_pool_warm(cfg: &Config, _ctx: Option<&bn_core::resolver::Resolved>) -> i32 {
    let now = bn_core::util::now_epoch_secs();
    let repos = pooled_repos();
    let last_warm = last_pool_warm_by_repo(&bn_core::events::read_all());
    match plan_idle_pool_refresh(
        bn_core::idle::is_idle(cfg, now, POOL_REFRESH_IDLE_MINS),
        &repos,
        &last_warm,
        now,
        POOL_REFRESH_MIN_AGE_SECS,
    ) {
        PoolRefreshPlan::Skip(why) => {
            println!("pool-warm: {why}");
            0
        }
        PoolRefreshPlan::Warm(targets) => {
            println!(
                "pool-warm: refreshing {} pool(s): {}",
                targets.len(),
                targets.join(", ")
            );
            let mut failed = 0;
            for repo in &targets {
                // `here = true`: warm inline in this cron window instead of spawning a nested one.
                if !pool_warm(cfg, Some(repo.clone()), true, false) {
                    eprintln!("pool-warm: {repo} refresh failed");
                    failed += 1;
                }
            }
            i32::from(failed > 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The pool clock delegates to bn_core::util::now_epoch_secs, so `$BN_NOW` (the deterministic
    // override) is honored here — the old raw SystemTime body silently ignored it, drifting pool
    // slot-age math off the test clock. tat.rs's now_epoch_secs delegates to the same fn.
    #[test]
    fn now_epoch_honors_bn_now_override() {
        // BN_NOW is process-global; serialize on bn-core's shared env lock so this can't race any
        // other env-touching test in the bn-cli binary (issue #358). The guard restores on drop.
        let _g = bn_core::test_support::env_lock();
        let _now = bn_core::test_support::EnvGuard::set("BN_NOW", "1234567890");
        assert_eq!(now_epoch(), 1_234_567_890);
    }

    #[test]
    fn prune_keeps_in_size_warm_removes_stale() {
        // In-size, clean, unleased idle slot = the pool itself → keep.
        assert_eq!(
            prune_decision(SlotClass::default()),
            Action::Skip("in-size warm slot")
        );
        // Over-size or orphan, otherwise safe → remove.
        assert_eq!(
            prune_decision(SlotClass {
                over_size: true,
                ..Default::default()
            }),
            Action::Remove
        );
        assert_eq!(
            prune_decision(SlotClass {
                orphan: true,
                ..Default::default()
            }),
            Action::Remove
        );
    }

    #[test]
    fn prune_never_touches_leased_dirty_inuse_even_if_over_size() {
        let base = SlotClass {
            over_size: true,
            ..Default::default()
        };
        assert_eq!(
            prune_decision(SlotClass {
                leased: true,
                ..base
            }),
            Action::Skip("leased")
        );
        assert_eq!(
            prune_decision(SlotClass {
                in_use: true,
                ..base
            }),
            Action::Skip("in use")
        );
        assert_eq!(
            prune_decision(SlotClass {
                dirty: true,
                ..base
            }),
            Action::Skip("uncommitted changes")
        );
    }

    #[test]
    fn destroy_skips_protected_without_flags_removes_with() {
        let none = DestroyFlags::default();
        let all = DestroyFlags {
            include_leased: true,
            include_dirty: true,
            include_in_use: true,
        };
        // Plain idle slot → remove.
        assert_eq!(destroy_decision(SlotClass::default(), none), Action::Remove);
        // Protected classes skipped without the flag, removed with it.
        let leased = SlotClass {
            leased: true,
            ..Default::default()
        };
        assert!(matches!(destroy_decision(leased, none), Action::Skip(_)));
        assert_eq!(destroy_decision(leased, all), Action::Remove);
        let dirty = SlotClass {
            dirty: true,
            ..Default::default()
        };
        assert!(matches!(destroy_decision(dirty, none), Action::Skip(_)));
        assert_eq!(destroy_decision(dirty, all), Action::Remove);
        let in_use = SlotClass {
            in_use: true,
            ..Default::default()
        };
        assert!(matches!(destroy_decision(in_use, none), Action::Skip(_)));
        assert_eq!(destroy_decision(in_use, all), Action::Remove);
    }

    #[test]
    fn slot_parts_and_find_root() {
        let slot = PathBuf::from("/home/x/worktree/arena/_pool/2");
        let (repo, n, pdir) = slot_parts(&slot).unwrap();
        assert_eq!(repo, "arena");
        assert_eq!(n, 2);
        assert_eq!(pdir, PathBuf::from("/home/x/worktree/arena/_pool"));

        // A path that isn't a slot.
        assert!(slot_parts(&PathBuf::from("/home/x/worktree/arena/feature")).is_none());

        // cwd nested inside a slot resolves back to the slot root.
        let deep = PathBuf::from("/home/x/worktree/arena/_pool/2/src/lib");
        assert_eq!(find_slot_root(&deep), Some(slot));
    }

    #[test]
    fn leases_round_trip() {
        let mut l = Leases::default();
        l.slots.insert(
            1,
            Lease {
                branch: "feat-x".into(),
                holder: "edd".into(),
                pid: 4321,
                at: 1000,
                boot_id: "boot-xyz".into(),
            },
        );
        let json = serde_json::to_string(&l).unwrap();
        let back: Leases = serde_json::from_str(&json).unwrap();
        assert_eq!(back.slots.len(), 1);
        assert_eq!(back.slots.get(&1).unwrap().branch, "feat-x");
        assert_eq!(back.slots.get(&1).unwrap().boot_id, "boot-xyz");
    }

    #[test]
    fn lease_without_boot_id_parses_as_unknown_boot() {
        // A leases.json written before boot-scoping (no `boot_id`) must still load — the field
        // defaults to empty, which `boot_opt` maps to "unknown boot" so liveness stays pid-only.
        let json = r#"{"slots":{"1":{"branch":"feat-x","holder":"edd","pid":4321,"at":1000}}}"#;
        let back: Leases = serde_json::from_str(json).unwrap();
        let lease = back.slots.get(&1).unwrap();
        assert_eq!(lease.boot_id, "");
        assert_eq!(boot_opt(&lease.boot_id), None);
    }

    #[test]
    fn fmt_age_buckets() {
        assert_eq!(fmt_age(42), "42s");
        assert_eq!(fmt_age(120), "2m");
        assert_eq!(fmt_age(7200), "2h");
        assert_eq!(fmt_age(172_800), "2d");
    }

    // #457 acceptance: `bn dispatch --pool` leases a warm slot via this same primitive. Drive
    // `lease_for_dispatch` against a real bn-style bare repo + worktree layout (WT_ROOT) with a
    // pools config marking the repo pooled, and assert: a fresh lease creates a slot checked out on
    // the branch; a same-branch re-lease is an idempotent WARM hit (fresh=false, checkout
    // untouched — no fresh install); distinct branches take distinct slots; and a full pool refuses
    // (whereupon dispatch falls back to a cold worktree).
    #[cfg(unix)]
    #[test]
    fn lease_for_dispatch_leases_reuses_and_fills_pool() {
        let git = |args: &[&str]| assert!(crate::git::git_ok(args), "git {args:?} failed");
        let head_branch = |slot: &Path| {
            crate::git::git_capture(
                &[
                    "-C",
                    &slot.to_string_lossy(),
                    "rev-parse",
                    "--abbrev-ref",
                    "HEAD",
                ],
                None,
                true,
            )
        };

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Upstream repo with one commit on main.
        let up = root.join("upstream");
        let up_s = up.to_string_lossy().to_string();
        std::fs::create_dir_all(&up).unwrap();
        git(&["-C", &up_s, "init", "-q", "-b", "main"]);
        git(&["-C", &up_s, "config", "user.email", "t@example.com"]);
        git(&["-C", &up_s, "config", "user.name", "t"]);
        std::fs::write(up.join("a.txt"), "1").unwrap();
        git(&["-C", &up_s, "add", "-A"]);
        git(&["-C", &up_s, "commit", "-qm", "c1"]);

        // Bare clone under the WT_ROOT bare dir, bn-style, so `layout()` finds `repo.git`.
        let bare = root.join("bare").join("repo.git");
        std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
        let bare_s = bare.to_string_lossy().to_string();
        git(&["clone", "--bare", "-q", &up_s, &bare_s]);
        git(&[
            "--git-dir",
            &bare_s,
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ]);
        git(&["--git-dir", &bare_s, "fetch", "-q", "origin"]);

        // Pools config marks `repo` pooled with 3 slots: the reserved controller slot 1, plus two
        // worker slots (2 and 3) — the only ones a dispatch may lease.
        let pools = root.join("pools");
        std::fs::write(&pools, "repo 3\n").unwrap();

        // Isolate process-global env behind bn-core's shared lock: WT_ROOT drives `layout()`,
        // WT_POOLS the pools config, BN_STATE_DIR the best-effort pool event log.
        let _g = bn_core::test_support::env_lock();
        let _root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
        let _pools = bn_core::test_support::EnvGuard::set("WT_POOLS", &pools);
        let state = tempfile::tempdir().unwrap();
        let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", state.path());

        let cfg = bn_core::test_support::config(root);

        // Fresh lease → lowest idle *worker* slot (2, since 1 is the controller's), a live checkout
        // switched onto the branch.
        let (slot1, fresh1) = lease_for_dispatch(&cfg, "repo", "dispatch/x").expect("first lease");
        assert!(fresh1, "a new branch lease is fresh");
        assert!(
            slot1.ends_with("_pool/2"),
            "lowest idle worker slot, got {slot1:?}"
        );
        assert!(is_live_slot(&slot1), "slot is a live warm checkout");
        assert_eq!(head_branch(&slot1).as_deref(), Some("dispatch/x"));

        // A warm-cache artifact (stand-in for node_modules) must survive a warm re-lease.
        std::fs::write(slot1.join("warm_cache_marker"), "warm").unwrap();

        // Same branch again → idempotent warm hit: same slot, not fresh, checkout untouched.
        let (slot1b, fresh1b) = lease_for_dispatch(&cfg, "repo", "dispatch/x").expect("re-lease");
        assert_eq!(slot1b, slot1, "same branch hands back the same slot");
        assert!(
            !fresh1b,
            "re-lease of an already-leased branch is not fresh"
        );
        assert!(
            slot1.join("warm_cache_marker").exists(),
            "warm cache survived — no fresh checkout/install"
        );

        // A distinct branch takes the next idle worker slot.
        let (slot2, fresh2) = lease_for_dispatch(&cfg, "repo", "dispatch/y").expect("second lease");
        assert!(fresh2);
        assert!(slot2.ends_with("_pool/3"), "second slot, got {slot2:?}");

        // Worker range full (2 and 3 leased to a live holder) → refuse; dispatch falls back to
        // cold. The idle controller slot is never handed out to cover the shortfall.
        assert!(
            lease_for_dispatch(&cfg, "repo", "dispatch/z").is_none(),
            "a full pool refuses to lease"
        );
        assert!(
            !slot_path(&pool_dir(&root.join("worktree"), "repo"), 1).exists(),
            "the controller slot was never created for a worker"
        );
    }

    /// Build a bn-style bare repo (`<root>/bare/repo.git`) with one commit on `main`, so `layout()`
    /// (driven by `WT_ROOT=<root>`) finds it and the pool can cut slots off it. Shared by the
    /// recovery + path-resolution tests below.
    #[cfg(unix)]
    fn build_pooled_bare(root: &Path) {
        let git = |args: &[&str]| assert!(crate::git::git_ok(args), "git {args:?} failed");
        let up = root.join("upstream");
        let up_s = up.to_string_lossy().to_string();
        std::fs::create_dir_all(&up).unwrap();
        git(&["-C", &up_s, "init", "-q", "-b", "main"]);
        git(&["-C", &up_s, "config", "user.email", "t@example.com"]);
        git(&["-C", &up_s, "config", "user.name", "t"]);
        std::fs::write(up.join("a.txt"), "1").unwrap();
        git(&["-C", &up_s, "add", "-A"]);
        git(&["-C", &up_s, "commit", "-qm", "c1"]);
        let bare = root.join("bare").join("repo.git");
        std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
        let bare_s = bare.to_string_lossy().to_string();
        git(&["clone", "--bare", "-q", &up_s, &bare_s]);
        git(&[
            "--git-dir",
            &bare_s,
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ]);
        git(&["--git-dir", &bare_s, "fetch", "-q", "origin"]);
    }

    // The WSL/reboot recovery guarantee: a lease survives on disk across a restart, but its recorded
    // pid can be recycled to a *live* process on the next boot — so a pid check alone would pin the
    // slot leased forever. Boot-scoping fixes it: when the boot id changes, the prior-boot lease is
    // provably stale and auto-reclaimed on the next access, even though its pid still reads alive.
    #[cfg(unix)]
    #[test]
    fn lease_recovers_after_a_simulated_wsl_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        build_pooled_bare(root);
        // The smallest pool: the reserved controller slot plus a single worker (slot 2). One held
        // worker means "full", so only a genuine recovery can free it.
        let pools = root.join("pools");
        std::fs::write(&pools, "repo 2\n").unwrap();

        let _g = bn_core::test_support::env_lock();
        let _root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
        let _pools = bn_core::test_support::EnvGuard::set("WT_POOLS", &pools);
        let state = tempfile::tempdir().unwrap();
        let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", state.path());
        let _boot_a = bn_core::test_support::EnvGuard::set("BN_BOOT_ID", "boot-A");
        let cfg = bn_core::test_support::config(root);

        // Boot A: lease the only worker slot to X. Its holder pid is this test's live parent.
        let (slot1, fresh1) = lease_for_dispatch(&cfg, "repo", "dispatch/x").expect("first lease");
        assert!(fresh1);
        assert!(slot1.ends_with("_pool/2"));

        // Still boot A, holder alive → the durable lease holds; a second branch can't take it, and
        // it is NOT pre-emptively reclaimed (we don't reclaim live-boot leases).
        assert!(
            lease_for_dispatch(&cfg, "repo", "dispatch/y").is_none(),
            "a live-boot lease is durable, not reclaimed while the pool is merely contended"
        );

        // Simulate a WSL restart: the boot id changes. The X lease's pid may still read as alive
        // (recycled), but its boot no longer matches, so it's provably stale.
        drop(_boot_a);
        let _boot_b = bn_core::test_support::EnvGuard::set("BN_BOOT_ID", "boot-B");

        let (slot1b, fresh1b) = lease_for_dispatch(&cfg, "repo", "dispatch/y")
            .expect("after a reboot the prior-boot lease is auto-recovered and reusable");
        assert_eq!(
            slot1b, slot1,
            "the recovered slot is reused for the new lease"
        );
        assert!(fresh1b, "Y is a fresh lease of the recovered slot");

        // The slot is now leased to Y and re-stamped with the current boot.
        let pdir = root.join("worktree").join("repo").join("_pool");
        let leases = read_leases(&pdir);
        let held = leases
            .slots
            .get(&2)
            .expect("the worker slot is leased to Y after recovery");
        assert_eq!(held.branch, "dispatch/y");
        assert_eq!(
            held.boot_id, "boot-B",
            "the recovered slot carries the current boot id, not the stale one"
        );
    }

    // `pool metrics <PATH>` resolves the repo (and thus its pool) from a filesystem path — a pool
    // slot, or the bare dir — so you can point it at any checkout. A non-repo path resolves to none.
    #[cfg(unix)]
    #[test]
    fn metrics_resolves_repo_from_a_slot_or_bare_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        build_pooled_bare(root);
        let pools = root.join("pools");
        std::fs::write(&pools, "repo 1\n").unwrap();

        let _g = bn_core::test_support::env_lock();
        let _root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
        let _pools = bn_core::test_support::EnvGuard::set("WT_POOLS", &pools);
        let state = tempfile::tempdir().unwrap();
        let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", state.path());
        let cfg = bn_core::test_support::config(root);

        // Materialize a slot so there's a real worktree `.git` pointer to resolve through.
        let (slot1, _) = lease_for_dispatch(&cfg, "repo", "dispatch/x").expect("lease");

        // A slot path resolves to the repo name + its bare.
        let (name, bare) = resolve_metrics_repo(&cfg, Some(&slot1.to_string_lossy()), None)
            .expect("a pool slot path resolves");
        assert_eq!(name, "repo");
        assert!(bare.ends_with("repo.git"), "resolved bare, got {bare:?}");

        // The bare dir itself resolves too.
        let (name2, _) = resolve_metrics_repo(&cfg, Some(&bare.to_string_lossy()), None)
            .expect("a bare repo path resolves");
        assert_eq!(name2, "repo");

        // A path that's neither a bare nor inside a worktree resolves to nothing.
        let nope = root.join("nope");
        assert!(resolve_metrics_repo(&cfg, Some(&nope.to_string_lossy()), None).is_none());
    }

    fn warm_ev(repo: &str, ts: i64) -> bn_core::events::Event {
        let mut fields = serde_json::Map::new();
        fields.insert("repo".to_string(), repo.into());
        fields.insert("warmed".to_string(), 1.into());
        bn_core::events::Event {
            kind: "pool_warm".to_string(),
            ts,
            fields,
        }
    }

    #[test]
    fn last_pool_warm_by_repo_keeps_the_freshest_per_repo() {
        let evs = vec![
            warm_ev("arena", 100),
            warm_ev("arena", 300), // newer arena wins
            warm_ev("bn", 200),
            // A non-pool_warm event and a repo-less one are both ignored.
            bn_core::events::Event {
                kind: "pool_get".to_string(),
                ts: 999,
                fields: serde_json::Map::new(),
            },
        ];
        let m = last_pool_warm_by_repo(&evs);
        assert_eq!(m.get("arena"), Some(&300));
        assert_eq!(m.get("bn"), Some(&200));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn plan_idle_pool_refresh_gates_on_idle_pools_and_staleness() {
        let now = 1_000_000;
        let repos = vec!["arena".to_string(), "bn".to_string(), "arena".to_string()];
        let min_age = 4 * 3600;

        // Busy box → skip regardless of pools.
        assert_eq!(
            plan_idle_pool_refresh(false, &repos, &BTreeMap::new(), now, min_age),
            PoolRefreshPlan::Skip("box is active — deferring pool refresh".into())
        );

        // No pools configured → skip.
        assert_eq!(
            plan_idle_pool_refresh(true, &[], &BTreeMap::new(), now, min_age),
            PoolRefreshPlan::Skip("no pooled repos configured".into())
        );

        // Idle + never warmed → warm every pooled repo, de-duped in config order.
        assert_eq!(
            plan_idle_pool_refresh(true, &repos, &BTreeMap::new(), now, min_age),
            PoolRefreshPlan::Warm(vec!["arena".to_string(), "bn".to_string()])
        );

        // Idle but a repo was warmed inside the window → only the stale one is refreshed.
        let mut last = BTreeMap::new();
        last.insert("arena".to_string(), now - 3600); // 1h ago → fresh, skipped
        last.insert("bn".to_string(), now - 5 * 3600); // 5h ago → stale, warmed
        assert_eq!(
            plan_idle_pool_refresh(true, &repos, &last, now, min_age),
            PoolRefreshPlan::Warm(vec!["bn".to_string()])
        );

        // Idle but all pools fresh → skip.
        let mut fresh = BTreeMap::new();
        fresh.insert("arena".to_string(), now - 60);
        fresh.insert("bn".to_string(), now - 60);
        assert_eq!(
            plan_idle_pool_refresh(true, &repos, &fresh, now, min_age),
            PoolRefreshPlan::Skip("all pools refreshed recently".into())
        );
    }
}
