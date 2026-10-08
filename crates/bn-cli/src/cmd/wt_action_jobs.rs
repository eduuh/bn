//! The native jobs behind [`bn_core::wt_actions`] — the verbs the worktree picker offers as rows.
//!
//! Each one is deliberately thin. The work these do already existed as a `bn` subcommand; what was
//! missing was a *runnable job name* for it, because a picker row runs its verb as `bn run <job>`
//! in a window it can watch ([`crate::cmd::wt_action_run`]). So the bodies here reuse
//! [`crate::wt::run_code`] and the existing job functions rather than reimplementing anything, and
//! the file earns its place by keeping the wrappers out of `jobs.rs`, which is at the size ratchet.
//!
//! Two of the roster's seven verbs needed no wrapper at all — `pool-warm` and `wt-cleanup` were
//! already in the registry. That is the shape to aim for: a new verb should usually be a row and a
//! registry line, not a new implementation.
use crate::wt::{PoolCmd, WtCmd};
use crate::*;

/// How a direct shell verb ended up, for the line the picker prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Started {
    /// Running in its own tmux window in the repo's session.
    Now,
    /// Its window already holds a live run; nothing new was started.
    AlreadyRunning,
}

impl Started {
    /// The tail of the picker's `✓ <verb> — …` line.
    pub(crate) fn describe(&self) -> String {
        match self {
            Started::Now => "started".to_string(),
            Started::AlreadyRunning => "already running".to_string(),
        }
    }
}

/// Start a raw shell command for one picker verb right now, in its own tmux window in `repo`'s
/// session — the same window mechanism [`crate::cmd::wt_action_run::start_action`] uses for a job
/// name, but for a verb (like a single pool slot's warm/rebuild) that is a shell command rather
/// than a `bn run <job>` name.
pub(crate) fn start_shell_action(
    cfg: &Config,
    label: &str,
    repo: &str,
    target: &str,
    cmd: &str,
) -> Result<Started, String> {
    let window = crate::wt::sanitize_branch(target);
    match crate::cmd::wt_action_run::open_window(cfg, repo, &window, cmd) {
        crate::cmd::wt_action_run::WindowOpen::Opened => Ok(Started::Now),
        crate::cmd::wt_action_run::WindowOpen::Live => Ok(Started::AlreadyRunning),
        crate::cmd::wt_action_run::WindowOpen::Unavailable => {
            Err(format!("could not start {label} here: tmux unavailable"))
        }
    }
}

/// `build` — run the repo's `build.sh` for one `(repo, branch)`.
///
/// The registry had no entry for the one thing every warm slot already does. `wt pool warm` shells
/// `bn build --repo R --branch main` per slot, so the behaviour existed but had no runnable name,
/// which is why "build this" could not be a picker row.
///
/// `--repo`/`--branch` come from the entry's context rather than the cwd, and that is not a
/// convenience: a pool slot is a *detached* checkout, so `git branch --show-current` is empty there
/// and cwd resolution fails outright (see `wt_pool::warm::run_bn_build`).
pub(crate) fn job_build(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    let Some(ctx) = ctx else {
        eprintln!("build: needs --repo and --branch (a picker build names its own target)");
        return 2;
    };
    crate::cmd::build::run_build_cmd(
        cfg,
        None,
        Some(ctx.repo.clone()),
        Some(ctx.branch.clone()),
    )
}

/// `build-main` — bring the repo's main up to date, build it there, then install it on this
/// machine when the repo has an `install` job.
///
/// The two halves are separate existing pieces: [`crate::cmd::main_sync::sync_repo`] does the
/// fetch and the fast-forward of both the main checkout and the main slot, and `bn build` runs the
/// script. Composing them beats a third implementation of "fetch and ff main", which is precisely
/// what `sync_repo`'s own doc says `bn reset` used to hand-roll and get wrong.
///
/// The install half is the repo's own `install` job, resolved like `build` (branch → repo →
/// global), so only a repo that says how to install itself gets installed; for every other repo
/// the action stays fetch-and-build.
///
/// Scoped to one repo when the run names one, which is how the picker runs it. With no
/// context it falls through to the fleet-wide `main-sync`, so the job stays useful from a cron.
pub(crate) fn job_build_main(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    let Some(ctx) = ctx else {
        return crate::cmd::main_sync::job_main_sync(cfg, None);
    };
    let (bare_dir, worktree_dir) = wt::layout(cfg);
    let bare = bare_dir.join(format!("{}.git", ctx.repo));
    if !crate::cmd::main_sync::sync_repo(&bare, &worktree_dir, &ctx.repo) {
        eprintln!("build-main: could not bring {} up to date", ctx.repo);
        return 1;
    }
    // Build in the main worktree the sync just fast-forwarded, never the main slot: the sync
    // leaves a slot that is in use or dirty where it was, and building there installed a bn 114
    // commits behind main. Refuse rather than build anything but the remote tip.
    let dir = crate::main_slot::main_worktree(&worktree_dir, &ctx.repo);
    if !dir.is_dir() {
        eprintln!("build-main: no main checkout for {} to build in", ctx.repo);
        return 1;
    }
    let dir_s = dir.to_string_lossy().to_string();
    let head = crate::git::git_capture(&["-C", &dir_s, "rev-parse", "HEAD"], None, true);
    let tip = crate::cmd::main_sync::remote_tip_commit(&bare);
    if head.is_none() || head != tip {
        eprintln!(
            "build-main: {} is at {}, not the latest main ({}) — not building",
            dir.display(),
            head.as_deref().unwrap_or("?"),
            tip.as_deref().unwrap_or("?")
        );
        return 1;
    }
    let prev = std::env::current_dir().ok();
    if std::env::set_current_dir(&dir).is_err() {
        eprintln!("build-main: cannot enter {}", dir.display());
        return 1;
    }
    let mut code = crate::cmd::build::run_build_cmd(
        cfg,
        None,
        Some(ctx.repo.clone()),
        Some("main".to_string()),
    );
    if code == 0 && bn_core::resolver::job_script(cfg, Some(ctx), "install").is_some() {
        println!("build-main: build passed — installing {} from main", ctx.repo);
        code = crate::cmd::build::run_build_cmd(
            cfg,
            Some("install".to_string()),
            Some(ctx.repo.clone()),
            Some("main".to_string()),
        );
    }
    if let Some(prev) = prev {
        let _ = std::env::set_current_dir(prev);
    }
    code
}

/// `pool-grow` — cut one more warm slot and build it, without touching the slots already there.
///
/// Not a pool *size* setting, deliberately. [`bn_core::pool::derived_size`] observes the slots on
/// disk precisely because the config that used to declare a size drifted from reality and was
/// deleted; re-introducing a knob here would re-introduce the drift. The pool already grows by one
/// at the lease when a dispatch finds it full (`wt_pool::ensure_spare_slot`) — this is that same
/// growth, asked for before you need it rather than after.
///
/// The two outcomes need different follow-through, and conflating them is a trap worth naming: a
/// pool whose slots have caught up with its size must have the mark *raised*, which means cutting
/// the checkout — and cutting it is precisely what stops `warm --new` from recognising the slot as
/// new. Growing then warming `--new` therefore left the fresh slot cold, which is the one job
/// growing had. So a cut slot is built directly, and only a pool that already had room is handed to
/// `warm --new` (which cuts and builds the gaps itself).
pub(crate) fn job_pool_grow(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    let repo = ctx.map(|c| c.repo.clone());
    match crate::wt_pool::warm::grow_pool(cfg, repo.clone()) {
        crate::wt_pool::warm::Grown::Cut { n, slot, repo } => {
            println!("cut pool slot {n} — building it");
            if crate::wt_pool::warm::build_one_slot(&repo, &slot) {
                0
            } else {
                // The slot exists and is leasable either way — it is simply not provisioned. Say so
                // and fail, so a grow does not report success over a broken build script.
                eprintln!("pool-grow: slot {n} is cut but its build failed");
                1
            }
        }
        crate::wt_pool::warm::Grown::AlreadyRoom => wt::run_code(
            cfg,
            WtCmd::Pool {
                action: PoolCmd::Warm {
                    repo,
                    here: true,
                    new: true,
                    slot: None,
                },
            },
        ),
        // Neither is a failure, and neither should exit non-zero: a failed row asks for a retry of
        // work that cannot succeed until a human deletes a slot or clones the repo.
        crate::wt_pool::warm::Grown::AtCeiling(n) => {
            println!("pool is at the ceiling ({n} slots) — nothing cut");
            0
        }
        crate::wt_pool::warm::Grown::NotPooled => 0,
    }
}

/// `fetch-prs` — fetch the repo's remotes so the picker's PR rows are current.
///
/// The picker never blocks on a forge (`tat/discovery.rs` serves a stale answer and hands the
/// refetch to a detached warm), which is right for a repaint and wrong when you have *asked* for
/// current data. This is the asking: a fetch whose completion the row can show.
pub(crate) fn job_fetch_prs(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    let code = wt::run_code(cfg, WtCmd::Fetch);
    if let Some(ctx) = ctx {
        // Drop this repo's cached PR list so the next paint refetches rather than serving the
        // answer this fetch just invalidated.
        crate::tat::discovery::invalidate_remote_prs(&ctx.repo);
    }
    code
}

/// `pull-prs` — materialize a worktree for every open PR of yours that lacks one.
///
/// `wt remote --all`, which already handles both forges and both worktree layouts. `--no-tmux`
/// because cutting six windows behind your back is not what "pull my PRs" asked for.
pub(crate) fn job_pull_prs(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    wt::run_code(
        cfg,
        WtCmd::Remote {
            target: None,
            all: true,
            no_tmux: true,
        },
    )
}

/// `wt-prune` — reclaim what git and the lease ledger have lost track of.
///
/// Two prunes, because they answer to different bookkeeping: `wt prune` drops stale worktree
/// *registrations*, while `wt pool prune` reclaims over-size and orphaned *slots*. Neither touches
/// an in-size idle warm slot — that is the pool — nor anything leased, dirty or in use.
///
/// `--yes` is safe here only because `prune`'s own refusals are the gate; this never passes
/// `--include-*`, so a slot someone is sitting in survives.
pub(crate) fn job_wt_prune(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    let registrations = wt::run_code(cfg, WtCmd::Prune);
    let slots = wt::run_code(
        cfg,
        WtCmd::Pool {
            action: PoolCmd::Prune {
                repo: ctx.map(|c| c.repo.clone()),
                yes: true,
                all: false,
                verbose: false,
            },
        },
    );
    registrations.max(slots)
}

/// `verify` — run the repo's gate (build → test → lint) against one worktree.
///
/// The row-menu counterpart of `build`: `bn verify` itself is cwd-only and ends in
/// `std::process::exit`, so it cannot be called from a job. The two halves underneath it can be:
/// [`crate::cmd::verify::resolve_verify_target`] takes a `Resolved` and
/// [`crate::cmd::verify::run_verify_gate`] takes an **explicit** working directory, which is the
/// whole reason this is a composition rather than a reimplementation.
///
/// The checkout comes from git's own worktree list, never from joining
/// `<worktree_dir>/<repo>/<branch>`: a dispatched branch lives in `_pool/N`, so that join misses
/// most of a real fleet — the same trap `emit_agent_recover` documents.
///
/// A repo with no gate at all is `Ok(None)`, which is a clean no-op rather than a failure: nothing
/// to run is not the same as something that failed to run.
pub(crate) fn job_verify(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    let Some(ctx) = ctx else {
        eprintln!("verify: needs --repo and --branch (a picker verify names its own target)");
        return 2;
    };
    let Some(dir) = bn_core::lifecycle::live_worktree_paths(&cfg.bare_dir())
        .get(&ctx.repo)
        .and_then(|by_branch| by_branch.get(&ctx.branch))
        .cloned()
    else {
        eprintln!("verify: no checkout for {}/{}", ctx.repo, ctx.branch);
        return 1;
    };
    match crate::cmd::verify::resolve_verify_target(cfg, ctx, None) {
        Ok(None) => {
            println!("verify: {} has no build/test/lint gate — nothing to run", ctx.repo);
            0
        }
        Ok(Some(gate)) => match crate::cmd::verify::run_verify_gate(&gate, &dir, true, None) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("verify: {e}");
                1
            }
        },
        Err(e) => {
            eprintln!("verify: {e}");
            2
        }
    }
}
