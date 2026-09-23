//! `bn control` — lifecycle and durable request protocol for coordination-only agents.

use crate::*;
use bn_core::controller::{
    self, ControllerRecord, ControllerStatus, ControllerWorkerEvent, ControllerWorkerEventType,
};
use serde::Deserialize;
use std::io::{ErrorKind, Read};
use std::time::Duration;

const CONTROLLER_EVENT_POLL_INTERVAL: Duration = Duration::from_secs(1);

fn valid_controller_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn append_controller_worker_event(
    controller_id: &str,
    worker_session_id: &str,
    repo: &str,
    branch: &str,
    event_type: ControllerWorkerEventType,
    summary: &str,
    url: Option<&str>,
) -> std::io::Result<()> {
    let event = ControllerWorkerEvent::new(
        controller_id,
        worker_session_id,
        repo,
        branch,
        event_type,
        summary,
        url.map(str::to_string),
        bn_core::util::now_epoch_secs(),
    );
    controller::append_worker_event(&event)
}

fn recorded_worker_session_id(cfg: &Config, ctx: &Resolved) -> Option<String> {
    std::env::var("BN_WORKER_SESSION_ID")
        .ok()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .or_else(|| {
            bn_core::store::read(&cfg.note_file(ctx))
                .ok()?
                .front
                .collab
                .iter()
                .rev()
                .find_map(|item| {
                    let rest = item.text.trim().strip_prefix("agent-session:")?;
                    rest.split_whitespace().nth(1).map(str::to_string)
                })
        })
}

pub(crate) fn append_inherited_worker_event(
    cfg: &Config,
    ctx: &Resolved,
    event_type: ControllerWorkerEventType,
    summary: &str,
    url: Option<&str>,
) -> std::io::Result<bool> {
    let Some(controller_id) = std::env::var("BN_CONTROLLER_ID")
        .ok()
        .map(|id| id.trim().to_string())
        .filter(|id| valid_controller_id(id))
    else {
        return Ok(false);
    };
    let worker_session_id = recorded_worker_session_id(cfg, ctx).ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            "BN_CONTROLLER_ID is set but the worker session id is unavailable",
        )
    })?;
    let repo = std::env::var("BN_WORKER_REPO")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| ctx.repo.clone());
    let branch = std::env::var("BN_WORKER_BRANCH")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| ctx.branch.clone());
    append_controller_worker_event(
        &controller_id,
        &worker_session_id,
        &repo,
        &branch,
        event_type,
        summary,
        url,
    )?;
    Ok(true)
}

/// Build the controller seed prompt.
///
/// Load and strictly render the Markdown-defined controller operating brief.
fn controller_seed(
    cfg: &Config,
    ctx: Option<&Resolved>,
    agent: &str,
    controller_id: &str,
) -> Result<String, String> {
    let catalog = bn_core::agents::discover(cfg, ctx).map_err(|error| error.to_string())?;
    let def = catalog.require(agent).map_err(|error| error.to_string())?;
    let mut args = serde_json::Map::new();
    args.insert(
        "controller_id".into(),
        serde_json::Value::String(controller_id.to_string()),
    );
    def.render(Some(&args)).map_err(|error| error.to_string())
}

/// The `session:window` a controller's window lives at — in the session of the repo whose
/// controller slot it leases, like every other agent bn opens.
fn control_target(repo: &str, window_name: &str) -> String {
    format!("{repo}:{window_name}")
}

fn enqueue_request(controller_id: &str, request: &[String]) -> Vec<String> {
    if request.is_empty() {
        return Vec::new();
    }
    controller::enqueue(
        &bn_core::backlog::backlog_path(),
        controller_id,
        &[request.join(" ")],
        bn_core::util::now_epoch_secs(),
    )
    .unwrap_or_else(|e| fail(&format!("bn control start: enqueue request: {e}")))
}

fn nudge_controller_with_message(
    controller: &ControllerRecord,
    message: &str,
) -> std::io::Result<()> {
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    std::process::Command::new(exe)
        .args(["agent", "send", &controller.target, message, "--detach"])
        .spawn()
        .map(|_| ())
}

fn nudge_controller(controller: &ControllerRecord) {
    let _ = nudge_controller_with_message(
        controller,
        "A durable controller request is queued. Lease and coordinate the next request now.",
    );
}

fn controller_alive(target: &str) -> bool {
    let target = crate::proc::exact_target(target);
    let Some(status) = proc::tmux(&[
        "display-message",
        "-p",
        "-t",
        &target,
        "#{pane_dead} #{pane_pid}",
    ]) else {
        return false;
    };
    let Some((dead, pid)) = status.trim().split_once(' ') else {
        return false;
    };
    dead == "0"
        && pid
            .parse::<i32>()
            .ok()
            .is_some_and(crate::cmd::killport::process_exists)
}

pub(crate) fn reconcile_controller_liveness() -> std::io::Result<()> {
    let now = bn_core::util::now_epoch_secs();
    controller::update_registry(&controller::registry_path(), |registry| {
        for record in &mut registry.controllers {
            let dead_running =
                record.status == ControllerStatus::Running && !controller_alive(&record.target);
            let stale_starting = record.status == ControllerStatus::Starting
                && now.saturating_sub(record.started_at) >= 30;
            if dead_running || stale_starting {
                record.status = ControllerStatus::Gone;
                record.stopped_at = Some(now);
            }
        }
    })
}

fn controller_window_exists(target: &str) -> bool {
    let target = crate::proc::exact_target(target);
    proc::tmux(&["display-message", "-p", "-t", &target, "#{window_id}"]).is_some()
}

fn kill_controller_window(target: &str) -> bool {
    let target = crate::proc::exact_target(target);
    let Some(window_id) = proc::tmux(&["display-message", "-p", "-t", &target, "#{window_id}"])
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
    else {
        return false;
    };
    crate::reap::kill_window_trees(std::slice::from_ref(&window_id));
    proc::tmux_run(&["kill-window", "-t", &window_id])
}

enum StartReservation {
    Existing(ControllerRecord),
    Launch {
        record: ControllerRecord,
        stale_target: Option<String>,
    },
}

#[allow(clippy::too_many_arguments)]
fn start_controller(
    cfg: &Config,
    id: Option<String>,
    name: Option<String>,
    harness: Option<String>,
    agent: &str,
    request: Vec<String>,
    json: bool,
) {
    let id = id.unwrap_or_else(|| format!("ctrl-{}", &gen_uuid()[..8]));
    if !valid_controller_id(&id) {
        fail("bn control start: --id must contain only letters, digits, '-' or '_'");
    }
    // Explicit --harness is validated + availability-gated; omit it to inherit the machine's
    // configured default (`[harness].default` / `$BN_HARNESS`, else copilot).
    let harness = match harness.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(requested) => match bn_core::harness::normalize(requested) {
            Some(canonical) if bn_core::harness::is_available(cfg, canonical) => {
                canonical.to_string()
            }
            Some(canonical) => fail(&format!(
                "bn control start: harness '{canonical}' is unavailable on this machine"
            )),
            None => fail("bn control start: --harness must be copilot or claude"),
        },
        None => bn_core::harness::preferred_harness(cfg).to_string(),
    };

    let registry_path = controller::registry_path();
    let now = bn_core::util::now_epoch_secs();
    let session_id = gen_uuid();
    let source_name = if request.is_empty() {
        id.clone()
    } else {
        request.join(" ")
    };
    let task_name = bn_core::agent::derive_task_name(name.as_deref(), &source_name);
    let window_name = bn_core::agent::task_window_name(&task_name, &id);
    let invoked_from = std::env::current_dir().unwrap_or_default();
    let resolved = resolver::resolve_hint(cfg, &invoked_from);
    let Some(repo) = resolved.as_ref().map(|ctx| ctx.repo.clone()) else {
        fail(&format!(
            "bn control start: not inside a repo ({}). cd into the repo you want a controller for \
             and retry.",
            invoked_from.display()
        ));
    };
    // Addressable only once the repo is known: the controller's window lives in that repo's
    // session, so there is no target to name before this point.
    let target = control_target(&repo, &window_name);
    // The controller runs in its own reserved pool slot (`_pool/1`), never in the main worktree —
    // main hosts no agents and stays clean. Leasing here means you can start a controller from
    // anywhere in the repo; the slot, not your cwd, is where it lands.
    let Some((cwd, _fresh)) = wt_pool::lease_controller_slot(cfg, &repo) else {
        fail(&format!(
            "bn control start: could not lease the controller slot for {repo}. Check \
             `wt pool status`; a running controller holds it until `bn control stop`."
        ));
    };
    let branch = wt_pool::CONTROLLER_LEASE_LABEL.to_string();
    let candidate = ControllerRecord {
        controller_id: id.clone(),
        session_id,
        task_name,
        harness: harness.clone(),
        repo,
        branch,
        target: target.clone(),
        cwd: cwd.to_string_lossy().into_owned(),
        status: ControllerStatus::Starting,
        started_at: now,
        stopped_at: None,
    };
    let reservation = controller::update_registry(&registry_path, |registry| {
        if let Some(existing) = registry.get(&id).cloned() {
            let starting = existing.status == ControllerStatus::Starting
                && now.saturating_sub(existing.started_at) < 30;
            let running =
                existing.status == ControllerStatus::Running && controller_alive(&existing.target);
            if starting || running {
                return StartReservation::Existing(existing);
            }
            let stale_target = Some(existing.target);
            registry.upsert(candidate.clone());
            return StartReservation::Launch {
                record: candidate.clone(),
                stale_target,
            };
        }
        registry.upsert(candidate.clone());
        StartReservation::Launch {
            record: candidate.clone(),
            stale_target: None,
        }
    })
    .unwrap_or_else(|e| fail(&format!("bn control start: reserve controller: {e}")));

    let (mut record, stale_target) = match reservation {
        StartReservation::Launch {
            record,
            stale_target,
        } => (record, stale_target),
        StartReservation::Existing(existing) => {
            let queued = enqueue_request(&id, &request);
            if !queued.is_empty() {
                nudge_controller(&existing);
            }
            if json {
                println!(
                    "{}",
                    Envelope::success(
                        "control_start",
                        serde_json::json!({
                            "controller_id": id,
                            "already_running": true,
                            "target": existing.target,
                            "queued": queued,
                        }),
                    )
                    .to_json()
                );
            } else {
                println!(
                    "controller {id} already running at {}{}",
                    existing.target,
                    if queued.is_empty() {
                        String::new()
                    } else {
                        format!(" · queued {}", queued.join(", "))
                    }
                );
            }
            return;
        }
    };

    if let Some(stale_target) = stale_target {
        let removed = kill_controller_window(&stale_target);
        if !removed && controller_window_exists(&stale_target) {
            let _ = controller::update_registry(&registry_path, |registry| registry.stop(&id, now));
            fail("bn control start: could not remove stale controller window");
        }
    }
    controller::requeue_leased(&bn_core::backlog::backlog_path(), &id, now)
        .unwrap_or_else(|e| fail(&format!("bn control start: recover leased requests: {e}")));
    let queued = enqueue_request(&id, &request);
    let exe = std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("bn"))
        .to_string_lossy()
        .into_owned();
    let seed = controller_seed(cfg, resolved.as_ref(), agent, &id)
        .unwrap_or_else(|error| fail(&format!("bn control start: {error}")));
    let cmd = format!(
        "{} control watch --id {} & exec env BN_CONTROLLER_ID={} {} agent launch {} --fresh --yolo --session {} --seed {}",
        wt::shell_single_quote(&exe),
        wt::shell_single_quote(&id),
        wt::shell_single_quote(&id),
        wt::shell_single_quote(&exe),
        wt::shell_single_quote(&harness),
        wt::shell_single_quote(&record.session_id),
        wt::shell_single_quote(&seed),
    );
    // Reserve the controller slot for exactly this control agent — the lease layer independently
    // refuses a Control lease anywhere but `_pool/1`, and refuses any other kind *in* it. Refuse
    // mode: never displace another live agent, so two controllers can't share the slot. Rolled back
    // if the window fails to open, released on `bn control stop`.
    let reservation = match agent_lease::reserve(agent_lease::AgentLeaseRequest {
        repo: &record.repo,
        branch: &record.branch,
        worktree: std::path::Path::new(&record.cwd),
        session_id: &record.session_id,
        harness: &record.harness,
        kind: bn_core::agent::AgentLeaseKind::Control,
        tmux_target: &record.target,
        replace: false,
        in_place: false,
        trusted: false,
    }) {
        Ok(reservation) => reservation,
        Err(error) => {
            let _ =
                controller::update_registry(&registry_path, |registry| registry.stop(&id, now));
            fail(&format!("bn control start: {error}"));
        }
    };
    // `record.repo`, not the literal `"control"` this used to pass: that argument is stamped on the
    // window as `@bn_repo`, so every controller window was advertising a repo that does not exist.
    let opened = wt::open_worktree_window_cmd_named(
        &record.repo,
        &id,
        &window_name,
        &cwd,
        false,
        Some(&cmd),
        true,
    );
    if !opened {
        if let Err(error) = reservation.rollback() {
            eprintln!("bn control start: agent lease rollback failed; {error}");
        }
        let session_id = record.session_id;
        let _ = controller::update_registry(&registry_path, |registry| {
            if registry
                .get(&id)
                .is_some_and(|current| current.session_id == session_id)
            {
                registry.stop(&id, now);
            }
        });
        fail("bn control start: could not launch controller tmux window");
    }
    agent_lease::mark_window(reservation.lease());
    reservation.commit();

    bn_core::cost::attribute_session(bn_core::cost::SessionLaunch {
        session_id: &record.session_id,
        work_class: bn_core::cost::WorkClass::Orchestration,
        repo: &record.repo,
        branch: &record.branch,
        task_name: Some(&record.task_name),
        harness: Some(&record.harness),
        worktree: Some(&record.cwd),
        controller_id: Some(&record.controller_id),
    });
    record.status = ControllerStatus::Running;
    let activated = controller::update_registry(&registry_path, |registry| {
        let reserved = registry.get(&id).is_some_and(|current| {
            current.session_id == record.session_id && current.status == ControllerStatus::Starting
        });
        if reserved {
            registry.upsert(record.clone());
        }
        reserved
    })
    .unwrap_or_else(|e| fail(&format!("bn control start: persist controller: {e}")));
    if !activated {
        kill_controller_window(&record.target);
        fail("bn control start: controller start was cancelled");
    }

    if json {
        println!(
            "{}",
            Envelope::success(
                "control_start",
                serde_json::json!({
                    "controller_id": id,
                    "target": target,
                    "queued": queued,
                    "controller": record,
                }),
            )
            .to_json()
        );
    } else {
        println!(
            "started controller {id} at {target}{}",
            if queued.is_empty() {
                String::new()
            } else {
                format!(" · queued {}", queued.join(", "))
            }
        );
    }
}

fn watch_controller(id: String) {
    let mut reactor = crate::event_loop::EventLoop::with_wake_queue(
        "bn control watch",
        vec![crate::event_loop::Watch::file(controller::registry_path())],
        &[],
    );
    loop {
        let registry = match controller::load_registry(&controller::registry_path()) {
            Ok(registry) => registry,
            Err(error) => {
                eprintln!("bn control watch: read controllers: {error}");
                if reactor.wait(CONTROLLER_EVENT_POLL_INTERVAL)
                    == crate::event_loop::LoopEvent::Shutdown
                {
                    return;
                }
                continue;
            }
        };
        let Some(record) = registry.get(&id) else {
            return;
        };
        if record.status == ControllerStatus::Stopped {
            return;
        }
        if record.status == ControllerStatus::Starting {
            if reactor.wait(CONTROLLER_EVENT_POLL_INTERVAL)
                == crate::event_loop::LoopEvent::Shutdown
            {
                return;
            }
            continue;
        }
        if !controller_alive(&record.target) {
            return;
        }
        if let Err(error) = controller::save_heartbeat(
            &controller::heartbeat_path(&id),
            &record.session_id,
            bn_core::util::now_epoch_secs(),
        ) {
            eprintln!("bn control watch: write heartbeat: {error}");
        }
        let queue = bn_core::wake::WakeQueue::default();
        let target = bn_core::wake::WakeTarget::Controller { id: id.clone() };
        let pending = match queue.pending(&target) {
            Ok(pending) => pending,
            Err(error) => {
                eprintln!("bn control watch: read wake queue: {error}");
                Vec::new()
            }
        };
        if pending.is_empty() {
            if reactor.wait(CONTROLLER_EVENT_POLL_INTERVAL)
                == crate::event_loop::LoopEvent::Shutdown
            {
                return;
            }
            continue;
        }
        let pending_ids: std::collections::BTreeSet<String> =
            pending.iter().map(|wake| wake.event_id.clone()).collect();
        let details = pending
            .iter()
            .map(|wake| wake.summary.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let result = nudge_controller_with_message(
            record,
            &format!(
                "Owned worker events arrived on the durable controller back-channel:\n\
                 {details}\nReconcile these outcomes and continue coordination."
            ),
        );
        match result {
            Ok(()) => {
                if let Err(error) = queue.acknowledge(&target, &pending_ids) {
                    eprintln!("bn control watch: acknowledge wake queue: {error}");
                }
            }
            Err(error) => eprintln!("bn control watch: tail or wake events: {error}"),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct TurnEndHookInput {
    #[serde(default)]
    stop_hook_active: bool,
    #[serde(default, alias = "sessionId")]
    session_id: Option<String>,
}

fn print_guard_block(reason: impl Into<String>) {
    println!(
        "{}",
        serde_json::json!({
            "decision": "block",
            "reason": reason.into(),
        })
    );
}

fn turn_end_guard(cfg: &Config, id: Option<String>) {
    let controller_id = id
        .or_else(|| std::env::var("BN_CONTROLLER_ID").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| valid_controller_id(value));
    let Some(controller_id) = controller_id else {
        println!("{{}}");
        return;
    };

    let mut input = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut input) {
        print_guard_block(format!(
            "Controller turn-end guard could not read hook input: {error}. Continue coordination."
        ));
        return;
    }
    let hook = if input.trim().is_empty() {
        TurnEndHookInput::default()
    } else {
        match serde_json::from_str::<TurnEndHookInput>(&input) {
            Ok(hook) => hook,
            Err(error) => {
                print_guard_block(format!(
                    "Controller turn-end guard received invalid hook input: {error}. Continue \
                         coordination."
                ));
                return;
            }
        }
    };

    let registry = match controller::load_registry(&controller::registry_path()) {
        Ok(registry) => registry,
        Err(error) => {
            print_guard_block(format!(
                "Controller turn-end guard could not read controller state: {error}. Continue \
                     coordination."
            ));
            return;
        }
    };
    let Some(record) = registry.get(&controller_id) else {
        println!("{{}}");
        return;
    };
    if hook
        .session_id
        .as_deref()
        .is_some_and(|session_id| session_id != record.session_id)
    {
        println!("{{}}");
        return;
    }
    let open_requests =
        controller::queue_entries(&bn_core::backlog::backlog_path(), &controller_id)
            .iter()
            .filter(|entry| {
                matches!(
                    entry.status,
                    bn_core::backlog::BacklogStatus::Pending
                        | bn_core::backlog::BacklogStatus::Leased
                )
            })
            .count();
    let sessions = match bn_core::agent::session_registry(cfg) {
        Ok(sessions) => sessions,
        Err(error) => {
            print_guard_block(format!(
                "Controller turn-end guard could not inspect owned workers: {error}. Continue \
                     coordination."
            ));
            return;
        }
    };
    let live_workers = sessions
        .iter()
        .filter(|session| {
            session.controller_id.as_deref() == Some(controller_id.as_str())
                && session.session_id != record.session_id
                && matches!(
                    session.status,
                    bn_core::agent::SessionStatus::Live | bn_core::agent::SessionStatus::Idle
                )
        })
        .count();
    let heartbeat = controller::load_heartbeat(&controller::heartbeat_path(&controller_id))
        .ok()
        .flatten();
    let decision = controller::turn_end_decision(&controller::TurnEndGuardInput {
        controller_running: record.status == ControllerStatus::Running,
        session_id: &record.session_id,
        open_requests,
        live_workers,
        heartbeat: heartbeat.as_ref(),
        now: bn_core::util::now_epoch_secs(),
        stop_hook_active: hook.stop_hook_active,
    });
    match decision {
        controller::TurnEndDecision::Allow => println!("{{}}"),
        controller::TurnEndDecision::Block { reason } => print_guard_block(reason),
    }
}

fn status_controllers(id: Option<String>, json: bool) {
    reconcile_controller_liveness()
        .unwrap_or_else(|e| fail(&format!("bn control status: reconcile controllers: {e}")));
    let registry = controller::load_registry(&controller::registry_path())
        .unwrap_or_else(|e| fail(&format!("bn control status: read controllers: {e}")));
    let launches = bn_core::cost::launch_records();
    let mut joined = controller::join_ownership(&registry.controllers, &launches);
    if let Some(id) = id {
        joined.retain(|view| view.controller.controller_id == id);
    }
    let backlog_path = bn_core::backlog::backlog_path();
    let stuck_lanes = controller_stuck_lanes();
    let rows: Vec<serde_json::Value> = joined
        .iter()
        .map(|view| {
            let queue = controller::queue_entries(&backlog_path, &view.controller.controller_id);
            serde_json::json!({
                "controller": view.controller,
                "workers": view.workers.iter().map(|worker| serde_json::json!({
                    "session_id": worker.session_id,
                    "repo": worker.repo,
                    "branch": worker.branch,
                    "launched_at": worker.launched_at,
                    "stuck": stuck_lanes.get(&controller_stuck_key(
                        &view.controller.controller_id,
                        &worker.repo,
                        &worker.branch,
                    )),
                })).collect::<Vec<_>>(),
                "queue": queue,
            })
        })
        .collect();
    if json {
        println!(
            "{}",
            Envelope::success("control_status", serde_json::json!({ "controllers": rows }))
                .to_json()
        );
        return;
    }
    if rows.is_empty() {
        println!("No controllers.");
        return;
    }
    for view in joined {
        let queue = controller::queue_entries(&backlog_path, &view.controller.controller_id);
        let pending = queue
            .iter()
            .filter(|entry| entry.status == bn_core::backlog::BacklogStatus::Pending)
            .count();
        let leased = queue
            .iter()
            .filter(|entry| entry.status == bn_core::backlog::BacklogStatus::Leased)
            .count();
        println!(
            "{} · {:?} · {} · workers {} · queue {} pending/{} leased",
            view.controller.controller_id,
            view.controller.status,
            view.controller.target,
            view.workers.len(),
            pending,
            leased
        );
        for worker in view.workers {
            println!(
                "  worker {}:{} · {}",
                worker.repo, worker.branch, worker.session_id
            );
            if let Some(stuck) = stuck_lanes.get(&controller_stuck_key(
                &view.controller.controller_id,
                &worker.repo,
                &worker.branch,
            )) {
                println!(
                    "    stuck/{}: {} · unblock: {}",
                    stuck.kind.label(),
                    stuck.reason,
                    stuck.suggested_unblock
                );
            }
        }
    }
}

fn controller_stuck_key(controller_id: &str, repo: &str, branch: &str) -> (String, String, String) {
    (
        controller_id.to_string(),
        repo.to_ascii_lowercase(),
        branch.to_ascii_lowercase(),
    )
}

fn controller_stuck_lanes(
) -> std::collections::HashMap<(String, String, String), bn_core::agent::StuckLane> {
    bn_core::orchestrate::run_paths()
        .into_iter()
        .filter_map(|path| bn_core::orchestrate::OrchestrateState::load(&path))
        .flat_map(|state| {
            state.queue.into_iter().filter_map(move |item| {
                Some((
                    controller_stuck_key(
                        &item.controller_id?,
                        if item.repo.is_empty() {
                            &state.repo
                        } else {
                            &item.repo
                        },
                        &item.branch?,
                    ),
                    item.stuck?,
                ))
            })
        })
        .collect()
}

fn stop_controller(cfg: &Config, id: String, json: bool) {
    let path = controller::registry_path();
    let registry = controller::load_registry(&path)
        .unwrap_or_else(|e| fail(&format!("bn control stop: read controllers: {e}")));
    let Some(record) = registry.get(&id).cloned() else {
        fail(&format!("bn control stop: unknown controller '{id}'"));
    };
    let stopped_window = kill_controller_window(&record.target);
    if !stopped_window && controller_window_exists(&record.target) {
        fail("bn control stop: could not terminate controller window");
    }
    // Free both leases this controller held — the agent lease and the pool slot itself — so the
    // slot is immediately available again rather than waiting for the next reconcile to notice the
    // window is gone. Both are idempotent for controllers started before leasing existed.
    if let Err(error) = agent_lease::release(&record.session_id) {
        eprintln!("bn control stop: {error}");
    }
    wt_pool::release_controller_slot(cfg, &record.repo);
    controller::update_registry(&path, |registry| {
        registry.stop(&id, bn_core::util::now_epoch_secs())
    })
    .unwrap_or_else(|e| fail(&format!("bn control stop: persist controller: {e}")));
    if json {
        println!(
            "{}",
            Envelope::success(
                "control_stop",
                serde_json::json!({
                    "controller_id": id,
                    "window_closed": stopped_window,
                }),
            )
            .to_json()
        );
    } else {
        println!(
            "stopped controller {id}{}",
            if stopped_window {
                ""
            } else {
                " (window already absent)"
            }
        );
    }
}

fn lease_request(id: String, json: bool) {
    let request = controller::lease_next(
        &bn_core::backlog::backlog_path(),
        &id,
        bn_core::util::now_epoch_secs(),
    )
    .unwrap_or_else(|e| fail(&format!("bn control lease: {e}")));
    if json {
        println!(
            "{}",
            Envelope::success("control_lease", serde_json::json!({ "request": request })).to_json()
        );
    } else if let Some(request) = request {
        println!("{} · {}", request.id, request.item);
    } else {
        println!("No queued requests.");
    }
}

fn complete_request(id: String, request_id: String, failed: bool, json: bool) {
    let changed = controller::drain(
        &bn_core::backlog::backlog_path(),
        &id,
        &request_id,
        !failed,
        bn_core::util::now_epoch_secs(),
    )
    .unwrap_or_else(|e| fail(&format!("bn control complete: {e}")));
    if !changed {
        fail("bn control complete: request is absent or already terminal");
    }
    if json {
        println!(
            "{}",
            Envelope::success(
                "control_complete",
                serde_json::json!({
                    "controller_id": id,
                    "request_id": request_id,
                    "status": if failed { "failed" } else { "done" },
                }),
            )
            .to_json()
        );
    } else {
        println!(
            "{} request {request_id}",
            if failed { "failed" } else { "completed" }
        );
    }
}

pub(crate) fn emit_control(cfg: &Config, action: ControlCmd, json: bool) {
    match action {
        ControlCmd::Start {
            id,
            name,
            harness,
            agent,
            request,
        } => start_controller(cfg, id, name, harness, &agent, request, json),
        ControlCmd::Status { id } => status_controllers(id, json),
        ControlCmd::Stop { id } => stop_controller(cfg, id, json),
        ControlCmd::Lease { id } => lease_request(id, json),
        ControlCmd::Complete {
            id,
            request_id,
            failed,
        } => complete_request(id, request_id, failed, json),
        ControlCmd::Watch { id } => watch_controller(id),
        ControlCmd::TurnEndGuard { id } => turn_end_guard(cfg, id),
    }
}
