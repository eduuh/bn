//! Pool warming — the `bn wt pool warm` command and the `pool-warm` cron behind it.
//!
//! Split out of `wt_pool.rs` under the 1000-line ratchet. The seam is deliberate rather than
//! arbitrary: warming is the one pool operation that *builds* (fetch, checkout/advance per slot,
//! then `bn build` in each), and it is the only one with a cron entry point, so it carries its own
//! resolution rules and its own failure semantics. Slot classification, leasing, prune/destroy and
//! status stay in the parent.
//!
//! "Warm" means two things, and it used to mean only the second: a slot's deps and build cache are
//! already there, **and** the tree they were built from is current. A warm that skipped the first
//! left the pool pinned at whatever the bare clone last happened to fetch, so every dispatch that
//! leased an existing slot started from a stale base — and the build cache the warm so carefully
//! maintained was a cache of stale code.
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
        // `target` is already `=<session>`, so appending `:pool-warm` would anchor only the
        // session and let the window half fnmatch. Build the pair and anchor both halves.
        let _ = crate::proc::tmux_run(&[
            "select-window",
            "-t",
            &crate::proc::exact_target(&format!("{session}:pool-warm")),
        ]);
    }
    true
}

/// `bn wt pool warm` — resolve the named repo (or the cwd's, or an fzf pick) and warm it. Since
/// pooling stopped being a registration, [`resolve_pool`] and the cron's [`resolve_dispatch_pool`]
/// differ only in how they learn the repo's name, and "that repo isn't pooled" has stopped being a
/// possible answer.
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
    // One warm per pool at a time. The cron timer and the queue's dispatch of the same job once
    // ran together, and the loser's `status` + `clean -df` deleted the tracked files the winner's
    // `switch` was still writing (see `slot_lock`). A warm is idempotent, so the one already
    // running is the answer — yielding to it is a success, not a failed refresh.
    let Some(_warm_lock) = slot_lock::try_lock_warm(&ctx.pool_dir) else {
        println!(
            "{YELLOW}Another warm is already running for {} — leaving the pool to it.{NC}",
            ctx.repo
        );
        return true;
    };
    let bare_s = ctx.bare.to_string_lossy().to_string();

    let Some(base) = refresh_base(&ctx.repo, &bare_s) else {
        return false;
    };

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
    println!(
        "{BLUE}Warming {} slots for {} at {base}…{NC}",
        ctx.size, ctx.repo
    );
    // Phase 1 — ensure every non-skipped slot has a live checkout *at the current base*. Serial on
    // purpose: `git worktree add` takes the bare repo's lock, so parallel adds would only contend.
    // Never warm a slot that's leased (a running agent owns it), dirty (uncommitted local work),
    // or — under `--new` — already holding a *live* checkout; those are counted as skipped and
    // left untouched.
    let mut skipped = 0u32;
    // Reported on the `pool_warm` event so `pool metrics` can name the slots warm can never
    // touch — a dirty idle slot otherwise stays stale and unbuilt for as long as nobody notices.
    let mut skipped_dirty: Vec<u32> = Vec::new();
    let mut failed = 0u32;
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
            skipped_dirty.push(n);
            continue;
        }
        if is_live_slot(&slot) {
            advance_idle_slot(n, &slot_s, &bare_s, &base);
        } else {
            if exists {
                eprintln!("{YELLOW}[{n}] broken checkout — recreating…{NC}");
                force_remove_worktree(&bare_s, &slot);
            } else {
                eprintln!("{BLUE}[{n}] creating checkout…{NC}");
            }
            if !add_detached_slot(&bare_s, &slot.to_string_lossy()) {
                eprintln!("{RED}[{n}] failed to create checkout{NC}");
                failed += 1;
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
    // What the pick must avoid until the next warm says otherwise.
    write_dirty_marks(&ctx.pool_dir, &skipped_dirty);
    let (warmed, build_skipped) = match &build {
        None => {
            // No build script means a live checkout is all there is to be warm; stamp them so the
            // pick treats every checkout in such a pool alike.
            for (n, _) in &ready {
                write_warm_stamp(&ctx.pool_dir, *n, &base);
            }
            (ready.len() as u32, 0)
        }
        Some(_) => build_slots_warm(&rctx.repo, &ready),
    };
    // A ready slot whose build.sh failed is still leasable, just not fully provisioned — count it
    // separately (made) from the fully-warmed slots so the summary can distinguish the two. A
    // build that declined to run (busy toolchain) is neither: it joins the skipped count.
    let skipped = skipped + build_skipped;
    let made = ready.len() as u32 - warmed - build_skipped;
    let complete = warm_complete(ctx.size as u32, warmed, skipped, failed, made);
    // Record the refresh so `bn metrics` can surface pool freshness — how many slots warmed, and
    // when the pool was last refreshed (one event per `pool warm`, alongside pool_get/pool_return).
    {
        let mut f = serde_json::Map::new();
        f.insert("repo".to_string(), ctx.repo.clone().into());
        f.insert("warmed".to_string(), warmed.into());
        f.insert("size".to_string(), (ctx.size as u64).into());
        f.insert("skipped".to_string(), skipped.into());
        f.insert("build_skipped".to_string(), build_skipped.into());
        f.insert(
            "skipped_dirty".to_string(),
            skipped_dirty
                .iter()
                .map(|n| (*n as u64).into())
                .collect::<Vec<serde_json::Value>>()
                .into(),
        );
        f.insert("failed".to_string(), (failed + made).into());
        f.insert("complete".to_string(), complete.into());
        let _ = bn_core::events::append("pool_warm", f);
    }
    let mut summary = format!("Warmed {warmed}/{} slots for {}", ctx.size, ctx.repo);
    if made > 0 {
        summary += &format!(" · {made} checked out but build failed");
    }
    if failed > 0 {
        summary += &format!(" · {failed} checkout failures");
    }
    if skipped > 0 {
        summary += &format!(" · {skipped} skipped (leased/dirty/existing)");
    }
    println!("{GREEN}{summary}.{NC}");
    warm_succeeded(failed, made)
}

/// Fetch origin, then resolve the base every slot in this warm is created at or advanced onto.
///
/// The fetch has to come first and it has to be here. Every commit a warm reaches for comes out of
/// the bare clone, so without it the pool warms itself to whatever main looked like the last time
/// something else happened to fetch — `bn dispatch` refreshes origin on its way past, but nothing
/// else that touches a slot does, and the `pool-warm` cron runs on its own.
///
/// Best-effort, the same posture [`crate::cmd::main_sync`] takes: being offline is a fact about the
/// network, not a reason to skip the warm. Say so and carry on against the last-fetched refs — a
/// slot warmed against a week-old base still beats no slot at all. But only when that last-fetched
/// ref actually exists: a missing cached `origin/main` means there is no trustworthy warm base, so
/// the cron must fail instead of silently warming whatever `default_base` would invent.
fn refresh_base(repo: &str, bare_s: &str) -> Option<String> {
    if !git_quiet(&["--git-dir", bare_s, "fetch", "--quiet", "--prune", "origin"]) {
        match wt::git_out(&[
            "--git-dir",
            bare_s,
            "rev-parse",
            "--verify",
            "refs/remotes/origin/main",
        ]) {
            Some(_) => {
                eprintln!(
                    "{YELLOW}Could not fetch origin for {repo} — warming against the last-fetched refs.{NC}"
                );
                return Some(default_base(bare_s));
            }
            None => {
                eprintln!(
                    "{RED}Could not fetch origin for {repo} and no cached origin/main exists{NC}"
                );
                return None;
            }
        }
    }
    Some(default_base(bare_s))
}

/// Move an idle slot's existing checkout onto `base` and report which of the two it was.
///
/// The half of "warm" that used to be missing. Warming left an existing checkout wherever its last
/// tenant left it and only re-ran the build, so a slot created weeks ago stayed pinned at that
/// week's main forever: `bn dispatch` cutting a *new* branch got the fresh base from the bare
/// repo's refs, but every other way into a slot — re-dispatching an existing branch, `wt pool get`,
/// `wt open`, a controller slot — inherited the stale tree, and the deps this job keeps warm were
/// built against it.
///
/// Reuses [`warm_reset`] — the same detach-and-clean `wt pool return` performs — so "an idle slot"
/// keeps meaning one thing across the pool. It re-resolves the base itself, which costs a couple of
/// git spawns beside a full build; `base` here is only what we report. Reached only for a slot that
/// has already passed the leased / occupied / dirty guards, so there is nothing of anyone's to
/// discard: `dirty_count` counts untracked files too, and `clean -df` never touches ignored ones,
/// which is what keeps the deps intact.
fn advance_idle_slot(n: u32, slot_s: &str, bare_s: &str, base: &str) {
    let before = head_sha(slot_s);
    warm_reset(slot_s, bare_s);
    if head_sha(slot_s) == before {
        eprintln!("{BLUE}[{n}] checkout present, already at {base}{NC}");
    } else {
        eprintln!("{GREEN}[{n}] advanced to {base}{NC}");
    }
}

/// The commit a checkout is on, or `None` when git can't say. `None == None` compares equal, so an
/// unreadable slot reports "already at" rather than a phantom advance — the honest answer when we
/// have no evidence it moved.
fn head_sha(slot_s: &str) -> Option<String> {
    wt::git_out(&["-C", slot_s, "rev-parse", "HEAD"])
}

fn warm_complete(
    size: u32,
    warmed: u32,
    skipped: u32,
    checkout_failed: u32,
    build_failed: u32,
) -> bool {
    warmed + skipped == size && checkout_failed == 0 && build_failed == 0
}

/// Whether the attempted refresh itself succeeded. Safe skips are not job failures: warming cannot
/// and must not touch leased, occupied, dirty, or deliberately excluded existing slots.
fn warm_succeeded(checkout_failed: u32, build_failed: u32) -> bool {
    checkout_failed == 0 && build_failed == 0
}

/// Build every ready slot: prime the first at full parallelism (seeding the sccache cache), then
/// build the rest concurrently sharing a job budget so the box isn't oversubscribed. Returns
/// `(warmed, skipped)`: how many built cleanly (each now carrying a warm stamp), and how many
/// declined to build this pass — a `build.sh` that exits [`BUILD_SKIPPED_EXIT`] because a sibling
/// build already holds the shared toolchain. Those used to exit 0 and be counted as warm, which
/// is how a pool reported "Warmed 3/6" with two slots that had never installed a dependency.
fn build_slots_warm(repo: &str, ready: &[(u32, PathBuf)]) -> (u32, u32) {
    use std::sync::atomic::{AtomicU32, Ordering};
    let Some(((first_n, first_slot), rest)) = ready.split_first() else {
        return (0, 0);
    };
    let (mut warmed, mut skipped) = (0u32, 0u32);
    eprintln!("{BLUE}[{first_n}] building (priming cache)…{NC}");
    match run_bn_build(repo, first_slot, None) {
        BuildOutcome::Built => warmed += 1,
        BuildOutcome::Skipped => skipped += 1,
        BuildOutcome::Failed => eprintln!("{RED}[{first_n}] build failed{NC}"),
    }
    if rest.is_empty() {
        return (warmed, skipped);
    }
    // Divide the cores across the concurrent builds (at least one each). sccache hits keep these
    // light, but an unprimed crate still compiles, so bound the fan-out rather than letting each
    // `cargo` grab every core.
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let jobs = (cpus / rest.len()).max(1);
    let ok = AtomicU32::new(0);
    let declined = AtomicU32::new(0);
    std::thread::scope(|s| {
        for (n, slot) in rest {
            let ok = &ok;
            let declined = &declined;
            s.spawn(move || {
                eprintln!("{BLUE}[{n}] building…{NC}");
                match run_bn_build(repo, slot, Some(jobs)) {
                    BuildOutcome::Built => {
                        ok.fetch_add(1, Ordering::Relaxed);
                    }
                    BuildOutcome::Skipped => {
                        eprintln!("{YELLOW}[{n}] build declined this pass (busy toolchain) — not warm{NC}");
                        declined.fetch_add(1, Ordering::Relaxed);
                    }
                    BuildOutcome::Failed => eprintln!("{RED}[{n}] build failed{NC}"),
                }
            });
        }
    });
    (
        warmed + ok.load(Ordering::Relaxed),
        skipped + declined.load(Ordering::Relaxed),
    )
}

/// What [`grow_pool`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Grown {
    /// A brand-new slot was cut; it still needs a build. Carries what the builder needs — the slot
    /// path and the canonical repo name — rather than a number the caller would have to re-resolve
    /// against a pool it does not hold.
    Cut { n: u32, slot: PathBuf, repo: String },
    /// The pool already had an unbuilt slot below its size — nothing to cut, just warm it.
    AlreadyRoom,
    /// At [`bn_core::pool::MAX_POOL_SIZE`]. Each slot is a full checkout plus its build cache, so
    /// the ceiling is disk rather than taste.
    AtCeiling(usize),
    /// The repo has no pool to grow (not a bare clone under the primary root).
    NotPooled,
}

/// Cut one more warm slot, ahead of the demand that would otherwise cut it.
///
/// The pool already grows by one at the lease, in [`acquire_slot`], when a dispatch finds every
/// slot taken — so this adds no new policy, only the chance to pay for that growth before you are
/// waiting on it. Notably it is **not** a size setting: [`bn_core::pool::derived_size`] observes
/// the slots on disk precisely because the config that used to declare a size drifted from them and
/// was deleted, and a knob here would reintroduce exactly that drift.
///
/// Two cases, and the distinction is why this cannot just be `add_detached_slot`: a young pool has
/// a derived size of [`bn_core::pool::DEFAULT_POOL_SIZE`] with only one slot actually on disk, so
/// there is already room and cutting at `high_water + 1` would leave a gap. Only a pool whose slots
/// have caught up with its size needs the high-water mark raised.
pub(crate) fn grow_pool(cfg: &Config, repo: Option<String>) -> Grown {
    let Some(ctx) = resolve_pool(cfg, repo) else {
        return Grown::NotPooled;
    };
    let high_water = bn_core::pool::pool_high_water(&ctx.pool_dir);
    if high_water < ctx.size {
        return Grown::AlreadyRoom;
    }
    if high_water >= bn_core::pool::MAX_POOL_SIZE {
        return Grown::AtCeiling(high_water);
    }
    let n = (high_water + 1) as u32;
    let bare_s = ctx.bare.to_string_lossy().to_string();
    let slot = slot_path(&ctx.pool_dir, n);
    if add_detached_slot(&bare_s, &slot.to_string_lossy()) {
        log_pool_event("grow", &ctx.repo, n, "", None);
        Grown::Cut {
            n,
            slot,
            repo: ctx.repo,
        }
    } else {
        // git refused the checkout — report it as no room rather than inventing a slot number the
        // warm would then fail to build.
        Grown::AtCeiling(high_water)
    }
}

/// Build exactly one slot, for a caller that already knows which.
///
/// `pool-grow` needs this and cannot use `warm --new`: cutting the slot is what raises the pool's
/// high-water mark, and cutting it also makes it a *live* checkout — so by the time `--new` looked,
/// the slot it was meant to build no longer counted as new and was skipped. Growing a pool left the
/// new slot cold, which is the one thing growing it was for.
///
/// Warming the whole pool instead would rebuild every existing slot to provision one, so the honest
/// primitive is "build this slot", which is what [`build_slots_warm`] does per slot anyway.
pub(crate) fn build_one_slot(repo: &str, slot: &Path) -> bool {
    matches!(run_bn_build(repo, slot, None), BuildOutcome::Built)
}

/// Exit status a repo's `build.sh` uses to say "not this time" — a sibling build already holds
/// the shared toolchain, come back later. `EX_TEMPFAIL` from sysexits, so it can never be confused
/// with a real failure (1) or a completed build (0). Warm counts it as skipped, never as warm.
pub(crate) const BUILD_SKIPPED_EXIT: i32 = 75;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildOutcome {
    /// Exit 0 — the slot's deps and build are in place; a warm stamp now records it.
    Built,
    /// [`BUILD_SKIPPED_EXIT`] — the script declined to run; the slot is exactly as warm as before.
    Skipped,
    Failed,
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
fn run_bn_build(repo: &str, slot: &Path, jobs: Option<usize>) -> BuildOutcome {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("bn"));
    let mut cmd = Command::new(exe);
    cmd.args(["build", "--repo", repo, "--branch", "main"])
        .current_dir(slot)
        .env("CARGO_INCREMENTAL", "0");
    if let Some(j) = jobs {
        cmd.env("CARGO_BUILD_JOBS", j.to_string());
    }
    match cmd.status().ok().and_then(|s| s.code()) {
        Some(0) => {
            // The stamp is what the pick reads; write it only for a build that actually finished.
            if let Some((_, n, pdir)) = slot_parts(slot) {
                write_warm_stamp(
                    &pdir,
                    n,
                    &head_sha(&slot.to_string_lossy()).unwrap_or_default(),
                );
            }
            BuildOutcome::Built
        }
        Some(BUILD_SKIPPED_EXIT) => BuildOutcome::Skipped,
        _ => BuildOutcome::Failed,
    }
}

/// Whether a mandatory pool refresh has real work to do right now.
///
/// This is the cheap predicate the queue tick uses before enqueueing mandatory warm work. The
/// expensive build still runs through [`job_pool_warm`], which re-checks the same gate before doing
/// anything; this only keeps the queue from filling with "wake up and skip" entries.
/// `pool-warm` cron — refresh every due repo's warm-slot pool after its daily deadline, so a leased
/// dispatch always lands on a ready checkout. When queued with `bn run pool-warm --repo <repo>`, the context
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
            let _ = bn_core::events::append("pool_refresh_attempt", serde_json::Map::new());
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

/// Warm one repo's pool on behalf of the `pool-warm` cron.
///
/// Resolved through [`resolve_dispatch_pool`], which takes the repo name straight rather than
/// resolving a cwd — the cron has no worktree to stand in. Historically this mattered far more:
/// `resolve_pool` refused any repo absent from `~/.config/wt/pools` while `bn dispatch` pooled
/// every clone regardless, so a queued `pool-warm --repo <repo>` failed exit 1 on a pool that
/// plainly existed on disk. The config is gone and the two resolvers now agree by construction.
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

#[cfg(test)]
mod completion_tests {
    use super::{warm_complete, warm_succeeded};

    #[test]
    fn successful_safe_skips_advance_pool_freshness() {
        assert!(warm_complete(3, 3, 0, 0, 0));
        assert!(warm_complete(3, 2, 1, 0, 0));
        assert!(!warm_complete(3, 2, 0, 1, 0));
        assert!(!warm_complete(3, 2, 0, 0, 1));
    }

    #[test]
    fn safe_skips_do_not_fail_the_warm_job() {
        assert!(warm_succeeded(0, 0));
        assert!(!warm_succeeded(1, 0));
        assert!(!warm_succeeded(0, 1));
    }
}

#[cfg(test)]
mod freshness_tests {
    //! The "warm means current" half, against real git. A unit test cannot assert this: the whole
    //! claim is about what `git fetch` does to refs the next `git switch` resolves, which is
    //! exactly the part a fake would have to invent.
    use super::*;
    use crate::wt_pool::git_integration_tests::{build_repo_into, git};

    /// A bn-shaped bare clone of a live source repo: `wt clone` configures the
    /// `+refs/heads/*:refs/remotes/origin/*` refspec, and without it `origin/main` never exists and
    /// a fetch has nowhere to land. Returns `(tempdir, source_path, bare_path)`.
    fn bn_clone() -> (tempfile::TempDir, String, String) {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let bare = tmp.path().join("bare.git");
        build_repo_into(&src, &bare);
        let bare_s = bare.to_string_lossy().to_string();
        assert!(git(&[
            "--git-dir",
            &bare_s,
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ]));
        assert!(git(&["--git-dir", &bare_s, "fetch", "-q", "origin"]));
        (tmp, src.to_string_lossy().to_string(), bare_s)
    }

    /// Advance the source repo's `main` by one commit — main moving on while the pool sits idle,
    /// which is the whole situation a warm exists to absorb.
    fn commit_upstream(src_s: &str, name: &str) -> String {
        std::fs::write(Path::new(src_s).join(name), "new\n").unwrap();
        assert!(git(&["-C", src_s, "add", "-A"]));
        assert!(git(&["-C", src_s, "commit", "-qm", name]));
        wt::git_out(&["-C", src_s, "rev-parse", "HEAD"]).expect("upstream HEAD")
    }

    #[test]
    fn refreshing_the_base_picks_up_a_commit_that_landed_after_the_clone() {
        let _env = bn_core::test_support::env_lock();
        let (_tmp, src_s, bare_s) = bn_clone();
        let tip = commit_upstream(&src_s, "b.txt");

        // Before the fetch the bare clone still resolves the base to the old tip …
        let stale = wt::git_out(&["--git-dir", &bare_s, "rev-parse", &default_base(&bare_s)]);
        assert_ne!(stale.as_deref(), Some(tip.as_str()));

        // … and after it, to the commit that just landed.
        let base = refresh_base("bn", &bare_s).expect("fresh base");
        assert_eq!(
            wt::git_out(&["--git-dir", &bare_s, "rev-parse", &base]).as_deref(),
            Some(tip.as_str()),
        );
    }

    #[test]
    fn warming_moves_an_idle_slot_onto_the_new_tip() {
        let _env = bn_core::test_support::env_lock();
        let (tmp, src_s, bare_s) = bn_clone();
        let slot = tmp.path().join("slot2");
        assert!(add_detached_slot(&bare_s, &slot.to_string_lossy()));
        let slot_s = slot.to_string_lossy().to_string();
        let created_at = head_sha(&slot_s).expect("slot HEAD");

        let tip = commit_upstream(&src_s, "b.txt");
        let base = refresh_base("bn", &bare_s).expect("fresh base");
        advance_idle_slot(2, &slot_s, &bare_s, &base);

        assert_ne!(head_sha(&slot_s).as_deref(), Some(created_at.as_str()));
        assert_eq!(head_sha(&slot_s).as_deref(), Some(tip.as_str()));
        assert!(slot.join("b.txt").exists(), "the new file is checked out");
    }

    /// The build cache is the point of the pool, so advancing must not take the ignored deps with
    /// it — `warm_reset`'s `clean -df` deliberately omits `-x`.
    #[test]
    fn advancing_keeps_the_ignored_build_output() {
        let _env = bn_core::test_support::env_lock();
        let (tmp, src_s, bare_s) = bn_clone();
        let slot = tmp.path().join("slot2");
        assert!(add_detached_slot(&bare_s, &slot.to_string_lossy()));
        let slot_s = slot.to_string_lossy().to_string();
        // `.gitignore` in the fixture ignores `target/`.
        std::fs::create_dir_all(slot.join("target")).unwrap();
        std::fs::write(slot.join("target").join("cached"), "expensive\n").unwrap();

        commit_upstream(&src_s, "b.txt");
        let base = refresh_base("bn", &bare_s).expect("fresh base");
        advance_idle_slot(2, &slot_s, &bare_s, &base);

        assert!(slot.join("target").join("cached").exists());
    }

    /// A repo whose origin is unreachable still gets warmed — offline is a fact about the network,
    /// not a broken pool.
    #[test]
    fn an_unreachable_origin_still_yields_a_usable_base() {
        let _env = bn_core::test_support::env_lock();
        let (_tmp, _src_s, bare_s) = bn_clone();
        assert!(git(&[
            "--git-dir",
            &bare_s,
            "remote",
            "set-url",
            "origin",
            "/nonexistent/gone.git",
        ]));

        let base = refresh_base("bn", &bare_s).expect("fresh base");
        assert!(wt::git_out(&["--git-dir", &bare_s, "rev-parse", "--verify", &base]).is_some());
    }

    #[test]
    fn a_missing_cached_origin_main_fails_the_refresh() {
        let _env = bn_core::test_support::env_lock();
        let (_tmp, _src_s, bare_s) = bn_clone();
        assert!(git(&[
            "--git-dir",
            &bare_s,
            "update-ref",
            "-d",
            "refs/remotes/origin/main",
        ]));
        assert!(git(&[
            "--git-dir",
            &bare_s,
            "remote",
            "set-url",
            "origin",
            "/nonexistent/gone.git",
        ]));

        assert!(refresh_base("bn", &bare_s).is_none());
    }
}
