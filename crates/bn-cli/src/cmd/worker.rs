//! `bn worker` (#719, epic #707 M1) — thin CLI over the `bn_core::worker` attempt registry +
//! capacity snapshot. `status` renders the versioned payload (JSON or a human summary); `finish`
//! closes a running attempt (the tail of the fire-and-forget spawn line).

use crate::*;
use bn_core::worker::{self, registry, spawn};

/// Dispatch a `bn worker <sub>` command. Returns the process exit code.
pub(crate) fn emit_worker(cfg: &Config, action: WorkerCmd, json: bool) -> i32 {
    match action {
        WorkerCmd::Status => status(json),
        WorkerCmd::Finish {
            request_id,
            fail,
            exit_code,
        } => finish(cfg, &request_id, fail, exit_code, json),
        WorkerCmd::Start {
            request_id,
            attempt_id,
            task,
            repo,
            harness,
            memory_ceiling_mb,
        } => start(
            &request_id,
            &attempt_id,
            &task,
            &repo,
            &harness,
            memory_ceiling_mb,
            json,
        ),
        WorkerCmd::Run {
            attempt_id,
            request_id,
            memory_ceiling_mb,
            command,
        } => run(&attempt_id, &request_id, memory_ceiling_mb, &command, json),
        WorkerCmd::Reconcile => reconcile(json),
    }
}

/// Open the worker-local registry, or fail with the stable envelope on error.
fn open_registry(json: bool) -> registry::Registry {
    match registry::Registry::open(&worker::registry_path()) {
        Ok(reg) => reg,
        Err(e) => fail_json(json, "worker", "registry_open_failed", &e.to_string()),
    }
}

/// The volume the disk figure is measured on: the current worktree (repo root if resolvable, else
/// cwd). Best-effort — capacity is fail-open, so a bad path just yields `disk_avail_mb: null`.
fn worktree_root() -> std::path::PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();
    crate::git::git_capture(&["rev-parse", "--show-toplevel"], Some(&cwd), true)
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or(cwd)
}

fn status(json: bool) -> i32 {
    let reg = open_registry(json);
    let st = match worker::status::build(&reg, &worktree_root()) {
        Ok(st) => st,
        Err(e) => fail_json(json, "worker", "status_failed", &e.to_string()),
    };
    if json {
        let data = serde_json::to_value(&st).unwrap_or(serde_json::Value::Null);
        println!("{}", Envelope::success("worker", data).to_json());
    } else {
        print_status_human(&st);
    }
    0
}

fn print_status_human(st: &worker::WorkerStatus) {
    println!(
        "worker {} [{}] — {:?} (schema v{})",
        st.node.node_id,
        st.node.labels.join(", "),
        st.node.status,
        st.schema_version
    );
    let c = &st.capacity;
    let mb = |v: Option<u64>| v.map(|n| format!("{n}MB")).unwrap_or_else(|| "?".into());
    println!(
        "  capacity: mem {}/{} avail, swap {}/{} free, load1 {}, nproc {}, disk {} free",
        mb(c.mem_available_mb),
        mb(c.mem_total_mb),
        mb(c.swap_free_mb),
        mb(c.swap_total_mb),
        c.loadavg1
            .map(|l| format!("{l:.2}"))
            .unwrap_or_else(|| "?".into()),
        c.nproc,
        mb(c.disk_avail_mb),
    );
    if st.attempts.is_empty() {
        println!("  attempts: (none)");
    } else {
        println!("  attempts:");
        for a in &st.attempts {
            println!(
                "    {} {} [{}] task={} node={} ({})",
                a.state, a.attempt_id, a.request_id, a.task, a.node, a.updated_ts
            );
        }
    }
}

fn finish(
    cfg: &Config,
    request_id: &str,
    fail_flag: bool,
    exit_code: Option<i64>,
    json: bool,
) -> i32 {
    let reg = open_registry(json);
    let attempts = match reg.by_request(request_id) {
        Ok(a) => a,
        Err(e) => fail_json(json, "worker", "lookup_failed", &e.to_string()),
    };
    // Finish the most-recent RUNNING attempt for this request (list is newest-first).
    let Some(target) = attempts
        .into_iter()
        .find(|a| a.state == registry::AttemptState::Running)
    else {
        fail_json(
            json,
            "worker",
            "no_running_attempt",
            &format!("no RUNNING attempt for request {request_id}"),
        );
    };
    let success = !fail_flag;
    if let Err(e) = reg.finish(&target.attempt_id, success, exit_code, None) {
        fail_json(json, "worker", "finish_failed", &e.to_string());
    }
    if fail_flag {
        if let Some(ctx) = target_ctx(cfg, None, None) {
            let summary = exit_code.map_or_else(
                || "worker process failed".to_string(),
                |code| format!("worker process failed with exit code {code}"),
            );
            append_inherited_worker_event(
                cfg,
                &ctx,
                bn_core::controller::ControllerWorkerEventType::Failed,
                &summary,
                None,
            )
            .unwrap_or_else(|e| {
                fail_json(json, "worker", "controller_event_failed", &e.to_string())
            });
        }
    }
    let final_state = if success { "SUCCEEDED" } else { "FAILED" };
    if json {
        let data = serde_json::json!({
            "attempt_id": target.attempt_id,
            "request_id": request_id,
            "state": final_state,
            "exit_code": exit_code,
        });
        println!("{}", Envelope::success("worker", data).to_json());
    } else {
        println!("worker: attempt {} → {final_state}", target.attempt_id);
    }
    0
}

/// The conventional `tmux -L orch` session an attempt's agent runs under — the attempt's liveness
/// domain. Kept in one place so admission (record) and the probe (check) agree.
fn agent_session(request_id: &str) -> String {
    format!("agent-{request_id}")
}

/// Read `MemAvailable` (MB) from `/proc/meminfo`. `None` off Linux or when unreadable — admission
/// then fails open on the memory gate (matching bn's other guards).
fn read_mem_available_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/meminfo").ok()?;
        bn_core::machine::parse_meminfo(&s).available_mb
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Liveness probe for a reservation: alive if its `tmux -L orch` session still exists, else if its
/// pid is still running (`kill -0`). Either signal being live keeps the attempt live; both
/// unreadable/absent ⇒ dead (self-heal reclaims the slot).
fn is_alive(tmux_session: Option<&str>, pid: Option<i64>) -> bool {
    if let Some(sess) = tmux_session {
        if tmux_has_session(sess) {
            return true;
        }
    }
    if let Some(pid) = pid {
        if pid > 0 && process_alive(pid) {
            return true;
        }
    }
    false
}

fn tmux_has_session(session: &str) -> bool {
    bn_core::tmux::tmux_ok(&["-L", "orch", "has-session", "-t", session])
}

fn process_alive(pid: i64) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Whether `systemd-run` is on PATH (so [`spawn::wrap`] can prefer the cgroup scope over the
/// `ulimit -v` fallback).
fn has_systemd_run() -> bool {
    std::process::Command::new("systemd-run")
        .args(["--user", "--scope", "--quiet", "--collect", "--", "true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn worker_dispatch_argv(
    executable: &std::path::Path,
    repo: &str,
    task: &str,
    harness: &str,
) -> Vec<String> {
    let mut argv = vec![executable.to_string_lossy().into_owned()];
    argv.extend(
        crate::cmd::dispatch::DispatchInvocation::new(task, repo, harness)
            .verify(true)
            .yolo(true)
            .json(true)
            .args(),
    );
    argv
}

fn worker_monitor_argv(
    executable: &std::path::Path,
    attempt_id: &str,
    request_id: &str,
    memory_ceiling_mb: u64,
    dispatch_argv: &[String],
) -> Vec<String> {
    let mut argv = vec![
        executable.to_string_lossy().into_owned(),
        "worker".to_string(),
        "run".to_string(),
        "--attempt-id".to_string(),
        attempt_id.to_string(),
        "--request-id".to_string(),
        request_id.to_string(),
        "--memory-ceiling-mb".to_string(),
        memory_ceiling_mb.to_string(),
        "--".to_string(),
    ];
    argv.extend_from_slice(dispatch_argv);
    argv
}

fn shell_quote(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    if arg
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_./:@%+=".contains(&b))
    {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', "'\"'\"'"))
}

fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn launch_tmux_session(session: &str, argv: &[String]) -> std::io::Result<()> {
    let mut command =
        crate::proc::tmux_command(&["-L", "orch", "new-session", "-d", "-s", session]);
    let status = command.arg(shell_join(argv)).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "tmux exited with status {status}"
        )))
    }
}

fn admit_and_launch(
    reg: &mut registry::Registry,
    req: &registry::AdmissionRequest,
    monitor_argv: &[String],
    read_memory: &dyn Fn() -> Option<u64>,
    alive: &dyn Fn(Option<&str>, Option<i64>) -> bool,
    launch: &dyn Fn(&str, &[String]) -> std::io::Result<()>,
) -> Result<registry::AdmissionOutcome, String> {
    let outcome = reg
        .admit(req, read_memory, alive)
        .map_err(|e| e.to_string())?;
    if outcome == registry::AdmissionOutcome::Accepted {
        if let Err(e) = launch(&req.tmux_session, monitor_argv) {
            let msg = format!("launch failed: {e}");
            reg.finish(&req.attempt_id, false, None, Some(&msg))
                .map_err(|finish_err| format!("{msg}; registry finish failed: {finish_err}"))?;
            return Err(msg);
        }
    }
    Ok(outcome)
}

fn dispatch_agent_launched(stdout: &[u8]) -> bool {
    String::from_utf8_lossy(stdout)
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .and_then(|value| {
            value
                .get("data")
                .and_then(|data| data.get("agent_launched"))
                .and_then(|launched| launched.as_bool())
        })
        .unwrap_or(false)
}

/// `bn worker start` — atomic, worker-authoritative admission (#720). Exit codes: 0 accepted or
/// already_exists, 1 rejected_capacity, 2 internal error.
fn start(
    request_id: &str,
    attempt_id: &str,
    task: &str,
    repo: &str,
    harness: &str,
    memory_ceiling_mb: u64,
    json: bool,
) -> i32 {
    let mut reg = match registry::Registry::open(&worker::registry_path()) {
        Ok(reg) => reg,
        Err(e) => return internal_error(json, "registry_open_failed", &e.to_string()),
    };
    let req = registry::AdmissionRequest {
        attempt_id: attempt_id.to_string(),
        request_id: request_id.to_string(),
        task: task.to_string(),
        node: worker::node_id(),
        memory_estimate_mb: memory_ceiling_mb,
        tmux_session: agent_session(request_id),
    };
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => return internal_error(json, "current_exe_failed", &e.to_string()),
    };
    let dispatch_argv = worker_dispatch_argv(&executable, repo, task, harness);
    let monitor_argv = worker_monitor_argv(
        &executable,
        attempt_id,
        request_id,
        memory_ceiling_mb,
        &dispatch_argv,
    );
    let outcome = match admit_and_launch(
        &mut reg,
        &req,
        &monitor_argv,
        &read_mem_available_mb,
        &is_alive,
        &launch_tmux_session,
    ) {
        Ok(o) => o,
        Err(e) => return internal_error(json, "start_failed", &e),
    };

    let (verdict, code) = match &outcome {
        registry::AdmissionOutcome::Accepted => ("accepted", 0),
        registry::AdmissionOutcome::AlreadyExists => ("already_exists", 0),
        registry::AdmissionOutcome::RejectedCapacity { .. } => ("rejected_capacity", 1),
    };
    let reason = match &outcome {
        registry::AdmissionOutcome::RejectedCapacity { reason } => Some(reason.clone()),
        _ => None,
    };
    // The cgroup-cap wrapper the caller should launch the agent under (accepted only).
    let cap = spawn::MemoryCap::from_estimate_mb(memory_ceiling_mb);

    if json {
        let mut data = serde_json::json!({
            "verdict": verdict,
            "request_id": request_id,
            "attempt_id": attempt_id,
            "tmux_session": agent_session(request_id),
            "memory_ceiling_mb": memory_ceiling_mb,
        });
        if let Some(r) = &reason {
            data["reason"] = serde_json::Value::String(r.clone());
        }
        if matches!(outcome, registry::AdmissionOutcome::Accepted) {
            data["memory_cap"] = serde_json::json!({
                "hard_mb": cap.hard_mb,
                "soft_mb": cap.soft_mb,
                "systemd_run": has_systemd_run(),
            });
        }
        println!("{}", Envelope::success("worker", data).to_json());
    } else {
        match &reason {
            Some(r) => println!("worker start: {verdict} — {r}"),
            None => println!("worker start: {verdict} (attempt {attempt_id})"),
        }
    }
    code
}

fn run(
    attempt_id: &str,
    request_id: &str,
    memory_ceiling_mb: u64,
    command: &[String],
    json: bool,
) -> i32 {
    let Some((program, args)) = command.split_first() else {
        return internal_error(json, "missing_command", "worker run requires a command");
    };
    let output = std::process::Command::new(program)
        .args(args)
        .env("BN_AGENT_MEMORY_CEILING_MB", memory_ceiling_mb.to_string())
        .env("BN_WORKER_REQUEST_ID", request_id)
        .output();
    match output {
        Ok(output) if output.status.success() && dispatch_agent_launched(&output.stdout) => {}
        Ok(output) => {
            let code = output.status.code().map(i64::from).or(Some(1));
            let reg = open_registry(json);
            let result = if output.status.success() {
                "worker dispatch did not launch an agent"
            } else {
                "worker dispatch failed"
            };
            if let Err(e) = reg.finish(attempt_id, false, code, Some(result)) {
                return internal_error(json, "finish_failed", &e.to_string());
            }
            return code.and_then(|c| i32::try_from(c).ok()).unwrap_or(1);
        }
        Err(e) => {
            let reg = open_registry(json);
            if let Err(finish_err) = reg.finish(
                attempt_id,
                false,
                None,
                Some("worker dispatch did not start"),
            ) {
                return internal_error(
                    json,
                    "dispatch_start_and_finish_failed",
                    &format!("{e}; {finish_err}"),
                );
            }
            return internal_error(json, "dispatch_start_failed", &e.to_string());
        }
    }

    loop {
        let reg = open_registry(json);
        match reg.get(attempt_id) {
            Ok(Some(attempt)) => match attempt.state {
                registry::AttemptState::Running => {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
                registry::AttemptState::Succeeded => return 0,
                registry::AttemptState::Failed => {
                    return attempt
                        .exit_code
                        .and_then(|code| i32::try_from(code).ok())
                        .unwrap_or(1);
                }
                registry::AttemptState::Queued => {
                    return internal_error(
                        json,
                        "illegal_attempt_state",
                        "worker monitor found a QUEUED attempt",
                    );
                }
            },
            Ok(None) => {
                return internal_error(
                    json,
                    "attempt_missing",
                    "worker monitor attempt disappeared",
                );
            }
            Err(e) => return internal_error(json, "attempt_lookup_failed", &e.to_string()),
        }
    }
}

/// `bn worker reconcile` — clear orphaned RUNNING reservations (dead tmux session/pid).
fn reconcile(json: bool) -> i32 {
    let mut reg = match registry::Registry::open(&worker::registry_path()) {
        Ok(reg) => reg,
        Err(e) => return internal_error(json, "registry_open_failed", &e.to_string()),
    };
    let cleared = match reg.reconcile(&is_alive) {
        Ok(c) => c,
        Err(e) => return internal_error(json, "reconcile_failed", &e.to_string()),
    };
    if json {
        let data = serde_json::json!({ "cleared": cleared, "count": cleared.len() });
        println!("{}", Envelope::success("worker", data).to_json());
    } else if cleared.is_empty() {
        println!("worker reconcile: no orphaned attempts");
    } else {
        println!(
            "worker reconcile: cleared {} orphaned attempt(s):",
            cleared.len()
        );
        for id in &cleared {
            println!("  {id}");
        }
    }
    0
}

/// Emit the failure envelope (or a stderr line) and return exit code 2 (internal error) — distinct
/// from `rejected_capacity`'s exit 1, per the `bn worker start` contract.
fn internal_error(json: bool, code: &str, msg: &str) -> i32 {
    if json {
        println!(
            "{}",
            bn_core::render::Envelope::failure("worker", code, msg).to_json()
        );
    } else {
        eprintln!("{msg}");
    }
    2
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn admission(attempt_id: &str, request_id: &str) -> registry::AdmissionRequest {
        registry::AdmissionRequest {
            attempt_id: attempt_id.to_string(),
            request_id: request_id.to_string(),
            task: "707".to_string(),
            node: "fedora".to_string(),
            memory_estimate_mb: 1024,
            tmux_session: agent_session(request_id),
        }
    }

    #[test]
    fn worker_dispatch_uses_canonical_lane_without_mutating_item() {
        let argv = worker_dispatch_argv(std::path::Path::new("/opt/bn"), "bn", "707", "copilot");
        assert_eq!(argv[0], "/opt/bn");
        assert_eq!(argv[1], "dispatch");
        assert!(argv.contains(&"--repo".to_string()));
        assert!(argv.contains(&"bn".to_string()));
        assert!(argv.contains(&"--agent".to_string()));
        assert!(argv.contains(&"copilot".to_string()));
        assert!(argv.contains(&"--verify".to_string()));
        assert!(argv.contains(&"--yolo".to_string()));
        assert_eq!(argv[2], "707");
    }

    #[test]
    fn dispatch_launch_result_requires_agent_launched_true() {
        assert!(dispatch_agent_launched(
            br#"{"ok":true,"data":{"agent_launched":true}}"#
        ));
        assert!(!dispatch_agent_launched(
            br#"{"ok":true,"data":{"agent_launched":false}}"#
        ));
        assert!(!dispatch_agent_launched(
            br#"{"ok":false,"error":{"code":"dispatch_failed"}}"#
        ));
    }

    #[test]
    fn accepted_attempt_launches_once_and_retry_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = registry::Registry::open(&dir.path().join("worker.sqlite")).unwrap();
        let req = admission("attempt-1", "request-1");
        let launches = Cell::new(0);
        let launch = |_: &str, _: &[String]| {
            launches.set(launches.get() + 1);
            Ok(())
        };
        let monitor = vec!["bn".to_string(), "worker".to_string(), "run".to_string()];

        let first = admit_and_launch(
            &mut reg,
            &req,
            &monitor,
            &|| Some(4096),
            &|_, _| true,
            &launch,
        )
        .unwrap();
        let retry = admit_and_launch(
            &mut reg,
            &req,
            &monitor,
            &|| Some(4096),
            &|_, _| true,
            &launch,
        )
        .unwrap();

        assert_eq!(first, registry::AdmissionOutcome::Accepted);
        assert_eq!(retry, registry::AdmissionOutcome::AlreadyExists);
        assert_eq!(launches.get(), 1);
    }

    #[test]
    fn launch_failure_marks_reservation_failed() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = registry::Registry::open(&dir.path().join("worker.sqlite")).unwrap();
        let req = admission("attempt-1", "request-1");
        let result = admit_and_launch(
            &mut reg,
            &req,
            &["bn".to_string()],
            &|| Some(4096),
            &|_, _| true,
            &|_, _| Err(std::io::Error::other("tmux unavailable")),
        );

        assert!(result.unwrap_err().contains("tmux unavailable"));
        let attempt = reg.get("attempt-1").unwrap().unwrap();
        assert_eq!(attempt.state, registry::AttemptState::Failed);
        assert!(attempt.result.unwrap().contains("launch failed"));
    }

    #[test]
    fn shell_join_preserves_spaces_and_quotes() {
        assert_eq!(
            shell_join(&["bn".into(), "a b".into(), "it's".into()]),
            "bn 'a b' 'it'\"'\"'s'"
        );
    }
}
