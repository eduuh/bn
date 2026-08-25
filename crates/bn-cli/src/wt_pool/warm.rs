//! Pool warming — the `bn wt pool warm` command and the `pool-warm` cron behind it.
//!
//! Split out of `wt_pool.rs` under the 1000-line ratchet. The seam is deliberate rather than
//! arbitrary: warming is the one pool operation that *builds* (checkout per slot, then `bn build`
//! in each), and it is the only one with a cron entry point, so it carries its own resolution
//! rules and its own failure semantics. Slot classification, leasing, prune/destroy and status
//! stay in the parent.
use super::*;

/// Template written when a repo has no `build.sh` yet — the user fills in the install/build.
const BUILD_TEMPLATE: &str = "#!/bin/sh
# `bn build` runs this (cwd = the repo checkout) to install deps / build. `bn wt pool warm` runs
# `bn build` in every pool slot, so the slot stays warm across get/return — and a slot is built
# exactly the way you build by hand. Keep it idempotent: it re-runs on every `wt pool warm`.
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

/// `bn wt pool warm` — resolve the named repo through [`resolve_pool`], which requires it to be
/// registered in the pools config, then warm it. The strict resolver is deliberate here: a person
/// typed a repo name, so "that repo isn't pooled" is the answer they need. The cron warms a
/// *dispatch-created* pool instead and resolves accordingly — see [`warm_pool_for_cron`].
pub(crate) fn pool_warm(cfg: &Config, repo: Option<String>, here: bool, new: bool) -> bool {
    let Some(ctx) = resolve_pool(cfg, repo) else {
        return false;
    };
    warm_pool(cfg, &ctx, here, new)
}

/// Warm one already-resolved pool: ensure every non-skipped slot holds a live checkout, then build
/// them. Split from [`pool_warm`] so the caller owns *how the pool is resolved* — the two callers
/// disagree on that, and conflating them is what made the `pool-warm` cron fail on every pool
/// `bn dispatch` created implicitly.
fn warm_pool(cfg: &Config, ctx: &PoolCtx, here: bool, new: bool) -> bool {
    // Warming is long (checkout + build per slot), so inside tmux run it in a window in the repo's
    // own session — you watch it live instead of it blocking/hiding. `--here` (and the window
    // itself) run inline.
    if !here && in_tmux() && warm_in_window(ctx, new) {
        return true;
    }
    let _ = std::fs::create_dir_all(&ctx.pool_dir);
    let bare_s = ctx.bare.to_string_lossy().to_string();

    // Does this repo have a `build.sh`? Only to decide whether there's anything to build (and to
    // scaffold a template if absent) — the build itself is `bn build`, which resolves the script
    // the same way, so the pool can never drift from the command a human runs by hand.
    let rctx = bn_core::resolver::Resolved {
        repo: ctx.repo.to_lowercase(),
        branch: "main".to_string(),
    };
    let build = match bn_core::resolver::job_script(cfg, Some(&rctx), "build") {
        Some((_, path)) => Some(path),
        None => {
            // Machine-independent `JobScope::Repo` tier — `<notes_dir_for(repo)>/jobs/<repo>/` —
            // so a freshly scaffolded build.sh is immediately shared with every machine that
            // syncs the notes repo, instead of landing in a per-machine copy nobody else sees.
            let dir = cfg.notes_dir_for(&rctx.repo).join("jobs").join(&rctx.repo);
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
    // `--new` — already holding a *live* checkout; those are counted as skipped and left untouched.
    let mut skipped = 0u32;
    let mut ready: Vec<(u32, PathBuf)> = Vec::new();
    for n in 1..=ctx.size as u32 {
        let slot = slot_path(&ctx.pool_dir, n);
        let slot_s = slot.to_string_lossy().to_string();
        let exists = slot.exists();
        // `--new` fills only slots that lack a usable checkout, so it must key off *liveness*, not
        // mere directory presence: a broken stub (dir left by an interrupted `worktree add`, no
        // `.git`) has no checkout and still needs one. Skipping it on `exists` alone is what let a
        // `warm --new` no-op over a broken slot, leaving the next dispatch to pay a cold create.
        // Fall through to the recreate below.
        if new && is_live_slot(&slot) {
            eprintln!("{BLUE}[{n}] existing — skipping (--new){NC}");
            skipped += 1;
            continue;
        }
        if let Some(owner) = leases.owner_slot(n) {
            let l = &leases.slots[&owner];
            eprintln!("{YELLOW}[{n}] leased {} — skipping{NC}", l.branch);
            skipped += 1;
            continue;
        }
        // A slot can hold a live agent without holding a lease (one launched in place, or one whose
        // slot was handed back under it). Running build.sh in there would fight a working agent.
        if let Some(occupant) = slot_occupants(&slot).into_iter().next() {
            eprintln!(
                "{YELLOW}[{n}] in use by a live {} — skipping{NC}",
                occupant.describe()
            );
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

    // Phase 2 — build, by running `bn build` in each slot. A live checkout with no build script is
    // already warm. With one, prime the first slot to seed the shared sccache cache (repo
    // `.cargo/config.toml`), then build the rest in parallel: they compile the *same* base commit,
    // so they land as cache hits instead of repeating the work — collapsing a warm from N cold
    // builds to ~1 build + N-1 cache/link passes.
    let warmed = match &build {
        None => ready.len() as u32,
        Some(_) => build_slots_warm(&rctx.repo, &ready),
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
fn build_slots_warm(repo: &str, ready: &[(u32, PathBuf)]) -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    let Some(((first_n, first_slot), rest)) = ready.split_first() else {
        return 0;
    };
    let mut warmed = 0u32;
    eprintln!("{BLUE}[{first_n}] building (priming cache)…{NC}");
    if run_bn_build(repo, first_slot, None) {
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
                if run_bn_build(repo, slot, Some(jobs)) {
                    ok.fetch_add(1, Ordering::Relaxed);
                } else {
                    eprintln!("{RED}[{n}] build failed{NC}");
                }
            });
        }
    });
    warmed + ok.load(Ordering::Relaxed)
}

/// Warm one slot by running `bn build` in it (cwd = slot) — the same command a human runs by hand,
/// so the pool builds exactly the way `bn build` does and the two can't drift. `--repo/--branch
/// main` is required, not a nicety: a pool slot is a *detached* checkout, so `bn build`'s cwd
/// resolution finds no branch there. Re-executes *this* binary (`current_exe`) rather than whatever
/// `bn` is on PATH, so a dev build warms with itself. Inherits stdio; the child prepends
/// `~/.local/bin:~/.bin` to PATH for the script, and both env vars below reach it through the child.
/// `CARGO_INCREMENTAL=0`: a warm builds a fresh checkout with no prior incremental state to reuse,
/// and disabling it lets sccache cache the workspace crates too. `jobs` caps `cargo`'s parallelism
/// (`CARGO_BUILD_JOBS`) so several slots building at once share the cores instead of each grabbing
/// all of them.
fn run_bn_build(repo: &str, slot: &Path, jobs: Option<usize>) -> bool {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("bn"));
    let mut cmd = Command::new(exe);
    cmd.args(["build", "--repo", repo, "--branch", "main"])
        .current_dir(slot)
        .env("CARGO_INCREMENTAL", "0");
    if let Some(j) = jobs {
        cmd.env("CARGO_BUILD_JOBS", j.to_string());
    }
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

/// Whether an idle-time pool refresh has real work to do right now.
///
/// This is the cheap predicate the queue tick uses before enqueueing opportunistic warm work. The
/// expensive build still runs through [`job_pool_warm`], which re-checks the same gate before doing
/// anything; this only keeps the queue from filling with "wake up and skip" entries.
pub(crate) fn pool_refresh_due(cfg: &Config, now: i64) -> bool {
    matches!(
        crate::wt_pool_refresh::plan_pool_refresh(cfg, now),
        crate::wt_pool_refresh::PoolRefreshPlan::Warm(_)
    )
}

/// `pool-warm` cron — refresh every repo's warm-slot pool while the device is idle, so a leased
/// dispatch always lands on a ready checkout. Off-peak by schedule and idle-gated at runtime (never
/// builds on top of active work), with a per-repo staleness guard so a long idle stretch re-warms
/// each repo at most every few hours. When queued with `bn run pool-warm --repo <repo>`, the context
/// scopes the refresh to that one repo so the Repos page can schedule a pool build without turning
/// it into an all-pools sweep. Each repo's warm records a `pool_warm` event, so `bn metrics` shows
/// when the pools were last refreshed (the "record when we refresh pools" ask).
pub(crate) fn job_pool_warm(cfg: &Config, ctx: Option<&bn_core::resolver::Resolved>) -> i32 {
    if let Some(ctx) = ctx {
        return warm_pool_for_cron(cfg, &ctx.repo);
    }
    let now = bn_core::util::now_epoch_secs();
    match crate::wt_pool_refresh::plan_pool_refresh(cfg, now) {
        crate::wt_pool_refresh::PoolRefreshPlan::Skip(why) => {
            crate::cmd::jobs::native_job_line(&format!("pool-warm: {why}"), false);
            0
        }
        crate::wt_pool_refresh::PoolRefreshPlan::Warm(targets) => {
            crate::cmd::jobs::native_job_line(
                &format!(
                    "pool-warm: refreshing {} pool(s): {}",
                    targets.len(),
                    targets.join(", ")
                ),
                false,
            );
            let failed = targets
                .iter()
                .filter(|repo| warm_pool_for_cron(cfg, repo) != 0)
                .count();
            i32::from(failed > 0)
        }
    }
}

/// Warm one repo's pool on behalf of the `pool-warm` cron, resolving it the way the code that
/// *created* the slots does.
///
/// [`resolve_pool`] refuses any repo absent from `~/.config/wt/pools`, but a plain `bn dispatch`
/// pools every repo with a bn-managed bare clone at [`bn_core::pool::DEFAULT_DISPATCH_POOL_SIZE`]
/// (see [`resolve_dispatch_pool`]) — no registration required. So `schedule_warm` would queue
/// `pool-warm --repo <repo>` for a pool dispatch had just created, and the cron would fail exit 1
/// on a pool that plainly exists on disk. Resolving through the dispatch rules here is what keeps
/// the two ends of that handoff agreeing.
///
/// A repo with no bare clone is a **skip, not a failure**: the only way to reach that is a queued
/// warm outliving the clone it named, which is a stale queue entry rather than a broken cron, and
/// paging for it buries the failures that mean something. Every line goes through
/// [`crate::cmd::jobs::native_job_line`] so `bn cron show pool-warm` names the repo and the reason
/// instead of reporting "(no output captured)".
fn warm_pool_for_cron(cfg: &Config, repo: &str) -> i32 {
    let Some(ctx) = resolve_dispatch_pool(cfg, repo) else {
        crate::cmd::jobs::native_job_line(
            &format!("pool-warm: no bn-managed bare clone for {repo} — nothing to warm"),
            false,
        );
        return 0;
    };
    crate::cmd::jobs::native_job_line(
        &format!("pool-warm: refreshing {repo} pool ({} slots)", ctx.size),
        false,
    );
    // `here = true`: warm inline in this cron window instead of spawning a nested tmux one.
    if warm_pool(cfg, &ctx, true, false) {
        return 0;
    }
    crate::cmd::jobs::native_job_line(&format!("pool-warm: {repo} refresh failed"), true);
    1
}
