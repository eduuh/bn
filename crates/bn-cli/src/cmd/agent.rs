//! `bn agent` — fleet supervise/recover/checkin/send/digest/handoff/switch.
use crate::*;

mod dialog;
mod instruct;
mod kill;
mod read;
mod steer;
pub(crate) use dialog::*;
pub(crate) use instruct::*;
pub(crate) use kill::*;
pub(crate) use read::*;
pub(crate) use steer::*;

pub(crate) fn emit_agent_ingest(cfg: &Config, reingest: bool, as_json: bool) {
    let mode = bn_core::agent::IngestMode::poll(reingest);
    match bn_core::agent::ingest_with_mode(cfg, mode) {
        Ok(r) => {
            if as_json {
                let data = serde_json::json!({
                    "ingested": r.ingested, "skipped": r.skipped, "reingest": reingest });
                println!("{}", Envelope::success("agent_ingest", data).to_json());
            } else {
                println!("{}", r.summary(mode));
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
    // `agents.len()` is every session the supervisor could still read, which includes the ones that
    // have already finished — quoting that as "running" overstated the fleet by every agent that
    // exited in the last half hour.
    println!(
        "{} running · {} starting · {} active · {} idle · {} stuck · {} ended",
        fleet.running_count(),
        fleet.starting,
        fleet.active,
        fleet.idle,
        fleet.stuck,
        fleet.ended
    );
    for a in &fleet.agents {
        let glyph = match a.health {
            bn_core::agent::AgentHealth::Starting => "◍",
            bn_core::agent::AgentHealth::Active => "●",
            bn_core::agent::AgentHealth::Idle => "◐",
            bn_core::agent::AgentHealth::Stuck => "○",
            bn_core::agent::AgentHealth::Ended => "·",
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
    // Build the target list. Both arms ask *git* where a branch is checked out rather than
    // reconstructing `<worktree_dir>/<repo>/<branch>`: that join is a convention, and a dispatched
    // branch lives in a pool slot (`_pool/N`) while a reused directory keeps its old name — so the
    // reconstructed path misses most of a real fleet, and this swept right past the very agents it
    // exists to rescue. `None` means "could not locate", which is reported, not silently skipped.
    let mut targets: Vec<(String, String, Option<std::path::PathBuf>)> = Vec::new();
    if all {
        // git's own answer per repo, so pooled slots are swept instead of skipped by name.
        for (repo_lc, branches) in bn_core::lifecycle::live_worktree_paths(&cfg.bare_dir()) {
            for (branch, path) in branches {
                if branch == "main" {
                    continue;
                }
                targets.push((repo_lc.clone(), branch, Some(path)));
            }
        }
    } else {
        let ctx = target_ctx(cfg, repo, branch).unwrap_or_else(|| {
            fail("bn agent recover: not in a git repo — pass --repo/--branch or --all")
        });
        let wt = agent_worktree(cfg, &ctx.repo, &ctx.branch);
        targets.push((ctx.repo.clone(), ctx.branch, wt));
    }

    let gitout = |wt: &std::path::Path, args: &[&str]| -> Option<String> {
        crate::git::git_capture(args, Some(wt), true)
    };

    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut unlocated = 0usize;
    for (repo, branch, wt) in targets {
        let Some(wt) = wt.filter(|p| p.is_dir()) else {
            // Not a silent `continue`: "I could not find this branch's checkout" and "there was
            // nothing to recover" are different answers, and conflating them is what let a failed
            // recover read as a clean no-op.
            unlocated += 1;
            results.push(serde_json::json!({
                "repo": repo, "branch": branch, "action": "no-worktree",
            }));
            continue;
        };
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

    // `candidates` keeps its meaning — recoverable branches — so the "could not find the checkout"
    // rows ride alongside in `unlocated` rather than inflating it.
    let candidates = results.len() - unlocated;
    if as_json {
        let data = serde_json::json!({
            "candidates": candidates,
            "unlocated": unlocated,
            "results": results,
        });
        println!("{}", Envelope::success("agent_recover", data).to_json());
    } else if results.is_empty() {
        println!("nothing to recover (no worktree with commits ahead + no PR)");
    } else {
        for r in &results {
            let repo = r["repo"].as_str().unwrap_or("");
            let branch = r["branch"].as_str().unwrap_or("");
            let action = r["action"].as_str().unwrap_or("");
            if action == "no-worktree" {
                println!("  no-worktree {repo}/{branch} (git knows no checkout for it)");
                continue;
            }
            let ahead = r["commits_ahead"].as_u64().unwrap_or(0);
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
/// attention item so it surfaces in `bn next`; badge the window; record it in the note; close out its
/// task ([`crate::cmd::close_out`]); and, with `--notify`, fire a desktop toast. Keyed by
/// `done:<repo>/<branch>` so a re-run dedups — the durable event log is the source of truth.
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
    let closed = crate::cmd::close_out::run(cfg, &ctx, failed, report_only, as_json);
    if notify {
        // A failure is blocking — the work stopped and it is yours again. A success is a
        // completion, which the default bar keeps off the toast channel; but `--notify` is an
        // explicit ask, so it clears the bar (the quiet windows and budgets still apply).
        let (class, kind) = if failed {
            (crate::toast::Class::Blocking, "failed")
        } else {
            (crate::toast::Class::Completion, "done")
        };
        crate::toast::toast_requested(
            class,
            &format!("agent:{}/{}:{kind}", ctx.repo, ctx.branch),
            "agent",
            if failed { "Agent failed" } else { "Agent done" },
            &title,
        );
    }

    let raised = !skipped;
    if as_json {
        let env = Envelope::success(
            "agent_done",
            serde_json::json!({ "repo": ctx.repo, "branch": ctx.branch, "raised": raised, "updated": updated, "pr": pr, "failed": failed, "closed_task": closed.task, "closed_suggestion": closed.suggestion }),
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

/// Resolve a `repo:branch` target to `(worktree_path, harness, session_id)` for a `-p --resume`
/// steer turn.
///
/// Two sources, strongest first:
///
/// 1. the branch note's recorded `agent-session: <harness> <id>` line (latest wins) — what the
///    dispatcher wrote down when bn launched the agent;
/// 2. failing that, the **live transcript**: supervision found this agent by reading its session
///    log, so it holds the session id and the harness whether or not bn started it. That is a
///    resume key, and it is what lets a `claude` someone ran by hand in a terminal be steered
///    instead of being written off as observed-only. Where a branch hosts several live agents the
///    record in (1) cannot say which one it means, so this tier is also the *better* answer there —
///    but only when exactly one live agent claims the target, since otherwise we would be picking
///    for the caller.
///
/// `None` when the target isn't a `repo:branch`, or when neither source yields a session in a
/// checkout that still exists.
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
    // The dispatcher records `agent-session: <harness> <id>` in the collab section; take the latest.
    if let Some((harness, id)) = recorded_session(cfg, &repo, &branch) {
        // `<worktree_dir>/<repo>/<branch>` is a convention, not a fact: a dispatched branch lives in
        // a pool slot and a reused directory keeps its old name. A resume turn spawned in a
        // directory that doesn't exist fails after a full harness invocation, so resolve the real
        // checkout and fall through rather than hand back a path nothing is running in.
        if let Some(wt) = resolve_agent_channel(cfg, &repo, &branch)
            .worktree
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_dir())
        {
            return Some((wt, harness, id));
        }
    }
    observed_agent_session(cfg, &repo, &branch)
}

/// The live transcript's own resume key for a `repo:branch`, for agents no note recorded.
///
/// Supervision reads every running session's log, so it knows each one's session id, harness and
/// cwd — the three things a `--resume` turn needs — regardless of who launched it. Exactly one live
/// agent must claim the target: with two, only the caller knows which was meant, and silently
/// picking one is the mistake `ControlChannel::Ambiguous` exists to prevent.
///
/// This spawns a supervisor pass, so it is deliberately the *fallback*: steering is a
/// human-initiated write, not a polled read, and the recorded-session path above already covers
/// every agent bn launched itself.
fn observed_agent_session(
    cfg: &Config,
    repo: &str,
    branch: &str,
) -> Option<(std::path::PathBuf, String, String)> {
    let fleet = bn_core::agent::supervise(
        cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    );
    let mut hits = fleet
        .agents
        .iter()
        .filter(|a| a.repo == repo && a.branch == branch);
    let agent = hits.next()?;
    if hits.next().is_some() {
        return None; // two live sessions on this branch — the caller has to disambiguate
    }
    let wt = agent
        .worktree
        .as_deref()
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_dir())?;
    if agent.session_id.is_empty() || agent.harness.is_empty() {
        return None;
    }
    Some((wt, agent.harness.clone(), agent.session_id.clone()))
}


/// A `repo:branch` target as an exact-match tmux target — see [`bn_core::tmux::exact_target`],
/// which anchors *both* halves so a missing window cannot prefix-match a neighbour.
pub(crate) fn tmux_exact_target(target: &str) -> String {
    bn_core::tmux::exact_target(target)
}

/// The exact tmux target hosting `repo:branch`, and the checkout it is running in.
///
/// A window is named after whatever the launcher chose — the branch with slashes dashed, a pooled
/// slot, or a directory reused from earlier work — so the `repo:branch` identity bn addresses an
/// agent by is frequently *not* a window name. This runs the same tiered join the supervisor uses
/// ([`bn_core::agent::resolve_channels`]): lease, then window session id, then checkout, then the
/// window's `@bn_*` stamps, then the name. One `list-windows` plus one lease read.
fn resolve_agent_channel(cfg: &Config, repo: &str, branch: &str) -> bn_core::agent::AgentChannel {
    let windows = bn_core::agent::parse_window_rows(
        &proc::tmux_raw(&["list-windows", "-a", "-F", bn_core::agent::WINDOW_FORMAT])
            .unwrap_or_default(),
    );
    let leases = bn_core::agent::load_agent_leases(&bn_core::agent::agent_lease_registry_path())
        .unwrap_or_default()
        .leases;
    // The note's record seeds the session-id tiers; the git-known checkout seeds the path tier.
    let session = recorded_session(cfg, repo, branch)
        .map(|(_, id)| id)
        .unwrap_or_default();
    // git knows where a branch is really checked out; the conventional layout is the last resort
    // for a repo git can't enumerate (no bare clone), and only if it actually exists.
    let known = bn_core::lifecycle::worktree_path(cfg, repo, branch)
        .or_else(|| {
            Some(cfg.worktree_dir.join(repo).join(branch)).filter(|p: &std::path::PathBuf| p.is_dir())
        })
        .map(|p| p.to_string_lossy().into_owned());
    let key = bn_core::agent::AgentKey {
        session_id: &session,
        repo,
        branch,
        worktree: known.as_deref(),
    };
    let mut channel = bn_core::agent::resolve_channels(&[key], &windows, &leases)
        .pop()
        .unwrap_or_default();
    channel.worktree = channel
        .worktree
        .filter(|p| std::path::Path::new(p).is_dir())
        .or(known);
    channel
}

/// The checkout a `repo:branch` agent is really running in — git's answer, never the conventional
/// `<worktree_dir>/<repo>/<branch>`.
///
/// That join is a convention, not a fact: a dispatched branch lives in a pool slot (`_pool/N`) and
/// a worktree directory reused for later work keeps its original name, so reconstructing the path
/// lands in a directory that does not exist for most of a real fleet. Every caller that needs a
/// *cwd* — recovering a ship turn, swapping a harness in place, respawning an agent — goes through
/// here, which settles the path through the same tiered join the supervisor uses and only ever
/// returns a directory that exists.
pub(crate) fn agent_worktree(cfg: &Config, repo: &str, branch: &str) -> Option<std::path::PathBuf> {
    resolve_agent_channel(cfg, repo, branch)
        .worktree
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_dir())
}

/// The branch note's recorded `agent-session: <harness> <id>` (latest wins) — the resume channel's
/// only input, shared by [`resolve_agent_session`] and the window join.
pub(crate) fn recorded_session(cfg: &Config, repo: &str, branch: &str) -> Option<(String, String)> {
    let ctx = Resolved {
        repo: repo.to_string(),
        branch: branch.to_string(),
    };
    let note = bn_core::store::read(&cfg.note_file(&ctx)).ok()?;
    note.front.collab.iter().rev().find_map(|it| {
        let rest = it.text.trim().strip_prefix("agent-session:")?;
        let mut w = rest.split_whitespace();
        Some((w.next()?.to_string(), w.next()?.to_string()))
    })
}

/// A `repo:branch` target as the exact tmux target that actually hosts it.
///
/// [`tmux_exact_target`] alone only guards against fnmatch widening — it still assumes the window is
/// *called* `repo:branch`, which is how steering came to miss every pooled agent. Ids and
/// already-exact targets pass through untouched, and a target the join can't place falls back to the
/// literal so nothing regresses when tmux is unreachable.
pub(crate) fn resolve_tmux_target(cfg: &Config, target: &str) -> String {
    if target.starts_with(['=', '%', '@', '$']) {
        return target.to_string();
    }
    let Some((repo, branch)) = target.split_once(':') else {
        return tmux_exact_target(target);
    };
    resolve_agent_channel(cfg, repo, branch)
        .tmux_target
        .as_deref()
        .map_or_else(|| tmux_exact_target(target), tmux_exact_target)
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
    /// Directories to widen the harness's allowed working set with, beyond the launch cwd.
    pub(crate) add_dirs: Vec<String>,
    pub(crate) passthrough: Vec<String>,
}

impl LaunchPlan {
    /// "yolo mode" (skip approval prompts) applies to an *interactive* launch (no seed) OR when the
    /// caller explicitly opts in with `--yolo`. SECURITY: a seed may embed attacker-influenceable
    /// content, so a seeded launch stays gated unless the caller (who owns the trust decision)
    /// passes `--yolo`. Matches the zsh launcher.
    pub(super) fn yolo(&self) -> bool {
        self.seed.is_none() || self.yolo_force
    }
    /// `--add-dir <dir>` per requested directory. Both CLIs spell this the same way.
    ///
    /// Applied on **every** path, resume included, and independently of [`Self::yolo`]: the
    /// allowed working set is a property of the session, not of how its prompts are answered, so
    /// a resumed agent must be given the same boundary as a fresh one or it silently loses access
    /// to bn's own state halfway through a task.
    pub(super) fn add_dir_args(&self, harness: bn_core::mcp::Harness) -> Vec<String> {
        super::agent_scope::add_dir_args(harness, &self.add_dirs)
    }

    /// `--session-id <sid>` when a session is pinned, else empty (let the harness pick its own).
    pub(super) fn session_flag(&self) -> Vec<String> {
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
///
/// `since` is the RFC-3339 instant the directory's current tenancy began, for a pool slot — one
/// path that many branches pass through, where a session recorded before the current lease belongs
/// to the previous tenant and resuming it is the "why am I back in someone else's conversation?"
/// bug. `None` for a per-branch worktree, whose whole history is its own.
pub(crate) fn pick_copilot_resume<'a>(
    rows: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
    cwd: &str,
    since: Option<&str>,
) -> Option<String> {
    let mut best: Option<(&str, &str)> = None; // (updated_at, id)
    for (c, ts, id) in rows {
        if c != cwd || id.is_empty() {
            continue;
        }
        if since.is_some_and(|floor| ts < floor) {
            continue;
        }
        if best.is_none_or(|(bts, _)| ts > bts) {
            best = Some((ts, id));
        }
    }
    best.map(|(_, id)| id.to_string())
}


/// One launched agent session, as the note and the cost log record it.
pub(crate) struct LaunchedSession<'a> {
    pub(crate) ctx: &'a Resolved,
    pub(crate) harness: &'a str,
    pub(crate) session_id: &'a str,
    pub(crate) worktree: &'a str,
    pub(crate) work_class: bn_core::cost::WorkClass,
    pub(crate) task_name: Option<&'a str>,
    pub(crate) controller_id: Option<&'a str>,
}

/// Record a launched agent session: the note's `agent-session: <harness> <id>` collab line, plus the
/// cost log's work-class attribution.
///
/// The single writer for every launch path — `bn dispatch`'s lane and `--attach` forms and a
/// hand-typed `claude` through the shell wrapper — because that note line is the *only* input to
/// [`resolve_agent_session`] and the supervisor's resume channel. Three hand-rolled copies could
/// drift from the two readers; one cannot. Callers must pass an id the harness will actually run
/// under (see [`effective_session_id`]) — recording an id that was never pinned puts a lie in the
/// note that `bn agent send` would later try to resume.
pub(crate) fn record_launched_session(cfg: &Config, s: LaunchedSession<'_>) {
    let line = format!("agent-session: {} {}", s.harness, s.session_id);
    // Idempotent on the exact line: `bn dispatch --attach` records, then execs a launcher that
    // reaches this same writer, and one session should leave one record.
    let _ = commands::add(cfg, s.ctx, "collab", &line, Some(&line), &today());
    bn_core::cost::attribute_session(bn_core::cost::SessionLaunch {
        session_id: s.session_id,
        work_class: s.work_class,
        repo: &s.ctx.repo,
        branch: &s.ctx.branch,
        task_name: s.task_name,
        harness: Some(s.harness),
        worktree: Some(s.worktree),
        controller_id: s.controller_id,
    });
}

/// Whether the user's own harness arguments already decide which session this launch runs in.
///
/// `claude --continue`, `claude --resume <id>` and an explicit `--session-id` are the user driving
/// the session themselves; bn pinning a freshly minted id on top would either be ignored or refused
/// by the harness as a conflicting flag. Pure.
pub(crate) fn passthrough_controls_session(passthrough: &[String]) -> bool {
    passthrough.iter().any(|a| {
        let flag = a.split('=').next().unwrap_or(a);
        matches!(
            flag,
            "--continue" | "-c" | "--resume" | "-r" | "--session-id" | "--session"
        )
    })
}

/// The harness session id this launch will *actually* run under, or `None` when the harness picks
/// its own and bn never learns it up front.
///
/// Mirrors [`build_claude_args`] / [`build_copilot_args`] branch for branch, because the answer is
/// exactly "which branch did the arg builder take". `claude --continue` is the `None` case: bn may
/// have minted an id, but it is never passed, so the live session ends up with the harness's own —
/// and a note recording the minted one would send `bn agent send --resume` at a session that does
/// not exist. Copilot resumes by appending to the same session, so a resume id *is* the live id.
pub(crate) fn effective_session_id(
    harness: Harness,
    plan: &LaunchPlan,
    has_transcripts: bool,
    copilot_resume: Option<&str>,
) -> Option<String> {
    if !plan.fresh {
        match harness {
            Harness::Claude => {
                if let Some(id) = &plan.resume_session {
                    return Some(id.clone());
                }
                if has_transcripts {
                    return None;
                }
            }
            Harness::Copilot => {
                if let Some(id) = copilot_resume {
                    return Some(id.to_string());
                }
            }
        }
    }
    plan.session.clone()
}

/// Set `BN_AGENT_GUARD=0` to bypass the within-repo placement rules for one launch. The repository
/// requirement is unconditional: bn never creates a harness session it cannot attach to a branch.
fn launch_guard_enabled() -> bool {
    !matches!(
        std::env::var("BN_AGENT_GUARD").as_deref().map(str::trim),
        Ok("0")
    )
}

fn resolve_launch_context(cfg: &Config, cwd: &std::path::Path) -> Result<Resolved, &'static str> {
    resolver::resolve(cfg, cwd).map_err(|_| {
        "bn agent launch: sessions require a git worktree on a branch; enter or create a repository first"
    })
}

/// Refuse to start an agent where one may not run, and otherwise take this worktree's lease.
///
/// Three rules, in order:
///
/// 1. **Every session belongs to a repository branch.** Planning, investigation, and coding
///    sessions all need a resolvable git worktree so bn can persist and recover their context.
/// 2. **The main worktree hosts no agents.** It stays clean on `main` — the same checkout the
///    repo's `pre-commit` hook refuses commits in. Rather than dead-ending, this asks what the work
///    is and hands the whole thing to `bn dispatch`, which leases a warm pool slot, seeds the goal
///    and a todo, and launches the agent there (execs — never returns).
/// 3. **One agent per worktree.** A checkout already held by a live agent refuses the second one,
///    naming the holder and the two ways out (steer it, or kill it).
///
/// A launch that survives both takes a [`AgentLeaseKind::Dispatch`] lease for the worktree, so a
/// hand-typed `claude` is leased exactly like a dispatched one and the *next* launch sees it. The
/// lease is committed before the exec (which runs no destructors); `bn wt cleanup`/`reap` and the
/// reconcile sweep release it when the window goes away.
///
/// It also records the session on the branch note exactly as `bn dispatch` does — without that line
/// a hand-typed agent has no resume channel, which is the difference between a steerable agent and
/// one the dashboard can only watch. `session` is the id the harness will really run under
/// ([`effective_session_id`]); `None` means the harness picked its own, so there is nothing
/// truthful to write and the window join is left to provide the channel.
fn enforce_launch_placement(
    cfg: &Config,
    cwd: &std::path::Path,
    harness: Harness,
    session: Option<&str>,
    pinned_session: &str,
) -> Option<()> {
    let canonical = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let ctx = resolve_launch_context(cfg, &canonical).unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(1);
    });
    if !launch_guard_enabled() {
        return None;
    }
    if bn_core::agent::is_main_worktree_path(&canonical) {
        redirect_from_main(&canonical, harness);
    }
    match agent_lease::worktree_holder(&canonical) {
        // A launcher already leased this window for this very launch (a dispatched worker, the
        // controller in its slot). Nothing to add — leasing again would only collide with itself.
        agent_lease::WorktreeHolder::Ours => return None,
        agent_lease::WorktreeHolder::Other(owner) => {
            // An agent that never registered has no `repo:branch` to steer by — its window is the
            // only handle there is, so the advice has to be phrased against that instead.
            let steer = match (&owner.repo, &owner.branch) {
                (Some(repo), Some(branch)) => format!(
                    "  steer it:  bn agent send {repo}:{branch} \"<message>\"\n  \
                     or free it: bn agent kill {repo}:{branch}"
                ),
                _ => format!(
                    "  it never registered with bn, so it has no repo:branch to steer — \
                     stop it in its window: {}",
                    owner.tmux_target
                ),
            };
            eprintln!(
                "bn agent launch: {} already has a live {} — one agent per worktree.\n  {}\n{steer}",
                canonical.display(),
                owner.kind,
                owner.describe(),
            );
            std::process::exit(1);
        }
        agent_lease::WorktreeHolder::Free => {}
    }
    // 3. **Agents live in tmux windows.** Typed on a bare terminal there is no window to steer, kill
    //    or even name, so the agent would run outside the control plane bn exists to provide. Open
    //    the window bn would have opened and attach to it — the same shape `bn dispatch` produces.
    if redirect_into_window_wanted() {
        redirect_into_window(cfg, &ctx, &canonical, harness, pinned_session);
    }

    // Lease under the caller's session id when it gave one (`bn dispatch --attach` passes
    // `--session <sid>`), so that caller can release this exact lease when the foreground agent
    // exits. A hand-typed launch has no id yet and mints one.
    let session_id = session
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(gen_uuid);
    // Outside tmux there is no window to lease against, and inventing `repo:branch` would stamp
    // `@bn_session_id` onto whatever unrelated window happens to bear that name. Lease with no
    // target instead, and skip the stamping that would go with it.
    let current_window =
        crate::proc::tmux_raw(&["display-message", "-p", "#{session_name}:#{window_name}"])
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty() && std::env::var_os("TMUX").is_some());
    let tmux_target = current_window.clone().unwrap_or_default();
    match agent_lease::reserve(agent_lease::AgentLeaseRequest {
        repo: &ctx.repo,
        branch: &ctx.branch,
        worktree: &canonical,
        session_id: &session_id,
        harness: harness.bin(),
        kind: bn_core::agent::AgentLeaseKind::Dispatch,
        tmux_target: &tmux_target,
        replace: false,
        in_place: true,
    }) {
        Ok(reservation) => {
            if current_window.is_some() {
                agent_lease::mark_window(reservation.lease());
            }
            reservation.commit();
            // Only a session the harness will actually run under earns a note record; a
            // `--continue` launch (session `None`) is left to the window join for its channel.
            if let Some(sid) = session.map(str::trim).filter(|s| !s.is_empty()) {
                record_launched_session(
                    cfg,
                    LaunchedSession {
                        ctx: &ctx,
                        harness: harness.bin(),
                        session_id: sid,
                        worktree: &canonical.to_string_lossy(),
                        // A human typing `claude` in a worktree is shipping work, not running the
                        // fleet — the same class `bn dispatch` gives a worker.
                        work_class: bn_core::cost::WorkClass::Coding,
                        task_name: None,
                        controller_id: None,
                    },
                );
            }
        }
        // A refusal here is the placement invariant talking (the controller slot, or a main worktree
        // whose path shape the check above didn't catch). Surface it verbatim and stop.
        Err(error) => {
            eprintln!("bn agent launch: {error}");
            std::process::exit(1);
        }
    }
    None
}

/// Whether a launch typed outside tmux should be moved into a bn-managed window.
///
/// Every gate has to hold, because this is the one rule that *relocates* the user's terminal:
/// there must be no surrounding tmux session (inside one, the current window is already a fine
/// home), a reachable tmux server, and a real terminal on both ends — a script, a hook or a CI step
/// piping `claude` must never be teleported into a window it cannot see. `BN_AGENT_TMUX_REDIRECT=0`
/// turns it off outright, the same shape of valve as `BN_AGENT_GUARD=0`.
fn redirect_into_window_wanted() -> bool {
    use std::io::IsTerminal;
    if std::env::var_os("TMUX").is_some() {
        return false;
    }
    if matches!(
        std::env::var("BN_AGENT_TMUX_REDIRECT").as_deref().map(str::trim),
        Ok("0")
    ) {
        return false;
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return false;
    }
    // A reachable server is required: `new-window` cannot adopt a session that doesn't exist yet
    // here, and a failed relocation would strand the launch.
    crate::proc::tmux_raw(&["list-sessions", "-F", "#{session_name}"]).is_some()
}

/// Move a bare-terminal launch into the tmux window bn would have opened for this branch, and
/// attach to it.
///
/// Mirrors what `bn dispatch` does for a lane, minus the branch and worktree creation: lease the
/// checkout against the window that is about to exist, record the session, open the window running
/// this very command inside it, then attach. The inner `bn agent launch` sees the lease as
/// [`agent_lease::WorktreeHolder::Ours`] and so neither re-leases nor re-records.
///
/// Returns normally only when the window could not be opened — the caller then continues with the
/// in-place launch rather than leaving the user with nothing.
fn redirect_into_window(
    cfg: &Config,
    ctx: &Resolved,
    worktree: &std::path::Path,
    harness: Harness,
    pinned_session: &str,
) {
    let window = bn_core::orchestrate::tmux_window_name(&ctx.branch);
    let tmux_target = format!("{}:{window}", ctx.repo);
    let reservation = match agent_lease::reserve(agent_lease::AgentLeaseRequest {
        repo: &ctx.repo,
        branch: &ctx.branch,
        worktree,
        session_id: pinned_session,
        harness: harness.bin(),
        kind: bn_core::agent::AgentLeaseKind::Dispatch,
        tmux_target: &tmux_target,
        replace: false,
        in_place: false,
    }) {
        Ok(reservation) => reservation,
        Err(error) => {
            eprintln!("bn agent launch: {error}");
            std::process::exit(1);
        }
    };

    // Re-run *this* invocation inside the window, with the session pinned so the window's agent and
    // the record we are about to write name the same session.
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("bn"));
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    if !argv.iter().any(|a| a == "--session") {
        // Before any `--`, so it lands on the launcher rather than the harness passthrough.
        let at = argv.iter().position(|a| a == "--").unwrap_or(argv.len());
        argv.splice(at..at, ["--session".to_string(), pinned_session.to_string()]);
    }
    let cmd = std::iter::once(exe.to_string_lossy().into_owned())
        .chain(argv)
        .map(|a| wt::shell_single_quote(&a))
        .collect::<Vec<_>>()
        .join(" ");

    eprintln!("bn agent launch: opening {tmux_target} — attaching.");
    if !wt::open_worktree_window_cmd_named(
        &ctx.repo,
        &ctx.branch,
        &window,
        worktree,
        false,
        Some(&cmd),
        true,
    ) {
        eprintln!("bn agent launch: could not open {tmux_target} — starting here instead.");
        let _ = reservation.rollback();
        return;
    }
    agent_lease::mark_window(reservation.lease());
    reservation.commit();
    record_launched_session(
        cfg,
        LaunchedSession {
            ctx,
            harness: harness.bin(),
            session_id: pinned_session,
            worktree: &worktree.to_string_lossy(),
            work_class: bn_core::cost::WorkClass::Coding,
            task_name: None,
            controller_id: None,
        },
    );

    // Focus the new window before attaching, so the client lands on the agent rather than whatever
    // window the session happened to leave current.
    let _ = crate::proc::tmux_ok(&["select-window", "-t", &tmux_exact_target(&tmux_target)]);
    exec_harness(
        "tmux",
        &[
            "attach-session".to_string(),
            "-t".to_string(),
            format!("={}", ctx.repo),
        ],
    )
}

/// The main-worktree redirect: ask what the work is, then hand off to `bn dispatch`, which leases a
/// warm pool slot, seeds the new branch's note (goal + todo) and launches the agent in this pane.
/// Execs, so it never returns. With no terminal to prompt on (a script, a hook) it prints the
/// command to run and exits non-zero instead of guessing a goal.
fn redirect_from_main(worktree: &std::path::Path, harness: Harness) -> ! {
    use std::io::Write;
    eprintln!(
        "bn agent launch: {} is the main worktree — it hosts no agents and stays clean.",
        worktree.display()
    );
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!(
            "  Start the work in its own pool slot instead:\n    bn dispatch \"<what you're doing>\" --agent {} --pool --attach",
            harness.bin()
        );
        std::process::exit(1);
    }
    eprint!("  What is this worktree for? (blank to abort) > ");
    let _ = std::io::stderr().flush();
    let mut goal = String::new();
    if std::io::stdin().read_line(&mut goal).is_err() || goal.trim().is_empty() {
        eprintln!("  Aborted — nothing dispatched.");
        std::process::exit(1);
    }
    let goal = goal.trim().to_string();
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("bn"));
    eprintln!("  Leasing a pool slot for: {goal}");
    exec_harness(
        &exe.to_string_lossy(),
        &[
            "dispatch".into(),
            "--agent".into(),
            harness.bin().into(),
            "--yolo".into(),
            "--pool".into(),
            "--attach".into(),
            "--".into(),
            goal,
        ],
    )
}

/// `bn agent launch` — the native port of `workflow/bin/agent`. Enforces where an agent may run,
/// resolves the seed, pre-trusts the worktree for Copilot, computes the resume-or-fresh argv, and
/// **execs** the harness (this process is replaced; on success it never returns). A Copilot resume
/// whose session no longer exists is retried exactly once as a fresh launch. On an unknown harness
/// it exits 2; on exec failure it prints the error and exits 127, mirroring a failed `exec` in the
/// shell launcher.
///
/// This is the single chokepoint every entry point funnels through — the tmux keybinds, `bn
/// dispatch`, `bn control start`, the `claude`/`copilot` shell wrappers, and a plain `cd <wt> && bn
/// agent launch` — so the placement rule in [`enforce_launch_placement`] lives here rather than
/// being restated by each caller.
///
/// Two of that rule's branches replace this process with something other than the harness: a launch
/// in the main worktree becomes `bn dispatch`, and a launch on a bare terminal becomes a tmux
/// attach onto the window the agent was started in. Either way this call does not return.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_agent_launch(
    cfg: &Config,
    harness: Option<String>,
    seed: Option<String>,
    seed_file: Option<String>,
    yolo: bool,
    fresh: bool,
    session: Option<String>,
    resume_session: Option<String>,
    add_dirs: Vec<String>,
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

    // Pin a session id when the caller didn't give one, exactly as `bn dispatch` does. Without it a
    // fresh hand-typed launch takes whatever id the harness invents, which bn never learns — and an
    // id bn doesn't know is an agent bn cannot resume. Skipped when the user drove the session
    // themselves (`claude --continue`, `claude --resume <id>`): pinning on top of their flag is at
    // best redundant and at worst an argument conflict.
    let user_owns_session = passthrough_controls_session(&passthrough);
    let mut plan = LaunchPlan {
        seed,
        yolo_force: yolo,
        fresh,
        add_dirs: super::agent_scope::launch_add_dirs(cfg, std::path::Path::new(&cwd), add_dirs),
        session: session
            .filter(|s| !s.trim().is_empty())
            .or_else(|| (!user_owns_session).then(gen_uuid)),
        resume_session,
        passthrough,
    };
    let pinned_session = plan.session.clone().unwrap_or_default();

    // Resolve the resume-or-fresh decision *before* the placement guard, so the guard can record the
    // id this launch will genuinely run under. Both probes are read-only filesystem checks.
    if harness == Harness::Claude
        && !plan.fresh
        && plan
            .resume_session
            .as_deref()
            .is_some_and(|id| !claude_has_session(&cwd, id))
    {
        eprintln!("bn agent launch: recorded Claude session is missing; starting fresh");
        plan.fresh = true;
    }
    let has_transcripts = harness == Harness::Claude && claude_has_transcripts(&cwd);
    let copilot_resume = (harness == Harness::Copilot)
        .then(|| {
            plan.resume_session
                .clone()
                .or_else(|| copilot_resume_id(&cwd))
        })
        .flatten();
    let effective = effective_session_id(harness, &plan, has_transcripts, copilot_resume.as_deref());

    enforce_launch_placement(
        cfg,
        std::path::Path::new(&cwd),
        harness,
        effective.as_deref(),
        &pinned_session,
    );

    match harness {
        Harness::Claude => {
            let args = with_claude_controller_hook(
                super::agent_scope::build_claude_args(&plan, has_transcripts),
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
            let resume_id = copilot_resume;
            let args = super::agent_scope::build_copilot_args(&plan, resume_id.as_deref());
            if resume_id.is_some() && !plan.fresh {
                match run_copilot_resume(harness.bin(), &args) {
                    Ok((status, stderr)) if should_retry_copilot_resume(status, &stderr) => {
                        eprintln!(
                            "bn agent launch: recorded Copilot session is missing; retrying fresh"
                        );
                        plan.fresh = true;
                        let fresh_args = super::agent_scope::build_copilot_args(&plan, None);
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
///
/// Returns whether the switch succeeded. The result rather than a bare `exit` because
/// Automated recovery can call this for each usage-limited agent: one target whose worktree or window
/// can't be resolved must not abort the sweep for the rest. The single-target CLI route turns a
/// `false` into a non-zero exit.
#[must_use]
pub(crate) fn emit_switch(
    cfg: &Config,
    ctx: &Resolved,
    harness: Option<String>,
    prompt: Option<String>,
    in_place: bool,
    reason: Option<String>,
    as_json: bool,
) -> bool {
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
                eprintln!("{msg}");
            }
            return false;
        }
    };

    // The checkout to swap, asked of git ([`agent_worktree`]) rather than reconstructed — a pooled
    // or reused directory is not at `<worktree_dir>/<repo>/<branch>`, and respawning a pane with
    // `-c <that path>` would root the incoming harness in a directory that does not exist.
    //
    // Resolved *before* the note is touched: everything below writes (a WIP bullet, a todo, the
    // handoff packet, a metrics row), and a switch that cannot happen must not leave a paper trail
    // claiming it did.
    let Some(wt) = agent_worktree(cfg, &ctx.repo, &ctx.branch) else {
        let msg = format!(
            "bn switch: no checkout for {}:{} — git lists no worktree for the branch, so there is \
             nothing to swap in place",
            ctx.repo, ctx.branch
        );
        if as_json {
            println!(
                "{}",
                Envelope::failure("switch", "no_worktree", msg).to_json()
            );
        } else {
            eprintln!("{msg}");
        }
        return false;
    };

    // Capture the session's *content*, not just its counts: the uncommitted worktree diff (what
    // was done) and its last user prompt (what was asked). Both fold forward so the incoming
    // harness can actually finish the work.
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
                eprintln!("bn switch: {e}");
            }
            return false;
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
    let swap = in_place.then(|| run_in_place_swap(cfg, ctx, &wt, &to, &launch));

    if as_json {
        let data = serde_json::json!({
            "from": from,
            "to": to,
            "outgoing": outgoing,
            "wip": wip,
            "last_request": last_request,
            "launch_command": launch,
            "handoff": payload::handoff_data(&handoff),
        });
        let envelope = switch_envelope(data, swap.as_ref(), &launch);
        let ok = envelope.ok;
        println!("{}", envelope.to_json());
        return ok;
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
            return false;
        }
        _ => println!("\nStart {to} here:\n  {launch}"),
    }
    true
}

/// The envelope `bn switch --json` prints, given the swap outcome.
///
/// A `--in-place` swap that failed used to ride inside a **success** envelope with the reason buried
/// at `data.in_place.error`, so `bn switch --json` exited 0 with `ok:true` while nothing had been
/// swapped — and the dashboard cheerfully toasted "restarting under claude…" over an agent that
/// never moved. A failure the caller can only find by reading a nested field is a failure the caller
/// will not find. Pure → unit-testable; `launch` is folded into the message so the operator still
/// gets the command to run by hand.
fn switch_envelope(
    mut data: serde_json::Value,
    swap: Option<&SwapOutcome>,
    launch: &str,
) -> Envelope {
    if let Some(s) = swap {
        data["in_place"] = serde_json::json!({
            "pane": s.pane,
            "trusted": s.trusted,
            "error": s.error,
        });
    }
    match swap {
        Some(SwapOutcome {
            error: Some(why), ..
        }) => Envelope::failure(
            "switch",
            "swap_failed",
            format!("bn switch --in-place: {why} — start it manually:\n  {launch}"),
        ),
        _ => Envelope::success("switch", data),
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
    cfg: &Config,
    ctx: &Resolved,
    wt: &std::path::Path,
    to: &str,
    launch: &str,
) -> SwapOutcome {
    let trusted = apply_folder_trust(&wt.to_string_lossy(), to);
    // First pane of the agent's window. Resolved through the same tiered join `send` and `kill` use
    // ([`resolve_agent_channel`]) rather than assuming the window is *called* `repo:branch` — it
    // usually isn't. A pooled slot or a reused directory names the window after whatever the
    // launcher chose, which is exactly how the in-place swap came to miss every pooled agent.
    let window_pane = resolve_agent_channel(cfg, &ctx.repo, &ctx.branch)
        .tmux_target
        .and_then(|t| proc::tmux(&["list-panes", "-t", &tmux_exact_target(&t), "-F", "#{pane_id}"]))
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
mod launch_context_tests {
    use super::*;

    #[test]
    fn sessions_require_a_git_worktree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = resolve_launch_context(&Config::default(), dir.path())
            .expect_err("a plain directory must not back a session");
        assert!(error.contains("sessions require a git worktree"));
    }
}
