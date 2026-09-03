//! `bn today` — Today's agent (#577). Two halves, one command:
//!
//! * the **review surface** — `run` / `list` / `accept` / `dismiss` over the suggestions store,
//!   built on the deterministic fold in [`bn_core::today`];
//! * the **hosted agent** — `bn today agent start|stop|status`, which launches a live harness
//!   session into the tmux `planning` session, seeded with today's findings packet, and leaves it
//!   steerable from the Today page.
//!
//! The launcher mirrors `bn control start` step for step (lease → window → mark → record), because
//! that ordering is what makes an agent addressable: the lease and the window's `@bn_session_id`
//! stamp are tiers 1 and 2 of the join in [`bn_core::agent::channel`], and without them the Steer
//! panel is grey no matter how alive the session is.
use crate::cmd::queue_producers;
use crate::*;
use bn_core::suggestions::{self, Suggestion, SuggestionStatus};
use bn_core::today::{self, AgentRecord, AgentStatus};

/// Serialize a suggestion for the `--json` surfaces (matches the web `/api/suggestions` shape).
fn suggestion_json(s: &Suggestion, bl: &bn_core::backlog::Backlog) -> serde_json::Value {
    serde_json::json!({
        "id": s.id,
        "title": s.title,
        "rationale": s.rationale,
        "source": s.source,
        "source_key": s.source_key,
        "url": s.url,
        "created_at": s.created_at,
        "status": s.status.as_str(),
        "task": s.task,
        "queue": suggestions::queue_view(s, bl),
    })
}

fn print_suggestion(s: &Suggestion, bl: &bn_core::backlog::Backlog) {
    println!("  [{}] {}  ({})", s.status.as_str(), s.title, s.id);
    if !s.rationale.is_empty() {
        println!("      {}", s.rationale);
    }
    // What Today fed the queue, and what became of it — the half that makes the loop legible rather
    // than a proposal list that never visibly turns into anything.
    match suggestions::queue_view(s, bl) {
        // The suggestion's own status wins the glyph: a shipped proposal reads as done even while its
        // entry is still being resolved in the queue, because the work it stood for has landed.
        Some(q) if q.live && s.status != SuggestionStatus::Done => println!(
            "      → {}/{} · {} · {}",
            q.repo, q.entry, q.kind, q.status
        ),
        Some(q) => println!("      ✓ {}/{} · {}", q.repo, q.entry, q.status),
        None if s.status == SuggestionStatus::Accepted => {
            println!("      → not queued yet — `bn today sync`")
        }
        None => {}
    }
    if !s.url.is_empty() {
        println!("      ↗ {}", s.url);
    }
}

/// `bn today run` (and bare `bn today`) — the manual trigger. Runs the agent, then reports what it
/// added and lists the currently open (suggested) proposals.
pub(crate) fn today_run(lookback_days: Option<i64>, per_source: Option<usize>, as_json: bool) {
    let lookback = lookback_days.unwrap_or(today::DEFAULT_LOOKBACK_DAYS);
    let per_source = per_source.unwrap_or(today::DEFAULT_PER_SOURCE);
    let report = match today::run(&today(), lookback, per_source) {
        Ok(r) => r,
        Err(e) => {
            fail_json(as_json, "today", "run_failed", &format!("bn today: {e}"));
        }
    };
    let open = suggestions::list(Some(SuggestionStatus::Suggested), 0);
    let bl = bn_core::backlog::load(&bn_core::backlog::backlog_path());
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "today",
                serde_json::json!({
                    "report": report,
                    "suggestions": open.iter().map(|s| suggestion_json(s, &bl)).collect::<Vec<_>>(),
                })
            )
            .to_json()
        );
        return;
    }
    println!(
        "Today's agent: {} source{}, {} finding{} → {} new suggestion{}",
        report.sources,
        if report.sources == 1 { "" } else { "s" },
        report.items,
        if report.items == 1 { "" } else { "s" },
        report.added,
        if report.added == 1 { "" } else { "s" },
    );
    if open.is_empty() {
        println!("No open suggestions — record cron findings (`bn findings record`) to seed them.");
        return;
    }
    println!("\nSuggested tasks ({} open):", open.len());
    for s in &open {
        print_suggestion(s, &bl);
    }
}

/// `bn today list [--status] [--limit]` — read the suggestions store back.
pub(crate) fn today_list(status: Option<&str>, limit: usize, as_json: bool) {
    let filter = match status {
        Some(s) => match SuggestionStatus::parse(s) {
            Some(st) => Some(st),
            None => fail_json(
                as_json,
                "today",
                "bad_status",
                &format!("bn today list: unknown status '{s}' (use suggested|accepted|dismissed|done)"),
            ),
        },
        None => None,
    };
    let items = suggestions::list(filter, limit);
    let bl = bn_core::backlog::load(&bn_core::backlog::backlog_path());
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "today-list",
                serde_json::json!({
                    "count": items.len(),
                    "suggestions": items.iter().map(|s| suggestion_json(s, &bl)).collect::<Vec<_>>(),
                })
            )
            .to_json()
        );
        return;
    }
    if items.is_empty() {
        println!("No suggestions — run `bn today` to synthesize them from the cron findings.");
        return;
    }
    for s in &items {
        print_suggestion(s, &bl);
    }
}

/// The reconcile the tick runs: every accepted suggestion, silently. Returns how many are in flight.
pub(crate) fn today_reconcile(cfg: &Config) -> usize {
    suggestions::list(Some(SuggestionStatus::Accepted), 0)
        .iter()
        .filter(|s| accept_into_queue(cfg, &s.id, true).is_some())
        .count()
}

/// `bn today sync` — reconcile every accepted suggestion against the queue.
///
/// The step that makes the enhance→work hand-off need no callback. An accepted suggestion whose
/// placement pass has since stamped a `repo:` is now admissible, so the sweep queues the real work;
/// one accepted while no dispatcher was running gets queued the first time one is. Both are the same
/// operation as an accept, which is why this simply re-runs it: idempotence is what lets a sweep be
/// the recovery mechanism rather than a second code path that can disagree with the first.
pub(crate) fn today_sync(cfg: &Config, as_json: bool) {
    let accepted = suggestions::list(Some(SuggestionStatus::Accepted), 0);
    let mut queued = Vec::new();
    for s in &accepted {
        if let Some(q) = accept_into_queue(cfg, &s.id, as_json) {
            queued.push(serde_json::json!({
                "id": s.id,
                "task": q.task_id,
                "repo": q.repo,
                "entry": q.entry_id,
                "kind": q.kind,
            }));
        }
    }
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "today-sync",
                serde_json::json!({ "accepted": accepted.len(), "queued": queued })
            )
            .to_json()
        );
    } else if accepted.is_empty() {
        println!("Nothing accepted — `bn today accept <id>` is what feeds the queue.");
    } else {
        println!(
            "Reconciled {} accepted suggestion{}: {} in flight.",
            accepted.len(),
            if accepted.len() == 1 { "" } else { "s" },
            queued.len()
        );
    }
}


/// `bn today accept|dismiss <id>` — move one suggestion in its review lifecycle.
///
/// Accepting is not a bookkeeping change: it is the moment a proposal becomes work, so it is also
/// the moment the work enters the queue. See [`accept_into_queue`].
pub(crate) fn today_set_status(
    cfg: &Config,
    id: &str,
    status: SuggestionStatus,
    as_json: bool,
) {
    match suggestions::set_status(id, status) {
        Ok(true) => {}
        Ok(false) => fail_json(
            as_json,
            "today",
            "not_found",
            &format!("bn today: no suggestion '{id}'"),
        ),
        Err(e) => fail_json(as_json, "today", "write_failed", &format!("bn today: {e}")),
    }
    // The status is written first and unconditionally. The owner's decision is the durable thing;
    // whether the machine could act on it right now is not, and losing "I said yes to this" because
    // the queue was unreachable would be the worse failure.
    let queued = match status {
        SuggestionStatus::Accepted => accept_into_queue(cfg, id, as_json),
        _ => None,
    };
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "today-status",
                serde_json::json!({
                    "id": id,
                    "status": status.as_str(),
                    "task": queued.as_ref().map(|q| q.task_id.clone()),
                    "queued_repo": queued.as_ref().map(|q| q.repo.clone()),
                    "queued_id": queued.as_ref().map(|q| q.entry_id.clone()),
                    "queued_kind": queued.as_ref().map(|q| q.kind),
                })
            )
            .to_json()
        );
    } else if status != SuggestionStatus::Accepted {
        // Accept reports itself — it has a task and a queue destination to name, and every path
        // through it says what happened. Printing a bare status line as well would repeat it.
        println!("suggestion {id}: {}", status.as_str());
    }
}

/// What an accept put into the queue.
pub(crate) struct QueuedSuggestion {
    pub(crate) task_id: String,
    pub(crate) repo: String,
    pub(crate) entry_id: String,
    /// `work` when the task passed the gate, `enhance` when this entry resolves its placement first.
    pub(crate) kind: &'static str,
}

/// Feed an accepted suggestion to the queue: mint its governing task, judge it, and enqueue either
/// the work or the pass that will make the work placeable.
///
/// Returns `None` when nothing was queued — the suggestion is still accepted, and the reconcile
/// sweep ([`today_sync`]) will pick it up once whatever blocked it clears. Three things block it:
/// no live dispatcher, a task the gate handed to a person, and a failed write.
pub(crate) fn accept_into_queue(
    cfg: &Config,
    id: &str,
    quiet: bool,
) -> Option<QueuedSuggestion> {
    let suggestion = suggestions::get(id)?;
    // The repo the finding points at, when it points at a known one. `None` is not a failure: it is
    // the input that makes the gate say `no_repo` and routes this to the enhance agent below.
    let known = bn_core::issues::known_repos(cfg).into_iter().collect();
    let repo = bn_core::suggestions::repo_from_url(&suggestion.url, &known);
    let task_id = crate::cmd::task_mint::task_for_suggestion(cfg, &suggestion, repo.as_deref());
    if task_id.is_empty() {
        if !quiet {
            eprintln!("bn today accept: could not record a task for {id}");
        }
        return None;
    }
    let _ = suggestions::link_task(id, &task_id);

    if !queue_producers::active_with_fallback(
        cfg,
        queue_producers::TODAY,
        "leaving it accepted but unqueued",
    ) {
        if !quiet {
            println!("suggestion {id}: accepted · task {task_id}");
            println!("  not queued — no live dispatcher. `bn today sync` once one is running.");
        }
        return None;
    }

    // Already in flight? Ask the **backlog**, by source — not the cached `queued_id` on the
    // suggestion, which a crash between the enqueue and the link would have left empty. This is the
    // check that makes a second accept a no-op instead of a duplicate, and it also stops the work
    // being queued alongside the placement pass that is still resolving where it goes.
    let source = suggestion.queue_source();
    if let Some(live) = bn_core::backlog::load(&bn_core::backlog::backlog_path())
        .active_for_source(&source)
        .cloned()
    {
        let _ = suggestions::link_queued(
            id,
            &live.repo,
            &live.id,
            bn_core::util::now_epoch_secs(),
        );
        if !quiet {
            println!("suggestion {id}: accepted · task {task_id}");
            println!(
                "  already queued {}/{} ({})",
                live.repo,
                live.id,
                live.status.label()
            );
        }
        return Some(QueuedSuggestion {
            task_id,
            repo: live.repo,
            entry_id: live.id,
            kind: if live.kind == bn_core::backlog::BacklogKind::Enhance {
                "enhance"
            } else {
                "work"
            },
        });
    }

    // Judge through the shared gate body, so an accept stamps `smart:`, counts its tries, and hits
    // the three-strike backstop exactly as a worktree transition does.
    let task = crate::cmd::task_mint::load_task(cfg, &task_id)?;
    let decision = bn_core::pipeline::gate::judge(cfg, &task, &today());
    // Read the placement off the **task**, never off the URL that minted it. An earlier enhance pass
    // records `repo:` there, and that is the whole point of the pass — deciding from the URL would
    // find nothing, re-queue the enhance, and loop forever on a task that is already placed.
    let placed = task.meta("repo").map(str::trim).filter(|r| !r.is_empty());
    if matches!(
        decision,
        bn_core::pipeline::gate::GateDecision::Refused { stuck: true, .. }
    ) {
        // Waiting on a person: the gate has already raised the attention item, so queueing another
        // enhance attempt would only burn a lane against a task three agents have failed at.
        if !quiet {
            println!("suggestion {id}: accepted · task {task_id}");
            if let Some(line) = decision.refusal_line() {
                println!("  {line}");
            }
        }
        return None;
    }
    let plan = match (decision.admits(), placed) {
        (true, Some(repo)) => queue_producers::TodayPlan::Work {
            repo,
            task_id: &task_id,
            kind: bn_core::backlog::BacklogKind::Ship,
        },
        // Not ready, or ready but unplaced. Either way the next thing to run is the pass that
        // resolves where the work happens.
        _ => queue_producers::TodayPlan::Enhance { task_id: &task_id },
    };
    let kind = match plan {
        queue_producers::TodayPlan::Work { .. } => "work",
        queue_producers::TodayPlan::Enhance { .. } => "enhance",
    };
    if !quiet {
        println!("suggestion {id}: accepted · task {task_id}");
    }
    let (repo, entry_id) = queue_producers::capture_today(cfg, &suggestion, &plan, quiet)?;
    let _ = suggestions::link_queued(id, &repo, &entry_id, bn_core::util::now_epoch_secs());
    Some(QueuedSuggestion {
        task_id,
        repo,
        entry_id,
        kind,
    })
}

// ---------------------------------------------------------------------------
// The hosted agent — `bn today agent start|stop|status`
// ---------------------------------------------------------------------------

/// What a successful start did: the record it wrote, and whether it actually launched anything.
#[derive(Debug)]
pub(crate) struct StartOutcome {
    pub(crate) record: AgentRecord,
    /// True when a live agent for today was already up and this call left it alone.
    pub(crate) already_running: bool,
    pub(crate) message: String,
}

/// Whether a tmux window exists at `target` (the un-`=`-prefixed `session:window` form).
fn window_exists(target: &str) -> bool {
    let exact = format!("={target}");
    proc::tmux(&["display-message", "-p", "-t", &exact, "#{window_id}"]).is_some()
}

/// The `session` half of a `session:window` target.
fn session_of(target: &str) -> &str {
    target.split(':').next().unwrap_or(target)
}

/// How many windows a tmux session has right now. `None` when the session is gone (or tmux is).
fn window_count(session: &str) -> Option<usize> {
    proc::tmux_raw(&["list-windows", "-t", &format!("={session}"), "-F", "#{window_id}"])
        .map(|out| out.lines().filter(|l| !l.trim().is_empty()).count())
}

/// Close the window at `target`, killing the process tree under it first (the same two-step
/// `bn control stop` uses — a bare `kill-window` can leave orphaned harness children behind).
///
/// If the agent's window is the **last** one in its repository session, a plain shell window is
/// opened first. tmux destroys a session when its final window closes; stopping Today's agent must
/// not also close the repository session around it.
fn kill_window(target: &str, fallback_cwd: &str) -> bool {
    let exact = format!("={target}");
    let Some(window_id) = proc::tmux(&["display-message", "-p", "-t", &exact, "#{window_id}"])
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
    else {
        return false;
    };
    let session = session_of(target);
    if window_count(session) == Some(1) {
        // `-d` so a client attached elsewhere isn't yanked into the replacement.
        let _ = proc::tmux_ok(&[
            "new-window",
            "-d",
            "-t",
            &format!("={session}:"),
            "-c",
            fallback_cwd,
        ]);
    }
    crate::reap::kill_window_trees(std::slice::from_ref(&window_id));
    proc::tmux_run(&["kill-window", "-t", &window_id])
}

/// Launch Today's agent, or report the live one. `Err` carries the reason this box cannot host an
/// agent at all — which is the cron's signal to fall back to the offline fold rather than fail.
///
/// Ordering matters and mirrors `bn control start`:
/// 1. reserve the lease *before* opening the window, so a second `start` racing this one loses;
/// 2. open the window running `bn agent launch` with the session id pinned;
/// 3. `mark_window` + `commit` + `record_launched_session` — the three writes that make the
///    session findable by the channel join, and therefore steerable.
pub(crate) fn start_hosted_agent(
    cfg: &Config,
    harness: Option<&str>,
    lookback_days: Option<i64>,
) -> Result<StartOutcome, String> {
    let date = today();
    let hint = resolver::resolve_hint(cfg, &std::env::current_dir().unwrap_or_default());
    let catalog =
        bn_core::agents::discover(cfg, hint.as_ref()).map_err(|error| error.to_string())?;
    let definition = catalog
        .require("today-agent")
        .map_err(|error| error.to_string())?;
    if !definition.repo_available_here(cfg) {
        let repo = definition.requires_repo.as_deref().unwrap_or_default();
        return Err(format!(
            "today-agent requires repository '{repo}' to be registered and available on '{}'",
            cfg.machine()
        ));
    }
    let host_window = definition
        .host_window
        .as_deref()
        .ok_or("today-agent.md requires host_window")?;

    // Already up? Trust the window, not the record: a record whose window is gone is stale, and
    // re-launching over a live one would give the day two agents and an ambiguous steer channel.
    if let Some(existing) = today::read_record() {
        if existing.date == date
            && existing.status != AgentStatus::Stopped
            && window_exists(&existing.tmux_target)
        {
            let message = format!("Today's agent is already running in {}", existing.tmux_target);
            return Ok(StartOutcome {
                record: existing,
                already_running: true,
                message,
            });
        }
    }

    let harness = match harness.map(str::trim).filter(|s| !s.is_empty()) {
        Some(requested) => match bn_core::harness::normalize(requested) {
            Some(canonical) if bn_core::harness::is_available(cfg, canonical) => canonical,
            Some(canonical) => {
                return Err(format!("harness '{canonical}' is unavailable on this machine"))
            }
            None => return Err("harness must be copilot or claude".to_string()),
        },
        None => bn_core::harness::preferred_harness(cfg),
    };
    if !bn_core::harness::is_available(cfg, harness) {
        return Err(format!("no harness available (tried '{harness}')"));
    }
    if proc::tmux_raw(&["list-sessions", "-F", "#{session_name}"]).is_none() {
        return Err("no reachable tmux server".to_string());
    }

    // Today's agent is rooted in the configured personal notes checkout. Require that directory to
    // resolve to a real repo/branch, then use that identity for its note, lease, and tmux session.
    let cwd = cfg.personal_root.clone();
    if !cwd.is_dir() {
        return Err(format!("planning root {} does not exist", cwd.display()));
    }
    let ctx = resolver::resolve(cfg, &cwd).map_err(|_| {
        format!(
            "today agent requires a git worktree, but {} is not on a repository branch",
            cwd.display()
        )
    })?;
    let target = format!("{}:{}", ctx.repo, ctx.branch);
    let tmux_target = format!("{}:{host_window}", ctx.repo);
    let lookback = lookback_days.unwrap_or(today::DEFAULT_LOOKBACK_DAYS);
    let packet = today::gather_packet_json(&date, lookback);
    let seed = today::build_seed(definition, &date, &packet)
        .map_err(|error| format!("today agent prompt: {error}"))?;
    let session_id = crate::cmd::dispatch::gen_uuid();

    let reservation = agent_lease::reserve(agent_lease::AgentLeaseRequest {
        repo: &ctx.repo,
        branch: &ctx.branch,
        worktree: &cwd,
        session_id: &session_id,
        harness,
        kind: bn_core::agent::AgentLeaseKind::Today,
        tmux_target: &tmux_target,
        replace: false,
        in_place: false,
    })?;

    let exe = std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("bn"))
        .to_string_lossy()
        .into_owned();
    let cmd = format!(
        "exec {} agent launch {} --fresh --yolo --session {} --seed {}",
        wt::shell_single_quote(&exe),
        wt::shell_single_quote(harness),
        wt::shell_single_quote(&session_id),
        wt::shell_single_quote(&seed),
    );

    let opened = wt::open_worktree_window_cmd_named(
        &ctx.repo,
        &ctx.branch,
        host_window,
        &cwd,
        false,
        Some(&cmd),
        true,
        None,
    );
    if !opened {
        if let Err(e) = reservation.rollback() {
            eprintln!("bn today agent start: lease rollback failed; {e}");
        }
        return Err(format!("could not open {tmux_target}"));
    }

    agent_lease::mark_window(reservation.lease());
    reservation.commit();
    crate::cmd::agent::record_launched_session(
        cfg,
        crate::cmd::agent::LaunchedSession {
            ctx: &ctx,
            harness,
            session_id: &session_id,
            worktree: &cwd.to_string_lossy(),
            work_class: bn_core::cost::WorkClass::Orchestration,
            task_name: Some(&definition.name),
            controller_id: None,
        },
    );

    let message =
        format!("started {harness} in {tmux_target} — steer it with `bn agent send {target} \"…\"`");
    let record = AgentRecord {
        date,
        session_id,
        harness: harness.to_string(),
        repo: ctx.repo,
        branch: ctx.branch,
        target,
        tmux_target,
        cwd: cwd.to_string_lossy().into_owned(),
        status: AgentStatus::Running,
        started_at: bn_core::util::now_epoch_secs(),
        stopped_at: None,
    };
    today::write_record(&record).map_err(|e| format!("could not record the agent: {e}"))?;

    Ok(StartOutcome {
        record,
        already_running: false,
        message,
    })
}

/// `bn today agent start` — launch (or report) Today's hosted agent.
pub(crate) fn today_agent_start(
    cfg: &Config,
    harness: Option<&str>,
    lookback_days: Option<i64>,
    as_json: bool,
) {
    // Reconcile before starting anything. Yesterday's accepted-but-unqueued work is the day's
    // oldest debt, and an agent that opens by re-proposing work it already accepted is worse than
    // useless.
    today_sync(cfg, false);
    match start_hosted_agent(cfg, harness, lookback_days) {
        Ok(outcome) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success(
                        "today-agent-start",
                        serde_json::json!({
                            "already_running": outcome.already_running,
                            "agent": outcome.record,
                        })
                    )
                    .to_json()
                );
            } else {
                println!("Today's agent: {}", outcome.message);
            }
        }
        // A box with no tmux server or no harness cannot host an agent, but it still wants its
        // suggestions — so fall back to the offline synthesis rather than doing nothing. A run
        // that silently accomplishes nothing is worse than one that does the lesser thing loudly.
        Err(reason) => {
            println!("Today's agent: cannot host one here ({reason}) — synthesizing offline");
            match today::run(
                &today(),
                lookback_days.unwrap_or(today::DEFAULT_LOOKBACK_DAYS),
                today::DEFAULT_PER_SOURCE,
            ) {
                Ok(r) => println!(
                    "Today's agent: {} source(s), {} finding(s) → {} new suggestion(s)",
                    r.sources, r.items, r.added
                ),
                Err(e) => fail_json(as_json, "today-agent-start", "start_failed", &format!("{e}")),
            }
        }
    }
}

/// `bn today agent stop` — close the window and release the lease. Idempotent: stopping an agent
/// that is already gone is a success, because the state the caller asked for is the state they get.
pub(crate) fn today_agent_stop(as_json: bool) {
    let now = bn_core::util::now_epoch_secs();
    let record = today::read_record();
    let killed = record
        .as_ref()
        .map(|r| kill_window(&r.tmux_target, &r.cwd))
        .unwrap_or(false);
    if let Some(r) = &record {
        if let Err(e) = agent_lease::release(&r.session_id) {
            eprintln!("bn today agent stop: lease release failed; {e}");
        }
    }
    if let Err(e) = today::mark_stopped(now) {
        fail_json(
            as_json,
            "today-agent-stop",
            "write_failed",
            &format!("bn today agent stop: {e}"),
        );
    }
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "today-agent-stop",
                serde_json::json!({ "stopped": record.is_some(), "window_killed": killed })
            )
            .to_json()
        );
    } else if record.is_none() {
        println!("Today's agent: nothing recorded — nothing to stop.");
    } else if killed {
        println!("Today's agent: stopped.");
    } else {
        println!("Today's agent: marked stopped (its window was already gone).");
    }
}

/// `bn today agent status` — the record joined with the live fleet, so "recorded" and "actually
/// alive" are never conflated.
pub(crate) fn today_agent_status(cfg: &Config, as_json: bool) {
    let view = today::status_view(cfg);
    if as_json {
        println!(
            "{}",
            Envelope::success("today-agent-status", view).to_json()
        );
        return;
    }
    match today::read_record() {
        None => println!("Today's agent: never started. `bn today agent start` launches it."),
        Some(r) => {
            let live = view["running"].as_bool().unwrap_or(false);
            println!(
                "Today's agent ({}): {} · {} · {}",
                r.date,
                if live { "running" } else { "not running" },
                r.harness,
                r.tmux_target
            );
            println!("  steer: bn agent send {} \"…\"", r.target);
        }
    }
}
