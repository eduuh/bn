//! `bn insights`/`bn metrics`/`bn ingest`/`bn event` — analytics + ingestion.
use crate::*;

pub(crate) fn emit_insights(
    cfg: &bn_core::resolver::Config,
    since: Option<i64>,
    write: bool,
    set_baseline: bool,
    as_json: bool,
) {
    let ins = bn_core::insights::insights(cfg, &today(), since);
    if set_baseline {
        let path = match bn_core::insights::set_baseline(cfg, &ins) {
            Ok(p) => p,
            Err(e) => fail(&format!("bn insights --set-baseline: {e}")),
        };
        if as_json {
            println!(
                "{}",
                Envelope::success(
                    "insights",
                    serde_json::json!({
                        "baseline_set": path.display().to_string(),
                        "date": ins.end,
                        "cycle_median_hours": ins.pr_cycle.median_hours,
                        "deep_work_ratio": ins.deep_work.ratio,
                        "bn_time_ratio": ins.scoreboard.bn_time.ratio,
                    }),
                )
                .to_json()
            );
        } else {
            println!(
                "scoreboard baseline recorded ({}): {}",
                ins.end,
                path.display()
            );
        }
        return;
    }
    if write {
        let week = bn_core::util::iso_week(&ins.end)
            .unwrap_or_else(|| fail("bn insights --write: can't derive the ISO week for today"));
        let path = cfg.insights_file(&week);
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                fail(&format!("bn insights --write: {e}"));
            }
        }
        if let Err(e) = std::fs::write(&path, bn_core::insights::render(&ins)) {
            fail(&format!("bn insights --write: {e}"));
        }
        if as_json {
            println!(
                "{}",
                Envelope::success(
                    "insights",
                    serde_json::json!({ "written": path.display().to_string(), "week": week }),
                )
                .to_json()
            );
        } else {
            println!("insights written: {}", path.display());
        }
        return;
    }
    if as_json {
        println!(
            "{}",
            Envelope::success("insights", payload::insights_data(&ins)).to_json()
        );
    } else {
        print!("{}", bn_core::insights::render(&ins));
    }
}

pub(crate) fn emit_metrics(
    cfg: &bn_core::resolver::Config,
    since: i64,
    snapshot: bool,
    trend: bool,
    as_json: bool,
) {
    use bn_core::clock::Clock;
    let clock = bn_core::clock::RealClock;
    let now_today = clock.today();
    let now = clock.now_epoch_secs();
    if trend {
        emit_metrics_trend(cfg, as_json);
        return;
    }
    let s = bn_core::metrics::scorecard(
        cfg,
        &bn_core::runner::RealRunner,
        now,
        since,
        &now_today,
    );
    if snapshot {
        match bn_core::metrics::write_snapshot(cfg, &s, &now_today) {
            Ok(p) => println!("snapshot written: {}", p.display()),
            Err(e) => fail(&format!("bn metrics --snapshot: {e}")),
        }
        return;
    }
    if as_json {
        println!(
            "{}",
            Envelope::success("metrics", payload::scorecard_data(&s)).to_json()
        );
        return;
    }
    let m = &s.pr;
    let prev = bn_core::metrics::latest_snapshot_before(cfg, &now_today);
    let opt_days = |d: Option<i64>| d.map(|n| format!("{n}d")).unwrap_or_else(|| "—".into());
    let lag = match (s.close_out.lag_median_hours, s.close_out.lag_p90_hours) {
        (Some(med), Some(p90)) => format!("median {} · p90 {}", dur(med), dur(p90)),
        _ => "—".to_string(),
    };

    // Anomalies — personal baselines lead the card so the eye lands on what changed vs your own
    // median. "baselining…" until enough history accrues; quiet when nothing's off.
    let b = &s.baselines;
    if !b.sufficient {
        println!(
            "Baselines  baselining… ({} of {} snapshots in the {}-day window)",
            b.samples,
            bn_core::metrics::Baselines::min_samples(),
            b.window_days
        );
    } else {
        let anomalies = b.anomalies();
        if anomalies.is_empty() {
            println!("Baselines  all metrics within your normal range");
        } else {
            println!("Anomalies — vs your {}-day baseline", b.window_days);
            for p in anomalies {
                println!(
                    "  ⚑ {:<12} {}  (median {:.1} · p25 {:.1} · p75 {:.1})",
                    p.label,
                    p.note.as_deref().unwrap_or(""),
                    p.median,
                    p.p25,
                    p.p75
                );
            }
        }
    }

    // WIP guard — forward-looking: what to finish next and when the desk clears. Speaks up only
    // when in-flight branches are above the healthy band; otherwise stays quiet.
    let w = &s.wip_guard;
    if !w.healthy {
        println!("WIP guard");
        let stale = if w.stale > 0 {
            format!("  ({} stale ≥4d)", w.stale)
        } else {
            String::new()
        };
        println!("  in-flight   {}{stale}", w.in_flight);
        if let (Some(rec), Some(reason)) = (&w.recommend, &w.recommend_reason) {
            println!("  finish next {rec}  ({reason})");
        }
        match &w.clear_by {
            Some(date) => println!("  clear-by    {date}  (at current merge cadence)"),
            None => println!("  clear-by    —  (no merges in window to forecast from)"),
        }
    }

    // Close-out velocity — the headline: what slows down finishing work.
    println!("Close-out velocity — last {} days", s.window_days);
    println!("  WIP            {}  (open branches w/ todos)", s.wip);
    println!("  open branches  {}", s.open_branches);
    println!(
        "  branch age     median {} · oldest {}",
        opt_days(s.branch_age_median_days),
        opt_days(s.branch_age_max_days)
    );
    println!(
        "  close-out lag  {lag}  ({} closed)",
        s.close_out.closed_in_window
    );
    if s.partial.items > 0 {
        println!(
            "  in progress    {} todo(s) at avg {:.0}%  (bn done --pct)",
            s.partial.items,
            s.partial.avg_pct.unwrap_or(0.0)
        );
    }
    if let Some(p) = &prev {
        // Direction since the last snapshot — lower lag/WIP is better, more merges is better.
        let lag_d = delta_hours(
            p.card.close_out.lag_median_hours,
            s.close_out.lag_median_hours,
        );
        let wip_d = delta_int(p.card.wip as i64, s.wip as i64);
        let merged_d = delta_int(p.card.pr.merged as i64, s.pr.merged as i64);
        println!(
            "  vs {}     lag {lag_d} · WIP {wip_d} · merged {merged_d}",
            p.date
        );
    }

    // Throughput — PR flow.
    let pct = m
        .completion_rate
        .map(|r| format!("{:.0}%", r * 100.0))
        .unwrap_or_else(|| "—".to_string());
    let cycle = match (m.cycle_median_hours, m.cycle_p90_hours) {
        (Some(med), Some(p90)) => format!("median {} · p90 {}", dur(med), dur(p90)),
        _ => "—".to_string(),
    };
    println!("PRs");
    println!("  opened      {}", m.opened);
    println!("  merged      {}", m.merged);
    println!("  closed      {}", m.closed);
    let lead = match (m.lead_median_hours, m.lead_p90_hours) {
        (Some(med), Some(p90)) => format!("median {} · p90 {}", dur(med), dur(p90)),
        _ => "—  (needs pr-observe lead_secs)".to_string(),
    };
    println!("  in-flight   {}  (open now)", m.in_flight);
    println!("  completion  {pct}  (merged of finished)");
    println!("  cycle time  {cycle}  (open→merge)");
    println!("  lead time   {lead}  (first commit→merge)");
    // Review latency — time-to-first-review on your PRs, the code-review bottleneck.
    let rl = &s.review_latency;
    let review = match (rl.first_review_median_hours, rl.first_review_p90_hours) {
        (Some(med), Some(p90)) => format!(
            "median {} · p90 {}  ({} reviewed)",
            dur(med),
            dur(p90),
            rl.reviewed
        ),
        _ => "—  (needs pr-observe pr_review events)".to_string(),
    };
    println!("  review wait {review}  (open→first review)");

    // Flow — uninterrupted focus from the activity stream.
    println!("Flow");
    println!(
        "  focus time  {}h total · longest block {}h",
        s.flow.total_minutes / 60,
        s.flow.longest_block_minutes / 60
    );
    // Fragmentation — today's context-switch / focus-block picture (cognitive-load axis).
    let fr = &s.fragmentation;
    println!(
        "  today       {} switch{} across {} branch{} · longest block {}m",
        fr.switches,
        if fr.switches == 1 { "" } else { "es" },
        fr.branches,
        if fr.branches == 1 { "" } else { "es" },
        fr.longest_block_minutes
    );

    // Impact — feature vs maintenance commit mix (DX Core 4 Impact axis).
    let im = &s.impact;
    println!("Impact — last {} days", s.window_days);
    match im.feature_ratio {
        Some(r) => println!(
            "  mix         {:.0}% feature · {:.0}% maintenance  ({} feat · {} maint{})",
            r * 100.0,
            (1.0 - r) * 100.0,
            im.feature,
            im.maintenance,
            if im.unclassified > 0 {
                format!(" · {} unclassified", im.unclassified)
            } else {
                String::new()
            }
        ),
        None => println!("  mix         —  (no classified commits in window)"),
    }
    if let Some(p) = &prev {
        // Trend since the last snapshot — higher feature share means more new capability.
        let dir = delta_pct(p.card.impact.feature_ratio, im.feature_ratio);
        println!("  vs {}   feature share {dir}", p.date);
    }

    // Agents — what Claude + Copilot did, from their session telemetry.
    let a = &s.agent;
    if a.sessions > 0 {
        let fmt_counts = |v: &[(String, usize)]| {
            v.iter()
                .map(|(k, n)| format!("{k} {n}"))
                .collect::<Vec<_>>()
                .join(" · ")
        };
        println!("Agents");
        println!(
            "  sessions    {}  ({}h agent time)",
            a.sessions,
            a.total_minutes / 60
        );
        println!("  harness     {}", fmt_counts(&a.by_harness));
        println!("  models      {}", fmt_counts(&a.by_model));
        println!(
            "  cost        {:.2} AIU  (Copilot only)  ·  +{}/-{} lines",
            a.cost_aiu, a.lines_added, a.lines_removed
        );
    }

    // Harness switches — Claude↔Copilot hops, a rate-limit-pressure signal. A spike in
    // `limit`-reason switches flags a capped harness to bias new dispatches away from (#326).
    let sw = &s.switches;
    if sw.total > 0 {
        let join = |v: &[(String, usize)]| {
            v.iter()
                .map(|(k, n)| format!("{k} {n}"))
                .collect::<Vec<_>>()
                .join(" · ")
        };
        println!("Harness switches — last {} days", s.window_days);
        println!("  total       {}  ({})", sw.total, join(&sw.by_reason));
        println!("  direction   {}", join(&sw.by_direction));
        if !sw.limit_source.is_empty() {
            println!(
                "  limit src   {}  (harness hitting limits — bias dispatches away)",
                join(&sw.limit_source)
            );
        }
    }

    // Churn / turnover — re-revised lines, split agent vs hand (the AI-era quality axis).
    let ch = &s.churn;
    if !ch.counted {
        println!("Churn — not counted here (this repo is excluded from the scorecard)");
    } else if ch.total_lines > 0 {
        println!(
            "Churn — last {} days (re-revised within {}d)",
            s.window_days, ch.churn_days
        );
        let rate = ch
            .churn_rate
            .map(|r| format!("{:.0}%", r * 100.0))
            .unwrap_or_else(|| "—".into());
        println!(
            "  rate        {rate}  ({} of {} lines reworked)",
            ch.churned_lines, ch.total_lines
        );
        match ch.agent_vs_hand_ratio {
            Some(r) => {
                let health = if r > 1.5 {
                    "above the 1.5× healthy line"
                } else {
                    "within the 1.5× healthy line"
                };
                println!("  agent vs hand {r:.1}× your hand-written churn — {health}");
            }
            None if ch.agent_lines == 0 => {
                println!("  agent vs hand —  (no agent-attributed lines in window)")
            }
            None => println!("  agent vs hand —  (no hand-written churn to compare against)"),
        }
    }

    // Pools — this machine's warm-slot refresh + lease activity, freshest repo first. Answers
    // "are my worktree pools actually warm, and are dispatches hitting warm slots?" (per-box).
    let pl = &s.pools;
    if !pl.repos.is_empty() {
        println!("Pools — last {} days", s.window_days);
        if let Some(last) = pl.last_refresh {
            let ago = dur(((now - last).max(0) as f64) / 3600.0);
            println!("  refreshed   {ago} ago  (most recent warm across repos)");
        }
        for r in &pl.repos {
            let fresh = if r.last_refresh > 0 {
                format!("warmed {} ago", dur(((now - r.last_refresh).max(0) as f64) / 3600.0))
            } else {
                "never warmed".to_string()
            };
            let hits = if r.leases > 0 {
                format!(" · {}/{} warm hits", r.warm_hits, r.leases)
            } else {
                String::new()
            };
            let recov = if r.recoveries > 0 {
                format!(" · {} recovered", r.recoveries)
            } else {
                String::new()
            };
            println!(
                "  {:<11} {}/{} slots · {} refresh{} · {fresh}{hits}{recov}",
                r.repo,
                r.warmed,
                r.size,
                r.refreshes,
                if r.refreshes == 1 { "" } else { "es" },
            );
        }
    }
}

/// `bn metrics --trend` — the snapshot history as a compact table, oldest first, so the direction
/// over weeks is visible at a glance.
pub(crate) fn emit_metrics_trend(cfg: &bn_core::resolver::Config, as_json: bool) {
    let snaps = bn_core::metrics::read_snapshots(cfg);
    if as_json {
        let rows: Vec<_> = snaps
            .iter()
            .map(|s| {
                serde_json::json!({
                    "date": s.date,
                    "wip": s.card.wip,
                    "merged": s.card.pr.merged,
                    "close_out_lag_median_hours": s.card.close_out.lag_median_hours,
                })
            })
            .collect();
        println!(
            "{}",
            Envelope::success("metrics_trend", serde_json::json!({ "snapshots": rows })).to_json()
        );
        return;
    }
    if snaps.is_empty() {
        println!("No snapshots yet — the weekly job (or `bn metrics --snapshot`) writes them.");
        return;
    }
    println!(
        "{:<12}  {:>4}  {:>6}  {:>10}",
        "date", "WIP", "merged", "lag(med)"
    );
    for s in &snaps {
        let lag = s
            .card
            .close_out
            .lag_median_hours
            .map(dur)
            .unwrap_or_else(|| "—".into());
        println!(
            "{:<12}  {:>4}  {:>6}  {:>10}",
            s.date, s.card.wip, s.card.pr.merged, lag
        );
    }
}

/// Signed delta of a lower-is-better hours metric vs a prior value, as an arrowed string. `None`
/// on either side → "—".
pub(crate) fn delta_hours(prev: Option<f64>, now: Option<f64>) -> String {
    match (prev, now) {
        (Some(p), Some(n)) => {
            let d = n - p;
            if d.abs() < 0.05 {
                "·".into()
            } else if d < 0.0 {
                format!("▼{}", dur(-d)) // less lag — good
            } else {
                format!("▲{}", dur(d))
            }
        }
        _ => "—".into(),
    }
}

/// Signed integer delta vs a prior value, arrowed (▲ is "more", ▼ is "less"); the reader judges
/// whether more is good for that metric.
pub(crate) fn delta_int(prev: i64, now: i64) -> String {
    let d = now - prev;
    match d.cmp(&0) {
        std::cmp::Ordering::Equal => "·".into(),
        std::cmp::Ordering::Greater => format!("▲{d}"),
        std::cmp::Ordering::Less => format!("▼{}", -d),
    }
}

/// Signed delta of a 0..1 ratio vs a prior value, rendered as arrowed percentage points. `None`
/// on either side → "—". ▲ is a larger share; the reader judges whether that's the direction
/// they want.
pub(crate) fn delta_pct(prev: Option<f64>, now: Option<f64>) -> String {
    match (prev, now) {
        (Some(p), Some(n)) => {
            let d = (n - p) * 100.0;
            if d.abs() < 0.5 {
                "·".into()
            } else if d > 0.0 {
                format!("▲{:.0}pp", d)
            } else {
                format!("▼{:.0}pp", -d)
            }
        }
        _ => "—".into(),
    }
}

/// `bn event <kind> --field k=v …` — append a productivity event to the synced metrics log.
/// All-digit values become JSON numbers (so `number=52` joins with PR metrics); everything else
/// is a string. Best-effort-friendly but reports IO errors so a cron job can surface a failure.
/// Run `git -C <dir> <args>` and capture stdout (raw, untrimmed), or None on failure. Thin
/// delegate to the runner-backed [`crate::git::git_capture`] (trim=false preserves the raw output
/// this wrapper's callers parse line-by-line).
pub(crate) fn git_capture_in(dir: &std::path::Path, args: &[&str]) -> Option<String> {
    crate::git::git_capture(args, Some(dir), false)
}

/// Sum `git diff --numstat` output into (insertions, deletions, files). Each line is
/// tab-separated added/deleted/path; binary files report '-' which counts as 0 lines.
pub(crate) fn parse_numstat(numstat: &str) -> (i64, i64, i64) {
    let (mut insertions, mut deletions, mut files) = (0i64, 0i64, 0i64);
    for line in numstat.lines().filter(|l| !l.trim().is_empty()) {
        let mut it = line.split('\t');
        let added = it.next().unwrap_or("");
        let deleted = it.next().unwrap_or("");
        files += 1;
        insertions += added.parse::<i64>().unwrap_or(0);
        deletions += deleted.parse::<i64>().unwrap_or(0);
    }
    (insertions, deletions, files)
}

/// Pull a branch's session result back into its note + metrics. Summarizes the commits on the
/// branch (vs `base`) and the diffstat into the note's `## Progress`, and logs an `ingest` metrics
/// event. Read-only on git; the loop's closing move — what got done flows back to where it was
/// planned. No commits vs base → nothing to ingest (no write).
pub(crate) fn run_ingest(cfg: &Config, ctx: &Resolved, base: &str, json: bool) {
    let wt = cfg.worktree_dir.join(&ctx.repo).join(&ctx.branch);
    if !wt.is_dir() {
        fail(&format!(
            "bn ingest: no worktree at {} — nothing to ingest",
            wt.display()
        ));
    }

    let range = format!("{base}..HEAD");
    let log = git_capture_in(&wt, &["log", "--format=%h %s", &range]).unwrap_or_default();
    let commits: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    if commits.is_empty() {
        if json {
            println!(
                "{}",
                Envelope::success(
                    "ingest",
                    serde_json::json!({ "branch": ctx.branch, "base": base, "commits": 0 }),
                )
                .to_json()
            );
        } else {
            println!("Nothing to ingest: no commits on {} vs {base}", ctx.branch);
        }
        return;
    }

    // Diffstat since the merge-base (base...HEAD).
    let numstat =
        git_capture_in(&wt, &["diff", "--numstat", &format!("{base}...HEAD")]).unwrap_or_default();
    let (insertions, deletions, files) = parse_numstat(&numstat);

    let subjects = commits
        .iter()
        .map(|c| c.split_once(' ').map(|(_, s)| s).unwrap_or(c))
        .collect::<Vec<_>>()
        .join("; ");
    let bullet = format!(
        "Ingested {date}: {n} commit(s), +{insertions}/-{deletions} in {files} file(s) — {subjects}",
        date = today(),
        n = commits.len(),
    );
    let _ = commands::append_body(cfg, ctx, "progress", &bullet, &today());

    let mut map = serde_json::Map::new();
    map.insert("branch".into(), serde_json::json!(ctx.branch));
    map.insert("commits".into(), serde_json::json!(commits.len()));
    map.insert("files".into(), serde_json::json!(files));
    map.insert("insertions".into(), serde_json::json!(insertions));
    map.insert("deletions".into(), serde_json::json!(deletions));
    let _ = bn_core::events::append_metric_at(
        &cfg.metrics_log_path(),
        "ingest",
        bn_core::util::now_epoch_secs(),
        map,
    );

    if json {
        println!(
            "{}",
            Envelope::success(
                "ingest",
                serde_json::json!({
                    "branch": ctx.branch,
                    "base": base,
                    "commits": commits.len(),
                    "files": files,
                    "insertions": insertions,
                    "deletions": deletions,
                }),
            )
            .to_json()
        );
        return;
    }
    println!("Ingested {} → note Progress + metrics", ctx.branch);
    println!(
        "  {} commit(s), +{insertions}/-{deletions} in {files} file(s)",
        commits.len()
    );
}

pub(crate) fn emit_event(
    cfg: &Config,
    kind: &str,
    fields: &[String],
    ts: Option<i64>,
    as_json: bool,
) {
    let mut map = serde_json::Map::new();
    for f in fields {
        let Some((k, v)) = f.split_once('=') else {
            eprintln!("bn event: ignoring malformed field '{f}' (expected k=v)");
            continue;
        };
        let val = match v.parse::<i64>() {
            Ok(n) if !v.is_empty() => serde_json::json!(n),
            _ => serde_json::json!(v),
        };
        map.insert(k.to_string(), val);
    }
    let when = ts.unwrap_or_else(bn_core::util::now_epoch_secs);
    match bn_core::events::append_metric_at(&cfg.metrics_log_path(), kind, when, map) {
        Ok(()) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success("event", serde_json::json!({ "kind": kind, "ts": when }))
                        .to_json()
                );
            } else {
                println!("recorded {kind} @ {when}");
            }
        }
        Err(e) => {
            eprintln!("bn event: {e}");
            std::process::exit(1);
        }
    }
}

/// Hours as a compact human duration.
pub(crate) fn dur(hours: f64) -> String {
    if hours < 48.0 {
        format!("{:.0}h", hours)
    } else {
        format!("{:.1}d", hours / 24.0)
    }
}
