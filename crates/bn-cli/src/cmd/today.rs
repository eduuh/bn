//! `bn today` — Today's agent (#577). Two halves, one command:
//!
//! * the **review surface** — `run` / `list` / `accept` / `dismiss` over the suggestions store,
//!   built on the deterministic fold in [`bn_core::today_agent`];
//! * the **hosted agent** — `bn today agent start|stop|status`, which launches a live harness
//!   session into the tmux `planning` session, seeded with today's findings packet, and leaves it
//!   steerable from the Today page.
//!
//! The launcher mirrors `bn control start` step for step (lease → window → mark → record), because
//! that ordering is what makes an agent addressable: the lease and the window's `@bn_session_id`
//! stamp are tiers 1 and 2 of the join in [`bn_core::agent::channel`], and without them the Steer
//! panel is grey no matter how alive the session is.
use crate::*;
use bn_core::suggestions::{self, Suggestion, SuggestionStatus};
use bn_core::today_agent::{self, AgentRecord, AgentStatus};

/// Serialize a suggestion for the `--json` surfaces (matches the web `/api/suggestions` shape).
fn suggestion_json(s: &Suggestion) -> serde_json::Value {
    serde_json::json!({
        "id": s.id,
        "title": s.title,
        "rationale": s.rationale,
        "source": s.source,
        "source_key": s.source_key,
        "url": s.url,
        "created_at": s.created_at,
        "status": s.status.as_str(),
    })
}

fn print_suggestion(s: &Suggestion) {
    println!("  [{}] {}  ({})", s.status.as_str(), s.title, s.id);
    if !s.rationale.is_empty() {
        println!("      {}", s.rationale);
    }
    if !s.url.is_empty() {
        println!("      ↗ {}", s.url);
    }
}

/// `bn today run` (and bare `bn today`) — the manual trigger. Runs the agent, then reports what it
/// added and lists the currently open (suggested) proposals.
pub(crate) fn today_run(lookback_days: Option<i64>, per_source: Option<usize>, as_json: bool) {
    let lookback = lookback_days.unwrap_or(today_agent::DEFAULT_LOOKBACK_DAYS);
    let per_source = per_source.unwrap_or(today_agent::DEFAULT_PER_SOURCE);
    let report = match today_agent::run(&today(), lookback, per_source) {
        Ok(r) => r,
        Err(e) => {
            fail_json(as_json, "today", "run_failed", &format!("bn today: {e}"));
        }
    };
    let open = suggestions::list(Some(SuggestionStatus::Suggested), 0);
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "today",
                serde_json::json!({
                    "report": report,
                    "suggestions": open.iter().map(suggestion_json).collect::<Vec<_>>(),
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
        print_suggestion(s);
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
                &format!("bn today list: unknown status '{s}' (use suggested|accepted|dismissed)"),
            ),
        },
        None => None,
    };
    let items = suggestions::list(filter, limit);
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "today-list",
                serde_json::json!({
                    "count": items.len(),
                    "suggestions": items.iter().map(suggestion_json).collect::<Vec<_>>(),
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
        print_suggestion(s);
    }
}

/// `today-agent` cron (#577) — the scheduled trigger. Opt-in via `[today].enabled`; a clean no-op
/// when off (a declined gate isn't a failure).
///
/// Starts the **hosted** agent, which is what Today's agent now is. When this box cannot host one
/// — no tmux server, or no harness installed — it falls back to the offline synthesis rather than
/// doing nothing: a headless machine still wants its suggestions, and a cron that silently
/// accomplishes nothing is worse than one that does the lesser thing loudly.
pub(crate) fn job_today_agent(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    if !resolver::JobConfig::load().today_enabled {
        println!("today-agent: Today's agent disabled — set [today].enabled to opt in");
        return 0;
    }
    match start_hosted_agent(cfg, None, None) {
        Ok(outcome) => {
            println!("today-agent: {}", outcome.message);
            0
        }
        Err(reason) => {
            println!("today-agent: cannot host an agent here ({reason}) — synthesizing offline");
            job_today_offline()
        }
    }
}

/// The offline fold, as a job result: what a box with no tmux and no harness still gets.
fn job_today_offline() -> i32 {
    match today_agent::run(
        &today(),
        today_agent::DEFAULT_LOOKBACK_DAYS,
        today_agent::DEFAULT_PER_SOURCE,
    ) {
        Ok(r) => {
            println!(
                "today-agent: {} source(s), {} finding(s) → {} new suggestion(s)",
                r.sources, r.items, r.added
            );
            0
        }
        Err(e) => {
            eprintln!("today-agent: {e}");
            1
        }
    }
}

/// `bn today accept|dismiss <id>` — move one suggestion in its review lifecycle.
pub(crate) fn today_set_status(id: &str, status: SuggestionStatus, as_json: bool) {
    match suggestions::set_status(id, status) {
        Ok(true) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success(
                        "today-status",
                        serde_json::json!({ "id": id, "status": status.as_str() })
                    )
                    .to_json()
                );
            } else {
                println!("suggestion {id}: {}", status.as_str());
            }
        }
        Ok(false) => fail_json(
            as_json,
            "today",
            "not_found",
            &format!("bn today: no suggestion '{id}'"),
        ),
        Err(e) => fail_json(as_json, "today", "write_failed", &format!("bn today: {e}")),
    }
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
/// If the agent's window is the **last** one in its session, a plain shell window is opened first.
/// tmux destroys a session when its final window closes, and `planning` is a *persistent* session:
/// tmux.conf creates it on server start and prefix `C-p` expects it to be there. Stopping today's
/// agent must not also take away the place you plan from.
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
    let target = today_agent::target(&date);
    let tmux_target = today_agent::tmux_target();

    // Already up? Trust the window, not the record: a record whose window is gone is stale, and
    // re-launching over a live one would give the day two agents and an ambiguous steer channel.
    if let Some(existing) = today_agent::read_record() {
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

    // The agent is rooted where the planning session is rooted (tmux.conf uses the personal-notes
    // repo), so dropping into its window lands you in the same place your own planning does.
    let cwd = cfg.personal_root.clone();
    if !cwd.is_dir() {
        return Err(format!("planning root {} does not exist", cwd.display()));
    }

    let ctx = Resolved {
        repo: today_agent::TODAY_REPO.to_string(),
        branch: date.clone(),
    };
    let lookback = lookback_days.unwrap_or(today_agent::DEFAULT_LOOKBACK_DAYS);
    let packet = today_agent::gather_packet_json(&date, lookback);
    let hint = resolver::resolve_hint(cfg, &std::env::current_dir().unwrap_or_default());
    let seed = today_agent::build_seed(cfg, hint.as_ref(), &date, &packet)
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
        today_agent::TODAY_REPO,
        &date,
        today_agent::TODAY_WINDOW,
        &cwd,
        false,
        Some(&cmd),
        true,
        Some(wt::planning_session()),
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
            task_name: Some("today-agent"),
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
    today_agent::write_record(&record).map_err(|e| format!("could not record the agent: {e}"))?;

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
        Err(reason) => fail_json(
            as_json,
            "today-agent-start",
            "start_failed",
            &format!("bn today agent start: {reason}"),
        ),
    }
}

/// `bn today agent stop` — close the window and release the lease. Idempotent: stopping an agent
/// that is already gone is a success, because the state the caller asked for is the state they get.
pub(crate) fn today_agent_stop(as_json: bool) {
    let now = bn_core::util::now_epoch_secs();
    let record = today_agent::read_record();
    let killed = record
        .as_ref()
        .map(|r| kill_window(&r.tmux_target, &r.cwd))
        .unwrap_or(false);
    if let Some(r) = &record {
        if let Err(e) = agent_lease::release(&r.session_id) {
            eprintln!("bn today agent stop: lease release failed; {e}");
        }
    }
    if let Err(e) = today_agent::mark_stopped(now) {
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
    let view = today_agent::status_view(cfg);
    if as_json {
        println!(
            "{}",
            Envelope::success("today-agent-status", view).to_json()
        );
        return;
    }
    match today_agent::read_record() {
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


