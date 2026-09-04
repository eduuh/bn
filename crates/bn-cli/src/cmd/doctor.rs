//! `bn doctor`/`bn next`/`bn gap`/`bn wrap`/`bn rate` — health, next-action, day close.
use crate::*;

/// Is `bn shell-init`'s `claude`/`copilot` wrapper actually driving launches in this shell?
///
/// Reads the two signals only a live process has: `$BN_SHELL_WRAPPERS`, exported by the wrappers
/// themselves (a child cannot see its parent's shell functions, so this is the sole proof they were
/// sourced), and the login rc's text, the only place a *shadowing* redefinition after the source
/// line is visible. Quiet for a shell bn ships no wrappers for — `bn shell-init` emits bash and zsh
/// only. The classification itself is pure ([`bn_core::doctor::shell_init_problem`]).
fn shell_init_finding() -> Option<bn_core::doctor::Finding> {
    use bn_core::doctor::{Finding, ShellInitFinding, SHELL_WRAPPER_MARKER};
    let shell_path = std::env::var("SHELL").unwrap_or_default();
    let shell = std::path::Path::new(&shell_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let home = dirs::home_dir().unwrap_or_default();
    let candidates: Vec<std::path::PathBuf> = match shell.as_str() {
        "zsh" => vec![std::env::var("ZDOTDIR")
            .map(|d| std::path::Path::new(&d).join(".zshrc"))
            .unwrap_or_else(|_| home.join(".zshrc"))],
        "bash" => vec![home.join(".bashrc"), home.join(".bash_profile")],
        _ => return None,
    };
    // `read_to_string` follows the symlink a dotfiles repo usually leaves here; the first readable
    // candidate is the one the shell would have sourced.
    let (rc_path, rc) = candidates
        .into_iter()
        .find_map(|p| std::fs::read_to_string(&p).ok().map(|body| (p, body)))?;
    let marker = std::env::var(SHELL_WRAPPER_MARKER).ok();
    let problem = bn_core::doctor::shell_init_problem(&rc, marker.as_deref())?;
    let (label, fix) = problem.describe(&shell);
    Some(Finding::ShellInit(ShellInitFinding {
        shell,
        rc_path: rc_path.to_string_lossy().into_owned(),
        problem: label.to_string(),
        fix,
    }))
}

pub(crate) fn emit_doctor(cfg: &Config, stale_days: i64, brief: bool, as_json: bool) {
    // #534: reconcile stale `wt-stuck:*` notices (worktree reaped out of band) so the attention
    // fold below reflects reality. Skipped in --brief (the fast SessionStart hook) — the wt-cleanup
    // cron keeps the set clean, so the brief path just reads the already-reconciled state.
    if !brief {
        wt::reconcile_stuck(cfg);
    }
    let agent_leases = (!brief).then(agent_lease::reconcile);
    // Audit the reconciled lease registry for main↔control invariant violations (a non-control
    // agent on main/master, or a control agent off it). The acquire-time guard prevents new ones,
    // so this only fires for pre-guard or hand-edited leases — surfaced so an operator can clear
    // them. Pure over the registry, so it needs no tmux/process probing.
    let lease_violations: Vec<String> = if brief {
        Vec::new()
    } else {
        let path = bn_core::agent::agent_lease_registry_path();
        bn_core::agent::load_agent_leases(&path)
            .map(|reg| bn_core::agent::lease_invariant_violations(&reg))
            .unwrap_or_default()
    };
    // Fold the current repo's pressing todos (overdue / priority A) when we're inside one.
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = bn_core::resolver::repo_root(&cwd);
    let ctx = target_ctx(cfg, None, None);
    let repo_ref = root.as_deref().zip(ctx.as_ref().map(|c| c.repo.as_str()));
    let mut findings = bn_core::doctor::doctor(cfg, &today(), stale_days, repo_ref);
    // Folded in here rather than in the core aggregation because it reads *this process's*
    // environment and login rc — machine state, not notes state — which the notes-scoped
    // aggregation (and its tests) has no business depending on.
    findings.extend(shell_init_finding());
    bn_core::doctor::sort_findings(&mut findings);
    let findings = findings;
    // Machine-level cron drift (global tier): declared-but-unregistered jobs. A fresh box has the
    // manifest and zero timers — surface it here so SessionStart/`bn doctor` flags it unprompted.
    let drift = cron_drift(cfg, None);
    if as_json {
        let mut data = payload::doctor_data(&findings);
        if let (Some(obj), Some(report)) = (data.as_object_mut(), &agent_leases) {
            obj.insert(
                "agent_leases".into(),
                serde_json::to_value(report).unwrap_or(serde_json::Value::Null),
            );
        }
        if let Some(obj) = data.as_object_mut() {
            if !lease_violations.is_empty() {
                obj.insert(
                    "agent_lease_violations".into(),
                    serde_json::to_value(&lease_violations).unwrap_or(serde_json::Value::Null),
                );
            }
        }
        println!("{}", Envelope::success("doctor", data).to_json());
        return;
    }
    if brief {
        // One line for the SessionStart hook; silent when all clear. Drift leads — an unregistered
        // fleet is a machine-setup problem worth fixing before the per-note items.
        if !drift.is_empty() {
            println!(
                "⚠ {} cron(s) declared but not registered — run `bn cron register`{}",
                drift.len(),
                findings
                    .first()
                    .map(|w| format!(" · {}", w.brief_label()))
                    .unwrap_or_default()
            );
        } else if let Some(worst) = findings.first() {
            println!(
                "⚠ {} item{} need attention · {}",
                findings.len(),
                if findings.len() == 1 { "" } else { "s" },
                worst.brief_label()
            );
        }
        if let Some(report) = &agent_leases {
            if report.conflicts > 0 || !report.errors.is_empty() {
                println!(
                    "⚠ agent ownership: {} conflict(s), {} duplicate window(s) reaped, {} error(s)",
                    report.conflicts,
                    report.duplicate_windows_reaped,
                    report.errors.len()
                );
                for error in &report.errors {
                    println!("  {error}");
                }
            }
        }
        return;
    }
    if !drift.is_empty() {
        println!(
            "⚠ {} cron(s) declared but not registered on this machine — run `bn cron register`:",
            drift.len()
        );
        println!("  {}", drift.join(", "));
    }
    // Pointing at `bn cron register` is only useful advice when register would do something. On a
    // machine with no `[machines.<name>]` profile it would not, so name that instead.
    print_cron_disabled_notice(cfg, None);
    if !lease_violations.is_empty() {
        println!(
            "⚠ {} agent lease(s) break the main↔control invariant — run `bn control stop` / reap the offender:",
            lease_violations.len()
        );
        for violation in &lease_violations {
            println!("  {violation}");
        }
    }
    if findings.is_empty() {
        let lease_clear = agent_leases
            .as_ref()
            .is_none_or(|report| report.conflicts == 0 && report.errors.is_empty());
        if drift.is_empty() && lease_clear && lease_violations.is_empty() {
            println!("All clear — nothing needs attention");
        }
        return;
    }
    use bn_core::doctor::Finding;
    for f in &findings {
        match f {
            Finding::Note(n) => {
                println!("{}/{}  [{}]", n.repo, n.branch, n.flags.join(" "));
                println!(
                    "  {} open, {} blockers · idle {}d (last {})",
                    n.open_todos, n.blockers, n.idle_days, n.last_activity
                );
            }
            Finding::Cron(c) => {
                let loc = c.repo.as_deref().unwrap_or("global");
                println!("cron {}  [cron_fail]", c.job);
                println!("  {} · {} failure(s) on record", loc, c.failures);
            }
            Finding::Staff(s) => {
                let overdue_h = s.overdue_secs / 3600;
                println!("staff {}  [missed]", s.agent);
                if s.last_run == 0 {
                    println!("  {} · scheduled run missed (never run)", s.on_calendar);
                } else {
                    println!(
                        "  {} · scheduled run missed ~{overdue_h}h ago (silence ≠ death — no card, no run)",
                        s.on_calendar
                    );
                }
            }
            Finding::Attn(a) => {
                let loc = match (&a.repo, &a.branch) {
                    (Some(r), Some(b)) => format!(" {r}/{b}"),
                    (Some(r), None) => format!(" {r}"),
                    _ => String::new(),
                };
                println!(
                    "{}  [{}] {} ({}){}",
                    a.id, a.severity, a.title, a.source, loc
                );
                if let Some(link) = &a.link {
                    println!("  {link}");
                }
            }
            Finding::Todo(t) => {
                let pri = t.priority.map(|c| format!("({c}) ")).unwrap_or_default();
                let flag = if t.overdue {
                    format!("overdue:{}d", t.overdue_days)
                } else {
                    "todo".to_string()
                };
                println!("{}  [{}] {}{}", t.id, flag, pri, t.summary);
                let branch = t
                    .branch
                    .as_deref()
                    .map(|b| format!("/{b}"))
                    .unwrap_or_default();
                let due = t
                    .due
                    .as_deref()
                    .map(|d| format!(" · due {d}"))
                    .unwrap_or_default();
                println!("  {}{}{}", t.repo, branch, due);
            }
            Finding::Inbox(i) => {
                let loc = match (&i.repo, &i.branch) {
                    (Some(r), Some(b)) => format!(" · hinted {r}/{b}"),
                    _ => String::new(),
                };
                println!("{}  [inbox:{}d] {}", i.id, i.idle_days, i.text);
                println!("  captured {}{} — bn q drain", i.created, loc);
            }
            Finding::ShellInit(s) => {
                println!("shell  [{}] {}", s.shell, s.problem);
                println!("  {} — fix: {}", s.rc_path, s.fix);
            }
            Finding::Mcp(m) => {
                println!(
                    "mcp {}  [unresolvable] '{}' {} ({})",
                    m.server, m.command, m.problem, m.config
                );
                println!("  fix: {}", m.fix);
            }
            Finding::StaleStaff(s) => {
                println!(
                    "staff  [stale:{}d] no briefing card since {}",
                    s.idle_days, s.last_card
                );
                println!(
                    "  the staff home machine may be down — re-home `[staff].home` onto a live box"
                );
            }
            Finding::Backlog(b) => {
                println!(
                    "backlog  [untasked] {} queued item(s) name no task",
                    b.untasked
                );
                for subject in b.sample.iter().take(3) {
                    println!("  {subject}");
                }
                println!(
                    "  these predate #1001 and will run with no durable record; they are left in place deliberately so nothing in flight is orphaned"
                );
            }
            Finding::NotesLeak(l) => {
                println!(
                    "notes  [leak] {} foreign machine subtree(s) under the personal notes root",
                    l.foreign_machines.len()
                );
                println!(
                    "  {} carries {} — they surface on this work box via cross-machine views",
                    l.personal_root,
                    l.foreign_machines.join(", ")
                );
                println!(
                    "  fix: set [personal].notes_root to your work notes repo ({}) or remove the clone",
                    l.work_root
                );
            }
        }
    }
}

/// `bn next` — the ranked action list. Human view shows the top `limit` with a "+N more" tail
/// and a pointer to the views that own the overflow; `--json` returns the full list (matching the
/// MCP tool) so an agent can plan over everything. Momentum-aware by default (#246): actions on
/// the currently-punched-in repo/branch rank first within their priority band; `--no-momentum`
/// keeps the pure priority/staleness order.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_next(
    cfg: &Config,
    limit: usize,
    brief: bool,
    cached: bool,
    refresh: bool,
    clear_cache: bool,
    no_momentum: bool,
    fit: bool,
    as_json: bool,
) {
    use bn_core::clock::Clock;
    let clock = bn_core::clock::RealClock;
    let today = clock.today();
    let now = clock.now_epoch_secs();

    if clear_cache {
        match bn_core::next::clear_cache() {
            Ok(cleared) => {
                if as_json {
                    println!(
                        "{}",
                        Envelope::success(
                            "next_clear_cache",
                            serde_json::json!({
                                "cleared": cleared,
                                "path": bn_core::next::cache_path().display().to_string(),
                            }),
                        )
                        .to_json()
                    );
                } else if cleared {
                    println!("Cleared attention cache");
                } else {
                    println!("No attention cache to clear");
                }
            }
            Err(e) => {
                if as_json {
                    println!(
                        "{}",
                        Envelope::failure("next_clear_cache", "io", e.to_string()).to_json()
                    );
                } else {
                    eprintln!("bn: could not clear attention cache: {e}");
                }
                std::process::exit(1);
            }
        }
        return;
    }

    // `--refresh` is the cron's write-only path: recompute, rewrite the cache, say nothing.
    if refresh {
        if let Err(e) = bn_core::next::refresh_cache(cfg, &today, now) {
            eprintln!("bn: could not refresh attention cache: {e}");
        }
        // Warm the doctor picker's snapshot on the same `attention-sync` tick, so prefix-J
        // (`bn doctor --pick`) opens instantly too instead of re-walking every branch note.
        if let Err(e) =
            bn_core::doctor::refresh_cache(cfg, &today, bn_core::doctor::DEFAULT_STALE_DAYS, now)
        {
            eprintln!("bn: could not refresh doctor attention cache: {e}");
        }
        return;
    }

    // `--cached` serves the prefix-J picker an instant snapshot; on a miss/stale cache it computes
    // once and warms the cache so the next open is instant too. Otherwise compute live as before.
    let actions = if cached {
        bn_core::next::cached_or_compute(cfg, &today, now)
    } else {
        bn_core::next::next_actions(cfg, &today, now)
    };
    // Momentum re-rank at read time (after any cache read — the cache stays punch-free): a no-op
    // with no open punch, so `--no-momentum` and a punched-out desk are the same order.
    let actions = if no_momentum {
        actions
    } else {
        bn_core::next::apply_momentum(cfg, actions, &today, now)
    };

    // The gap to the next meeting from the offline cache (#247): drives the header, and — with
    // `--fit` — sizes the plan band to the time actually left. `None` when there's no fresh
    // upcoming event, which makes `--fit` a clean no-op.
    let gap = bn_core::meetings::gap(now);
    let (actions, untimed, dropped) = if fit {
        let report = bn_core::calib::report(cfg);
        let plan = bn_core::next::fit_plan(actions, gap.as_ref(), &report, &cfg.coarse_sizes);
        (plan.kept, plan.unsized_tail, plan.dropped)
    } else {
        (actions, Vec::new(), 0)
    };

    // Routine snapshot (#801): folded into both the JSON envelope and, when it's suppressing
    // non-urgent guidance, a transparent one-line explanation in the human view — the filtering
    // itself already happened inside `next_actions`.
    let routine_snap = bn_core::routine::current_snapshot(cfg, now);

    if as_json {
        let mut data = payload::next_data(&actions);
        if let Some(obj) = data.as_object_mut() {
            if let Some(g) = &gap {
                obj.insert(
                    "gap".into(),
                    serde_json::to_value(g).unwrap_or(serde_json::Value::Null),
                );
            }
            if let Some(snap) = &routine_snap {
                obj.insert(
                    "routine".into(),
                    serde_json::to_value(snap).unwrap_or(serde_json::Value::Null),
                );
            }
            if fit {
                obj.insert(
                    "unsized".into(),
                    payload::next_data(&untimed)["actions"].clone(),
                );
                obj.insert("dropped".into(), serde_json::json!(dropped));
            }
        }
        println!("{}", Envelope::success("next", data).to_json());
        return;
    }
    if brief {
        // One line for the SessionStart hook; silent when the desk is clear.
        if let Some(top) = actions.first() {
            println!(
                "▶ {} next action{} · {}",
                actions.len(),
                if actions.len() == 1 { "" } else { "s" },
                top.title
            );
        }
        return;
    }

    // Gap header (#247): whenever a meeting is < 2h out, or `--fit` is actively sizing to it, so
    // the dead-zone framing ("only 38m — what fits?") leads the list. `render_gap` handles the
    // "no upcoming events" wording, but with no gap at all we simply print no header.
    if let Some(g) = &gap {
        if fit || g.minutes <= bn_core::meetings::GAP_HEADER_WINDOW_MINS {
            println!(
                "{} {}\n",
                bn_core::glyph::CLOCK,
                bn_core::meetings::render_gap(Some(g))
            );
        }
    }
    // Routine explanation (#801): only when it's actually suppressing something, so an
    // unprotected work/learning block (or routine disabled) stays silent.
    if let Some(reason) = routine_snap.as_ref().and_then(|s| s.explanation()) {
        println!("{reason}\n");
    }
    // Notification gate: same discipline as the routine line above — say it once when toasts were
    // actually withheld, stay silent otherwise. The withheld items are already in the list below,
    // so this only explains why the desktop stayed quiet about them.
    {
        let mut ledger = bn_core::notify::policy::load_ledger();
        ledger.roll(&today);
        // Two different stories, so two different lines: a policy hold is the gate working and
        // needs nothing from you, while an undeliverable toast means the channel itself is
        // broken and every interrupt raised today was lost. Summed, the second hid inside the
        // first.
        let undeliverable = ledger.undeliverable();
        if undeliverable > 0 {
            println!(
                "{} {undeliverable} notification(s) could not be delivered today — no working toast backend — bn notify status\n",
                bn_core::glyph::BELL_SLASH
            );
        }
        let withheld = ledger.withheld_by_policy();
        if withheld > 0 {
            println!("{} {withheld} notification(s) held back today — bn notify status\n", bn_core::glyph::BELL);
        }
    }

    if actions.is_empty() && untimed.is_empty() {
        println!("Nothing queued — desk is clear ✓");
        return;
    }
    // Show the top `limit` (already priority-sorted), grouped into dashboard sections so a long
    // mixed list stays scannable. Group only the shown slice, so the "+N more" tail still reflects
    // the real overflow.
    let shown = &actions[..actions.len().min(limit)];
    let groups = bn_core::next::group_by_category(shown);
    for (gi, (cat, items)) in groups.iter().enumerate() {
        if gi > 0 {
            println!();
        }
        println!("{}", cat.header());
        for a in items {
            // Momentum badge (#246) — keeps the ranking explainable: `·here` = the repo/branch
            // you're punched into right now, `·near` = same repo, different branch.
            let badge = match a.momentum {
                Some(bn_core::next::Momentum::Branch) => " ·here",
                Some(bn_core::next::Momentum::Repo) => " ·near",
                None => "",
            };
            println!("  {} {}{badge}", a.kind_glyph(), a.title);
            println!("     {}", a.detail);
            if let Some(link) = &a.link {
                println!("     ↗ {link}");
            }
        }
    }
    let extra = actions.len().saturating_sub(limit);
    if extra > 0 {
        println!("\n… +{extra} more — see `bn doctor`, `bn reviews`, `bn daily`");
    }

    // `--fit` tails: the goals that didn't fit (counted, not listed), then the un-estimated ones
    // surfaced in full — never silently hidden, so an unsized backlog stays visible for `--est`.
    if fit {
        if dropped > 0 {
            let (plural, verb) = if dropped == 1 {
                ("", "doesn't")
            } else {
                ("s", "don't")
            };
            println!("\n… +{dropped} goal{plural} {verb} fit this gap");
        }
        if !untimed.is_empty() {
            println!("\nUNSIZED — add `--est` to size these against the gap");
            for a in &untimed {
                println!("  {} {}", a.kind_glyph(), a.title);
                if let Some(link) = &a.link {
                    println!("     ↗ {link}");
                }
            }
        }
    }
}

/// `bn gap` (#247) — print the gap to your next meeting from the offline meetings cache, or
/// "no upcoming events" when there's no fresh upcoming event. Read-only, no network. `--json`
/// returns `{ "gap": Gap | null }`.
pub(crate) fn emit_gap(as_json: bool) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let gap = bn_core::meetings::gap(now);
    if as_json {
        println!(
            "{}",
            Envelope::success("gap", serde_json::json!({ "gap": gap })).to_json()
        );
        return;
    }
    println!("{}", bn_core::meetings::render_gap(gap.as_ref()));
}

/// `bn gap ingest` (#247) — the `meetings` cron's write path: read JSON/JSONL meetings from stdin
/// and REPLACE the cache for `date` (default today). Prints the count ingested; `--json` returns
/// `{ "count", "date" }`. A garbage line in the pipe is skipped, never fatal.
pub(crate) fn gap_ingest(date: Option<String>, as_json: bool) {
    use bn_core::clock::Clock;
    use std::io::Read;
    let clock = bn_core::clock::RealClock;
    let today = date.unwrap_or_else(|| clock.today());
    let now = clock.now_epoch_secs();
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        eprintln!("bn gap ingest: failed to read stdin");
        std::process::exit(1);
    }
    let events = bn_core::meetings::parse_items(&input);
    match bn_core::meetings::ingest(events, &today, now) {
        Ok(n) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success(
                        "gap_ingest",
                        serde_json::json!({ "count": n, "date": today }),
                    )
                    .to_json()
                );
            } else {
                println!(
                    "meetings cache: {n} event{} for {today}",
                    if n == 1 { "" } else { "s" }
                );
            }
        }
        Err(e) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::failure("gap_ingest", "io", e.to_string()).to_json()
                );
            } else {
                eprintln!("bn gap ingest: {e}");
            }
            std::process::exit(1);
        }
    }
}

/// `bn wrap` (#98) — the end-of-day digest. Resolves the current note (best-effort, for the
/// "un-noted commits" marker), reads git/gh through the real runner, then prints the digest.
/// Silent, zero-exit when the day is clean — the property the deleted poller lacked; the folded-in
/// PM shutdown wrap (#274) and `bn run wrap-digest` both rely on this no-output-on-clean behaviour.
pub(crate) fn emit_wrap(cfg: &Config, as_json: bool) {
    use bn_core::runner::RealRunner;
    let today = today();

    // A resolved note dates "un-noted commits"; with no note (cron in an arbitrary cwd) we pass
    // `today` so that section stays empty rather than flagging every commit.
    let note_since = target_ctx(cfg, None, None)
        .and_then(|ctx| bn_core::store::read(&cfg.note_file(&ctx)).ok())
        .map(|note| bn_core::reports::note_last_activity(&note))
        .unwrap_or_else(|| today.clone());

    let w = bn_core::reports::wrap(cfg, &today, &note_since, &RealRunner);

    if as_json {
        println!(
            "{}",
            Envelope::success("wrap", payload::wrap_data(&w)).to_json()
        );
        return;
    }
    if w.is_empty() {
        return; // clean day — silent, clean exit
    }
    print!("{}", bn_core::reports::render_wrap(&w));
}

/// `bn rate` (#110) — record today's DXI self-rating beside the daily files. Only acts when DXI
/// is opted in via config; otherwise it's a no-op nudge (the prompt is never forced).
pub(crate) fn emit_rate(cfg: &Config, focus: u8, friction: u8, mood: Option<&str>, as_json: bool) {
    if !resolver::JobConfig::load().dxi_enabled {
        if as_json {
            println!(
                "{}",
                Envelope::failure(
                    "rate",
                    "dxi_disabled",
                    "DXI is disabled — enable [dxi] in config"
                )
                .to_json()
            );
        } else {
            eprintln!("bn rate: DXI is disabled — set [dxi].enabled in config.toml to opt in");
        }
        return;
    }
    match bn_core::daily::rate(cfg, &today(), focus, friction, mood) {
        Ok(r) => {
            if as_json {
                let v = serde_json::to_value(&r).unwrap_or_default();
                println!("{}", Envelope::success("rate", v).to_json());
            } else {
                let mood = r
                    .mood
                    .as_deref()
                    .map(|m| format!(" · {m}"))
                    .unwrap_or_default();
                println!(
                    "Rated {} — focus {}/5 · friction {}/5{mood}",
                    r.date, r.focus, r.friction
                );
            }
        }
        Err(e) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::failure("rate", "store_error", e.to_string()).to_json()
                );
            } else {
                fail(&format!("bn rate: {e}"));
            }
        }
    }
}
