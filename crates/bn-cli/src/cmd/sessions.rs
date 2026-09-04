//! `bn sessions` — the persistent cross-repo agent-session registry.

use crate::*;

pub(crate) fn resolve_registered_session<'a>(
    rows: &'a [bn_core::agent::SessionRecord],
    query: Option<&str>,
    worktree: &str,
) -> Result<Option<&'a bn_core::agent::SessionRecord>, bn_core::agent::ResolveSessionError> {
    let selector = query.map_or(
        bn_core::agent::SessionSelector::LatestForWorktree(worktree),
        bn_core::agent::SessionSelector::NameOrId,
    );
    match bn_core::agent::resolve_session(rows, selector) {
        Ok(row) => Ok(Some(row)),
        Err(bn_core::agent::ResolveSessionError::NoWorktreeSession(_)) if query.is_none() => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn registry_rows(cfg: &Config) -> Vec<bn_core::agent::SessionRecord> {
    crate::cmd::control::reconcile_controller_liveness()
        .unwrap_or_else(|error| fail(&format!("bn sessions: reconcile controllers: {error}")));
    let mut rows = bn_core::agent::session_registry(cfg)
        .unwrap_or_else(|error| fail(&format!("bn sessions: read registry: {error}")));
    let live = agent_live::collect(
        cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    );
    for view in live {
        let Some(session_id) = view.session_id.as_deref() else {
            continue;
        };
        let Some(row) = rows.iter_mut().find(|row| {
            bn_core::cost::canonical_session_id(&row.session_id)
                == bn_core::cost::canonical_session_id(session_id)
        }) else {
            continue;
        };
        row.status = match view.state.as_str() {
            "failed" => bn_core::agent::SessionStatus::Failed,
            "done" => bn_core::agent::SessionStatus::Done,
            "waiting" | "blocked" | "needs-decision" => bn_core::agent::SessionStatus::Idle,
            _ => row.status,
        };
    }
    rows
}

pub(crate) fn emit_sessions(cfg: &Config, json: bool) {
    let rows = registry_rows(cfg);
    if json {
        println!(
            "{}",
            Envelope::success("sessions", serde_json::json!({ "sessions": rows })).to_json()
        );
        return;
    }
    if rows.is_empty() {
        println!("No agent sessions recorded.");
        return;
    }
    for row in rows {
        let controller = row
            .controller_id
            .as_deref()
            .map(|id| format!(" · controller {id}"))
            .unwrap_or_default();
        let pr = row
            .pr_url
            .as_deref()
            .map(|url| format!(" · {url}"))
            .unwrap_or_default();
        println!(
            "{:<5} {:<8} {} · {}/{} · {}{}{}",
            row.status.label(),
            row.harness,
            row.task_name,
            row.repo,
            row.branch,
            row.session_id,
            controller,
            pr,
        );
        println!("      {}", row.worktree);
    }
}

struct WindowLaunch<'a> {
    repo: &'a str,
    branch: &'a str,
    task_name: &'a str,
    window_scope: &'a str,
    worktree: &'a std::path::Path,
    command: &'a str,
    session_id: &'a str,
    harness: &'a str,
}

fn launch_session_window(launch: &WindowLaunch<'_>, json: bool) -> Result<(), String> {
    let window = bn_core::agent::task_window_name(launch.task_name, launch.window_scope);
    let window = format!(
        "{window} · resume-{:x}",
        bn_core::util::now_epoch_secs() & 0xffff
    );
    let branch = launch.branch.replace('/', "-");
    let lease_target = format!("{}:{window}", launch.repo);
    let reservation = agent_lease::reserve(agent_lease::AgentLeaseRequest {
        repo: launch.repo,
        branch: &branch,
        worktree: launch.worktree,
        session_id: launch.session_id,
        harness: launch.harness,
        kind: bn_core::agent::AgentLeaseKind::Dispatch,
        tmux_target: &lease_target,
        replace: false,
        in_place: false,
    })?;
    let launched = if json {
        wt::open_worktree_window_cmd_named_quiet(
            launch.repo,
            &branch,
            &window,
            launch.worktree,
            true,
            Some(launch.command),
            true,
        )
    } else {
        wt::open_worktree_window_cmd_named(
            launch.repo,
            &branch,
            &window,
            launch.worktree,
            true,
            Some(launch.command),
            true,
        )
    };
    if !launched {
        let _ = reservation.rollback();
        return Err("could not open the tmux session".into());
    }
    agent_lease::mark_window(reservation.lease());
    reservation.commit();
    Ok(())
}

/// Open a window running `command` in `worktree`, taking the agent lease for it.
///
/// The create half of `bn resume --all`'s restore, exposed so the crash path reuses the exact
/// launch this command already performs rather than reimplementing harness startup. Deliberately
/// *not* a split of `agent_relaunch`: this is the branch that has never killed a process, so
/// sharing it cannot leak a teardown into a recovery path.
pub(crate) fn launch_resume_window(
    repo: &str,
    branch: &str,
    task_name: &str,
    worktree: &std::path::Path,
    command: &str,
    session_id: &str,
    harness: &str,
) -> Result<(), String> {
    launch_session_window(
        &WindowLaunch {
            repo,
            branch,
            task_name,
            window_scope: branch,
            worktree,
            command,
            session_id,
            harness,
        },
        false,
    )
}

fn launch_recorded_session(row: &bn_core::agent::SessionRecord, json: bool, command_tag: &str) {
    let worktree = std::path::PathBuf::from(&row.worktree);
    if !worktree.is_dir() {
        fail_json(
            json,
            command_tag,
            "missing_worktree",
            &format!("recorded worktree is missing: {}", worktree.display()),
        );
    }
    if !matches!(row.harness.as_str(), "copilot" | "claude") {
        fail_json(
            json,
            command_tag,
            "unsupported_harness",
            &format!("session {} has no resumable harness", row.session_id),
        );
    }
    let exe = std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("bn"))
        .to_string_lossy()
        .into_owned();
    let command = format!(
        "{} agent launch {} --session {} --resume-session {}",
        wt::shell_single_quote(&exe),
        wt::shell_single_quote(&row.harness),
        wt::shell_single_quote(&row.session_id),
        wt::shell_single_quote(&row.session_id),
    );
    let launch = WindowLaunch {
        repo: &row.repo,
        branch: &row.branch,
        task_name: &row.task_name,
        window_scope: row.controller_id.as_deref().unwrap_or(&row.branch),
        worktree: &worktree,
        command: &command,
        session_id: &row.session_id,
        harness: &row.harness,
    };
    if let Err(error) = launch_session_window(&launch, json) {
        fail_json(json, command_tag, "agent_lease_or_tmux_failed", &error);
    }
    if json {
        println!(
            "{}",
            Envelope::success(
                command_tag,
                serde_json::json!({
                    "session": row,
                    "fresh": false,
                    "tmux_opened": true,
                }),
            )
            .to_json()
        );
    } else {
        println!(
            "resumed {} ({}) in {}",
            row.task_name,
            row.session_id,
            worktree.display()
        );
    }
}

fn launch_fresh_session(cfg: &Config, worktree: &std::path::Path, json: bool, command_tag: &str) {
    let ctx = resolver::resolve(cfg, worktree).unwrap_or_else(|_| {
        fail_json(
            json,
            command_tag,
            "not_in_git_worktree",
            "bn resume must be run inside a git worktree",
        )
    });
    let session_id = gen_uuid();
    let task_name = bn_core::agent::derive_task_name(None, &ctx.branch);
    let worktree_text = worktree.to_string_lossy().into_owned();
    bn_core::cost::attribute_session(bn_core::cost::SessionLaunch {
        session_id: &session_id,
        work_class: bn_core::cost::WorkClass::Coding,
        repo: &ctx.repo,
        branch: &ctx.branch,
        task_name: Some(&task_name),
        harness: Some("copilot"),
        worktree: Some(&worktree_text),
        controller_id: None,
    });
    let exe = std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("bn"))
        .to_string_lossy()
        .into_owned();
    let command = format!(
        "{} agent launch copilot --fresh --session {}",
        wt::shell_single_quote(&exe),
        wt::shell_single_quote(&session_id),
    );
    let launch = WindowLaunch {
        repo: &ctx.repo,
        branch: &ctx.branch,
        task_name: &task_name,
        window_scope: &ctx.branch,
        worktree,
        command: &command,
        session_id: &session_id,
        harness: "copilot",
    };
    if let Err(error) = launch_session_window(&launch, json) {
        fail_json(json, command_tag, "agent_lease_or_tmux_failed", &error);
    }
    if json {
        println!(
            "{}",
            Envelope::success(
                command_tag,
                serde_json::json!({
                    "session": serde_json::Value::Null,
                    "fresh": true,
                    "reason": "no_recorded_session",
                    "new_session_id": session_id,
                    "harness": "copilot",
                    "worktree": worktree_text,
                    "tmux_opened": true,
                }),
            )
            .to_json()
        );
    } else {
        println!(
            "No recorded agent session for this worktree; started a fresh Copilot session ({session_id}) in {}.",
            worktree.display()
        );
    }
}

pub(crate) fn resume_registered_session(
    cfg: &Config,
    query: Option<&str>,
    json: bool,
    command_tag: &str,
) {
    let rows = registry_rows(cfg);
    let worktree = std::env::current_dir()
        .ok()
        .and_then(|cwd| resolver::repo_root(&cwd))
        .map(|path| std::fs::canonicalize(&path).unwrap_or(path));
    if query.is_none() && worktree.is_none() {
        fail_json(
            json,
            command_tag,
            "not_in_git_worktree",
            "bn resume must be run inside a git worktree",
        );
    }
    let worktree_text = worktree
        .as_deref()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let row = resolve_registered_session(&rows, query, &worktree_text).unwrap_or_else(|error| {
        fail_json(
            json,
            command_tag,
            "session_not_found",
            &format!("{command_tag}: {error}"),
        )
    });
    match row {
        Some(row) => launch_recorded_session(row, json, command_tag),
        None => {
            let worktree = worktree.unwrap_or_else(|| {
                fail_json(
                    json,
                    command_tag,
                    "not_in_git_worktree",
                    "bn resume must be run inside a git worktree",
                )
            });
            launch_fresh_session(cfg, &worktree, json, command_tag);
        }
    }
}
