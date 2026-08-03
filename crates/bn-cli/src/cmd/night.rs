//! `bn night` — the overnight autonomous shift: plan/run/status/report.
use crate::*;

/// The kill-switch marker file: present == the shift is OFF (`bn night off`), absent == on. Lives
/// in the machine-local state dir, so the cron can check it before doing anything.
pub(crate) fn night_off_flag() -> std::path::PathBuf {
    resolver::state_dir().join("night-off")
}

/// Whether the kill switch is engaged.
pub(crate) fn night_is_off() -> bool {
    night_off_flag().exists()
}

/// The base branch the shift diffs against for file-overlap (and forks from). main by default;
/// `$BN_NIGHT_BASE` overrides.
pub(crate) fn night_base() -> String {
    std::env::var("BN_NIGHT_BASE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "main".to_string())
}

/// Gather tonight's ranked, status-tagged candidates: the promoted-todo queue (open auto-eligible
/// attention items) in `bn next` order, each paired with a [`bn_core::night::Candidate`] carrying
/// its dirty / live-session / changed-files status. The [`bn_core::attention::AttnItem`] is kept
/// alongside so the run step can dispatch a pick by id.
pub(crate) fn night_candidates(
    cfg: &Config,
) -> Vec<(bn_core::attention::AttnItem, bn_core::night::Candidate)> {
    let open = bn_core::attention::open_auto_eligible(cfg, &today());
    if open.is_empty() {
        return Vec::new();
    }
    // Rank by `bn next` (warm cache) — the same ordering brain as the attn board and picker.
    let ranked_ids: Vec<String> = {
        use bn_core::clock::Clock;
        let clock = bn_core::clock::RealClock;
        bn_core::next::cached_or_compute(cfg, &clock.today(), clock.now_epoch_secs())
            .into_iter()
            .filter_map(|a| a.attn_id)
            .collect()
    };
    let ranked = rank_to_start(open, &ranked_ids);
    // The live fleet (fleet knows): (repo, sanitized-branch) pairs a live agent window owns.
    let live: std::collections::HashSet<(String, String)> = agent_live::collect(
        cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    )
    .into_iter()
    .map(|v| (v.repo, v.branch))
    .collect();
    let base = night_base();
    ranked
        .into_iter()
        .filter_map(|item| {
            let repo = item.repo.clone()?;
            let branch = item.branch.clone()?;
            let (_, dirty, files) = wt::branch_worktree_status(cfg, &repo, &branch, &base);
            let live_session = live.contains(&(
                repo.to_lowercase(),
                wt::sanitize_branch(&branch).to_lowercase(),
            ));
            let cand = bn_core::night::Candidate {
                id: item.id.clone(),
                repo,
                branch,
                title: item.title.clone(),
                files,
                dirty,
                live_session,
            };
            Some((item, cand))
        })
        .collect()
}

pub(crate) fn emit_night_status(cfg: &Config, as_json: bool) {
    let _ = cfg;
    let jc = resolver::JobConfig::load();
    let off = night_is_off();
    let window = bn_core::night::parse_window(&jc.night_window);
    let now_min = bn_core::night::local_minutes(&bn_core::clock::RealClock);
    let in_win = match (window, now_min) {
        (Some(w), Some(m)) => Some(bn_core::night::in_window(m, w)),
        _ => None,
    };
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "night",
                serde_json::json!({
                    "enabled": jc.night_enabled,
                    "kill_switch_off": off,
                    "window": jc.night_window,
                    "in_window": in_win,
                    "max_dispatch": jc.night_max_dispatch,
                    "repos": jc.night_repos,
                }),
            )
            .to_json()
        );
        return;
    }
    println!("night shift — nightly fleet planner");
    println!(
        "  enabled:      {}",
        if jc.night_enabled {
            "yes".to_string()
        } else {
            "no — set [night] enabled = true (or $BN_NIGHT_ENABLED=1)".to_string()
        }
    );
    println!(
        "  kill switch:  {}",
        if off {
            "OFF (engaged — `bn night on` to re-enable)"
        } else {
            "clear"
        }
    );
    let when = match in_win {
        Some(true) => " (open now)",
        Some(false) => " (closed now)",
        None => "",
    };
    println!("  window:       {}{}", jc.night_window, when);
    println!("  max_dispatch: {}", jc.night_max_dispatch);
    if jc.night_repos.is_empty() {
        println!("  repos:        none — allowlist a repo in [night] repos to opt in");
    } else {
        println!("  repos:        {}", jc.night_repos.join(", "));
    }
}

/// `bn night off` / `bn night on` — toggle the kill switch (a state-dir marker the cron checks
/// first). `on = true` clears it; `on = false` engages it.
pub(crate) fn emit_night_switch(on: bool, as_json: bool) {
    let flag = night_off_flag();
    if on {
        let existed = flag.exists();
        if existed {
            let _ = std::fs::remove_file(&flag);
        }
    } else {
        if let Some(p) = flag.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let _ = std::fs::write(&flag, bn_core::util::now_epoch_secs().to_string());
    }
    if as_json {
        println!(
            "{}",
            Envelope::success("night", serde_json::json!({ "kill_switch_off": !on })).to_json()
        );
    } else if on {
        println!("night shift ENABLED — kill switch cleared");
    } else {
        println!("night shift DISABLED — kill switch engaged (`bn night on` to re-enable)");
    }
}

/// `bn night plan` — the dry-run: what tonight's shift would pick and why each candidate is
/// skipped. Pure read: no worktrees, agents, or audit entries.
pub(crate) fn emit_night_plan(cfg: &Config, as_json: bool) {
    let jc = resolver::JobConfig::load();
    let cands = night_candidates(cfg);
    let only: Vec<bn_core::night::Candidate> = cands.iter().map(|(_, c)| c.clone()).collect();
    let plan = bn_core::night::plan(&only, &jc.night_repos, jc.night_max_dispatch);
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "night",
                serde_json::json!({
                    "dry_run": true,
                    "enabled": jc.night_enabled,
                    "kill_switch_off": night_is_off(),
                    "max_dispatch": jc.night_max_dispatch,
                    "repos": jc.night_repos,
                    "picks": plan.picks,
                    "skips": plan.skips,
                }),
            )
            .to_json()
        );
        return;
    }
    println!("night shift — dry run (no side effects)");
    if night_is_off() {
        println!("  ⚠ kill switch ENGAGED — a real run would no-op (`bn night on`)");
    }
    if !jc.night_enabled {
        println!("  ⚠ not enabled — set [night] enabled = true (or $BN_NIGHT_ENABLED=1)");
    }
    print_night_picks(&plan, jc.night_max_dispatch);
    print_night_skips(&plan);
}

/// Shared render: the picks block for `plan` / `run`.
pub(crate) fn print_night_picks(plan: &bn_core::night::NightPlan, cap: usize) {
    if plan.picks.is_empty() {
        println!("  picks: none");
        return;
    }
    println!("  picks ({} / cap {}):", plan.picks.len(), cap);
    for p in &plan.picks {
        println!("    • {} — {}/{} · {}", p.id, p.repo, p.branch, p.title);
    }
}

/// Shared render: the skips block for `plan` / `run`.
pub(crate) fn print_night_skips(plan: &bn_core::night::NightPlan) {
    if plan.skips.is_empty() {
        return;
    }
    println!("  skipped ({}):", plan.skips.len());
    for s in &plan.skips {
        println!(
            "    – {} — {}/{}: {}",
            s.id,
            s.repo,
            s.branch,
            s.reason.label()
        );
    }
}

/// Print a decline reason (a run that no-ops on a gate) in text or JSON.
pub(crate) fn night_decline(reason: &str, as_json: bool) {
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "night",
                serde_json::json!({ "ran": false, "reason": reason })
            )
            .to_json()
        );
    } else {
        println!("night shift: {reason}");
    }
}

/// `bn night run` (and the `night-shift` cron) — plan, then dispatch each pick into its worktree
/// with an agent, recording an audit entry per item. Gated by the kill switch, `[night] enabled`,
/// and the run window (declines outside it unless `force`). The dispatched agents close the loop
/// overnight via `bn agent done` (the verify gate + draft PR live there); `bn night report`
/// digests the results in the morning.
pub(crate) fn emit_night_run(cfg: &Config, max: Option<usize>, force: bool, as_json: bool) {
    let jc = resolver::JobConfig::load();
    // Kill switch first — the cron checks this before anything else.
    if night_is_off() {
        night_decline("kill switch engaged — `bn night on` to re-enable", as_json);
        return;
    }
    if !jc.night_enabled {
        night_decline(
            "not enabled — set [night] enabled = true (or $BN_NIGHT_ENABLED=1)",
            as_json,
        );
        return;
    }
    if !force {
        match bn_core::night::parse_window(&jc.night_window) {
            Some(w) => {
                let m = bn_core::night::local_minutes(&bn_core::clock::RealClock).unwrap_or(0);
                if !bn_core::night::in_window(m, w) {
                    night_decline(
                        &format!(
                            "outside the night window {} — use --force to run now",
                            jc.night_window
                        ),
                        as_json,
                    );
                    return;
                }
            }
            // A malformed window must fail *closed* — never dispatch agents on a typo'd safety gate.
            None if !jc.night_window.trim().is_empty() => {
                night_decline(
                    &format!(
                        "invalid night window '{}' (want HH:MM-HH:MM) — use --force to run now",
                        jc.night_window
                    ),
                    as_json,
                );
                return;
            }
            // An empty window means "no window" — always allowed.
            None => {}
        }
    }
    let cap = max.unwrap_or(jc.night_max_dispatch);
    let cands = night_candidates(cfg);
    let by_id: std::collections::HashMap<String, bn_core::attention::AttnItem> = cands
        .iter()
        .map(|(item, _)| (item.id.clone(), item.clone()))
        .collect();
    let only: Vec<bn_core::night::Candidate> = cands.iter().map(|(_, c)| c.clone()).collect();
    let plan = bn_core::night::plan(&only, &jc.night_repos, cap);
    // Audit the run summary (what was picked + why each skip) before dispatching.
    let _ = bn_core::night::log_run(&bn_core::clock::RealClock, &plan);

    let mut dispatched: Vec<(String, bool)> = Vec::new();
    for pick in &plan.picks {
        let Some(item) = by_id.get(&pick.id) else {
            continue;
        };
        match crate::cmd::attn::run_attn_start_attributed(
            cfg,
            item,
            true,
            Some(bn_core::cost::WorkClass::Scheduled),
        ) {
            Some(o) => {
                let _ = bn_core::night::log_dispatch(
                    &bn_core::clock::RealClock,
                    &pick.id,
                    &pick.repo,
                    &pick.branch,
                    &pick.title,
                    o.launched,
                );
                dispatched.push((pick.id.clone(), o.launched));
            }
            None => eprintln!(
                "night: could not open a worktree for {} ({}/{})",
                pick.id, pick.repo, pick.branch
            ),
        }
    }

    if as_json {
        let disp: Vec<serde_json::Value> = dispatched
            .iter()
            .map(|(id, launched)| serde_json::json!({ "id": id, "launched": launched }))
            .collect();
        println!(
            "{}",
            Envelope::success(
                "night",
                serde_json::json!({
                    "ran": true,
                    "max_dispatch": cap,
                    "picks": plan.picks,
                    "skips": plan.skips,
                    "dispatched": disp,
                }),
            )
            .to_json()
        );
        return;
    }
    println!(
        "night shift — dispatched {} of {} pick(s) (cap {})",
        dispatched.len(),
        plan.picks.len(),
        cap
    );
    for (id, launched) in &dispatched {
        if let Some(p) = plan.picks.iter().find(|p| &p.id == id) {
            let tail = if *launched {
                ""
            } else {
                " (window opened, agent not launched)"
            };
            println!(
                "    • {} — {}/{} · {}{}",
                p.id, p.repo, p.branch, p.title, tail
            );
        }
    }
    print_night_skips(&plan);
    println!("  morning: `bn night report` digests the results into your daily plan");
}

/// `bn night report` — the morning digest: what the shift dispatched in the last `hours` (from the
/// audit log), where each landed (from its attention lifecycle), and a `bn daily` review goal per
/// PR waiting on review. Idempotent: a goal is added at most once per PR link.
pub(crate) fn emit_night_report(cfg: &Config, hours: i64, as_json: bool) {
    let cutoff = bn_core::util::now_epoch_secs() - hours.max(0) * 3600;
    let dispatches = bn_core::night::read_dispatches_since(cutoff);
    // Current lifecycle state per in-flight item, for outcome classification.
    let started: std::collections::HashMap<String, bn_core::attention::StartedItem> =
        bn_core::attention::started_items()
            .into_iter()
            .map(|s| (s.item.id.clone(), s))
            .collect();
    let items: Vec<bn_core::night::NightReportItem> = dispatches
        .iter()
        .map(|d| {
            let (is_started, done, link) = match started.get(&d.id) {
                Some(s) => (true, s.done, s.item.link.clone()),
                None => (false, false, None),
            };
            let outcome = bn_core::night::classify_outcome(is_started, done, link.is_some());
            bn_core::night::NightReportItem {
                id: d.id.clone(),
                repo: d.repo.clone(),
                branch: d.branch.clone(),
                title: d.title.clone(),
                outcome,
                link,
            }
        })
        .collect();

    // Auto-add a daily goal per PR waiting on review (idempotent by link).
    let goals = bn_core::night::report_goals(&items);
    let mut added: Vec<bn_core::night::NightGoal> = Vec::new();
    if !goals.is_empty() {
        let existing: std::collections::HashSet<String> =
            bn_core::daily::ensure_today(cfg, &today())
                .map(|d| {
                    d.front
                        .goals
                        .iter()
                        .filter_map(|g| g.link.clone())
                        .collect()
                })
                .unwrap_or_default();
        for g in &goals {
            if let Some(l) = &g.link {
                if existing.contains(l) {
                    continue;
                }
            }
            if bn_core::daily::add(cfg, &today(), &g.text, g.link.clone(), None).is_ok() {
                added.push(g.clone());
            }
        }
    }

    if as_json {
        println!(
            "{}",
            Envelope::success(
                "night",
                serde_json::json!({ "items": items, "goals_added": added }),
            )
            .to_json()
        );
        return;
    }
    if items.is_empty() {
        println!("night shift — no dispatches in the last {hours}h");
        return;
    }
    println!(
        "night shift — morning digest ({} dispatched in the last {hours}h)",
        items.len()
    );
    for it in &items {
        match &it.link {
            Some(l) => println!(
                "    • {} — {}/{} · {} [{}] → {}",
                it.id,
                it.repo,
                it.branch,
                it.title,
                it.outcome.label(),
                l
            ),
            None => println!(
                "    • {} — {}/{} · {} [{}]",
                it.id,
                it.repo,
                it.branch,
                it.title,
                it.outcome.label()
            ),
        }
    }
    if added.is_empty() {
        println!("  daily: no new review goals (already queued, or none ready)");
    } else {
        println!("  daily: added {} review goal(s):", added.len());
        for g in &added {
            println!("    ↳ {}", g.text);
        }
    }
}
