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
use bn_core::backlog::{self, AddSpec, BacklogKind, BacklogStatus, ExecSpec};
use bn_core::wt_actions::{When, WtAction};

/// How a direct command ended up, for the line the picker prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Started {
    /// Leased and running now, in the tmux window the heavy path opened.
    Now,
    /// Light work that ran to completion here, with this exit code. Not a picker verb's path —
    /// every one is heavy — but `ctrl-o → now` can reach an entry somebody else filed light.
    Ran { code: i32 },
    /// It was already running — nothing to do but watch the row.
    AlreadyRunning,
    /// Filed for the dispatcher instead — only on a machine that opted out of direct starts with
    /// `[queue] direct = "queued"`. `outcome` is the enqueue's own label.
    Queued { outcome: &'static str },
}

impl Started {
    /// The tail of the picker's `✓ <verb> — …` line.
    pub(crate) fn describe(&self) -> String {
        match self {
            Started::Now => "started".to_string(),
            Started::Ran { code: 0 } => "ran here and finished".to_string(),
            Started::Ran { code } => format!("ran here and exited {code}"),
            Started::AlreadyRunning => "already running".to_string(),
            Started::Queued { outcome } => format!("queued ({outcome})"),
        }
    }
}

/// Start one picker verb for `repo` right now.
///
/// The door a person's keypress comes through when [`bn_core::resolver::DirectMode`] is immediate,
/// which it is unless the machine opted out. The entry is filed exactly as [`enqueue_action`]
/// would file it — same subject, same id, same heavy foreground spec — and then leased and started
/// from *this* process through [`crate::cmd::queue_tick::dispatch_now`], so the row shows the same
/// running state a dispatched entry shows, and nothing waits on a pass.
///
/// **It never falls back to the queue.** It used to: a tick lock that could not be had, or a
/// spawn that failed, left the entry pending with the line reading `queued — could not start it
/// here`, and on a busy machine the dispatcher then held it for as long as it held everything
/// else. A keypress that reports "queued" and never runs is the failure this exists to remove, so
/// what could not start here is taken back off and reported as the error it is — press again, or
/// read why.
///
/// Filing and starting happen under the tick lock, because the dispatcher wakes on the append: a
/// pass that planned the entry between the two would lease it as well, and the build would run
/// twice.
///
/// An entry that is already filed is the *same* request being asked for again — `ctrl-o → now` on
/// a verb parked for the away window — and is started if it is pending. One that is already
/// running is left alone.
pub(crate) fn start_action(
    cfg: &Config,
    action: &WtAction,
    repo: &str,
    branch: Option<&str>,
) -> Result<Started, String> {
    let now = bn_core::util::now_epoch_secs();
    if !cfg
        .queue
        .direct_mode(bn_core::queue::off::is_off(now))
        .is_immediate()
    {
        return enqueue_action(cfg, action, repo, branch, When::Now)
            .map(|o| Started::Queued { outcome: o.label() });
    }
    let lock = crate::cmd::queue_tick::hold_tick_lock()
        .map_err(|why| format!("could not start {} here: {why} — press it again", action.label))?;
    let subject = action_subject(action, repo, branch);
    let spec = spec_for(cfg, action, repo, branch, When::Now);
    let path = backlog::backlog_path();
    let outcome = bn_core::queue::enqueue::enqueue(&path, repo, &subject, &spec, now)
        .map_err(|e| format!("could not file {}: {e}", action.label))?;
    let entry = outcome.entry();
    if entry.status == BacklogStatus::Leased {
        return Ok(Started::AlreadyRunning);
    }
    match crate::cmd::queue_tick::dispatch_now(cfg, &lock, entry, now) {
        Ok(crate::cmd::queue_tick::DirectStart::Launched) => Ok(Started::Now),
        Ok(crate::cmd::queue_tick::DirectStart::Ran { code }) => Ok(Started::Ran { code }),
        // It could not start here, and a person asked for it *here*: take it back off rather than
        // leave a `queued` row that the dispatcher may sit on for an hour.
        Err(why) => {
            let _ = backlog::cancel(&path, repo, &entry.id, bn_core::util::now_epoch_secs());
            Err(format!("could not start {} here: {why}", action.label))
        }
    }
}

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
    // Only file it if something will pick it up.
    //
    // This is the guard every other producer already asks for
    // (`queue_producers::active_with_fallback`), and the one this path skipped. The cost of
    // skipping it was not theoretical: on a machine with `queue = false` the picker printed
    // `✓ … queued` for every verb pressed and the rows sat at `⏳ queued` indefinitely — eight of
    // them, the oldest three and a half days old, against a dispatcher that had never existed.
    //
    // Refusing rather than running inline, and that is deliberate for *these* verbs specifically:
    // they are minutes-long builds and pool warms that the queue deliberately runs in their own
    // tmux window. Doing that synchronously inside an fzf callback would hang the picker. So say
    // what is wrong and what to change, which is a keystroke the person can act on.
    if !crate::cmd::queue_producers::active_with_fallback(
        cfg,
        crate::cmd::queue_producers::CRON,
        "so this verb cannot be queued",
    ) {
        return Err(dispatcher_down_message(cfg, action));
    }
    let subject = action_subject(action, repo, branch);
    let now = bn_core::util::now_epoch_secs();
    let spec = spec_for(cfg, action, repo, branch, when);
    bn_core::queue::enqueue::enqueue(&backlog::backlog_path(), repo, &subject, &spec, now)
        .map_err(|e| format!("could not queue {}: {e}", action.label))
}

/// The queue entry one picker verb files — shared by the queued and the direct door, so the two
/// cannot drift into filing different entries for the same keypress.
fn spec_for(cfg: &Config, action: &WtAction, repo: &str, branch: Option<&str>, when: When) -> AddSpec {
    let subject = action_subject(action, repo, branch);
    AddSpec {
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
            When::Away => cfg
                .queue
                .priority_for(action.job)
                .min(bn_core::queue::jobs::CRON_PRIORITY),
        },
        source: "queue".to_string(),
        task: crate::cmd::task_mint::task_for_machine_enqueue(cfg, repo, &subject, "queue"),
        exec: ExecSpec::Job {
            job: action.job.to_string(),
            repo: Some(repo.to_string()),
            branch: action.needs_branch.then(|| branch.unwrap_or("main").to_string()),
        },
        // Every picker verb is heavy, whatever the job's own rating. Heavy is the dispatcher's
        // "spawn it in a tmux window in the repo's session" path — the window closes on success
        // and stays to be read on failure — and a person who pressed a key in a picker should
        // never have to wonder *where* the thing they asked for is running. The express path
        // would run a light verb inline in the tick, in the dispatcher's own cwd, with its output
        // going only to the event log. A repo's own scripts need this doubly: their cwd is the
        // window's, which is the repo's main checkout, and they are unknown-cost by definition.
        //
        // The price is that picker verbs share the heavy cap (`[queue].heavy`, default 1), so a
        // long build holds a quick `fetch-prs` at `held · capacity` — which the row says.
        heavy: true,
        class_override: Some(when.class_override()),
        ..Default::default()
    }
}

/// Why a verb could not be queued, and the one thing to change.
///
/// Two different faults reach here and they need different answers: a machine that has the queue
/// switched off is a config edit away, while a machine that wants one but has none is a command
/// away. Reporting "queued" for either — which is what this used to do — is the failure this
/// message exists to replace.
fn dispatcher_down_message(cfg: &Config, action: &WtAction) -> String {
    // Checked before the machine-profile gate because it is the likelier of the two and the only
    // one a person may have done to themselves a minute ago. "Deferring needs a queue" is also the
    // whole of the answer: pressing Enter still starts the verb, which is what switching the queue
    // off asked for.
    if bn_core::queue::off::is_off(bn_core::util::now_epoch_secs()) {
        return format!(
            "{} cannot be deferred while the queue is switched off — press it to run it now, or \
             switch the queue back on (`bn queue on`)",
            action.label
        );
    }
    if !cfg.queue_enabled_here() {
        return format!(
            "{} needs the queue, which is off on this machine ('{}') — remove `queue = false` from [machine] in {}",
            action.label,
            cfg.machine(),
            bn_core::resolver::machine_config_path().display()
        );
    }
    format!(
        "{} needs the queue, but no dispatcher is running — start one with `bn queue run`",
        action.label
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::resolver::MachineProfile;

    fn action() -> &'static WtAction {
        bn_core::wt_actions::find("pool-warm").expect("pool-warm is in the roster")
    }

    fn machine(queue: bool) -> Config {
        Config {
            hostname: "testbox".to_string(),
            machine_profile: Some(MachineProfile {
                repos: vec!["*".to_string()],
                queue,
                cron: false,
                web: false,
            }),
            ..Config::default()
        }
    }

    /// The bug this whole change exists for. A picker verb on a machine with the queue switched off
    /// used to append a backlog entry and report `✓ … queued`; nothing would ever run it, and the
    /// row sat at `⏳ queued` indefinitely. It must refuse instead, and name the one thing to change.
    #[test]
    fn a_verb_is_refused_when_the_queue_is_off_here() {
        let err = enqueue_action(&machine(false), action(), "bn", None, When::Now)
            .expect_err("a machine with no queue must not accept queued work");
        assert!(
            err.contains("off on this machine") && err.contains("queue = false"),
            "the refusal should say what to change, got: {err}"
        );
    }

    /// The opt-out: a machine that says `direct = "queued"` files the verb through the ordinary
    /// door, guards and all, so a keypress there still refuses when nothing would run it.
    ///
    /// Under an isolated state dir because `start_action` now asks whether the queue is switched
    /// off, and that answer lives in the machine-wide event log — without this the test would read
    /// whatever position this developer's own switch happens to be in.
    #[test]
    fn the_queued_opt_out_takes_the_ordinary_door() {
        bn_core::test_support::with_state_dir(|| {
            let mut cfg = machine(false);
            cfg.queue.direct = bn_core::resolver::DirectMode::Queued;
            let err = start_action(&cfg, action(), "bn", None)
                .expect_err("queued mode inherits the queue-off refusal");
            assert!(err.contains("off on this machine"), "{err}");
        });
    }

    /// Every verb in the roster, in both switch positions — the sweep, so a verb added later is
    /// covered without anybody remembering to come back here.
    ///
    /// The two halves are the two doors a picker verb can take, and they answer differently on
    /// purpose. **Enter still works with the queue off**: `start_action` leases and starts the
    /// entry from the picker's own process, which is exactly what switching off asked for.
    /// **Deferring does not**: "run it when I'm away" needs something to hold the entry and hand it
    /// back later, and that is the queue. Refusing there, with a message naming `bn queue on`, is
    /// the honest answer — the alternative is a row that says `⏳ queued` against a queue that will
    /// never look at it, which is the failure `dispatcher_down_message` exists to prevent.
    #[test]
    fn every_verb_refuses_to_defer_while_the_queue_is_off() {
        bn_core::test_support::with_state_dir(|| {
            let _outside = bn_core::test_support::outside_a_dispatch();
            let now = bn_core::util::now_epoch_secs();
            bn_core::queue::off::off("testing", None, now).expect("switch the queue off");
            let cfg = machine(true);
            for action in bn_core::wt_actions::all() {
                let err = enqueue_action(&cfg, action, "bn", Some("main"), When::Away)
                    .expect_err("deferring needs a queue");
                assert!(
                    err.contains("cannot be deferred while the queue is switched off")
                        && err.contains("bn queue on"),
                    "{}: {err}",
                    action.id
                );
                // And the message must not send them to the config file: nothing there is wrong.
                assert!(!err.contains("queue = false"), "{}: {err}", action.id);
            }
        });
    }

    /// The switch does not change what a verb *is*. Same entry id either side of it, so a verb
    /// pressed with the queue off and the same verb queued later are one row, not two.
    #[test]
    fn the_switch_does_not_change_a_verbs_identity() {
        bn_core::test_support::with_state_dir(|| {
            let now = bn_core::util::now_epoch_secs();
            let ids: Vec<String> = bn_core::wt_actions::all()
                .iter()
                .map(|a| action_entry_id(a, "bn", Some("main")))
                .collect();
            bn_core::queue::off::off("testing", None, now).expect("switch the queue off");
            for (action, before) in bn_core::wt_actions::all().iter().zip(&ids) {
                assert_eq!(
                    &action_entry_id(action, "bn", Some("main")),
                    before,
                    "{} changed identity when the queue went off",
                    action.id
                );
            }
        });
    }

    /// The direct door files the same entry the queued one does — same id, same class, same heavy
    /// spawn — which is what lets the row, `bn queue why` and the finish callback treat the two
    /// identically.
    #[test]
    fn the_direct_spec_is_the_queued_spec() {
        // The spec mints a task line, which lands in `<work_root>/tasks.txt` — a temp dir, or
        // the test writes into the crate.
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut cfg = machine(true);
        cfg.personal_root = tmp.path().to_path_buf();
        let spec = spec_for(&cfg, action(), "bn", None, When::Now);
        assert!(spec.heavy);
        assert_eq!(spec.priority, 0);
        assert_eq!(spec.source, "queue");
        assert_eq!(
            spec.class_override,
            Some(bn_core::backlog::ClassOverride::Foreground)
        );
        assert!(
            matches!(&spec.exec, ExecSpec::Job { job, repo: Some(r), branch: None } if job == "pool-warm" && r == "bn")
        );
    }

    /// And when the queue *is* enabled but nothing is draining it, the answer is a different one —
    /// a command rather than a config edit. Two faults reach the same refusal, and pointing someone
    /// at the wrong one is worse than saying nothing.
    ///
    /// Tested through the message rather than through `enqueue_action`, deliberately: the enqueue
    /// path reads the machine-wide state directory, so a test of it would see whatever dispatcher
    /// this developer happens to be running. `daemon::is_running`'s own semantics are covered where
    /// they live, in `bn_core::queue::daemon`. The isolated state dir is still needed for the
    /// *third* fault this message now distinguishes — the queue being switched off, which is also
    /// read from that directory.
    #[test]
    fn a_running_queue_with_no_dispatcher_names_the_command_not_the_config() {
        bn_core::test_support::with_state_dir(|| {
            let msg = dispatcher_down_message(&machine(true), action());
            assert!(
                msg.contains("no dispatcher is running") && msg.contains("bn queue run"),
                "the refusal should name the command that fixes it, got: {msg}"
            );
            assert!(
                !msg.contains("queue = false"),
                "the config is not the problem when the queue is already enabled, got: {msg}"
            );
        });
    }
}
