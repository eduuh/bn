//! Native cron-job implementations. The name → fn table lives in [`crate::cmd::job_registry`].
use crate::*;
use std::cell::RefCell;

thread_local! {
    static NATIVE_JOB_OUTPUT: RefCell<String> = const { RefCell::new(String::new()) };
}

pub(crate) fn reset_native_job_output() {
    NATIVE_JOB_OUTPUT.with(|output| output.borrow_mut().clear());
}

pub(crate) fn take_native_job_output() -> String {
    NATIVE_JOB_OUTPUT.with(|output| std::mem::take(&mut *output.borrow_mut()))
}

pub(crate) fn native_job_line(line: &str, error: bool) {
    if error {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
    NATIVE_JOB_OUTPUT.with(|output| {
        let mut output = output.borrow_mut();
        output.push_str(line);
        output.push('\n');
    });
}

/// Run one cadence tick: every job in `children`, in order, through the normal `bn run` path.
///
/// These clusters used to be one systemd timer (and one `bn` process) per job, all firing on the
/// same schedule. Folding them behind a single timer costs no observability: each child still goes
/// through [`run_native_job`]/`run_job`, so it records its **own** `cron_run` event — `bn cron` and
/// `bn doctor` show the same per-job rows and per-job failure counts as before. It also preserves
/// each child's host gate, since `run_job` applies `job_allowed_here` per child.
///
/// A failing child never blocks its siblings, and the tick itself returns 0 even when one fails:
/// the child already recorded its own failure, so failing the parent too would double-report the
/// same problem and mask which job actually broke.
fn run_tick(cfg: &Config, ctx: Option<&Resolved>, tick: &str, children: &[&str]) -> i32 {
    let mut failed: Vec<&str> = Vec::new();
    for job in children {
        if crate::cmd::cron::run_job(cfg, ctx, job) != 0 {
            failed.push(job);
        }
    }
    if failed.is_empty() {
        native_job_line(&format!("{tick}: {} job(s) ok", children.len()), false);
    } else {
        native_job_line(
            &format!(
                "{tick}: {}/{} ok — failed: {}",
                children.len() - failed.len(),
                children.len(),
                failed.join(", ")
            ),
            true,
        );
    }
    0
}

/// `tick-5m` — the five-minute housekeeping cluster (see [`run_tick`]).
pub(crate) fn job_tick_5m(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    run_tick(cfg, ctx, "tick-5m", TICK_5M)
}

/// `tick-10m` — the ten-minute cluster (see [`run_tick`]).
pub(crate) fn job_tick_10m(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    run_tick(cfg, ctx, "tick-10m", TICK_10M)
}

/// `tick-15m` — the fifteen-minute cluster (see [`run_tick`]).
pub(crate) fn job_tick_15m(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    run_tick(cfg, ctx, "tick-15m", TICK_15M)
}


/// `watch` — one pass of the wake reactor (the cron form of `bn watch --once`). Toasts anything
/// newly needing you (agents waiting/blocked/stuck, agent completions, urgent attention) since the
/// last run; quiet when nothing changed. Native so it toasts identically on every platform.
pub(crate) fn job_watch(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    // Clear policy-allowed dialogs first, so an agent a rule covers never surfaces as "needs you".
    // Nothing else ran this sweep, so unattended agents sat on their first prompt.
    crate::cmd::agent_approve::sweep(cfg, false, false);
    emit_watch(cfg, true, 30, bn_core::agent::DEFAULT_STUCK_MINS, false);
    // Watchdogs ride the 2-minute cron too, so a timer lands within ~2m instead of ~5m.
    let db = bn_core::coord::CoordDb::open_default();
    crate::cmd::watchdog::tick_opened(cfg, db, bn_core::util::now_epoch_secs());
    0
}

/// `recall-index` — keep the machine-local semantic-recall index warm (#249): (re)embed only the
/// notes/investigation docs whose fingerprint changed since the last run, so it's cheap on a
/// schedule. Fully offline and idempotent.
pub(crate) fn job_recall_index(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    build_recall_index(cfg);
    0
}

/// `managed-repos` — clone + refresh the repos listed in `[managed_repos]` under the bn-managed
/// root (`~/.bn/repos`), using bn's own bare+worktree layout so `wt`/`tat` can pick them up. This
/// is how dotfiles (and any always-checked-out repo) land on a new machine without a manual
/// `git clone`. Idempotent: clone-if-absent, else fetch + fast-forward each worktree. Best-effort —
/// a single repo failing never aborts the rest, so it always exits 0 (matching the shell job it
/// replaced). Native so the git fan-out runs identically on Linux/macOS/Windows.
pub(crate) fn job_managed_repos(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let root = resolver::managed_repos_dir();
    if root.as_os_str().is_empty() {
        eprintln!("managed-repos: empty managed root");
        return 1;
    }
    let bare_root = root.join("bare");

    for url in &cfg.managed_repos {
        if url.is_empty() {
            continue;
        }
        let name = url.rsplit('/').next().unwrap_or(url);
        let name = name.strip_suffix(".git").unwrap_or(name);
        let bare = bare_root.join(format!("{name}.git"));

        if bare.is_dir() {
            let bare_s = bare.to_string_lossy();
            if !crate::git::git_ok(&[
                "--git-dir",
                &bare_s,
                "fetch",
                "--quiet",
                "--prune",
                "origin",
            ]) {
                eprintln!("managed-repos: fetch failed for {name}");
            }
            fast_forward_worktrees(&bare_s);
            println!("managed-repos: refreshed {name}");
        } else {
            println!("managed-repos: cloning {name}…");
            // Pin the clone under the managed root (mirrors the shell job's `WT_ROOT=$root`), but
            // via an explicit-root arg rather than mutating process env.
            if wt::cmd_clone_root(url, &root) {
                println!("managed-repos: cloned {name} -> {}", bare.display());
            } else {
                eprintln!("managed-repos: clone failed for {name} ({url})");
            }
        }
    }
    0
}

/// Fast-forward every worktree of a managed bare repo to its `origin/<branch>` counterpart
/// (best-effort — dirty or diverged worktrees are skipped, and a failed merge is ignored). Mirrors
/// the shell job's per-worktree `git merge --ff-only origin/$branch` fan-out.
fn fast_forward_worktrees(bare_s: &str) {
    let listing = crate::git::git_capture(
        &["--git-dir", bare_s, "worktree", "list", "--porcelain"],
        None,
        true,
    )
    .unwrap_or_default();

    // Parse `worktree <path>` / `branch refs/heads/<name>` pairs; detached/bare entries (no branch
    // line) are skipped.
    let mut path: Option<String> = None;
    for line in listing.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(p.trim().to_string());
        } else if let Some(b) = line.strip_prefix("branch ") {
            let branch = b.trim().strip_prefix("refs/heads/").unwrap_or(b.trim());
            if let Some(p) = path.take() {
                if std::path::Path::new(&p).is_dir() {
                    let _ = crate::git::git_ok(&[
                        "-C",
                        &p,
                        "merge",
                        "--ff-only",
                        &format!("origin/{branch}"),
                    ]);
                }
            }
        } else if line.trim().is_empty() {
            path = None;
        }
    }
}

/// `refresh-all` — the daily keep-current sweep. Clones/fast-forwards the bn-managed repos
/// (`[managed_repos]`, so dotfiles & co. stay current) and reconciles the cron timers with the
/// manifest (so a `crons.toml` change pulled into the repo takes effect without a manual
/// `bn cron register`). Both steps are idempotent and best-effort; always exits 0.
///
/// Note: the shell job it replaced also ran `bn refresh-all` up front, but no such subcommand ever
/// existed — that line errored to a no-op — so the native handler drops it and does the two real
/// steps only (#485).
pub(crate) fn job_refresh_all(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    let _ = job_managed_repos(cfg, ctx);
    emit_cron_register(cfg, ctx, false, false);
    0
}


/// Run a native job with the same bookkeeping as the script path: the per-machine `only_on` gate,
/// the `bn cron running` marker, and a `cron_run` event with duration + exit code. Native jobs are
/// global scope (they live in the binary, not a note dir), so the event is tagged `global`. The
/// native jobs stream straight to stdout/stderr; jobs that opt into [`native_job_line`] also
/// retain the same text in the event tail for `bn cron show` / `bn cron failures`.
pub(crate) fn run_native_job(
    cfg: &Config,
    ctx: Option<&Resolved>,
    job: &str,
    f: impl FnOnce(&Config, Option<&Resolved>) -> i32,
) -> i32 {
    crate::cmd::cron::run_recorded_job(
        cfg,
        ctx,
        job,
        resolver::JobScope::Global,
        || {
            reset_native_job_output();
            let code = f(cfg, ctx);
            let tail = tail_lines(&take_native_job_output(), 15);
            (code, tail)
        },
    )
}

/// `staff-archive` cron — the #275 store-retention sweep: compact fully-elapsed quarters of feed
/// cards into a per-quarter headline index and roll up `staff_run` ledger rows older than a year
/// into monthly aggregates. Idempotent, so a weekly (or caught-up-after-a-miss) run is always safe;
/// chronicles and retro transcripts are exempt. Always exits 0 — retention is best-effort
/// housekeeping, never a page.
/// `jobs/wt-cleanup.sh` — reclaim finished worktrees: merged-PR ships (cleanup), then done+clean
/// scouts (reap). Both sweep all repos; the reap keeps the note (findings) while removing the
/// disposable scout worktree, so investigation worktrees don't pile up.
pub(crate) fn job_wt_cleanup(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    // #534: clear orphaned merged-but-dirty notices whose worktrees were reaped out of band before
    // reclaiming the current crop — decoupled from the single `cleanup_one` removal path.
    wt::reconcile_stuck(cfg);
    let cleanup = wt::run_code(
        cfg,
        wt::WtCmd::Cleanup {
            all: true,
            force: false,
            worktree: None,
        },
    );
    let reap = wt::run_code(
        cfg,
        wt::WtCmd::Reap {
            all: true,
            yes: true,
        },
    );
    cleanup.max(reap)
}

/// `fleet-reap` (#503) — the fleet garbage-collector: terminate orphaned/stale agent process trees
/// (window gone / worktree removed / PR merged/closed) and prune dead tmux windows. This is what
/// makes the reaper automatic on macOS, where the systemd `wt-cleanup` timer never fires (#471):
/// `bn cron register` materializes it as a launchd timer. Acts (kills), checks PR state (the cron
/// can afford the network), and honors every safety gate — never a working / WIP / orchestrator
/// agent.
pub(crate) fn job_fleet_reap(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    reap::run_reap(
        cfg,
        reap::ReapOpts {
            kill: true,
            check_pr: true,
            ..reap::ReapOpts::default()
        },
    )
}

/// `next-sweep` — the detached helper launched by `bn next` so the opportunistic fleet sweep no
/// longer blocks the user's decision path.
pub(crate) fn job_next_sweep(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    reap::sweep(cfg);
    0
}

/// `jobs/attention-sync.sh` — recompute + rewrite the attention cache (`bn next --refresh`).
pub(crate) fn job_attention_sync(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    // Retire items whose PR has merged or closed before refreshing, so a dead PR's "ping
    // reviewers" nag doesn't survive in the cache the panel reads (#966).
    let retired = bn_core::attention::retire_dead_pr_items(&bn_core::daily::dead_prs(cfg), &today());
    for item in &retired {
        println!("attention-sync: retired {} — {}", item.id, item.title);
    }
    // refresh=true, no_momentum=true: the cache write is always punch-free (momentum is a
    // read-time re-rank), so the sync job never bakes the current punch into the snapshot.
    emit_next(cfg, 5, false, false, true, false, true, false, false);
    println!("attention-sync: attention cache refreshed");
    0
}

/// `jobs/daily-rollover.sh` — roll today's daily goals forward (`bn daily rollover`).
pub(crate) fn job_daily_rollover(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    match bn_core::daily::rollover(cfg, &today()) {
        Ok(r) => {
            print!("{}", payload::rollover_text(&r));
            0
        }
        Err(e) => {
            eprintln!("daily-rollover: {e}");
            1
        }
    }
}

/// `jobs/metrics-snapshot.sh` — write today's scorecard snapshot (`bn metrics --snapshot`). Calls
/// the core writer directly (rather than `emit_metrics`) so a failure is recorded, not `exit`ed.
pub(crate) fn job_metrics_snapshot(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use bn_core::clock::Clock;
    let clock = bn_core::clock::RealClock;
    let now_today = clock.today();
    let s = bn_core::metrics::scorecard(
        cfg,
        &bn_core::runner::RealRunner,
        clock.now_epoch_secs(),
        30,
        &now_today,
    );
    match bn_core::metrics::write_snapshot(cfg, &s, &now_today) {
        Ok(p) => {
            println!("metrics-snapshot: snapshot written: {}", p.display());
            0
        }
        Err(e) => {
            eprintln!("metrics-snapshot: {e}");
            1
        }
    }
}

/// The `gather-findings` cron (#897) — snapshot the local morning-planning caches into the
/// findings store ahead of `today-agent`. Runs the same offline adapters as `bn findings gather`
/// (PR review queue, your open PRs, inbound feed, stale/blocked branches, night-shift reviewables,
/// failing crons, sentinel, quarter goals, dojo cards), recording one durable run per non-empty
/// source. Additive and read-only over the caches: no network, no re-fetch, and no
/// feed/attention/daily side effect. A gather error is a non-fatal exit 1.
pub(crate) fn job_gather_findings(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    match bn_core::findings_gather::run(cfg) {
        Ok(report) => {
            native_job_line(
                &format!(
                    "gather-findings: {} source(s) recorded, {} item(s)",
                    report.recorded(),
                    report.total_items()
                ),
                false,
            );
            for s in report.sources.iter().filter(|s| s.recorded) {
                native_job_line(
                    &format!("  [{}] {} — {} item(s)", s.tier, s.source, s.items),
                    false,
                );
            }
            0
        }
        Err(e) => {
            native_job_line(&format!("gather-findings: {e}"), true);
            1
        }
    }
}

/// The `weekly-insights` cron — snapshot the current anchored week's `bn insights` retro into
/// `<notes>/<machine>/insights/<YYYY-Www>.md` so history accumulates and syncs. Sibling of
/// `metrics-snapshot`; writes directly (rather than `emit_insights`) so a failure is recorded,
/// not `exit`ed. Read-only over telemetry — never fetches.
pub(crate) fn job_weekly_insights(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use bn_core::clock::Clock;
    let today = bn_core::clock::RealClock.today();
    let ins = bn_core::insights::insights(cfg, &today, None);
    let Some(week) = bn_core::util::iso_week(&ins.end) else {
        eprintln!("weekly-insights: can't derive the ISO week for {today}");
        return 1;
    };
    let path = cfg.insights_file(&week);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("weekly-insights: {e}");
            return 1;
        }
    }
    match std::fs::write(&path, bn_core::insights::render(&ins)) {
        Ok(()) => {
            println!("weekly-insights: snapshot written: {}", path.display());
            0
        }
        Err(e) => {
            eprintln!("weekly-insights: {e}");
            1
        }
    }
}

/// `jobs/agent-observe.sh` — ingest agent session state (`bn agent ingest`).
pub(crate) fn job_agent_observe(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    emit_agent_ingest(cfg, false, false);
    0
}

/// `jobs/auto-review-poll.sh` — auto-review lifecycle poll (`bn review-pr poll`).
pub(crate) fn job_auto_review_poll(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    if review_pr::run(cfg, review_pr::ReviewPrCmd::Poll, false) {
        0
    } else {
        1
    }
}

/// `jobs/notify.sh` — offer each urgent attention item to the notification gate, then mark it
/// notified.
///
/// **`bn watch` owns attention delivery** (the `*:0/2` cron: faster, and it batches a burst into
/// one interrupt instead of one toast per item). This job therefore no longer rides `tick-5m` —
/// running both on their own cadences, each with its own dedup ledger, is what toasted every
/// urgent item twice. It stays registered so `bn run notify` and its cron-health row keep working,
/// and it keys deliveries `attn:<id>` exactly as watch does, so the shared ledger makes a
/// duplicate impossible even if the two ever run together again.
pub(crate) fn job_notify(_cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let items = bn_core::attention::to_notify(&today());
    if items.is_empty() {
        println!("notify: nothing new to push");
        return 0;
    }
    for it in &items {
        let body = match it.link.as_deref() {
            Some(l) if !l.is_empty() => l,
            _ => "urgent — needs attention",
        };
        // Urgent attention is blocking by definition: it is open, unacknowledged, and yours.
        let sent = crate::toast::toast(
            crate::toast::Class::Blocking,
            &format!("attn:{}", it.id),
            &it.source,
            &format!("bn: {}", it.title),
            body,
        );
        let _ = bn_core::attention::mark_notified(&it.id);
        println!(
            "notify: {} {} ({})",
            if sent { "toasted" } else { "held" },
            it.id,
            it.title
        );
    }
    0
}


/// `jobs/standup-push.sh` — copy today's standup line to the clipboard and toast it (autopilot).
pub(crate) fn job_standup_push(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let jc = resolver::JobConfig::load();
    if !jc.standup_enabled {
        println!("standup-push: standup autopilot disabled — nothing to push");
        return 0;
    }
    let s = bn_core::reports::standup(cfg, &today());
    let mut line = bn_core::reports::standup_line(&s);
    if jc.standup_fetch_delta {
        if let Some(d) = bn_core::reports::pr_delta(&bn_core::runner::RealRunner) {
            line.push_str(&format!(" · {d}"));
        }
    }
    if line.trim().is_empty() {
        println!("standup-push: nothing to push");
        return 0;
    }
    // The clipboard copy is the point of this job and never interrupts, so it runs regardless of
    // policy; the toast is a digest and stays off the toast channel under the default bar.
    bn_core::notify::clipboard(&line);
    crate::toast::toast(
        crate::toast::Class::Digest,
        &format!("standup:{}", today()),
        "standup",
        "bn standup",
        &line,
    );
    println!("{line}");
    0
}

/// `jobs/wrap-digest.sh` — end-of-day toast: the wrap digest plus a "still on you" attention tail.
pub(crate) fn job_wrap_digest(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    if !resolver::JobConfig::load().wrap_enabled {
        println!("wrap-digest: digest disabled — nothing to push");
        return 0;
    }
    let today = today();
    // Mirror emit_wrap: with no note in cwd, date un-noted commits at `today` so that section
    // stays empty rather than flagging every commit.
    let note_since = target_ctx(cfg, None, None)
        .and_then(|ctx| bn_core::store::read(&cfg.note_file(&ctx)).ok())
        .map(|note| bn_core::reports::note_last_activity(&note))
        .unwrap_or_else(|| today.clone());
    let w = bn_core::reports::wrap(cfg, &today, &note_since, &bn_core::runner::RealRunner);
    let mut digest = if w.is_empty() {
        String::new()
    } else {
        bn_core::reports::render_wrap(&w).trim_end().to_string()
    };

    if let Some(tail) = attn_source_tail(&bn_core::attention::open_items(&today)) {
        if digest.is_empty() {
            digest = tail;
        } else {
            digest.push('\n');
            digest.push_str(&tail);
        }
    }

    if digest.trim().is_empty() {
        println!("wrap-digest: clean day (or disabled) — nothing to push");
        return 0;
    }
    crate::toast::toast(
        crate::toast::Class::Digest,
        &format!("wrap:{today}"),
        "wrap",
        "bn wrap — end of day",
        &digest,
    );
    println!("{digest}");
    0
}

/// "Still on you: N emails, N Teams replies, N meetings" from open attention items, or `None` when
/// none of those sources are present. Replaces the jq pipeline in `wrap-digest.sh`.
pub(crate) fn attn_source_tail(items: &[bn_core::attention::AttnItem]) -> Option<String> {
    source_tail(items.iter().map(|i| i.source.as_str()))
}

/// The pure counting/pluralizing core of [`attn_source_tail`], over a stream of source names.
pub(crate) fn source_tail<'a>(sources: impl Iterator<Item = &'a str>) -> Option<String> {
    let (mut e, mut t, mut m) = (0usize, 0usize, 0usize);
    for s in sources {
        match s {
            "email" => e += 1,
            "teams" => t += 1,
            "meetings" => m += 1,
            _ => {}
        }
    }
    let mut parts = Vec::new();
    if e > 0 {
        parts.push(format!("{e} email{}", if e == 1 { "" } else { "s" }));
    }
    if t > 0 {
        parts.push(format!(
            "{t} Teams repl{}",
            if t == 1 { "y" } else { "ies" }
        ));
    }
    if m > 0 {
        parts.push(format!("{m} meeting{}", if m == 1 { "" } else { "s" }));
    }
    (!parts.is_empty()).then(|| format!("Still on you: {}", parts.join(", ")))
}

/// `jobs/auto-review-trigger.sh` — when idle, prep the next queued PR unattended.
pub(crate) fn job_auto_review_trigger(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    review_pr::auto_trigger(cfg)
}

/// The last `lines` lines of the active pane in the tmux window for `repo`/`branch`, matched by
/// the same canonical key the board join uses (glyph-stripped). `None` when the window is gone or
/// tmux is absent — the stuck-capture is best-effort.
pub(crate) fn pane_tail_for(repo: &str, branch: &str, lines: usize) -> Option<String> {
    let want = bn_core::attention::window_key_parts(repo, branch);
    let listing = crate::proc::tmux_raw(&[
        "list-windows",
        "-a",
        "-F",
        "#{window_id}\t#{session_name}\t#{window_name}",
    ])?;
    let wid = listing.lines().find_map(|l| {
        let c: Vec<&str> = l.split('\t').collect();
        let name = c
            .get(2)?
            .trim_start_matches(|ch: char| !ch.is_ascii_alphanumeric());
        (bn_core::attention::window_key_parts(c.get(1)?, name) == want)
            .then(|| c.first().map(|s| s.to_string()))?
    })?;
    // `-t <window_id>` captures the window's active pane.
    let text = crate::proc::tmux_raw(&["capture-pane", "-p", "-J", "-t", &wid])?;
    let all: Vec<&str> = text.lines().collect();
    let trimmed_len = all
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map(|p| p + 1)
        .unwrap_or(0);
    let start = trimmed_len.saturating_sub(lines);
    let tail = all[start..trimmed_len].join("\n");
    (!tail.trim().is_empty()).then_some(tail)
}

/// window). Delegates to [`emit_night_run`], which gates on the kill switch, `[night] enabled`, and
/// the window; its text output is captured in the run's event tail. Always exits 0 — a gate that
/// declines (kill switch, disabled, off-window) is a clean no-op, not a failure.
/// Fold each *stuck* in-flight item's pane tail into its `detail` — a wedged run explains itself
/// on the board instead of silently sitting there. Overwrites on each sweep (latest state wins);
/// best-effort throughout.
pub(crate) fn capture_stuck_details() {
    use bn_core::attention::WorkStatus;
    let (classified, _) = attn_lanes();
    for (s, st) in classified {
        if st != WorkStatus::Stuck {
            continue;
        }
        let (Some(repo), Some(branch)) = (s.item.repo.as_deref(), s.item.branch.as_deref()) else {
            continue;
        };
        if let Some(tail) = pane_tail_for(repo, branch, 15) {
            let detail = format!("stuck — pane tail:\n{tail}");
            let _ = bn_core::attention::update(
                &s.item.id,
                None,
                None,
                None,
                None,
                None,
                Some(&detail),
                None,
                None,
            );
            println!(
                "attention-auto-trigger: captured pane tail for stuck {}",
                s.item.id
            );
        }
    }
}

/// `attention-auto-trigger` — the autonomous pickup loop. When idle, fill every free concurrency
/// slot with the next eligible `auto` attention items (self-authored, prompted, with a repo/branch),
/// each launched with its agent. Concurrency is capped by `BN_ATTN_AUTO_MAX` (default 1 → one at a
/// time): `next_auto_batch` returns at most (cap − already-running) items, so the live fleet never
/// exceeds the cap; a *done* item awaiting review frees its slot, so the queue keeps draining across
/// sweeps. Also folds stuck runs' pane tails into their detail each sweep, so a wedged item explains
/// itself. The trust boundary (auto only for `source=manual`) lives in `next_auto_batch`, so no
/// untrusted input ever launches an unattended agent.
pub(crate) fn job_attention_auto_trigger(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let now = bn_core::util::now_epoch_secs();
    // Stuck-capture runs regardless of the idle gate — it's read-only on tmux and makes the board
    // truthful even while you're active.
    capture_stuck_details();
    let idle_mins = env_i64("BN_ATTN_AUTO_IDLE_MINS", 5);
    if !bn_core::idle::is_idle(cfg, now, idle_mins) {
        return 0; // active — don't interrupt
    }
    // Fill every free slot in one idle sweep, highest-priority first.
    let batch = bn_core::attention::next_auto_batch(cfg, &today(), attn_auto_max());
    if batch.is_empty() {
        return 0; // at the concurrency cap, or nothing eligible
    }
    let mut rc = 0;
    for item in &batch {
        println!("attention-auto-trigger: {} — {}", item.id, item.title);
        match crate::cmd::attn::run_attn_start_attributed(
            cfg,
            item,
            true,
            Some(bn_core::cost::WorkClass::Scheduled),
        ) {
            Some(o) if o.launched => {}
            Some(_) => {
                eprintln!(
                    "attention-auto-trigger: opened the window but the agent launch failed for {}",
                    item.id
                );
                rc = 1;
            }
            None => {
                eprintln!(
                    "attention-auto-trigger: could not open a worktree for {} ({}/{})",
                    item.id,
                    item.repo.as_deref().unwrap_or("?"),
                    item.branch.as_deref().unwrap_or("?")
                );
                rc = 1;
            }
        }
    }
    rc
}

/// `jobs/agent-supervise.sh` — report fleet health and, when `BN_SUPERVISE_NUDGE=1`, nudge stuck
/// agents via tmux send-keys. The jq selection of stuck agents becomes a Rust filter.
pub(crate) fn job_agent_supervise(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let idle = env_i64("BN_SUPERVISE_IDLE_MINS", 5);
    let stuck_mins = env_i64("BN_SUPERVISE_STUCK_MINS", 15);
    if let Err(error) =
        agent_live::publish_actionable_wakes(cfg, idle, stuck_mins, bn_core::util::now_epoch_secs())
    {
        eprintln!("agent-supervise: wake queue: {error}");
        return 1;
    }
    let fleet = bn_core::agent::supervise(cfg, idle, stuck_mins);
    println!(
        "agent-supervise: {} running, {} stuck, {} ended (idle>{stuck_mins}m)",
        fleet.running_count(),
        fleet.stuck,
        fleet.ended
    );
    if fleet.stuck > 0 && std::env::var("BN_SUPERVISE_NUDGE").as_deref() == Ok("1") {
        let msg = std::env::var("BN_SUPERVISE_NUDGE_MSG").unwrap_or_else(|_| {
            "bn supervise: still there? continue with the current task, or commit + open a PR if it is done.".to_string()
        });
        for a in fleet
            .agents
            .iter()
            .filter(|a| a.health == bn_core::agent::AgentHealth::Stuck)
        {
            if tmux_send_keys(&a.target, &msg) {
                println!("agent-supervise: nudged {}", a.target);
            } else {
                eprintln!("agent-supervise: could not nudge {}", a.target);
            }
        }
    }
    0
}

/// Type `msg` then Enter into a tmux window (`bn agent send` mechanics) — true on delivery.
/// Exact-name target ([`tmux_exact_target`]) so a renumbered/prefix-matched window can't receive
/// another agent's nudge.
pub(crate) fn tmux_send_keys(target: &str, msg: &str) -> bool {
    let target = tmux_exact_target(target);
    let send = |key: &str| crate::proc::tmux_ok(&["send-keys", "-t", &target, key]);
    send(msg) && send("Enter")
}

/// `bn reviews --fetch` — rebuild the review-queue cache from GitHub: PRs awaiting your review and
/// your own open PRs, each stamped with state + reviewDecision. Replaces the gh+jq pipeline.
pub(crate) fn fetch_review_queue() -> i32 {
    if !gh_ready() {
        eprintln!("reviews: gh not found or not authenticated");
        return 0; // non-fatal, like the shim
    }
    let mut prs = Vec::new();
    for (query, tag) in [
        ("--review-requested=@me", "review"),
        ("--author=@me", "mine"),
    ] {
        prs.extend(gh_search_bucket(query, tag));
    }
    if prs.is_empty() {
        println!("reviews: nothing to review and no open PRs of mine — leaving cache as-is");
        return 0;
    }
    let cache = bn_core::reviews::ReviewCache {
        generated_at: bn_core::util::now_rfc3339_utc(),
        prs,
    };
    let path = bn_core::reviews::cache_path();
    let json = match serde_json::to_string_pretty(&cache) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("reviews: {e}");
            return 1;
        }
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, json) {
        Ok(()) => {
            println!(
                "reviews: wrote {} PR(s) to {}",
                cache.prs.len(),
                path.display()
            );
            0
        }
        Err(e) => {
            eprintln!("reviews: {e}");
            1
        }
    }
}

/// Trailing window `pr-observe` sweeps: PRs updated within this many days get (re)checked.
/// 30 matches the metrics scorecard window, so a fresh install backfills what the card shows.
pub(crate) const PR_OBSERVE_WINDOW_DAYS: i64 = 30;

/// `pr-observe` — fold the GitHub state of your own PRs into the metrics log as
/// `pr_opened`/`pr_merged`/`pr_closed`/`pr_review` events (the producer `bn metrics` was
/// missing, #213). One `gh search prs --author=@me` for the window, then one `gh pr view`
/// only for PRs whose terminal/review events are still unknown. Idempotent by
/// `(kind, repo, number)` against the whole-person log; events are stamped at their GitHub
/// timestamps, so the first run backfills the window truthfully.
pub(crate) fn job_pr_observe(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use super::pr_observe_gh::{gh_enrich_pr, gh_my_prs};
    use bn_core::pr_observe as po;
    if !gh_ready() {
        eprintln!("pr-observe: gh not found or not authenticated");
        return 0; // non-fatal, like the review-queue fetch
    }
    let seen = po::seen_keys(&bn_core::events::read_events_across(
        &cfg.metrics_logs_all_machines(),
    ));
    let since = bn_core::util::add_days(&today(), -PR_OBSERVE_WINDOW_DAYS).unwrap_or_else(today);
    let rows = gh_my_prs(&since);
    if rows.is_empty() {
        println!("pr-observe: no PRs of mine updated since {since}");
        return 0;
    }
    let mut prs = Vec::new();
    for row in rows {
        // Every repo is observed, including the scorecard-excluded ones. The exclusion is a
        // *reporting* rule, not a recording one: `metrics::visible_events` drops excluded repos at
        // read time for every metrics surface, so the scorecard stays unflattered while surfaces
        // that ask "what did I actually finish yesterday" (the standup fold) can still see the
        // work. Skipping here instead erased it from both, which is why a day spent entirely in
        // `bn` read back as "no completed tasks recorded".
        let mut pr = po::ObservedPr {
            repo: row.repo,
            number: row.number,
            title: row.title,
            created_ts: bn_core::util::parse_ts(&row.created_at),
            ..Default::default()
        };
        if po::needs_enrichment(&pr.repo, pr.number, row.closed, &seen) {
            gh_enrich_pr(&mut pr, &row.url);
        }
        prs.push(pr);
    }
    let pending = po::derive_events(&prs, &seen);
    if pending.is_empty() {
        println!("pr-observe: metrics log already up to date");
        return 0;
    }
    let n = pending.len();
    for ev in pending {
        if let Err(e) =
            bn_core::events::append_metric_at(&cfg.metrics_log_path(), ev.kind, ev.ts, ev.fields)
        {
            eprintln!("pr-observe: {e}");
            return 1;
        }
    }
    println!("pr-observe: appended {n} PR event(s)");
    0
}


/// `gh` present and authenticated.
pub(crate) fn gh_ready() -> bool {
    crate::gh::gh_ok(&["auth", "status"])
}

/// One `gh search prs <query>` bucket → `ReviewPr`s, each enriched with its state + reviewDecision
/// via `gh pr view` (matching the shim's per-PR verdict lookup).
pub(crate) fn gh_search_bucket(query: &str, tag: &str) -> Vec<bn_core::reviews::ReviewPr> {
    #[derive(serde::Deserialize)]
    struct SearchPr {
        repository: Repo,
        number: i64,
        #[serde(default)]
        title: String,
        author: Option<Author>,
        url: String,
        #[serde(default, rename = "createdAt")]
        created_at: String,
        #[serde(default, rename = "updatedAt")]
        updated_at: String,
    }
    #[derive(serde::Deserialize)]
    struct Repo {
        #[serde(rename = "nameWithOwner")]
        name_with_owner: String,
    }
    #[derive(serde::Deserialize, Default)]
    struct Author {
        #[serde(default)]
        login: String,
    }
    bn_core::runner::gh_json::<Vec<SearchPr>>(
        &bn_core::runner::RealRunner,
        &[
            "search",
            "prs",
            query,
            "--state=open",
            "--limit",
            "50",
            "--json",
            "repository,number,title,author,url,createdAt,updatedAt",
        ],
    )
    .unwrap_or_default()
    .into_iter()
    .map(|pr| {
        let (state, review_decision) = gh_verdict(&pr.url);
        bn_core::reviews::ReviewPr {
            repo: pr.repository.name_with_owner,
            number: pr.number,
            title: pr.title,
            author: pr.author.unwrap_or_default().login,
            url: pr.url,
            updated_at: pr.updated_at,
            created_at: pr.created_at,
            tag: tag.to_string(),
            state,
            review_decision,
        }
    })
    .collect()
}

/// A PR's `(state, reviewDecision)` from `gh pr view`; state lowercased and defaulted to `open`.
pub(crate) fn gh_verdict(url: &str) -> (String, Option<String>) {
    #[derive(serde::Deserialize, Default)]
    struct VerdictView {
        state: Option<String>,
        #[serde(rename = "reviewDecision")]
        review_decision: Option<String>,
    }
    let v = bn_core::runner::gh_json::<VerdictView>(
        &bn_core::runner::RealRunner,
        &["pr", "view", url, "--json", "state,reviewDecision"],
    )
    .unwrap_or_default();
    let state = v
        .state
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "open".to_string());
    let rd = v.review_decision.filter(|s| !s.is_empty());
    (state, rd)
}

/// Read an `i64` env var, falling back to `default` when unset or unparseable.
pub(crate) fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Max concurrent unattended `auto` agents the autonomous loop may run at once — the
/// `attention-auto-trigger` concurrency cap, from `BN_ATTN_AUTO_MAX` (default 1 → one at a time;
/// raise it to fan an overnight queue out across several agents). Clamped to at least 1.
pub(crate) fn attn_auto_max() -> usize {
    env_i64("BN_ATTN_AUTO_MAX", 1).max(1) as usize
}

/// The `commit-notes` job and its decision logic (split out under the file-size ratchet).
/// Re-exported so every `crate::cmd::jobs::…` path that existed before the split still resolves.
mod commit_notes;
pub(crate) use commit_notes::*;
