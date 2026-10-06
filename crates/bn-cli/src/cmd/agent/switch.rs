//! `bn agent switch` — hop a worktree to another harness mid-task, printing the resume command
//! or swapping the tmux pane in place.

use super::*;

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
