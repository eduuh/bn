//! `bn agent` — fleet supervise/recover/checkin/send/digest/handoff/switch.
use crate::*;

pub(crate) fn emit_agent_ingest(cfg: &Config, as_json: bool) {
    match bn_core::agent::ingest(cfg) {
        Ok(r) => {
            if as_json {
                let data = serde_json::json!({ "ingested": r.ingested, "skipped": r.skipped });
                println!("{}", Envelope::success("agent_ingest", data).to_json());
            } else {
                println!(
                    "ingested {} new session{} ({} already seen)",
                    r.ingested,
                    if r.ingested == 1 { "" } else { "s" },
                    r.skipped
                );
            }
        }
        Err(e) => fail(&format!("bn agent ingest: {e}")),
    }
}

/// `bn agent supervise` — the live-agent fleet: every running Claude session with its health,
/// flagging any idle past the stuck threshold. Read-only; `--json` emits the whole `Fleet` for
/// the cockpit FLEET panel.
pub(crate) fn emit_agent_supervise(cfg: &Config, idle: i64, stuck: i64, as_json: bool) {
    if let Err(error) =
        agent_live::publish_actionable_wakes(cfg, idle, stuck, bn_core::util::now_epoch_secs())
    {
        eprintln!("bn agent supervise: wake queue: {error}");
    }
    let fleet = bn_core::agent::supervise(cfg, idle, stuck);
    if as_json {
        println!(
            "{}",
            Envelope::success("agent_supervise", payload::agent_fleet_data(&fleet)).to_json()
        );
        return;
    }
    if fleet.agents.is_empty() {
        println!("No running agents.");
        return;
    }
    println!(
        "{} running · {} starting · {} active · {} idle · {} stuck",
        fleet.agents.len(),
        fleet.starting,
        fleet.active,
        fleet.idle,
        fleet.stuck
    );
    for a in &fleet.agents {
        let glyph = match a.health {
            bn_core::agent::AgentHealth::Starting => "◍",
            bn_core::agent::AgentHealth::Active => "●",
            bn_core::agent::AgentHealth::Idle => "◐",
            bn_core::agent::AgentHealth::Stuck => "○",
        };
        println!(
            "  {glyph} {:<24} {:<7} idle {:<5} {} turns",
            a.target,
            a.health.label(),
            ago(a.idle_secs),
            a.turns
        );
    }
}

/// Schedule the check-in via a systemd `--user` transient timer (`--on-active=Nmin`, fires once).
/// `--` guards `target`/`note` so a leading-dash value can't smuggle a flag into `bn agent send`.
pub(crate) fn schedule_checkin_systemd(bn: &str, mins: i64, target: &str, note: &str) -> bool {
    let on_active = format!("--on-active={mins}min");
    let unit = format!("--unit=bn-checkin-{}", bn_core::util::now_epoch_secs());
    std::process::Command::new("systemd-run")
        .args([
            "--user", "--quiet", &on_active, &unit, bn, "agent", "send", "--", target, note,
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Schedule the check-in as a DETACHED one-shot for platforms without systemd (macOS): a background
/// `sleep <secs>; bn agent send -- <target> <note>`. `target`/`note` are passed as positional argv
/// (`$1..$4`) — never interpolated into the script — so there is no shell injection. It survives the
/// `checkin` process exiting (the child reparents) but not a reboot, which is fine for a short nudge.
pub(crate) fn schedule_checkin_detached(bn: &str, mins: i64, target: &str, note: &str) -> bool {
    let secs = (mins * 60).to_string();
    std::process::Command::new("sh")
        .args([
            "-c",
            "sleep \"$1\"; exec \"$2\" agent send -- \"$3\" \"$4\"",
            "bn-checkin", // $0
            &secs,        // $1
            bn,           // $2
            target,       // $3
            note,         // $4
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// `bn agent checkin <mins> "<note>"` — self-schedule a nudge that, in `<mins>` minutes, runs
/// `bn agent send <target> "<note>"`: a systemd `--user` transient timer where systemd exists, else
/// a detached one-shot (macOS). The native analogue of the orchestrator's `schedule_with_note.sh`.
/// Target defaults to the current worktree's window.
pub(crate) fn emit_agent_checkin(
    cfg: &Config,
    mins: i64,
    note: &str,
    target: Option<String>,
    as_json: bool,
) {
    if mins <= 0 {
        fail("bn agent checkin: <mins> must be a positive number of minutes");
    }
    let target = target.unwrap_or_else(|| match target_ctx(cfg, None, None) {
        Some(ctx) => format!("{}:{}", ctx.repo, ctx.branch),
        None => fail("bn agent checkin: not in a git repo — pass --target repo:branch"),
    });
    if cfg!(windows) {
        fail("bn agent checkin: self-scheduling is unix-only");
    }
    let bn = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "bn".into());
    // macOS has no systemd; use a detached one-shot there. systemd-run is preferred where it
    // exists (survives more), so only fall to the detached path when there's no systemctl.
    let scheduled = if cfg!(target_os = "macos") || !have_systemctl() {
        schedule_checkin_detached(&bn, mins, &target, note)
    } else {
        schedule_checkin_systemd(&bn, mins, &target, note)
    };
    if as_json {
        let data = serde_json::json!({
            "scheduled": scheduled,
            "target": target,
            "mins": mins,
            "note": note,
        });
        let env = if scheduled {
            Envelope::success("agent_checkin", data)
        } else {
            Envelope::failure(
                "agent_checkin",
                "schedule_failed",
                "could not schedule the check-in",
            )
        };
        println!("{}", env.to_json());
        return;
    }
    if scheduled {
        println!("Check-in scheduled in {mins}m → {target}: {note}");
    } else {
        fail("bn agent checkin: could not schedule the check-in (no systemd-run/sh available)");
    }
}

/// `bn agent send <target> "<msg>"` — deliver a message to a running agent's tmux pane via
/// send-keys (typing the text, then Enter). `<target>` is a `repo:branch` window (the cockpit
/// shape) or a literal `session:window`. The orchestrator's `send-claude-message.sh` analogue.
/// `bn agent recover` — finish an interrupted ship turn: a worktree with commits ahead of `base`
/// but no recorded PR (its agent died before pushing). Dry-run reports candidates; `--yes` pushes
/// the branch and opens a PR (`gh pr create --fill`), recording it on the note. The safety net for
/// #3 — with async `--detach` steering, turns shouldn't die, but this reconciles the ones that do.
pub(crate) fn emit_agent_recover(
    cfg: &Config,
    repo: Option<String>,
    branch: Option<String>,
    all: bool,
    base: Option<&str>,
    yes: bool,
    as_json: bool,
) {
    let base = base.unwrap_or("main");
    // Build the target list: --all sweeps every worktree (repo/branch dirs), else one target.
    let mut targets: Vec<(String, String, std::path::PathBuf)> = Vec::new();
    if all {
        if let Ok(repos) = std::fs::read_dir(&cfg.worktree_dir) {
            for r in repos.flatten().filter(|e| e.path().is_dir()) {
                let repo = r.file_name().to_string_lossy().into_owned();
                if let Ok(branches) = std::fs::read_dir(r.path()) {
                    for b in branches.flatten().filter(|e| e.path().is_dir()) {
                        let br = b.file_name().to_string_lossy().into_owned();
                        if br == "main" || br == "_pool" {
                            continue;
                        }
                        targets.push((repo.clone(), br, b.path()));
                    }
                }
            }
        }
    } else {
        let ctx = target_ctx(cfg, repo, branch).unwrap_or_else(|| {
            fail("bn agent recover: not in a git repo — pass --repo/--branch or --all")
        });
        targets.push((
            ctx.repo.clone(),
            ctx.branch.clone(),
            cfg.worktree_dir.join(&ctx.repo).join(&ctx.branch),
        ));
    }

    let gitout = |wt: &std::path::Path, args: &[&str]| -> Option<String> {
        crate::git::git_capture(args, Some(wt), true)
    };

    let mut results: Vec<serde_json::Value> = Vec::new();
    for (repo, branch, wt) in targets {
        if !wt.is_dir() {
            continue;
        }
        // Commits ahead of base? (nothing to recover otherwise)
        let ahead = gitout(&wt, &["rev-list", "--count", &format!("{base}..HEAD")])
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        if ahead == 0 {
            continue;
        }
        // Already has a PR recorded on the note? then it's not stranded.
        let ctx = Resolved {
            repo: repo.clone(),
            branch: branch.clone(),
        };
        let has_pr = bn_core::store::read(&cfg.note_file(&ctx))
            .ok()
            .map(|n| n.front.pr.is_some())
            .unwrap_or(false);
        if has_pr {
            continue;
        }

        if !yes {
            results.push(serde_json::json!({
                "repo": repo, "branch": branch, "commits_ahead": ahead, "action": "would-recover"
            }));
            continue;
        }

        // --yes: push + open (or find) the PR + record it.
        let br = gitout(&wt, &["branch", "--show-current"]).unwrap_or_else(|| branch.clone());
        let wt_s = wt.to_string_lossy();
        let _ = crate::git::git_run(&["-C", wt_s.as_ref(), "push", "-u", "origin", &br]);
        let gh = |args: &[&str]| crate::gh::gh_capture_in(&wt, args);
        // Reuse an existing PR for this branch if one is already open; else create one.
        let url = gh(&[
            "pr",
            "list",
            "--head",
            &br,
            "--json",
            "url",
            "--jq",
            ".[0].url // empty",
        ])
        .filter(|s| s.starts_with("http"))
        .or_else(|| {
            gh(&["pr", "create", "--fill"]).and_then(|o| {
                o.lines()
                    .rev()
                    .find(|l| l.trim().starts_with("http"))
                    .map(str::to_string)
            })
        });
        if let Some(u) = &url {
            let _ = commands::set_pr(cfg, &ctx, u, &today());
        }
        results.push(serde_json::json!({
            "repo": repo, "branch": branch, "commits_ahead": ahead,
            "action": if url.is_some() { "recovered" } else { "push-ok-pr-failed" },
            "pr": url,
        }));
    }

    if as_json {
        let data = serde_json::json!({ "candidates": results.len(), "results": results });
        println!("{}", Envelope::success("agent_recover", data).to_json());
    } else if results.is_empty() {
        println!("nothing to recover (no worktree with commits ahead + no PR)");
    } else {
        for r in &results {
            let repo = r["repo"].as_str().unwrap_or("");
            let branch = r["branch"].as_str().unwrap_or("");
            let ahead = r["commits_ahead"].as_u64().unwrap_or(0);
            let action = r["action"].as_str().unwrap_or("");
            match r["pr"].as_str() {
                Some(pr) => println!("  {action} {repo}/{branch} ({ahead} commits) → {pr}"),
                None => println!("  {action} {repo}/{branch} ({ahead} commits ahead, no PR)"),
            }
        }
        if !yes {
            println!("(dry-run — pass --yes to push + open the PRs)");
        }
    }
}

/// `bn agent done` — the agent's completion push (event-driven, not polled): raise a durable, keyed
/// attention item (source `agent`, carrying repo/branch/PR link) so it surfaces in `bn next` and the
/// orchestrator's attention read; badge the window `@bn_agent done`; record it in the note; and,
/// with `--notify`, fire a desktop toast as the immediate wake. Keyed by `done:<repo>/<branch>` so a
/// re-run dedups. The durable event log is the source of truth — the toast/badge are just the nudge.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_agent_done(
    cfg: &Config,
    summary: Option<String>,
    pr: Option<String>,
    notify: bool,
    report_only: bool,
    failed: bool,
    repo: Option<String>,
    branch: Option<String>,
    as_json: bool,
) {
    let ctx = target_ctx(cfg, repo, branch)
        .unwrap_or_else(|| fail("bn agent done: not in a git repo — pass --repo/--branch"));
    let summary = summary.unwrap_or_default();
    // Report-only: mark the note so `bn promote` won't try to ship a non-code finding.
    if report_only {
        let _ = commands::add(
            cfg,
            &ctx,
            "decision",
            "finding: report-only (a recommendation — no code change to ship)",
            Some("finding: report-only"),
            &today(),
        );
    }
    let outcome = if failed { "failed" } else { "done" };
    let title = if summary.is_empty() {
        format!("{outcome}: {}/{}", ctx.repo, ctx.branch)
    } else {
        format!("{outcome}: {}/{} — {summary}", ctx.repo, ctx.branch)
    };
    let key = format!("{outcome}:{}/{}", ctx.repo, ctx.branch);
    // A completion is an outcome, not an actionable item: raise it at DONE_SEVERITY so it lands in
    // the review inbox (`bn feed`), never the actionable attention list (doctor / `bn next`).
    let out = bn_core::attention::add(
        "agent",
        &title,
        bn_core::attention::DONE_SEVERITY,
        Some(&ctx.repo),
        Some(&ctx.branch),
        pr.as_deref(),
        Some(&key),
        &today(),
    );
    // If the item already existed (e.g. a scout-phase `done`) and this call carries NEW info — most
    // importantly a PR link from the ship phase — UPDATE it in place rather than losing it to the
    // dedup key. So a scout→ship agent's PR actually surfaces on the completion item.
    let (existing_id, skipped) = out
        .as_ref()
        .ok()
        .map(|o| (o.id.clone(), o.skipped))
        .unwrap_or_default();
    let mut updated = false;
    if skipped && (pr.is_some() || !summary.is_empty()) {
        updated = bn_core::attention::update(
            &existing_id,
            Some(&title),
            None,
            None,
            None,
            pr.as_deref(),
            None,
            None,
            None,
        )
        .unwrap_or(false);
    }

    // Record in the note (durable) + badge the window done (Plane A) + optional toast (the wake).
    let progress = if let Some(p) = &pr {
        format!(
            "{} — agent {outcome}: {summary} (PR {p})",
            bn_core::util::now_stamp()
        )
    } else {
        format!(
            "{} — agent {outcome}: {summary}",
            bn_core::util::now_stamp()
        )
    };
    let _ = commands::append_body(cfg, &ctx, "progress", &progress, &today());
    // Fill the structured `front.pr` field on the path everyone runs (mirror `agent_recover`):
    // otherwise the PR only lands in the Progress prose + attention item and `bn pr` never gets
    // called, leaving `front.pr` empty ~half the time (issue #648, root cause 2).
    if let Some(p) = &pr {
        let _ = commands::set_pr(cfg, &ctx, p, &today());
        append_inherited_worker_event(
            cfg,
            &ctx,
            bn_core::controller::ControllerWorkerEventType::PrOpened,
            "PR opened",
            Some(p),
        )
        .unwrap_or_else(|e| fail(&format!("bn agent done: PR controller event: {e}")));
    }
    append_inherited_worker_event(
        cfg,
        &ctx,
        if failed {
            bn_core::controller::ControllerWorkerEventType::Failed
        } else {
            bn_core::controller::ControllerWorkerEventType::Done
        },
        &summary,
        pr.as_deref(),
    )
    .unwrap_or_else(|e| fail(&format!("bn agent done: controller event: {e}")));
    set_tmux_agent(outcome);
    if notify {
        bn_core::notify::toast(if failed { "Agent failed" } else { "Agent done" }, &title);
    }

    let raised = !skipped;
    if as_json {
        let env = Envelope::success(
            "agent_done",
            serde_json::json!({ "repo": ctx.repo, "branch": ctx.branch, "raised": raised, "updated": updated, "pr": pr, "failed": failed }),
        );
        println!("{}", env.to_json());
    } else {
        let what = if raised {
            "raised"
        } else if updated {
            "updated (PR attached)"
        } else {
            "already open"
        };
        println!(
            "✓ reported done → {}/{} (attention item {what})",
            ctx.repo, ctx.branch
        );
    }
}

/// Resolve a `repo:branch` target to `(worktree_path, harness, session_id)` from the branch note's
/// recorded `agent-session: <harness> <id>` line (latest wins), for reliable `-p --resume` steering.
/// `None` when the target isn't a `repo:branch`, has no note, or recorded no session.
pub(crate) fn resolve_agent_session(
    cfg: &Config,
    target: &str,
) -> Option<(std::path::PathBuf, String, String)> {
    let (repo, branch) = target.split_once(':')?;
    let (mut repo, mut branch) = (repo.to_string(), branch.to_string());
    let direct = Resolved {
        repo: repo.clone(),
        branch: branch.clone(),
    };
    if !cfg.note_file(&direct).exists() {
        let tmux_target = format!("={target}");
        let metadata = proc::tmux(&[
            "display-message",
            "-p",
            "-t",
            &tmux_target,
            "#{@bn_repo}\t#{@bn_branch}",
        ])?;
        let (recorded_repo, recorded_branch) = metadata.trim().split_once('\t')?;
        if recorded_repo.is_empty() || recorded_branch.is_empty() {
            return None;
        }
        repo = recorded_repo.to_string();
        branch = recorded_branch.to_string();
    }
    let ctx = Resolved {
        repo: repo.clone(),
        branch: branch.clone(),
    };
    let note = bn_core::store::read(&cfg.note_file(&ctx)).ok()?;
    // The dispatcher records `agent-session: <harness> <id>` in the collab section; take the latest.
    let (harness, id) = note.front.collab.iter().rev().find_map(|it| {
        let rest = it.text.trim().strip_prefix("agent-session:")?;
        let mut w = rest.split_whitespace();
        Some((w.next()?.to_string(), w.next()?.to_string()))
    })?;
    let wt = cfg.worktree_dir.join(repo).join(branch);
    Some((wt, harness, id))
}

/// Build the `-p --resume` command for a harness (shared by the sync + detached steer paths).
/// `None` for an unknown harness.
pub(crate) fn resume_command(
    harness: &str,
    id: &str,
    wt: &std::path::Path,
    msg: &str,
) -> Option<std::process::Command> {
    let mut c = match harness {
        "copilot" => {
            let mut c = std::process::Command::new("copilot");
            c.args(["-p", msg, &format!("--resume={id}")]);
            c.args(bn_core::mcp::Harness::Copilot.yolo_args());
            c
        }
        "claude" => {
            let mut c = std::process::Command::new("claude");
            c.args(["-p", msg, "--resume", id, "--output-format", "json"]);
            c.args(bn_core::mcp::Harness::Claude.yolo_args());
            c
        }
        _ => return None,
    };
    c.current_dir(wt);
    Some(c)
}

/// Steer an agent by running one reliable `-p --resume` turn in its worktree and returning the
/// agent's reply. Works for both harnesses and even against a live-held session (verified), unlike
/// send-keys. `None` if the harness is unknown or the process failed. **Blocks** for the whole turn.
pub(crate) fn steer_via_resume(
    harness: &str,
    id: &str,
    wt: &std::path::Path,
    msg: &str,
) -> Option<String> {
    let out = resume_command(harness, id, wt, msg)?.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if harness == "claude" {
        // Structured: pull `.result`; fall back to raw stdout if it isn't the JSON envelope.
        return serde_json::from_str::<serde_json::Value>(&stdout)
            .ok()
            .and_then(|v| v.get("result").and_then(|r| r.as_str()).map(String::from))
            .or(Some(stdout));
    }
    Some(stdout) // copilot: stdout carries the reply (stats go to stderr)
}

/// Fire a `-p --resume` turn **detached** and return immediately (its log path on success). The
/// child runs in its OWN process group (so a killed caller — e.g. a timed-out shell — doesn't take
/// it down) with output redirected to a log file. For long ship tasks: the agent works in the
/// background and reports back via `bn agent done`. `None` for an unknown harness / spawn failure.
pub(crate) fn steer_detached(
    harness: &str,
    id: &str,
    wt: &std::path::Path,
    msg: &str,
) -> Option<std::path::PathBuf> {
    let mut cmd = resume_command(harness, id, wt, msg)?;
    let log = bn_core::resolver::state_dir().join(format!(
        "steer-{}-{}.log",
        harness,
        bn_core::util::now_epoch_secs()
    ));
    if let Some(p) = log.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let out = std::fs::File::create(&log).ok()?;
    let err = out.try_clone().ok()?;
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(out))
        .stderr(std::process::Stdio::from(err));
    // Own process group so the detached turn survives the caller's group being killed.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().ok().map(|_| log)
}

/// Short pause after Escape (popup dismiss) before the submitting carriage return, giving the TUI a
/// moment to close any autocomplete popup so the CR lands on the message line rather than the popup.
pub(crate) const KEYS_SUBMIT_DELAY_MS: u64 = 120;

/// One step of the `--keys` steer sequence: either a `tmux` invocation (the argv *after* `tmux`) or
/// a pause. Split out so [`keys_paste_steps`] / [`keys_submit_steps`] can be pure, unit-testable
/// plans while the executor in [`emit_agent_send`] just runs each step (tmux I/O isn't cleanly
/// testable).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TmuxStep {
    /// Run `tmux <args...>`.
    Cmd(Vec<String>),
    /// Sleep this many milliseconds.
    DelayMs(u64),
}

/// A `repo:branch` target as an exact-match (`=`-prefixed) tmux target. Without `=`, tmux falls
/// back to prefix/fnmatch resolution — and window indices renumber when `wt cleanup` kills a
/// window, so anything loose can land keys in the *wrong agent's* pane. Ids (`%` pane, `@` window,
/// `$` session) and already-exact targets pass through untouched.
pub(crate) fn tmux_exact_target(target: &str) -> String {
    if target.starts_with(['=', '%', '@', '$']) {
        target.to_string()
    } else {
        format!("={target}")
    }
}

/// Build the tmux step plan that primes pane `target`'s input box with `msg` via a paste-buffer
/// named `buf`. Mirrors the seed launcher (`wt::launch_agent_in_window`): stage the message into a
/// buffer with `set-buffer` (a `--` guard so a leading `-` in `msg` isn't parsed as a flag),
/// `paste-buffer -d` it into the pane (one block, so an autosuggest/highlight TUI can't mangle a
/// char-by-char type; `-d` auto-deletes the buffer). Then dismiss any autocomplete popup the pasted
/// `@`/`#` may have opened with `Escape` and pause [`KEYS_SUBMIT_DELAY_MS`]. The submit is a
/// **separate** step ([`keys_submit_steps`]) so the executor can re-send and verify it. The target
/// is normalized to an exact-match name ([`tmux_exact_target`]). Pure: no tmux I/O.
pub(crate) fn keys_paste_steps(target: &str, buf: &str, msg: &str) -> Vec<TmuxStep> {
    let target = &tmux_exact_target(target);
    let cmd = |args: &[&str]| TmuxStep::Cmd(args.iter().map(|s| s.to_string()).collect());
    vec![
        cmd(&["set-buffer", "-b", buf, "--", msg]),
        cmd(&["paste-buffer", "-d", "-b", buf, "-t", target]),
        cmd(&["send-keys", "-t", target, "Escape"]),
        TmuxStep::DelayMs(KEYS_SUBMIT_DELAY_MS),
    ]
}

/// The submit step for a `--keys` steer: a **raw carriage return** via `send-keys -H 0d` (the literal
/// CR byte, 0x0d) rather than the named `Enter` key. Copilot's compose/autocomplete TUI was observed
/// to swallow the named `Enter` — leaving the message unsent in the box — while the raw CR submits
/// (#306). Kept separate + pure so the executor can re-send it under pane verification. The target is
/// normalized to an exact-match name ([`tmux_exact_target`]) so the CR lands in the same pane the
/// paste primed, never a prefix/fnmatch-resolved neighbor.
pub(crate) fn keys_submit_steps(target: &str) -> Vec<TmuxStep> {
    let target = tmux_exact_target(target);
    vec![TmuxStep::Cmd(
        ["send-keys", "-t", &target, "-H", "0d"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    )]
}

/// Print the outcome of a synchronous `-p --resume` steer turn — shared by the primary resume path
/// and the keys-fallback escalation, so both report identically.
pub(crate) fn report_resume(target: &str, harness: &str, msg: &str, reply: &str, as_json: bool) {
    if as_json {
        let data = serde_json::json!({
            "delivered": true, "via": "resume", "target": target,
            "harness": harness, "msg": msg, "reply": reply,
        });
        println!("{}", Envelope::success("agent_send", data).to_json());
    } else {
        println!("→ {target} ({harness}, resume): {msg}");
        if !reply.is_empty() {
            println!("← {reply}");
        }
    }
}

/// Send a message to a running agent. Default: the reliable `-p --resume` channel (harness + session
/// id from the note), which returns the agent's actual reply. `--keys` forces the visible tmux path:
/// it *pastes* the message into the live window via a tmux buffer (see [`keys_paste_steps`]), submits
/// with a raw carriage return (see [`keys_submit_steps`]), and **verifies** the pane picked it up
/// (a working spinner) rather than firing blind — retrying the CR and, if it still can't confirm,
/// escalating to `-p --resume` when a session id is known (#306). Also used as the fallback when no
/// session id is recorded for the target.
/// Kill a running agent by closing its tmux window. Idempotent: a `repo:branch` target with no live
/// window is reported as already gone (not an error), so the web dashboard's kill button and the
/// orchestrator's give-up path can call it without racing the window's own teardown. The target is
/// normalized to an exact-match name so a `kill-window` never resolves a prefix/fnmatch neighbor.
pub(crate) fn emit_agent_kill(target: &str, as_json: bool) {
    let target = target.trim();
    // Guard the shape (`repo:branch`) so a malformed/empty input can't widen the tmux target. The
    // arg is passed to tmux as a distinct argv element (never a shell), so there's no injection —
    // this is purely a "don't kill the wrong thing" check.
    if target.is_empty() || !target.contains(':') {
        let msg = format!("invalid agent target '{target}' (expected repo:branch)");
        if as_json {
            println!(
                "{}",
                Envelope::failure("agent_kill", "invalid_target", &msg).to_json()
            );
        } else {
            eprintln!("bn agent kill: {msg}");
        }
        return;
    }

    // List live windows once so we can distinguish "killed", "already gone" (idempotent success),
    // and "tmux unreachable" (a real failure) rather than treating every non-zero as the same.
    let windows =
        crate::proc::tmux(&["list-windows", "-a", "-F", "#{session_name}:#{window_name}"]);
    let (killed, gone, err) = match windows {
        None => (false, false, Some("tmux unreachable")),
        Some(list) => {
            let present = list.lines().any(|w| w == target);
            if !present {
                (false, true, None) // no such window — already gone
            } else {
                let ok = crate::proc::tmux_ok(&["kill-window", "-t", &tmux_exact_target(target)]);
                if ok {
                    (true, true, None)
                } else {
                    (false, false, Some("kill-window failed"))
                }
            }
        }
    };

    if let Some(e) = err {
        if as_json {
            println!(
                "{}",
                Envelope::failure("agent_kill", "kill_failed", e).to_json()
            );
        } else {
            eprintln!("bn agent kill: {target}: {e}");
        }
        return;
    }

    if as_json {
        let data = serde_json::json!({ "target": target, "killed": killed, "gone": gone });
        println!("{}", Envelope::success("agent_kill", data).to_json());
    } else if killed {
        println!("bn agent kill: {target} — window closed ✓");
    } else {
        println!("bn agent kill: {target} — already gone");
    }
}

/// Capture the last `lines` lines of a running agent's tmux pane (read-only). Powers the web
/// dashboard's live-pane view: the target is addressed exactly (`=repo:branch`) so a capture never
/// resolves a prefix/fnmatch neighbour, tmux's bottom padding of blank lines is trimmed, and the
/// trailing `lines` are returned. Emits a `render::Envelope` so the web API / MCP / CLI share one
/// shape; `--json` carries `{target, lines:[…], count}`. With `ansi`, `capture-pane -e` keeps the
/// pane's SGR escape sequences so a terminal renderer (the web live terminal) shows real colours.
pub(crate) fn emit_agent_pane(target: &str, lines: usize, ansi: bool, as_json: bool) {
    let target = target.trim();
    // Guard the shape (`repo:branch`) so a malformed/empty input can't widen the tmux target. The
    // arg is passed to tmux as a distinct argv element (never a shell), so there's no injection.
    if target.is_empty() || !target.contains(':') {
        let msg = format!("invalid agent target '{target}' (expected repo:branch)");
        if as_json {
            println!(
                "{}",
                Envelope::failure("agent_pane", "invalid_target", &msg).to_json()
            );
        } else {
            eprintln!("bn agent pane: {msg}");
        }
        return;
    }

    let exact = tmux_exact_target(target);
    // `-e` includes SGR (colour/attribute) escapes for a real-terminal render; without it the tail
    // is plain text. `-e` covers only text attributes, not cursor motion, so the frame stays
    // line-oriented (safe to re-render in a resized xterm).
    let mut cap_args: Vec<&str> = vec!["capture-pane", "-p"];
    if ansi {
        cap_args.push("-e");
    }
    cap_args.extend(["-t", &exact]);
    let Some(raw) = crate::proc::tmux_raw(&cap_args) else {
        let msg = "tmux unreachable or no such window";
        if as_json {
            println!(
                "{}",
                Envelope::failure("agent_pane", "capture_failed", msg).to_json()
            );
        } else {
            eprintln!("bn agent pane: {target}: {msg}");
        }
        return;
    };

    let tail = pane_tail(&raw, lines);

    if as_json {
        let data = serde_json::json!({
            "target": target,
            "lines": tail,
            "count": tail.len(),
        });
        println!("{}", Envelope::success("agent_pane", data).to_json());
    } else {
        for l in &tail {
            println!("{l}");
        }
    }
}

/// The last `n` lines of a captured pane: right-trim each line, drop tmux's bottom padding of blank
/// lines, then keep the trailing `n`. Interior blank lines are preserved so the pane's layout
/// survives; only the padded tail is dropped. Pure, so it's unit-testable without tmux.
pub(crate) fn pane_tail(raw: &str, n: usize) -> Vec<String> {
    let mut lines: Vec<String> = raw.lines().map(|l| l.trim_end().to_string()).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let start = lines.len().saturating_sub(n);
    lines[start..].to_vec()
}

pub(crate) fn emit_agent_send(
    cfg: &Config,
    target: &str,
    msg: &str,
    keys: bool,
    detach: bool,
    as_json: bool,
) {
    let session = resolve_agent_session(cfg, target);

    // Reliable path: run a resume turn against the recorded session, unless --keys was asked. Track
    // a failure so a keys fallback that also can't confirm doesn't re-try a resume that already died.
    let mut resume_failed = false;
    if !keys {
        if let Some((wt, harness, id)) = &session {
            // Async: fire the turn detached and return now (for long ship tasks — the agent reports
            // back via `bn agent done`). It survives the caller exiting.
            if detach {
                let log = steer_detached(harness, id, wt, msg);
                let ok = log.is_some();
                if as_json {
                    let data = serde_json::json!({
                        "delivered": ok, "via": "resume-detached", "target": target,
                        "harness": harness, "msg": msg,
                        "log": log.as_ref().map(|p| p.to_string_lossy().into_owned()),
                    });
                    println!("{}", Envelope::success("agent_send", data).to_json());
                } else if ok {
                    println!("→ {target} ({harness}, detached): {msg}");
                    println!(
                        "  running in background — the agent will `bn agent done` when finished"
                    );
                } else {
                    fail(&format!(
                        "bn agent send: could not spawn detached turn for {target}"
                    ));
                }
                return;
            }
            match steer_via_resume(harness, id, wt, msg) {
                Some(reply) => {
                    report_resume(target, harness, msg, &reply, as_json);
                    return;
                }
                None => {
                    // Resume failed (session gone / harness error) — fall through to send-keys.
                    resume_failed = true;
                    eprintln!("bn agent send: resume steer failed for {target}, trying send-keys");
                }
            }
        }
    }

    // Fallback / --keys: deliver into the live tmux window. We *paste* the message via a tmux buffer
    // rather than `send-keys <msg>` char-by-char — an autocomplete/autosuggest TUI (copilot) mangles
    // a typed message and opens an @files/#issues popup that eats an immediate Enter. After the paste
    // we submit with a raw CR and CONFIRM the pane picked it up (a working spinner) rather than
    // firing blind: retry the CR (a first CR often only dismisses the popup), and if we still can't
    // confirm, escalate to the reliable `-p --resume` channel when a session id is known (#306).
    let run_steps = |steps: Vec<TmuxStep>| -> bool {
        for step in steps {
            match step {
                TmuxStep::Cmd(args) => {
                    let ok = proc::tmux_ok(&args);
                    if !ok {
                        return false;
                    }
                }
                TmuxStep::DelayMs(ms) => {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                }
            }
        }
        true
    };
    let buf = format!("bn-steer-{}", bn_core::util::now_epoch_secs());
    let pasted = run_steps(keys_paste_steps(target, &buf, msg));
    let mut confirmed = false;
    if pasted {
        for _ in 0..agent_live::SUBMIT_BACKOFF.submit_attempts() {
            if !run_steps(keys_submit_steps(target)) {
                break;
            }
            if agent_live::steer_landed(target) {
                confirmed = true;
                break;
            }
        }
    }

    // Unconfirmed submit + a known session we haven't already exhausted → escalate to the reliable
    // resume channel. Guarded so the default path (which tried resume first) never re-tries it.
    if !confirmed && !resume_failed {
        if let Some((wt, harness, id)) = &session {
            if let Some(reply) = steer_via_resume(harness, id, wt, msg) {
                report_resume(target, harness, msg, &reply, as_json);
                return;
            }
        }
    }

    if as_json {
        let data = serde_json::json!({
            "delivered": confirmed, "via": "keys", "target": target, "msg": msg,
            "pasted": pasted, "confirmed": confirmed,
        });
        let env = if confirmed {
            Envelope::success("agent_send", data)
        } else if !pasted {
            Envelope::failure(
                "agent_send",
                "send_failed",
                "no such tmux window, or tmux absent",
            )
        } else {
            Envelope::failure(
                "agent_send",
                "unconfirmed_submit",
                "pasted into the input box but could not confirm submission (the TUI may have eaten the Enter); no session id recorded to escalate to --resume",
            )
        };
        println!("{}", env.to_json());
    } else if confirmed {
        println!("→ {target} (keys): {msg}");
    } else if !pasted {
        fail(&format!(
            "bn agent send: couldn't deliver to '{target}' (no recorded session and no tmux window)"
        ));
    } else {
        fail(&format!(
            "bn agent send: pasted into '{target}' but could not confirm submission (the TUI may have eaten the Enter); no session id recorded to escalate to --resume"
        ));
    }
}

/// `bn agent digest` — reconcile the branch's recorded agent sessions into proposed note lines.
/// Reads the note + sessions (read-only), then: `--json` emits the digest and writes nothing;
/// otherwise prints each proposed line and applies it on confirmation (or with `--yes`). Silent,
/// zero-exit when every recorded session is already reflected in the note. Mirrors `emit_sync`.
pub(crate) fn emit_agent_digest(cfg: &Config, ctx: &Resolved, yes: bool, as_json: bool) {
    use bn_core::agent::DigestKind;

    let note = match bn_core::store::read(&cfg.note_file(ctx)) {
        Ok(n) => n,
        Err(_) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::failure("agent_digest", "no_note", "No note for this branch")
                        .to_json()
                );
            } else {
                fail("bn agent digest: no note for this branch");
            }
            return;
        }
    };

    let digest = bn_core::agent::digest(cfg, &note, &ctx.repo, &ctx.branch);

    if as_json {
        // --json is the agent's read-only path: emit the proposals, write nothing.
        println!(
            "{}",
            Envelope::success("agent_digest", payload::agent_digest_data(&digest)).to_json()
        );
        return;
    }

    if digest.is_empty() {
        return; // note already reflects every session — silent, clean exit
    }

    let mut applied = 0usize;
    for p in &digest.proposals {
        let prompt = format!(
            "[{}] {} ({}, {}m, {})",
            p.kind.section(),
            p.text,
            p.harness,
            p.duration_mins,
            p.date
        );
        if !confirm_apply(yes, &prompt) {
            continue;
        }
        let res = match p.kind {
            DigestKind::Progress => {
                commands::append_body(cfg, ctx, "progress", &p.text, &today()).map(|_| ())
            }
            // Decision/todo go to their structured section; `if_absent` keeps re-runs idempotent.
            _ => commands::add(cfg, ctx, p.kind.section(), &p.text, Some(&p.text), &today())
                .map(|_| ()),
        };
        match res {
            Ok(_) => {
                println!("  ✓ {}", p.kind.section());
                applied += 1;
            }
            Err(e) => eprintln!("  ✗ {}: {}", p.kind.section(), e.message()),
        }
    }

    if applied > 0 {
        sync_tmux(cfg, ctx);
    } else {
        println!("Nothing applied.");
    }
}

/// `bn handoff write` — compose the packet from the note + author layers, persist it, and print
/// the orientation (or the JSON envelope).
pub(crate) fn emit_handoff_write(
    cfg: &Config,
    ctx: &Resolved,
    input: &bn_core::handoff::HandoffInput,
    as_json: bool,
) {
    match bn_core::handoff::write(cfg, ctx, &today(), input) {
        Ok(h) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success("handoff", payload::handoff_data(&h)).to_json()
                );
            } else {
                print!("{}", bn_core::handoff::render_orientation(&h));
            }
        }
        Err(e) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::failure("handoff", "no_note", format!("bn handoff: {e}")).to_json()
                );
            } else {
                fail(&format!("bn handoff: {e}"));
            }
        }
    }
}

/// `bn handoff read` — print the session-start orientation from the stored packet, composing one
/// from the note when none has been written yet (so a fresh branch still orients).
pub(crate) fn emit_handoff_read(cfg: &Config, ctx: &Resolved, as_json: bool) {
    let handoff = bn_core::handoff::read(cfg, ctx).or_else(|| {
        bn_core::store::read(&cfg.note_file(ctx))
            .ok()
            .map(|note| bn_core::handoff::compose(&note, &today(), &Default::default()))
    });
    match handoff {
        Some(h) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success("handoff", payload::handoff_data(&h)).to_json()
                );
            } else {
                print!("{}", bn_core::handoff::render_orientation(&h));
            }
        }
        None => {
            if as_json {
                println!(
                    "{}",
                    Envelope::failure("handoff", "no_note", "No note for this branch").to_json()
                );
            } else {
                fail("bn handoff: no note for this branch");
            }
        }
    }
}

/// `bn agent memory-hook` — the Claude Code SessionStart hook, ported from `scripts/wt-memory.sh`.
/// Points this worktree's auto-memory at `<root>/.claude/memory` (so it travels with the worktree)
/// by writing `autoMemoryDirectory` into the gitignored `<root>/.claude/settings.local.json` — the
/// checked-in `settings.json` is ignored for this key by a Claude Code security rule. Idempotent:
/// only rewrites when the value differs. Then prints the branch's handoff packet to orient the
/// session. Best-effort throughout: never fails the hook, and stays quiet when there's no note.
pub(crate) fn emit_memory_hook(cfg: &Config) {
    let root = std::env::var_os("CLAUDE_PROJECT_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            crate::git::git_capture(&["rev-parse", "--show-toplevel"], None, true)
                .map(std::path::PathBuf::from)
        })
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();

    let mem = root.join(".claude").join("memory");
    let settings = root.join(".claude").join("settings.local.json");
    let _ = std::fs::create_dir_all(&mem);
    let mem_str = mem.to_string_lossy().to_string();

    // Merge autoMemoryDirectory into the local settings, preserving any existing keys. Only write
    // when the value actually changes, so we don't churn the file on every session.
    let current = std::fs::read_to_string(&settings).ok();
    if let Some(text) = merge_auto_memory_dir(current.as_deref(), &mem_str) {
        let _ = std::fs::write(&settings, text);
    }

    // Orient the session: print the structured resume packet for this worktree's branch, quietly.
    if let Ok(ctx) = resolver::resolve(cfg, &root) {
        let handoff = bn_core::handoff::read(cfg, &ctx).or_else(|| {
            bn_core::store::read(&cfg.note_file(&ctx))
                .ok()
                .map(|note| bn_core::handoff::compose(&note, &today(), &Default::default()))
        });
        if let Some(h) = handoff {
            print!("{}", bn_core::handoff::render_orientation(&h));
        }
    }
}

/// Merge `autoMemoryDirectory = <mem>` into the `settings.local.json` body, preserving every other
/// key. Returns the new file text to write, or `None` when the value is already set (so the caller
/// skips the write and doesn't churn the file). Unparseable/absent input starts from `{}`.
fn merge_auto_memory_dir(current: Option<&str>, mem: &str) -> Option<String> {
    let parsed = current.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
    if parsed
        .as_ref()
        .and_then(|v| v.get("autoMemoryDirectory"))
        .and_then(|v| v.as_str())
        == Some(mem)
    {
        return None;
    }
    let mut obj = match parsed {
        Some(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    obj.insert(
        "autoMemoryDirectory".to_string(),
        serde_json::Value::String(mem.to_string()),
    );
    serde_json::to_string_pretty(&serde_json::Value::Object(obj))
        .ok()
        .map(|t| format!("{t}\n"))
}

// --- bn agent launch (ports workflow/bin/agent) ----------------------------

/// The two AI coding harnesses `bn agent launch` can drive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Harness {
    Copilot,
    Claude,
}

impl Harness {
    /// Parse the `[copilot|claude]` positional (default copilot). `None` on an unknown value so the
    /// caller can error with exit 2, mirroring the zsh launcher's `unknown agent` guard.
    fn parse(s: Option<&str>) -> Option<Harness> {
        match s.unwrap_or("copilot") {
            "copilot" => Some(Harness::Copilot),
            "claude" => Some(Harness::Claude),
            _ => None,
        }
    }
    /// The harness binary name (resolved via PATH at exec time).
    fn bin(self) -> &'static str {
        match self {
            Harness::Copilot => "copilot",
            Harness::Claude => "claude",
        }
    }
}

/// The resolved launch inputs shared by both harnesses' arg builders. `seed` is already
/// materialised (inline `--seed` or the contents of `--seed-file`, whichever resolved).
pub(crate) struct LaunchPlan {
    pub(crate) seed: Option<String>,
    pub(crate) yolo_force: bool,
    pub(crate) fresh: bool,
    pub(crate) session: Option<String>,
    pub(crate) resume_session: Option<String>,
    pub(crate) passthrough: Vec<String>,
}

impl LaunchPlan {
    /// "yolo mode" (skip approval prompts) applies to an *interactive* launch (no seed) OR when the
    /// caller explicitly opts in with `--yolo`. SECURITY: a seed may embed attacker-influenceable
    /// content, so a seeded launch stays gated unless the caller (who owns the trust decision)
    /// passes `--yolo`. Matches the zsh launcher.
    fn yolo(&self) -> bool {
        self.seed.is_none() || self.yolo_force
    }
    /// `--session-id <sid>` when a session is pinned, else empty (let the harness pick its own).
    fn session_flag(&self) -> Vec<String> {
        match &self.session {
            Some(s) => vec!["--session-id".to_string(), s.clone()],
            None => Vec::new(),
        }
    }
}

fn controller_launch() -> bool {
    let controller_id = std::env::var("BN_CONTROLLER_ID").ok();
    let worker_session_id = std::env::var("BN_WORKER_SESSION_ID").ok();
    controller_launch_env(controller_id.as_deref(), worker_session_id.as_deref())
}

fn controller_launch_env(controller_id: Option<&str>, worker_session_id: Option<&str>) -> bool {
    controller_id.is_some_and(|value| !value.trim().is_empty())
        && worker_session_id.is_none_or(|value| value.trim().is_empty())
}

fn controller_turn_end_hook_command() -> String {
    let executable = std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "bn".to_string());
    #[cfg(windows)]
    {
        format!(
            "& '{}' control turn-end-guard",
            executable.replace('\'', "''")
        )
    }
    #[cfg(not(windows))]
    {
        format!(
            "{} control turn-end-guard",
            wt::shell_single_quote(&executable)
        )
    }
}

fn controller_hook_entry() -> serde_json::Value {
    serde_json::json!({
        "type": "command",
        "command": controller_turn_end_hook_command(),
        "timeout": 15,
        "timeoutSec": 15,
    })
}

fn copilot_controller_hook_config() -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "hooks": {
            "agentStop": [controller_hook_entry()],
        },
    })
}

fn claude_controller_settings() -> String {
    serde_json::json!({
        "hooks": {
            "Stop": [{
                "hooks": [controller_hook_entry()],
            }],
        },
    })
    .to_string()
}

fn with_claude_controller_hook(mut args: Vec<String>, enabled: bool) -> Vec<String> {
    if enabled {
        args.splice(
            0..0,
            ["--settings".to_string(), claude_controller_settings()],
        );
    }
    args
}

fn copilot_hooks_dir() -> std::io::Result<std::path::PathBuf> {
    if let Some(home) = std::env::var_os("COPILOT_HOME") {
        return Ok(std::path::PathBuf::from(home).join("hooks"));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(std::path::PathBuf::from(home)
        .join(".copilot")
        .join("hooks"))
}

fn install_copilot_controller_hook_at(hooks_dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(hooks_dir)?;
    let path = hooks_dir.join("bn-controller.json");
    let body = serde_json::to_vec_pretty(&copilot_controller_hook_config())
        .map_err(std::io::Error::other)?;
    if std::fs::read(&path).ok().as_deref() == Some(body.as_slice()) {
        return Ok(());
    }
    let temp_path = hooks_dir.join(format!(".bn-controller.{}.tmp", gen_uuid()));
    let mut temp = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    if let Err(error) = std::io::Write::write_all(&mut temp, &body)
        .and_then(|()| temp.sync_all())
        .and_then(|()| std::fs::rename(&temp_path, &path))
    {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(())
}

fn install_copilot_controller_hook() -> std::io::Result<()> {
    install_copilot_controller_hook_at(&copilot_hooks_dir()?)
}

/// The Claude project-transcript dirname for `cwd`: every non-alphanumeric byte becomes `-`
/// (matching the zsh `sed 's/[^a-zA-Z0-9]/-/g'`). Transcripts live under
/// `~/.claude/projects/<this>/`.
pub(crate) fn claude_project_dirname(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Pick the newest recorded Copilot session id for `cwd` from `(session_cwd, updated_at, id)` rows.
/// ISO-8601 `updated_at` sorts lexically, so the string max is the newest. Pure — the IO wrapper
/// [`copilot_resume_id`] scans the `workspace.yaml` files and feeds it here. Empty ids are ignored.
pub(crate) fn pick_copilot_resume<'a>(
    rows: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
    cwd: &str,
) -> Option<String> {
    let mut best: Option<(&str, &str)> = None; // (updated_at, id)
    for (c, ts, id) in rows {
        if c != cwd || id.is_empty() {
            continue;
        }
        if best.is_none_or(|(bts, _)| ts > bts) {
            best = Some((ts, id));
        }
    }
    best.map(|(_, id)| id.to_string())
}

/// Build the argv (after the harness binary) for a **Claude** launch. Resumes with `--continue`
/// when this worktree has transcripts (unless `--fresh`); otherwise starts fresh, pinning the
/// session id and passing the seed as the initial prompt. Pure — mirrors the zsh `claude` branch.
pub(crate) fn build_claude_args(plan: &LaunchPlan, has_transcripts: bool) -> Vec<String> {
    let mut args = Vec::new();
    if plan.yolo() {
        args.extend(
            bn_core::mcp::Harness::Claude
                .yolo_args()
                .iter()
                .map(|s| s.to_string()),
        );
    }
    if !plan.fresh {
        if let Some(id) = &plan.resume_session {
            args.extend(["--resume".to_string(), id.clone()]);
            args.extend(plan.passthrough.iter().cloned());
            return args;
        }
    }
    if !plan.fresh && has_transcripts {
        args.push("--continue".to_string()); // resume: session already holds context
        args.extend(plan.passthrough.iter().cloned());
        return args;
    }
    args.extend(plan.session_flag());
    if let Some(seed) = &plan.seed {
        args.push(seed.clone()); // fresh: the seed is Claude's initial prompt (positional)
    }
    args.extend(plan.passthrough.iter().cloned());
    args
}

/// Build the argv (after the harness binary) for a **Copilot** launch. Resumes the resolved session
/// id with `--resume=` when one exists (unless `--fresh`); otherwise starts fresh, pinning the
/// session id and seeding the first turn with `-i <seed>`. Pure — mirrors the zsh `copilot` branch.
pub(crate) fn build_copilot_args(plan: &LaunchPlan, resume_id: Option<&str>) -> Vec<String> {
    let mut args = Vec::new();
    if plan.yolo() {
        args.extend(
            bn_core::mcp::Harness::Copilot
                .yolo_args()
                .iter()
                .map(|s| s.to_string()),
        );
    }
    if !plan.fresh {
        if let Some(rid) = resume_id {
            args.push(format!("--resume={rid}"));
            args.extend(plan.passthrough.iter().cloned());
            return args;
        }
    }
    args.extend(plan.session_flag());
    if let Some(seed) = &plan.seed {
        args.push("-i".to_string()); // copilot "start interactive and run this prompt"
        args.push(seed.clone());
    }
    args.extend(plan.passthrough.iter().cloned());
    args
}

const COPILOT_MISSING_SESSION_ERROR: &str = "No session, task, or name matched";

/// Run a Copilot resume attempt while teeing stderr to the pane and retaining enough output to
/// distinguish a missing session from unrelated failures. Only resume attempts use this wrapper;
/// fresh launches still `exec` directly so there can be at most one fallback.
fn run_copilot_resume(
    bin: &str,
    args: &[String],
) -> std::io::Result<(std::process::ExitStatus, String)> {
    use std::io::{Read, Write};
    use std::process::Stdio;

    const CAPTURE_LIMIT: usize = 64 * 1024;

    let mut child = crate::retry::io(&format!("bn agent launch: spawn {bin}"), || {
        std::process::Command::new(bin)
            .args(args)
            .stderr(Stdio::piped())
            .spawn()
    })?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("Copilot resume stderr pipe was not available"))?;
    let reader = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut captured = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = stderr.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            std::io::stderr().write_all(&chunk[..read])?;
            let remaining = CAPTURE_LIMIT.saturating_sub(captured.len());
            captured.extend_from_slice(&chunk[..read.min(remaining)]);
        }
        Ok(captured)
    });

    let status = child.wait()?;
    let captured = reader
        .join()
        .map_err(|_| std::io::Error::other("Copilot resume stderr reader panicked"))??;
    Ok((status, String::from_utf8_lossy(&captured).into_owned()))
}

fn copilot_session_missing(stderr: &str) -> bool {
    stderr.contains(COPILOT_MISSING_SESSION_ERROR)
}

fn should_retry_copilot_resume(status: std::process::ExitStatus, stderr: &str) -> bool {
    !status.success() && copilot_session_missing(stderr)
}

/// The most-recent Copilot session id recorded for `cwd`, by scanning
/// `~/.copilot/session-state/*/workspace.yaml`. IO wrapper over [`pick_copilot_resume`].
fn copilot_resume_id(cwd: &str) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let dir = std::path::Path::new(&home)
        .join(".copilot")
        .join("session-state");
    let mut rows: Vec<(String, String, String)> = Vec::new();
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let wy = entry.path().join("workspace.yaml");
        let Ok(body) = std::fs::read_to_string(&wy) else {
            continue;
        };
        let field = |key: &str| {
            body.lines()
                .find_map(|l| l.trim().strip_prefix(key))
                .map(|v| v.trim().to_string())
                .unwrap_or_default()
        };
        rows.push((field("cwd:"), field("updated_at:"), field("id:")));
    }
    pick_copilot_resume(
        rows.iter()
            .map(|(c, t, i)| (c.as_str(), t.as_str(), i.as_str())),
        cwd,
    )
}

/// Whether this worktree has any recorded Claude transcript (`~/.claude/projects/<proj>/*.jsonl`).
fn claude_has_transcripts(cwd: &str) -> bool {
    let Ok(home) = std::env::var("HOME") else {
        return false;
    };
    let proj = claude_project_dirname(cwd);
    let dir = std::path::Path::new(&home)
        .join(".claude")
        .join("projects")
        .join(proj);
    std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .any(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        })
        .unwrap_or(false)
}

fn claude_has_session(cwd: &str, session_id: &str) -> bool {
    let Ok(home) = std::env::var("HOME") else {
        return false;
    };
    std::path::Path::new(&home)
        .join(".claude")
        .join("projects")
        .join(claude_project_dirname(cwd))
        .join(format!("{session_id}.jsonl"))
        .is_file()
}

/// `bn agent launch` — the native port of `workflow/bin/agent`. Resolves the seed, pre-trusts the
/// worktree for Copilot, computes the resume-or-fresh argv, and **execs** the harness (this process
/// is replaced; on success it never returns). A Copilot resume whose session no longer exists is
/// retried exactly once as a fresh launch. On an unknown harness it exits 2; on exec failure it
/// prints the error and exits 127, mirroring a failed `exec` in the shell launcher.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_agent_launch(
    harness: Option<String>,
    seed: Option<String>,
    seed_file: Option<String>,
    yolo: bool,
    fresh: bool,
    session: Option<String>,
    resume_session: Option<String>,
    passthrough: Vec<String>,
) -> ! {
    let Some(harness) = Harness::parse(harness.as_deref()) else {
        eprintln!(
            "bn agent launch: unknown agent '{}' (use: claude | copilot)",
            harness.unwrap_or_default()
        );
        std::process::exit(2);
    };

    // Seed resolution: inline --seed wins; else read --seed-file (a missing/unreadable file
    // degrades to unseeded, since a relaunch resumes anyway and the seed would be unused).
    let seed = seed.or_else(|| seed_file.and_then(|f| std::fs::read_to_string(&f).ok()));

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut plan = LaunchPlan {
        seed,
        yolo_force: yolo,
        fresh,
        session,
        resume_session,
        passthrough,
    };

    match harness {
        Harness::Claude => {
            if !plan.fresh
                && plan
                    .resume_session
                    .as_deref()
                    .is_some_and(|id| !claude_has_session(&cwd, id))
            {
                eprintln!("bn agent launch: recorded Claude session is missing; starting fresh");
                plan.fresh = true;
            }
            let args = with_claude_controller_hook(
                build_claude_args(&plan, claude_has_transcripts(&cwd)),
                controller_launch(),
            );
            exec_harness(harness.bin(), &args)
        }
        Harness::Copilot => {
            if controller_launch() {
                install_copilot_controller_hook().unwrap_or_else(|error| {
                    fail(&format!(
                        "bn agent launch: install controller turn-end hook: {error}"
                    ))
                });
            }
            // Never block a fresh launch on the folder-trust dialog — pre-trust this worktree
            // (#658). Best-effort; reuses `bn trust-folder`'s config writer. Ensure copilot's
            // config dir exists first so the trust write lands even on a first-ever launch (the
            // zsh launcher silenced any failure here; we make it succeed instead).
            if let Ok(home) = std::env::var("HOME") {
                let _ = std::fs::create_dir_all(std::path::Path::new(&home).join(".copilot"));
            }
            crate::cmd::promote::apply_folder_trust(&cwd, "copilot");
            let resume_id = plan
                .resume_session
                .clone()
                .or_else(|| copilot_resume_id(&cwd));
            let args = build_copilot_args(&plan, resume_id.as_deref());
            if resume_id.is_some() && !plan.fresh {
                match run_copilot_resume(harness.bin(), &args) {
                    Ok((status, stderr)) if should_retry_copilot_resume(status, &stderr) => {
                        eprintln!(
                            "bn agent launch: recorded Copilot session is missing; retrying fresh"
                        );
                        plan.fresh = true;
                        let fresh_args = build_copilot_args(&plan, None);
                        exec_harness(harness.bin(), &fresh_args)
                    }
                    Ok((status, _)) => std::process::exit(status.code().unwrap_or(1)),
                    Err(err) => {
                        eprintln!("bn agent launch: exec {}: {err}", harness.bin());
                        std::process::exit(127);
                    }
                }
            }
            exec_harness(harness.bin(), &args)
        }
    }
}

/// Replace this process with `bin <args…>`. On Unix this is a real `execvp` (so the harness inherits
/// the tty/pane exactly as the zsh `exec` did); elsewhere we spawn, wait, and propagate the status.
/// Only returns via `std::process::exit` — never normally.
fn exec_harness(bin: &str, args: &[String]) -> ! {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // exec() only returns on failure, so rebuilding the command is safe between attempts.
        let err = crate::retry::io(&format!("bn agent launch: exec {bin}"), || {
            Err::<(), _>(std::process::Command::new(bin).args(args).exec())
        })
        .expect_err("exec only returns on failure");
        eprintln!("bn agent launch: exec {bin}: {err}");
        std::process::exit(127);
    }
    #[cfg(not(unix))]
    {
        match crate::retry::io(&format!("bn agent launch: spawn {bin}"), || {
            std::process::Command::new(bin).args(args).status()
        }) {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(err) => {
                eprintln!("bn agent launch: exec {bin}: {err}");
                std::process::exit(127);
            }
        }
    }
}

/// orientation plus the command to resume in the target harness. The continuity one-shot for
/// hopping harnesses mid-task (e.g. Claude hit a limit → finish in Copilot). With `in_place`,
/// the swap is performed rather than printed: trust is granted and the worktree's tmux pane is
/// respawned running the target harness (shell-backed), seeded to read the handoff.
pub(crate) fn emit_switch(
    cfg: &Config,
    ctx: &Resolved,
    harness: Option<String>,
    prompt: Option<String>,
    in_place: bool,
    reason: Option<String>,
    as_json: bool,
) {
    // Fold any freshly-finished sessions into metrics so the finished-session fallback is current
    // (idempotent; a live session is only read for the summary below, never ingested here).
    let _ = bn_core::agent::ingest(cfg);

    // The session we're abandoning: a live one on this branch wins (it's the one you're leaving),
    // else the most recently recorded. Resolved before `from`/`to` so we can name the harness being
    // taken over from truthfully.
    let outgoing = live_outgoing(cfg, ctx).or_else(|| {
        bn_core::agent::latest_session_for(cfg, &ctx.repo, &ctx.branch).map(|s| {
            bn_core::agent::SwitchOutgoing::from_session(&s, bn_core::util::now_epoch_secs())
        })
    });

    // Who we're leaving: the outgoing session's harness when one was resolved — the truthful answer
    // even when targeting another branch via --repo/--branch, where the harness *driving* bn is an
    // orchestrator, not the session being taken over. `detect_harness()` is only the fallback for
    // the bare in-worktree case with no outgoing session. `from` also seeds the default target
    // (switch to the *other* harness), so deriving it from the outgoing session fixes that too.
    let from = outgoing
        .as_ref()
        .map(|o| o.harness.clone())
        .or_else(bn_core::util::detect_harness);
    let to = match resolve_switch_target(harness.as_deref(), from.as_deref(), cfg) {
        Ok(t) => t,
        Err(msg) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::failure("switch", "bad_harness", msg).to_json()
                );
            } else {
                fail(&msg);
            }
            return;
        }
    };

    // Capture the session's *content*, not just its counts: the uncommitted worktree diff (what
    // was done) and its last user prompt (what was asked). Both fold forward so the incoming
    // harness can actually finish the work.
    let wt = cfg.worktree_dir.join(&ctx.repo).join(&ctx.branch);
    let wip = git_wip(&wt);
    let last_request = outgoing
        .as_ref()
        .and_then(|o| bn_core::agent::last_request_for(&o.session_id, &o.harness));

    // Fold the WIP into the note so the handoff (composed from the note) carries what was done:
    // a progress bullet (deduped against an identical line) and a review-and-commit todo.
    if let Some(wc) = &wip {
        let line = bn_core::agent::wip_progress_line(wc);
        let already = bn_core::store::read(&cfg.note_file(ctx))
            .map(|n| n.body.contains(&line))
            .unwrap_or(false);
        if !already {
            let _ = commands::append_body(cfg, ctx, "progress", &line, &today());
        }
        let todo = "Review & commit the WIP left when switching harnesses";
        let _ = commands::add(cfg, ctx, "todo", todo, Some(todo), &today());
    }

    // Compose + persist the handoff from the (now WIP-aware) note plus the switch narrative — the
    // author layer an agent would otherwise write by hand at session end.
    let narrative = bn_core::agent::switch_narrative(
        &to,
        outgoing.as_ref(),
        wip.as_ref(),
        last_request.as_deref(),
    );
    let input = bn_core::handoff::HandoffInput {
        narrative: Some(narrative),
        ..Default::default()
    };
    let handoff = match bn_core::handoff::write(cfg, ctx, &today(), &input) {
        Ok(h) => h,
        Err(e) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::failure("switch", "no_note", format!("bn switch: {e}")).to_json()
                );
            } else {
                fail(&format!("bn switch: {e}"));
            }
            return;
        }
    };

    // Record the switch to the synced metrics log. Every switch path lands here (`bn switch`, the
    // fleet auto-switch #307, orchestrator relaunch), so this is the single chokepoint that makes
    // harness-hop frequency visible in `bn metrics` (#326). Emitted *before* the in-place swap,
    // which respawns (and may kill) this very process — code after it may never run. Best-effort:
    // metrics are observability, not correctness, so a write error is swallowed. `from` is omitted
    // when unknown (a window-only outgoing row); `reason` defaults to a manual switch.
    {
        let mut fields = serde_json::Map::new();
        if let Some(f) = &from {
            fields.insert("from".into(), serde_json::json!(f));
        }
        fields.insert("to".into(), serde_json::json!(to));
        fields.insert(
            "reason".into(),
            serde_json::json!(reason.as_deref().unwrap_or("manual")),
        );
        fields.insert("repo".into(), serde_json::json!(ctx.repo));
        fields.insert("branch".into(), serde_json::json!(ctx.branch));
        fields.insert("machine".into(), serde_json::json!(cfg.machine()));
        let _ = bn_core::events::append_metric_at(
            &cfg.metrics_log_path(),
            "harness_switch",
            bn_core::util::now_epoch_secs(),
            fields,
        );
    }

    // The command to start the target harness here, seeded to load the packet first. File-based:
    // this command is often *typed into a pane* by an orchestrator (send-keys), where a long
    // inline prompt can truncate mid-string — park the prompt in a file so the typed command
    // stays short. Inline only if the temp write fails.
    //
    // yolo=true: a `switch` resumes YOUR own work in YOUR worktree — a trusted, manual action (the
    // same trust as an interactive launch, which already yolos), so the resumed harness runs
    // approval-free rather than prompting "allow?" for every tool. Folder trust is granted above.
    let resume_prompt = prompt.unwrap_or_else(|| default_resume_prompt(ctx, from.as_deref()));
    let launch = match wt::prompt_to_file("switch", &ctx.repo, &ctx.branch, &resume_prompt) {
        Some(f) => wt::harness_cmd_from_file(&to, &f, true, None),
        None => wt::harness_cmd(&to, &resume_prompt, true, None),
    };

    // --in-place: actually perform the swap. Runs before any output on purpose — when bn is
    // invoked from inside the pane being respawned, `respawn-pane -k` kills bn itself, so the
    // handoff and trust must already be on disk (they are, above) and any output after this
    // point may never print. In that self-swap case the user's evidence of success is the new
    // harness booting in their pane.
    let swap = in_place.then(|| run_in_place_swap(ctx, &wt, &to, &launch));

    if as_json {
        let mut data = serde_json::json!({
            "from": from,
            "to": to,
            "outgoing": outgoing,
            "wip": wip,
            "last_request": last_request,
            "launch_command": launch,
            "handoff": payload::handoff_data(&handoff),
        });
        if let Some(s) = &swap {
            data["in_place"] = serde_json::json!({
                "pane": s.pane,
                "trusted": s.trusted,
                "error": s.error,
            });
        }
        println!("{}", Envelope::success("switch", data).to_json());
        return;
    }

    print!("{}", bn_core::handoff::render_orientation(&handoff));
    match &swap {
        Some(SwapOutcome { pane: Some(p), .. }) => {
            println!("\nSwapped in place: {to} relaunched in pane {p} (shell-backed), seeded to read the handoff.");
        }
        Some(SwapOutcome {
            error: Some(why), ..
        }) => {
            eprintln!("bn switch --in-place: {why}");
            println!("\nStart {to} here:\n  {launch}");
        }
        _ => println!("\nStart {to} here:\n  {launch}"),
    }
}

/// `bn fleet --fix` — the recovery arm of the fleet board (#307): find every agent sitting at its
/// harness's usage-limit prompt (`state == "limited"`, inferred from the live pane) and switch
/// each to the *other* harness in place, so the work continues instead of waiting for the limit
/// to reset. Each switch is the full `bn switch --in-place`: handoff written, trust granted, pane
/// respawned. The target harness is the opposite of the limited agent's own; when the harness is
/// unknown (a window-only row), `emit_switch` derives it from the outgoing session as usual.
pub(crate) fn run_fleet_fix(cfg: &Config, idle: i64, stuck: i64, as_json: bool) {
    let limited: Vec<_> = agent_live::collect(cfg, idle, stuck)
        .into_iter()
        .filter(|r| r.state == "limited")
        .collect();
    if limited.is_empty() {
        if as_json {
            println!(
                "{}",
                Envelope::success("fleet_fix", serde_json::json!({ "switched": [] })).to_json()
            );
        } else {
            println!("bn fleet --fix: no usage-limited agents.");
        }
        return;
    }
    for r in &limited {
        let to = match r.harness.as_deref() {
            Some("claude") => Some("copilot".to_string()),
            Some("copilot") => Some("claude".to_string()),
            _ => None,
        };
        if !as_json {
            println!(
                "\n{} hit a usage limit — switching to {} in place…",
                r.target,
                to.as_deref().unwrap_or("the other harness")
            );
        }
        let Some(ctx) = target_ctx(cfg, Some(r.repo.clone()), Some(r.branch.clone())) else {
            eprintln!("bn fleet --fix: could not resolve {} — skipped", r.target);
            continue;
        };
        // The fleet auto-switch is always rate-limit driven — the agent hit its usage limit.
        emit_switch(
            cfg,
            &ctx,
            to,
            None,
            true,
            Some("limit".to_string()),
            as_json,
        );
    }
}

/// Outcome of `bn switch --in-place`: the pane the target harness was respawned into (`None`
/// when the swap fell back), which harness configs got folder trust, and why it fell back.
pub(crate) struct SwapOutcome {
    pane: Option<String>,
    trusted: Vec<&'static str>,
    error: Option<String>,
}

/// Perform the in-place harness swap for `ctx`'s worktree: grant `to` folder trust for the
/// worktree, then respawn its tmux pane with `launch` shell-backed (the pane drops to a shell
/// when the harness exits, so a crash leaves an inspectable pane instead of a dead window).
/// Targets the dispatch-layout window `=<repo>:<branch>`, else — when bn itself runs inside the
/// worktree — the calling pane (`$TMUX_PANE`). Never errors hard: a missing tmux target is
/// reported in the outcome and the caller falls back to printing the launch command.
pub(crate) fn run_in_place_swap(
    ctx: &Resolved,
    wt: &std::path::Path,
    to: &str,
    launch: &str,
) -> SwapOutcome {
    let trusted = apply_folder_trust(&wt.to_string_lossy(), to);
    // First pane of the worktree's window under the session-per-repo layout, if it exists.
    let window_pane = proc::tmux(&[
        "list-panes",
        "-t",
        &format!("={}:{}", ctx.repo, ctx.branch),
        "-F",
        "#{pane_id}",
    ])
    .and_then(|out| out.lines().next().map(|l| l.trim().to_string()))
    .filter(|p| !p.is_empty());
    // $TMUX_PANE is only a valid target when bn is running *inside* this worktree — from an
    // orchestrator window it would respawn the orchestrator's own pane.
    let cwd_in_wt = std::env::current_dir()
        .map(|d| d.starts_with(wt))
        .unwrap_or(false);
    let own_pane = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty());
    match swap_pane_target(ctx, window_pane, own_pane, cwd_in_wt) {
        Ok(pane) => {
            let args = swap_respawn_args(&pane, &wt.to_string_lossy(), launch);
            if proc::tmux_run(&args) {
                SwapOutcome {
                    pane: Some(pane),
                    trusted,
                    error: None,
                }
            } else {
                SwapOutcome {
                    pane: None,
                    trusted,
                    error: Some(format!("tmux respawn-pane failed for {pane}")),
                }
            }
        }
        Err(why) => SwapOutcome {
            pane: None,
            trusted,
            error: Some(why),
        },
    }
}

/// The tmux pane an in-place swap should respawn: the worktree's window pane when one exists,
/// else the calling pane — but only when bn is running inside the worktree, so a swap driven
/// from an orchestrator window can never respawn the orchestrator itself. Pure → unit-testable.
pub(crate) fn swap_pane_target(
    ctx: &Resolved,
    window_pane: Option<String>,
    own_pane: Option<String>,
    cwd_in_worktree: bool,
) -> Result<String, String> {
    if let Some(p) = window_pane {
        return Ok(p);
    }
    match own_pane {
        Some(p) if cwd_in_worktree => Ok(p),
        _ => Err(format!(
            "no tmux window for {}:{} (and not running inside its worktree)",
            ctx.repo, ctx.branch
        )),
    }
}

/// The tmux argv that swaps a pane's contents for the target harness: `respawn-pane -k` replaces
/// whatever is (or was) running, `-c` roots it in the worktree, and the command is suffixed with
/// `exec "${SHELL:-sh}"` so the pane is shell-backed — a harness exit or crash drops to a live
/// shell holding its output instead of tearing the pane down. Pure → unit-testable.
pub(crate) fn swap_respawn_args(pane: &str, wt: &str, launch: &str) -> Vec<String> {
    vec![
        "respawn-pane".into(),
        "-k".into(),
        "-t".into(),
        pane.into(),
        "-c".into(),
        wt.into(),
        format!("{launch}; exec \"${{SHELL:-sh}}\""),
    ]
}

/// The uncommitted worktree changes at switch time — tracked diff vs `HEAD` (numstat) plus any
/// untracked files. `None` when the tree is clean or `wt` isn't a readable git worktree. This is
/// the concrete "what was done" the switch folds into the note, read from git so it's accurate
/// regardless of which harness ran (Claude transcripts carry no diff stats).
pub(crate) fn git_wip(wt: &std::path::Path) -> Option<bn_core::agent::WorkingChanges> {
    let git = |args: &[&str]| crate::git::git_capture(args, Some(wt), false);
    let basename = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();

    let (mut ins, mut del) = (0i64, 0i64);
    let mut paths: Vec<String> = Vec::new();
    for line in git(&["--no-pager", "diff", "--numstat", "HEAD"])?.lines() {
        let mut cols = line.split('\t');
        let a = cols.next().unwrap_or("");
        let b = cols.next().unwrap_or("");
        let path = cols.next().unwrap_or("").trim();
        if path.is_empty() {
            continue;
        }
        ins += a.parse::<i64>().unwrap_or(0); // "-" for binary files → 0
        del += b.parse::<i64>().unwrap_or(0);
        paths.push(basename(path));
    }
    if let Some(untracked) = git(&["ls-files", "--others", "--exclude-standard"]) {
        for line in untracked.lines() {
            let line = line.trim();
            if !line.is_empty() {
                paths.push(basename(line));
            }
        }
    }
    if paths.is_empty() {
        return None;
    }
    let files = paths.len();
    let sample = paths.into_iter().take(3).collect();
    Some(bn_core::agent::WorkingChanges {
        files,
        insertions: ins,
        deletions: del,
        sample,
    })
}

/// Resolve `bn switch`'s target harness: an explicit arg (validated), else the opposite of the
/// harness driving bn, else the review harness, else copilot. Errors on an unknown explicit arg.
pub(crate) fn resolve_switch_target(
    arg: Option<&str>,
    from: Option<&str>,
    cfg: &Config,
) -> Result<String, String> {
    Ok(switch_target_or_default(arg, from)?.unwrap_or_else(|| wt::review_harness_name(cfg)))
}

/// The target harness before the config fallback: `Ok(Some(h))` for an explicit/derivable target,
/// `Ok(None)` when it must fall back to config, `Err` for an unknown explicit arg. Pure — the
/// testable core of [`resolve_switch_target`].
pub(crate) fn switch_target_or_default(
    arg: Option<&str>,
    from: Option<&str>,
) -> Result<Option<String>, String> {
    if let Some(h) = arg {
        return match h {
            "claude" | "copilot" => Ok(Some(h.to_string())),
            other => Err(format!(
                "bn switch: unknown harness '{other}' — use 'claude' or 'copilot'"
            )),
        };
    }
    Ok(match from {
        Some("claude") => Some("copilot".to_string()),
        Some("copilot") => Some("claude".to_string()),
        _ => None,
    })
}

/// The live session on this branch, if one is running — the session `bn switch` is leaving. A
/// full-fs supervise pass (one-shot; switch isn't a loop), filtered to this note key.
pub(crate) fn live_outgoing(
    cfg: &Config,
    ctx: &Resolved,
) -> Option<bn_core::agent::SwitchOutgoing> {
    let fleet = bn_core::agent::supervise(
        cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    );
    fleet
        .agents
        .iter()
        .find(|a| a.repo == ctx.repo && a.branch == ctx.branch)
        .map(bn_core::agent::SwitchOutgoing::from_live)
}

/// The default first prompt for the incoming harness: load the resume packet, then continue.
pub(crate) fn default_resume_prompt(ctx: &Resolved, from: Option<&str>) -> String {
    let switched = match from {
        Some(f) => format!(" after switching from {f}"),
        None => String::new(),
    };
    format!(
        "Resuming {}/{}{switched}. First run `bn handoff read` (and `bn cat`) to load the resume packet, then work the priority queue.",
        ctx.repo, ctx.branch
    )
}

pub(crate) fn emit_agent_sessions(
    cfg: &Config,
    repo: Option<String>,
    branch: Option<String>,
    as_json: bool,
) {
    let ctx = target_ctx(cfg, repo, branch).unwrap_or_else(|| fail("Not in a git repo"));
    let sessions = bn_core::agent::sessions_for(cfg, &ctx.repo, &ctx.branch);
    if as_json {
        println!(
            "{}",
            Envelope::success("agent_sessions", payload::agent_sessions_data(&sessions)).to_json()
        );
        return;
    }
    if sessions.is_empty() {
        println!("{}/{}: no agent sessions recorded", ctx.repo, ctx.branch);
        return;
    }
    println!(
        "{}/{} — {} agent sessions",
        ctx.repo,
        ctx.branch,
        sessions.len()
    );
    for s in &sessions {
        let model = s.models.first().map(String::as_str).unwrap_or("—");
        let cost = s
            .cost_nanoaiu
            .map(|c| format!("{:.2} AIU", c as f64 / 1e9))
            .unwrap_or_else(|| "—".into());
        println!(
            "  {}  {:<7} {:<18} {:>3}m · {} turns · {} tools · +{}/-{}  {}",
            bn_core::util::date_of(s.started_ts),
            s.harness,
            model,
            s.duration_secs() / 60,
            s.turns,
            s.tool_calls,
            s.lines_added,
            s.lines_removed,
            cost,
        );
    }
}

#[cfg(test)]
mod memory_hook_tests {
    use super::merge_auto_memory_dir;

    #[test]
    fn writes_from_empty_when_absent_or_unparseable() {
        for input in [None, Some("not json"), Some("")] {
            let out = merge_auto_memory_dir(input, "/w/.claude/memory").expect("should write");
            assert!(out.contains("\"autoMemoryDirectory\": \"/w/.claude/memory\""));
            assert!(out.ends_with('\n'));
        }
    }

    #[test]
    fn preserves_existing_keys_and_overwrites_stale_dir() {
        let input = r#"{"model":"opus","autoMemoryDirectory":"/old/mem"}"#;
        let out = merge_auto_memory_dir(Some(input), "/new/mem").expect("should rewrite");
        assert!(out.contains("\"model\": \"opus\""), "keeps unrelated keys");
        assert!(out.contains("\"autoMemoryDirectory\": \"/new/mem\""));
        assert!(!out.contains("/old/mem"));
    }

    #[test]
    fn idempotent_when_already_pointing_here() {
        let input = r#"{"autoMemoryDirectory":"/w/mem"}"#;
        assert!(
            merge_auto_memory_dir(Some(input), "/w/mem").is_none(),
            "no rewrite when the dir already matches"
        );
    }
}

#[cfg(test)]
mod launch_tests {
    use super::*;

    fn plan(
        seed: Option<&str>,
        yolo_force: bool,
        fresh: bool,
        session: Option<&str>,
        passthrough: &[&str],
    ) -> LaunchPlan {
        LaunchPlan {
            seed: seed.map(String::from),
            yolo_force,
            fresh,
            session: session.map(String::from),
            resume_session: None,
            passthrough: passthrough.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn harness_parse_defaults_and_rejects() {
        assert_eq!(Harness::parse(None), Some(Harness::Copilot));
        assert_eq!(Harness::parse(Some("copilot")), Some(Harness::Copilot));
        assert_eq!(Harness::parse(Some("claude")), Some(Harness::Claude));
        assert_eq!(Harness::parse(Some("codex")), None);
    }

    #[test]
    fn project_dirname_replaces_every_non_alnum() {
        assert_eq!(
            claude_project_dirname("/home/u/projects/worktree/bn/feat_x"),
            "-home-u-projects-worktree-bn-feat-x"
        );
    }

    #[test]
    fn pick_copilot_resume_takes_newest_matching_cwd() {
        let rows = [
            ("/w/a", "2026-01-01T00:00:00Z", "old"),
            ("/w/a", "2026-06-01T00:00:00Z", "new"),
            ("/w/b", "2026-12-01T00:00:00Z", "other"), // different cwd — ignored
            ("/w/a", "2026-03-01T00:00:00Z", ""),      // empty id — ignored
        ];
        assert_eq!(pick_copilot_resume(rows, "/w/a").as_deref(), Some("new"));
        assert_eq!(pick_copilot_resume(rows, "/w/none"), None);
    }

    // --- claude argv ------------------------------------------------------

    #[test]
    fn claude_resume_uses_continue_and_yolo_when_unseeded() {
        let args = build_claude_args(&plan(None, false, false, Some("sid"), &[]), true);
        // Unseeded → yolo; transcripts present + not fresh → --continue (session id NOT pinned).
        assert_eq!(args, ["--dangerously-skip-permissions", "--continue"]);
    }

    #[test]
    fn claude_exact_resume_uses_recorded_session_id() {
        let mut p = plan(None, false, false, Some("R1"), &[]);
        p.resume_session = Some("R1".into());
        assert_eq!(
            build_claude_args(&p, true),
            ["--dangerously-skip-permissions", "--resume", "R1"]
        );
    }

    #[test]
    fn claude_fresh_pins_session_and_passes_seed_as_prompt() {
        let args = build_claude_args(
            &plan(Some("do the thing"), false, false, Some("sid"), &[]),
            false,
        );
        // Seeded → NOT yolo (no --yolo); no transcripts → fresh: pin sid + seed as positional prompt.
        assert_eq!(args, ["--session-id", "sid", "do the thing"]);
    }

    #[test]
    fn claude_fresh_flag_overrides_existing_transcripts() {
        let args = build_claude_args(&plan(Some("seed"), true, true, Some("sid"), &["-x"]), true);
        // --fresh skips --continue even with transcripts; --yolo forces skip-permissions despite seed.
        assert_eq!(
            args,
            [
                "--dangerously-skip-permissions",
                "--session-id",
                "sid",
                "seed",
                "-x"
            ]
        );
    }

    #[test]
    fn claude_controller_launch_adds_stop_hook_settings() {
        let args = with_claude_controller_hook(
            build_claude_args(&plan(Some("seed"), true, true, Some("sid"), &[]), false),
            true,
        );
        assert_eq!(args[0], "--settings");
        let settings: serde_json::Value = serde_json::from_str(&args[1]).unwrap();
        assert_eq!(
            settings["hooks"]["Stop"][0]["hooks"][0]["command"],
            controller_turn_end_hook_command()
        );
        assert_eq!(args[2], "--dangerously-skip-permissions");
    }

    #[test]
    fn controller_hook_is_not_attached_to_owned_workers() {
        assert!(controller_launch_env(Some("ctrl-a"), None));
        assert!(!controller_launch_env(
            Some("ctrl-a"),
            Some("worker-session")
        ));
        assert!(!controller_launch_env(None, None));
    }

    #[test]
    fn ordinary_claude_launch_does_not_add_controller_settings() {
        let base = build_claude_args(&plan(None, false, false, Some("sid"), &[]), true);
        assert_eq!(with_claude_controller_hook(base.clone(), false), base);
    }

    // --- copilot argv -----------------------------------------------------

    #[test]
    fn copilot_controller_hook_uses_agent_stop_and_shared_guard() {
        let config = copilot_controller_hook_config();
        assert_eq!(config["version"], 1);
        assert_eq!(
            config["hooks"]["agentStop"][0]["command"],
            controller_turn_end_hook_command()
        );
        assert!(config["hooks"].get("Stop").is_none());
    }

    #[test]
    fn copilot_controller_hook_install_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = dir.path().join("hooks");

        install_copilot_controller_hook_at(&hooks).unwrap();
        let path = hooks.join("bn-controller.json");
        let first = std::fs::read(&path).unwrap();
        install_copilot_controller_hook_at(&hooks).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), first);
        let config: serde_json::Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(
            config["hooks"]["agentStop"][0]["command"],
            controller_turn_end_hook_command()
        );
    }

    #[test]
    fn copilot_resume_uses_resume_flag_and_yolo_when_unseeded() {
        let args = build_copilot_args(&plan(None, false, false, Some("sid"), &[]), Some("R1"));
        // Unseeded → yolo pair; resume id present + not fresh → --resume=<id> (session id NOT pinned).
        assert_eq!(
            args,
            ["--allow-all-tools", "--allow-all-paths", "--resume=R1"]
        );
    }

    #[test]
    fn copilot_fresh_pins_session_and_seeds_with_dash_i() {
        let args = build_copilot_args(&plan(Some("go"), false, false, Some("sid"), &[]), None);
        // Seeded → NOT yolo; no resume id → fresh: pin sid + `-i <seed>`.
        assert_eq!(args, ["--session-id", "sid", "-i", "go"]);
    }

    #[test]
    fn copilot_fresh_flag_ignores_resume_id_and_passthrough_after_seed() {
        let args = build_copilot_args(
            &plan(Some("go"), true, true, None, &["--disable-mcp-server", "x"]),
            Some("R1"),
        );
        // --fresh skips --resume even with a resume id; --yolo forces the allow-all pair despite seed;
        // passthrough trails the seed.
        assert_eq!(
            args,
            [
                "--allow-all-tools",
                "--allow-all-paths",
                "-i",
                "go",
                "--disable-mcp-server",
                "x"
            ]
        );
    }

    #[test]
    fn copilot_unseeded_fresh_no_session_is_bare_yolo() {
        let args = build_copilot_args(&plan(None, false, true, None, &[]), None);
        assert_eq!(args, ["--allow-all-tools", "--allow-all-paths"]);
    }

    #[cfg(unix)]
    #[test]
    fn copilot_resume_retries_only_for_missing_session_failure() {
        use std::os::unix::process::ExitStatusExt;

        let failed = std::process::ExitStatus::from_raw(1 << 8);
        let succeeded = std::process::ExitStatus::from_raw(0);
        let missing = "Error: No session, task, or name matched 'dead-session'.";

        assert!(should_retry_copilot_resume(failed, missing));
        assert!(!should_retry_copilot_resume(
            failed,
            "Error: authentication failed"
        ));
        assert!(!should_retry_copilot_resume(succeeded, missing));
    }

    #[test]
    fn copilot_missing_resume_falls_back_to_pinned_fresh_session() {
        let mut launch = plan(
            Some("resume the task"),
            true,
            false,
            Some("new-session"),
            &["--disable-mcp-server", "broken"],
        );
        assert_eq!(
            build_copilot_args(&launch, Some("phantom-session")),
            [
                "--allow-all-tools",
                "--allow-all-paths",
                "--resume=phantom-session",
                "--disable-mcp-server",
                "broken"
            ]
        );

        launch.fresh = true;
        let fallback = build_copilot_args(&launch, None);
        assert_eq!(
            fallback,
            [
                "--allow-all-tools",
                "--allow-all-paths",
                "--session-id",
                "new-session",
                "-i",
                "resume the task",
                "--disable-mcp-server",
                "broken"
            ]
        );
        assert!(!fallback.iter().any(|arg| arg.contains("phantom-session")));
    }
}

#[cfg(test)]
mod done_tests {
    use super::*;
    use bn_core::store;
    use bn_core::test_support::{config_with_hostname, with_state_and_notes_dir};

    #[test]
    fn agent_done_with_pr_sets_front_pr() {
        let dir = tempfile::tempdir().unwrap();
        with_state_and_notes_dir(dir.path(), "testhost", || {
            let cfg = config_with_hostname(dir.path(), "testhost");
            let ctx = crate::resolver::Resolved {
                repo: "demo".into(),
                branch: "feat".into(),
            };
            let pr = "https://github.com/eduuh/bn/pull/648";
            emit_agent_done(
                &cfg,
                Some("did the thing".into()),
                Some(pr.into()),
                false,
                false,
                false,
                Some(ctx.repo.clone()),
                Some(ctx.branch.clone()),
                true,
            );
            let note = store::read(&cfg.note_file(&ctx)).unwrap();
            assert_eq!(
                note.front.pr,
                Some(bn_core::model::Scalar::Str(pr.into())),
                "front.pr must be set on the primary `agent done --pr` path"
            );
        });
    }
}
