//! The native jobs behind [`bn_core::wt_actions`] — the verbs the worktree picker offers as rows.
//!
//! Each one is deliberately thin. The work these do already existed as a `bn` subcommand; what was
//! missing was a *queueable* name for it, because a picker row can only show progress for work the
//! queue can see. So the bodies here reuse [`crate::wt::run_code`] and the existing job functions
//! rather than reimplementing anything, and the file earns its place by keeping the wrappers out of
//! `jobs.rs`, which is already at the size ratchet.
//!
//! Two of the roster's seven verbs needed no wrapper at all — `pool-warm` and `wt-cleanup` were
//! already in the registry. That is the shape to aim for: a new verb should usually be a row and a
//! registry line, not a new implementation.
use crate::wt::{PoolCmd, WtCmd};
use crate::*;
use bn_core::backlog::{self, AddSpec, BacklogKind, ExecSpec};
use bn_core::wt_actions::{When, WtAction};

/// Queue one picker verb for `repo`, optionally against a specific `branch`.
///
/// Not [`crate::cmd::queue_producers::capture_job_with_subject`], and the difference is the point:
/// that producer stamps `source = "cron:<job>"`, which is precisely what
/// [`bn_core::queue::jobs::is_scheduled_firing`] reads to tell the machine's own sweep from a
/// person asking by hand. Borrowing it here would make every keypress in the picker indistinguish-
/// able from the daily cron — and, for `pool-warm`, inherit the away-only admission that would
/// leave "warm now" sitting until you left.
///
/// So this stamps `source = "queue"` (the door a person comes through) and states the class
/// outright. [`When::Now`] rides above routine scheduled work because someone is watching the row;
/// [`When::Away`] drops to the job's own floor and lets the existing away window pick it up.
///
/// Returns the enqueue outcome, so the caller can say "queued" or "already queued" rather than
/// implying it minted a fresh entry every time a key was pressed.
pub(crate) fn enqueue_action(
    cfg: &Config,
    action: &WtAction,
    repo: &str,
    branch: Option<&str>,
    when: When,
) -> Result<bn_core::queue::enqueue::Enqueued, String> {
    let subject = action_subject(action, repo, branch);
    let queue_repo = cfg.queue_home_repo();
    let now = bn_core::util::now_epoch_secs();
    let spec = AddSpec {
        kind: BacklogKind::Cron,
        priority: match when {
            // Above CRON_PRIORITY: a person is looking at the row waiting for it to stop spinning.
            When::Now => 0,
            // Never above routine scheduled work, whatever the job's own standing.
            //
            // The clamp is the whole point: `priority_for` answers "how urgent is this *job*", and
            // for a recovery job like `wt-cleanup` that is RECOVERY_PRIORITY — which would have
            // made "do it once I've gone" outrank "do it now", the one ordering the pair must never
            // produce. Deferring is a statement about *this request*, not a re-rating of the job.
            When::Away => bn_core::queue::jobs::priority_for(action.job)
                .min(bn_core::queue::jobs::CRON_PRIORITY),
        },
        source: "queue".to_string(),
        task: crate::cmd::task_mint::task_for_machine_enqueue(cfg, &queue_repo, &subject, "queue"),
        exec: ExecSpec::Job {
            job: action.job.to_string(),
            repo: Some(repo.to_string()),
            branch: action.needs_branch.then(|| branch.unwrap_or("main").to_string()),
        },
        heavy: bn_core::queue::jobs::is_heavy(action.job),
        class_override: Some(when.class_override()),
        ..Default::default()
    };
    bn_core::queue::enqueue::enqueue(&backlog::backlog_path(), &queue_repo, &subject, &spec, now)
        .map_err(|e| format!("could not queue {}: {e}", action.label))
}

/// The queue entry id this verb would file for `(repo, branch)`, whether or not it exists.
///
/// The id is [`bn_core::orchestrate::derive_id`] of the subject, so it is stable for a given verb
/// and target — which is what lets the picker ask "have I already filed this?" without keeping a
/// record of its own.
pub(crate) fn action_entry_id(action: &WtAction, repo: &str, branch: Option<&str>) -> String {
    bn_core::orchestrate::derive_id(&action_subject(action, repo, branch))
}

/// The one line naming this verb-and-target, and the sole input to its entry id.
///
/// One function rather than two matching `format!`s, because the id is derived from this string:
/// two spellings that drift apart would mean the picker files a second entry for work it is already
/// showing as queued.
fn action_subject(action: &WtAction, repo: &str, branch: Option<&str>) -> String {
    match branch {
        Some(b) => format!("{} — {repo}/{b}", action.label),
        None => format!("{} — {repo}", action.label),
    }
}

/// `build` — run the repo's `build.sh` for one `(repo, branch)`.
///
/// The registry had no entry for the one thing every warm slot already does. `wt pool warm` shells
/// `bn build --repo R --branch main` per slot, so the behaviour existed but had no queueable name,
/// which is why "build this" could not be a picker row.
///
/// `--repo`/`--branch` come from the entry's context rather than the cwd, and that is not a
/// convenience: a pool slot is a *detached* checkout, so `git branch --show-current` is empty there
/// and cwd resolution fails outright (see `wt_pool::warm::run_bn_build`).
pub(crate) fn job_build(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    let Some(ctx) = ctx else {
        eprintln!("build: needs --repo and --branch (a queued build names its own target)");
        return 2;
    };
    crate::cmd::build::run_build_cmd(
        cfg,
        None,
        Some(ctx.repo.clone()),
        Some(ctx.branch.clone()),
    )
}

/// `build-main` — bring the repo's main up to date, then build it there.
///
/// The two halves are separate existing pieces: [`crate::cmd::main_sync::sync_repo`] does the
/// fetch and the fast-forward of both the main checkout and the main slot, and `bn build` runs the
/// script. Composing them beats a third implementation of "fetch and ff main", which is precisely
/// what `sync_repo`'s own doc says `bn reset` used to hand-roll and get wrong.
///
/// Scoped to one repo when the entry carries one, which is how the picker queues it. With no
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
    // Build where `main` actually lives for this repo — the main slot, else the main worktree.
    // `landing` rather than `resolve`: the sync above has already laid the slot out if it was
    // going to, and a build is not a reason to cut a worktree that isn't there.
    let dir = crate::main_slot::landing(&bare_dir, &worktree_dir, &ctx.repo);
    if !dir.is_dir() {
        eprintln!("build-main: no main checkout for {} to build in", ctx.repo);
        return 1;
    }
    let prev = std::env::current_dir().ok();
    if std::env::set_current_dir(&dir).is_err() {
        eprintln!("build-main: cannot enter {}", dir.display());
        return 1;
    }
    let code = crate::cmd::build::run_build_cmd(
        cfg,
        None,
        Some(ctx.repo.clone()),
        Some("main".to_string()),
    );
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
                // and fail, so a queued grow does not report success over a broken build script.
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
                },
            },
        ),
        // Neither is a failure, and neither should exit non-zero: the queue would file a retry for
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
/// current data. This is the asking: a queued fetch whose completion the row can show.
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
/// because a queued job has no session to open windows in, and cutting six windows behind your
/// back is not what "pull my PRs" asked for.
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
        eprintln!("verify: needs --repo and --branch (a queued verify names its own target)");
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
