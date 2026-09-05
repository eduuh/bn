//! `bn sessions` — the persistent cross-repo agent-session registry.

use crate::*;

pub(crate) fn resolve_registered_session<'a>(
    rows: &'a [bn_core::agent::SessionRecord],
    query: Option<&str>,
    worktree: &str,
    branch: &str,
) -> Result<Option<&'a bn_core::agent::SessionRecord>, bn_core::agent::ResolveSessionError> {
    let selector = query.map_or(
        bn_core::agent::SessionSelector::LatestForWorktree { worktree, branch },
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

/// Why a checkout may not host a resumed agent, if it may not.
///
/// A repo's main worktree is off limits: it hosts no agents and only ever gets refreshed to latest.
/// `bn agent launch` enforces that at its own chokepoint, but every resume path builds the *window*
/// first — so without this a resume into main opened a detached pane that then sat on
/// `redirect_from_main`'s "What is this worktree for?" prompt with nobody looking at it.
fn off_limits(worktree: &std::path::Path) -> Option<String> {
    let canonical = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    bn_core::agent::is_within_main_worktree(&canonical).then(|| {
        format!(
            "{} is in the main worktree — it hosts no agents and stays clean. \
             Dispatch the work into a pool slot instead: bn dispatch \"<what you're doing>\"",
            canonical.display()
        )
    })
}

fn launch_session_window(launch: &WindowLaunch<'_>, json: bool) -> Result<(), String> {
    if let Some(refusal) = off_limits(launch.worktree) {
        return Err(refusal);
    }
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
    let resuming = bn_core::agent::transcript_backed(&row.harness, &row.session_id);
    let command = agent_launch_command(&row.harness, &row.session_id, resuming);
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
                    "fresh": !resuming,
                    "resumed_transcript": resuming,
                    "harness": row.harness,
                    "tmux_opened": true,
                }),
            )
            .to_json()
        );
    } else if resuming {
        println!(
            "resumed {} ({} · {}) in {}",
            row.task_name,
            row.harness,
            row.session_id,
            worktree.display()
        );
    } else {
        println!(
            "{} ({}) has no transcript left to resume; started a fresh {} session in {}.",
            row.task_name,
            row.session_id,
            row.harness,
            worktree.display()
        );
    }
}

/// The `bn agent launch` invocation for a recorded session.
///
/// `--resume-session` rides along only when a transcript backs the id. A harness handed an
/// unresumable id degrades silently to a fresh, unseeded agent — so passing it unconditionally is
/// how a resume reports success and hands back a blank prompt (#383). Mirrors
/// `resume_fleet::launch_command`, which has always gated on the same evidence.
fn agent_launch_command(harness: &str, session_id: &str, resuming: bool) -> String {
    let exe = std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("bn"))
        .to_string_lossy()
        .into_owned();
    let mut command = format!(
        "{} agent launch {} --session {}",
        wt::shell_single_quote(&exe),
        wt::shell_single_quote(harness),
        wt::shell_single_quote(session_id),
    );
    if resuming {
        command.push_str(&format!(
            " --resume-session {}",
            wt::shell_single_quote(session_id)
        ));
    } else {
        command.push_str(" --fresh");
    }
    command
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
    // The machine's configured harness, not a hardcoded `copilot`. Every other launcher already
    // asks `preferred_harness`; this one didn't, so a Claude user who ran `bn resume` in a worktree
    // with nothing recorded was dropped into Copilot without being told why.
    let harness = bn_core::harness::preferred_harness(cfg);
    bn_core::cost::attribute_session(bn_core::cost::SessionLaunch {
        session_id: &session_id,
        work_class: bn_core::cost::WorkClass::Coding,
        repo: &ctx.repo,
        branch: &ctx.branch,
        task_name: Some(&task_name),
        harness: Some(harness),
        worktree: Some(&worktree_text),
        controller_id: None,
    });
    let command = agent_launch_command(harness, &session_id, false);
    let launch = WindowLaunch {
        repo: &ctx.repo,
        branch: &ctx.branch,
        task_name: &task_name,
        window_scope: &ctx.branch,
        worktree,
        command: &command,
        session_id: &session_id,
        harness,
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
                    "harness": harness,
                    "worktree": worktree_text,
                    "tmux_opened": true,
                }),
            )
            .to_json()
        );
    } else {
        println!(
            "No recorded agent session for {}; started a fresh {harness} session ({session_id}) in {}.",
            ctx.branch,
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
    // The checkout alone does not identify the work. A pool slot hosts a different branch every
    // few hours, so `…/_pool/2` matched on path only would resume whatever branch held the slot
    // last. An explicit `bn resume <id>` needs no branch — the caller named the session.
    let branch = worktree
        .as_deref()
        .and_then(|path| resolver::resolve(cfg, path).ok())
        .map(|ctx| ctx.branch)
        .unwrap_or_default();
    if let Some(refusal) = worktree.as_deref().and_then(off_limits) {
        fail_json(json, command_tag, "main_worktree", &refusal);
    }
    let row = resolve_registered_session(&rows, query, &worktree_text, &branch).unwrap_or_else(
        |error| {
            fail_json(
                json,
                command_tag,
                "session_not_found",
                &format!("{command_tag}: {error}"),
            )
        },
    );
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

/// A recorded harness session that a recreated window should come back running.
pub(crate) struct WorktreeResume {
    pub(crate) harness: String,
    pub(crate) session_id: String,
    /// The `bn agent launch …` line for the window's pane.
    pub(crate) command: String,
}

/// The resumable session recorded for `worktree` on `branch`, if there is one.
///
/// Deliberately strict on both axes. The branch must match because pool slots are recycled, and
/// the id must still have a transcript because a harness handed a dead id quietly starts a blank
/// agent — either miss would put someone else's work, or no work at all, in a window that claims
/// to have restored yours. Returns `None` (never a fresh session) so the caller keeps the plain
/// shell it asked for when there is nothing genuine to bring back.
pub(crate) fn worktree_resume(
    cfg: &Config,
    worktree: &std::path::Path,
    branch: &str,
) -> Option<WorktreeResume> {
    if branch.trim().is_empty() {
        return None;
    }
    // Main is off limits however the window came to be recreated, so `wt go main` never turns into
    // an agent (or into `redirect_from_main`'s dispatch prompt).
    let canonical = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    if off_limits(&canonical).is_some() {
        return None;
    }
    let rows = bn_core::agent::session_registry(cfg).ok()?;
    let worktree_text = canonical.to_string_lossy().into_owned();
    let row = bn_core::agent::resolve_session(
        &rows,
        bn_core::agent::SessionSelector::LatestForWorktree {
            worktree: &worktree_text,
            branch,
        },
    )
    .ok()?;
    if !bn_core::agent::transcript_backed(&row.harness, &row.session_id) {
        return None;
    }
    Some(WorktreeResume {
        command: agent_launch_command(&row.harness, &row.session_id, true),
        harness: row.harness.clone(),
        session_id: row.session_id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::agent_launch_command;

    /// `--resume-session` is what actually restores the transcript, and `--fresh` is what stops the
    /// harness quietly continuing *some other* session in the same checkout. A recycled pool slot
    /// makes that "some other" a stranger's branch, so the unresumable case must say fresh out loud
    /// rather than leave the harness to guess.
    #[test]
    fn only_a_backed_session_is_asked_to_resume() {
        let resumed = agent_launch_command("claude", "abc-123", true);
        assert!(resumed.ends_with("agent launch 'claude' --session 'abc-123' --resume-session 'abc-123'"));

        let fresh = agent_launch_command("copilot", "abc-123", false);
        assert!(fresh.ends_with("agent launch 'copilot' --session 'abc-123' --fresh"));
        assert!(!fresh.contains("--resume-session"));
    }

    /// The harness is carried through verbatim. Hardcoding `copilot` here is what dropped a Claude
    /// user into the wrong agent whenever a worktree had nothing recorded.
    #[test]
    fn the_recorded_harness_is_the_one_launched() {
        assert!(agent_launch_command("claude", "id", true).contains("launch 'claude'"));
        assert!(agent_launch_command("copilot", "id", true).contains("launch 'copilot'"));
    }
}
