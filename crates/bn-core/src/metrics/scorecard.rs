//! The whole-person scorecard, personal baselines, and anomaly positions.

use super::*;
use crate::events;
use crate::resolver::Config;
use crate::runner::CommandRunner;
use serde::{Deserialize, Serialize};

/// The whole-person scorecard: close-out velocity + throughput over a trailing window. PR,
/// close-out, and flow come from the unioned metrics log across every machine (you-as-an-engineer,
/// not one laptop); WIP and branch age come from this machine's active notes.
#[derive(Debug, Serialize, Deserialize)]
pub struct Scorecard {
    pub window_days: i64,
    /// PR throughput block (opened/merged/closed/in-flight, completion rate, cycle time).
    pub pr: Metrics,
    /// Review-latency block — median time-to-first-review on your PRs (the code-review bottleneck
    /// the research centers on).
    pub review_latency: ReviewLatency,
    /// Active notes with at least one open todo — the context-switch tax.
    pub wip: usize,
    /// All active notes (open branches).
    pub open_branches: usize,
    pub branch_age_median_days: Option<i64>,
    pub branch_age_max_days: Option<i64>,
    pub close_out: CloseOut,
    pub flow: Flow,
    /// Forward-looking WIP / focus guard — in-flight count, the branch to finish next, clear-by
    /// forecast. The "what to finish / when you'll be clear" the rest of the card can't answer.
    pub wip_guard: WipGuard,
    /// Today's context-switch / focus-block picture — the cognitive-load axis.
    pub fragmentation: Fragmentation,
    /// Feature-vs-maintenance commit mix over the window — the DX Core 4 Impact axis.
    pub impact: ImpactMix,
    /// AI-agent session telemetry (Claude + Copilot) folded from `agent_session` events.
    pub agent: AgentMetrics,
    /// Harness-switch activity (Claude↔Copilot) folded from `harness_switch` events — a
    /// rate-limit-pressure signal: how often we hop harnesses, by reason and direction (#326).
    #[serde(default)]
    pub switches: Switches,
    /// Code churn / turnover, split agent-assisted vs hand-written — the AI-era quality axis (#128).
    #[serde(default)]
    pub churn: Churn,
    /// Partial progress across open branch todos (`bn done --pct`, #213) — multi-day work made
    /// visible before it closes, instead of reading as zero throughput.
    #[serde(default)]
    pub partial: PartialProgress,
    /// Personal baselines + anomaly flags — every metric positioned against your own rolling
    /// median/IQR from the snapshot history (#125). `sufficient` is false until enough history
    /// accrues. Computed last (it reads the rest of the card), so `--json`/MCP get it for free.
    #[serde(default)]
    pub baselines: Baselines,
    /// Warm-pool refresh + lease activity per repo, folded from this machine's `pool_warm` /
    /// `pool_get` / `pool_recover` events. Machine-local (a pool is per-box), unlike the
    /// cross-machine PR/flow blocks above.
    #[serde(default)]
    pub pools: PoolActivity,
}

/// This machine's warm-pool activity over the window — the Pools section source. Per-repo refresh
/// freshness (when the pool was last warmed + how many slots) and lease pressure (how often a
/// dispatch leased a slot, and how often it was a warm hit).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PoolActivity {
    /// Repos that saw pool activity in the window, most-recently-refreshed first.
    #[serde(default)]
    pub repos: Vec<PoolRepoActivity>,
    /// Most recent `pool_warm` across every repo (epoch secs), or `None` if never refreshed.
    pub last_refresh: Option<i64>,
}

/// One repo's warm-pool activity for the Pools section.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PoolRepoActivity {
    pub repo: String,
    /// Slots warmed on the most recent refresh, and the configured pool size then.
    pub warmed: i64,
    pub size: i64,
    /// Most recent `pool_warm` for this repo (epoch secs); 0 if only leases were seen.
    pub last_refresh: i64,
    /// `pool_warm` refreshes in the window.
    pub refreshes: i64,
    /// Slots leased (`pool_get`) in the window, and how many were warm hits (no cold rebuild).
    pub leases: i64,
    pub warm_hits: i64,
    /// Stale slots auto-recovered (`pool_recover`, e.g. after a WSL/host restart) in the window.
    pub recoveries: i64,
}

/// Fold `pool_warm` / `pool_get` / `pool_recover` events into per-repo pool activity over the
/// trailing window. Pure (no I/O) so the scorecard can unit-test the aggregation; the caller
/// supplies this machine's event log.
pub fn pool_activity(evs: &[events::Event], now: i64, window_days: i64) -> PoolActivity {
    let cutoff = now.saturating_sub(window_days.max(0).saturating_mul(86_400));
    let mut by_repo: std::collections::BTreeMap<String, PoolRepoActivity> =
        std::collections::BTreeMap::new();
    let mut last_refresh: Option<i64> = None;
    for e in evs.iter().filter(|e| e.ts >= cutoff) {
        // Only the pool lifecycle events contribute — ignore every other event that happens to
        // carry a `repo` field (cron_run, agent/dispatch events, …), or they'd show as phantom
        // zero-activity pool rows.
        if !matches!(e.kind.as_str(), "pool_warm" | "pool_get" | "pool_recover") {
            continue;
        }
        let Some(repo) = e.fields.get("repo").and_then(|v| v.as_str()) else {
            continue;
        };
        let entry = by_repo.entry(repo.to_string()).or_default();
        entry.repo = repo.to_string();
        match e.kind.as_str() {
            "pool_warm" => {
                entry.refreshes += 1;
                // The most-recent refresh sets the displayed warmed/size + freshness.
                if e.ts >= entry.last_refresh {
                    entry.last_refresh = e.ts;
                    entry.warmed = e.fields.get("warmed").and_then(|v| v.as_i64()).unwrap_or(0);
                    entry.size = e.fields.get("size").and_then(|v| v.as_i64()).unwrap_or(0);
                }
                last_refresh = Some(last_refresh.map_or(e.ts, |m| m.max(e.ts)));
            }
            "pool_get" => {
                entry.leases += 1;
                if e.fields
                    .get("warm")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    entry.warm_hits += 1;
                }
            }
            "pool_recover" => entry.recoveries += 1,
            _ => {}
        }
    }
    let mut repos: Vec<PoolRepoActivity> = by_repo.into_values().collect();
    // Freshest pool first; ties (and lease-only repos with last_refresh 0) fall back to repo name.
    repos.sort_by(|a, b| {
        b.last_refresh
            .cmp(&a.last_refresh)
            .then_with(|| a.repo.cmp(&b.repo))
    });
    PoolActivity {
        repos,
        last_refresh,
    }
}

/// Compute the full scorecard as of `now` (epoch secs) / `today` (`YYYY-MM-DD`) over the trailing
/// `window_days`. The one impure entry point: reads the cross-machine event log and active notes,
/// and the commit log through `runner` (for the feature-vs-maintenance mix).
pub fn scorecard(
    cfg: &Config,
    runner: &dyn CommandRunner,
    now: i64,
    window_days: i64,
    today: &str,
) -> Scorecard {
    let evs = visible_events(
        cfg,
        events::read_events_across(&cfg.metrics_logs_all_machines()),
    );
    let pr = compute(&evs, now, window_days);
    let review_latency = review_latency(&evs, now, window_days);
    let close_out = close_out_lag(&evs, now, window_days);
    let flow = flow(&evs, now, window_days);
    let fragmentation = fragmentation(&evs, today);
    let agent = agent_metrics(&evs, now, window_days);
    let switches = switches(&evs, now, window_days);

    // Feature-vs-maintenance mix: commits in the same trailing window, read through the seam.
    let since = crate::util::add_days(today, -window_days.max(0)).unwrap_or_else(|| today.into());
    let impact = impact_mix(runner, &since);

    // Churn / turnover: re-revised lines over the window, split agent vs hand. The git read goes
    // through the seam; attribution joins the cwd repo's agent-session windows from the synced log.
    let repo = current_repo_key(cfg, runner);
    let agent_windows = repo
        .as_deref()
        .map(|r| agent_windows_for_repo(&evs, r))
        .unwrap_or_default();
    let churn = churn(runner, &agent_windows, &since, CHURN_DAYS);

    let notes: Vec<_> = crate::rollups::all_notes(cfg)
        .into_iter()
        .filter(|n| !is_excluded_repo(&n.repo))
        .collect();
    let open_branches = notes.len();
    let wip = notes
        .iter()
        .filter(|n| n.note.front.todos.iter().any(|t| t.is_open_todo()))
        .count();
    let mut ages: Vec<i64> = notes
        .iter()
        .filter_map(|n| crate::util::days_since(&n.note.front.created, today))
        .collect();
    ages.sort_unstable();

    // Partial progress: open todos advanced with `bn done --pct` across the active notes.
    let pcts: Vec<u8> = notes
        .iter()
        .flat_map(|n| n.note.front.todos.iter())
        .filter(|t| t.is_open_todo())
        .filter_map(|t| t.pct)
        .collect();
    let partial = partial_progress(&pcts);

    // WIP guard: rank each open branch by how close to merge it is, forecast a clear-by date from
    // this window's merge cadence. Same note→rank mapping `bn next`'s top row uses.
    let wip_guard = wip_guard(&open_branch_wips(cfg, today), pr.merged, window_days, today);

    // Warm-pool activity: this machine's pool refresh + lease events (a pool is per-box, so read
    // the machine-local log directly rather than the cross-machine metrics union).
    let pools = pool_activity(&events::read_all(), now, window_days);

    let mut card = Scorecard {
        window_days,
        pr,
        review_latency,
        wip,
        open_branches,
        branch_age_median_days: percentile(&ages, 0.5),
        branch_age_max_days: ages.last().copied(),
        close_out,
        flow,
        wip_guard,
        fragmentation,
        impact,
        agent,
        switches,
        churn,
        partial,
        baselines: Baselines::default(),
        pools,
    };
    // Personal baselines: position this card's metrics against your own rolling history. Computed
    // last because it reads the just-built card; the history is the prior snapshots before today.
    let history = read_snapshots(cfg);
    card.baselines = baselines(
        &history,
        &card,
        today,
        BASELINE_WINDOW_DAYS,
        BASELINE_MIN_SAMPLES,
    );
    card
}

/// Re-revision horizon for churn — a line changed again within this many days of being written
/// counts as churn. 14 days matches the GitClear research definition.
const CHURN_DAYS: i64 = 14;

/// The cwd repo's bn-canonical name (lowercased) via `git rev-parse --show-toplevel`, for joining
/// churn commits to this repo's agent sessions. Under the worktree layout the toplevel is
/// `<worktree_dir>/<repo>/<branch-slug>`, so the repo is the first component under the worktree
/// root — the same derivation agent ingest stamps sessions with (#213: keying on the toplevel
/// *basename* yielded the branch slug, which never matched a session, so every line read as
/// hand-written). Falls back to the toplevel basename for a plain (non-worktree) checkout.
/// `None` outside a repo / when git is absent.
fn current_repo_key(cfg: &Config, runner: &dyn CommandRunner) -> Option<String> {
    let top = runner.git(&["rev-parse", "--show-toplevel"]).ok_stdout()?;
    std::path::Path::new(&top)
        .strip_prefix(&cfg.worktree_dir)
        .ok()
        .and_then(|rel| rel.components().next())
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .or_else(|| top.rsplit('/').next().map(str::to_string))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
}

/// Trailing window the personal baseline reads history over — 8 weeks, the developer-metrics
/// guidance for "4–6 weeks of baseline" with headroom for weekly snapshots.
const BASELINE_WINDOW_DAYS: i64 = 56;

/// Minimum in-window snapshots before the baseline speaks. Below this it says "baselining…" rather
/// than flag anomalies off too little history.
const BASELINE_MIN_SAMPLES: usize = 4;

/// Nearest-rank percentile of a pre-sorted float slice. `None` if empty. The float twin of
/// [`percentile`], for the baseline pass over snapshot metrics.
fn percentile_f(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    Some(sorted[idx])
}

/// One baselined metric the scorecard tracks against your own rolling history. `lower_is_better`
/// drives how an anomaly reads (a flow-time spike is bad; more merges is good).
#[derive(Debug, Clone, Copy)]
struct MetricSpec {
    key: &'static str,
    label: &'static str,
    lower_is_better: bool,
}

/// The scalar metrics baselined in [`baselines`]. Each pulls one number out of a [`Scorecard`];
/// `None` (a metric with no data that period) drops that snapshot from the metric's window. Kept
/// deliberately small — the headline numbers a reader scans — not every field on the card.
const BASELINE_METRICS: &[MetricSpec] = &[
    MetricSpec {
        key: "wip",
        label: "WIP",
        lower_is_better: true,
    },
    MetricSpec {
        key: "flow_minutes",
        label: "flow time",
        lower_is_better: false,
    },
    MetricSpec {
        key: "cycle_median_hours",
        label: "cycle time",
        lower_is_better: true,
    },
    MetricSpec {
        key: "lead_median_hours",
        label: "lead time",
        lower_is_better: true,
    },
    MetricSpec {
        key: "review_wait_median_hours",
        label: "review wait",
        lower_is_better: true,
    },
    MetricSpec {
        key: "close_out_lag_median_hours",
        label: "close-out lag",
        lower_is_better: true,
    },
    MetricSpec {
        key: "churn_rate",
        label: "churn rate",
        lower_is_better: true,
    },
];

/// Pull the scalar named `key` out of a scorecard, or `None` when that metric had no data. The one
/// place the baseline layer knows the card's shape, so [`BASELINE_METRICS`] stays declarative.
fn metric_value(card: &Scorecard, key: &str) -> Option<f64> {
    match key {
        "wip" => Some(card.wip as f64),
        "flow_minutes" => Some(card.flow.total_minutes as f64),
        "cycle_median_hours" => card.pr.cycle_median_hours,
        "lead_median_hours" => card.pr.lead_median_hours,
        "review_wait_median_hours" => card.review_latency.first_review_median_hours,
        "close_out_lag_median_hours" => card.close_out.lag_median_hours,
        "churn_rate" => card.churn.churn_rate,
        _ => None,
    }
}

/// Where the current reading sits versus its rolling baseline (median + p25/p75 over the window).
/// `anomaly` is true when the value falls outside `[p25, p75]` *and* in the unwanted direction —
/// the readings worth the eye, framed as a signal not a verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselinePosition {
    /// Metric key (stable, machine-facing — matches `metric_value`).
    pub key: String,
    /// Human label ("flow time").
    pub label: String,
    /// The current reading.
    pub value: f64,
    /// Rolling median over the window.
    pub median: f64,
    pub p25: f64,
    pub p75: f64,
    /// Multiple of the median (`value / median`), `None` when the median is 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vs_median: Option<f64>,
    /// True when the current value is an anomaly (outside the IQR, wrong direction).
    pub anomaly: bool,
    /// A short human note ("2.1× your median — unusually high"), present only on anomalies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The personal-baseline layer over every tracked metric: median + p25/p75 from your own rolling
/// snapshot history, with the current reading positioned against it and anomalies flagged. The
/// thin pass that turns absolute numbers into "normal for you / unusual" signals (#125). `sufficient`
/// is false until at least `min_samples` snapshots exist in the window — it says "baselining…"
/// rather than speak from too little history.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Baselines {
    /// Trailing window the baseline was computed over, in days.
    pub window_days: i64,
    /// How many historical snapshots fell in the window (the sample size the baseline rests on).
    pub samples: usize,
    /// Whether there's enough history to speak (`samples >= min_samples`).
    pub sufficient: bool,
    /// Per-metric position, in [`BASELINE_METRICS`] order; only metrics with both a current value
    /// and ≥1 historical sample appear.
    pub positions: Vec<BaselinePosition>,
}

impl Baselines {
    /// The minimum in-window snapshots before the baseline speaks — the "insufficient history"
    /// floor, exposed so the CLI can phrase the `baselining…` line.
    pub fn min_samples() -> usize {
        BASELINE_MIN_SAMPLES
    }

    /// The anomalous positions, most-deviant first — what `bn metrics` flags at the top.
    pub fn anomalies(&self) -> Vec<&BaselinePosition> {
        let mut a: Vec<&BaselinePosition> = self.positions.iter().filter(|p| p.anomaly).collect();
        a.sort_by(|x, y| {
            let dx = x.vs_median.map(|v| (v - 1.0).abs()).unwrap_or(0.0);
            let dy = y.vs_median.map(|v| (v - 1.0).abs()).unwrap_or(0.0);
            dy.partial_cmp(&dx).unwrap_or(std::cmp::Ordering::Equal)
        });
        a
    }
}

/// Compute the personal baselines for `current` against the trailing `window_days` of `history`
/// (snapshots strictly before `today`). Pure over the slice — the caller reads snapshots. For each
/// metric: gather the in-window historical values, take median/p25/p75, and flag the current
/// reading anomalous when it's outside the IQR in the unwanted direction. `min_samples` gates the
/// `sufficient` flag (the "insufficient history" guard).
pub fn baselines(
    history: &[Snapshot],
    current: &Scorecard,
    today: &str,
    window_days: i64,
    min_samples: usize,
) -> Baselines {
    // Snapshots strictly before today and within the window form the baseline sample.
    let in_window: Vec<&Snapshot> = history
        .iter()
        .filter(|s| s.date.as_str() < today)
        .filter(|s| {
            crate::util::days_since(&s.date, today)
                .map(|d| d >= 0 && d <= window_days)
                .unwrap_or(false)
        })
        .collect();
    let samples = in_window.len();
    let sufficient = samples >= min_samples;

    let mut positions = Vec::new();
    for spec in BASELINE_METRICS {
        let Some(value) = metric_value(current, spec.key) else {
            continue;
        };
        let mut vals: Vec<f64> = in_window
            .iter()
            .filter_map(|s| metric_value(&s.card, spec.key))
            .collect();
        if vals.is_empty() {
            continue;
        }
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let (Some(median), Some(p25), Some(p75)) = (
            percentile_f(&vals, 0.5),
            percentile_f(&vals, 0.25),
            percentile_f(&vals, 0.75),
        ) else {
            continue;
        };
        let vs_median = (median.abs() > f64::EPSILON).then(|| value / median);

        // Anomaly: outside the IQR, in the direction that's bad for this metric. We only speak when
        // there's enough history, so a thin baseline never cries wolf.
        let outside_high = value > p75;
        let outside_low = value < p25;
        let bad_direction = if spec.lower_is_better {
            outside_high
        } else {
            outside_low
        };
        let anomaly = sufficient && bad_direction;
        let note = anomaly.then(|| {
            let mult = vs_median
                .map(|m| format!("{m:.1}× your median"))
                .unwrap_or_else(|| "above your median".into());
            let dir = if outside_high { "high" } else { "low" };
            format!("{mult} — unusually {dir}")
        });

        positions.push(BaselinePosition {
            key: spec.key.to_string(),
            label: spec.label.to_string(),
            value,
            median,
            p25,
            p75,
            vs_median,
            anomaly,
            note,
        });
    }

    Baselines {
        window_days,
        samples,
        sufficient,
        positions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;

    #[test]
    fn current_repo_key_uses_worktree_layout_not_basename() {
        let _g = crate::test_support::env_lock();
        let mut cfg = Config::from_env();
        cfg.worktree_dir = "/w".into();
        // Worktree checkout: toplevel is <worktree_dir>/<repo>/<branch-slug> — the repo is the
        // first component under the root, NOT the basename (that's the branch slug, #213).
        let r = FakeRunner::new().on(
            "git",
            &["rev-parse", "--show-toplevel"],
            "/w/BN/dispatch-fix-metrics\n",
        );
        assert_eq!(current_repo_key(&cfg, &r), Some("bn".into()));
        // Plain checkout outside the worktree root → basename fallback.
        let r2 = FakeRunner::new().on(
            "git",
            &["rev-parse", "--show-toplevel"],
            "/home/me/projects/MyRepo\n",
        );
        assert_eq!(current_repo_key(&cfg, &r2), Some("myrepo".into()));
        // Not a repo / git absent → None.
        assert_eq!(current_repo_key(&cfg, &FakeRunner::new()), None);
    }

    #[test]
    fn pool_activity_folds_refreshes_and_leases_per_repo_newest_first() {
        use crate::metrics::ev;
        use serde_json::json;
        let now = 100 * DAY;
        let evs = vec![
            // arena: two refreshes; the later (day 99) sets the displayed warmed/size + freshness.
            ev(
                "pool_warm",
                90 * DAY,
                &[
                    ("repo", json!("arena")),
                    ("warmed", json!(4)),
                    ("size", json!(4)),
                ],
            ),
            ev(
                "pool_warm",
                99 * DAY,
                &[
                    ("repo", json!("arena")),
                    ("warmed", json!(3)),
                    ("size", json!(4)),
                ],
            ),
            ev(
                "pool_get",
                99 * DAY,
                &[
                    ("repo", json!("arena")),
                    ("slot", json!(1)),
                    ("warm", json!(true)),
                ],
            ),
            ev(
                "pool_get",
                99 * DAY,
                &[
                    ("repo", json!("arena")),
                    ("slot", json!(2)),
                    ("warm", json!(false)),
                ],
            ),
            ev(
                "pool_recover",
                99 * DAY,
                &[("repo", json!("arena")), ("slot", json!(2))],
            ),
            // bn: one older refresh → sorts after arena.
            ev(
                "pool_warm",
                80 * DAY,
                &[
                    ("repo", json!("bn")),
                    ("warmed", json!(2)),
                    ("size", json!(2)),
                ],
            ),
            // Outside the 30-day window → ignored.
            ev(
                "pool_warm",
                10 * DAY,
                &[
                    ("repo", json!("stale")),
                    ("warmed", json!(1)),
                    ("size", json!(1)),
                ],
            ),
            // No repo field → skipped, not a phantom row.
            ev("pool_warm", 99 * DAY, &[("warmed", json!(9))]),
            // A non-pool event that carries a repo must NOT create a phantom zero-activity row.
            ev("cron_run", 99 * DAY, &[("repo", json!("noise"))]),
        ];
        let pa = pool_activity(&evs, now, 30);
        assert_eq!(pa.last_refresh, Some(99 * DAY));
        assert_eq!(
            pa.repos.len(),
            2,
            "arena + bn; stale is out of window, the no-repo + non-pool events are skipped"
        );
        assert!(
            !pa.repos.iter().any(|r| r.repo == "noise"),
            "a non-pool event's repo must not appear as a pool row"
        );
        // Freshest pool first.
        let arena = &pa.repos[0];
        assert_eq!(arena.repo, "arena");
        assert_eq!(arena.last_refresh, 99 * DAY);
        assert_eq!(arena.warmed, 3, "latest refresh wins over the day-90 one");
        assert_eq!(arena.size, 4);
        assert_eq!(arena.refreshes, 2);
        assert_eq!(arena.leases, 2);
        assert_eq!(arena.warm_hits, 1);
        assert_eq!(arena.recoveries, 1);
        let bn = &pa.repos[1];
        assert_eq!(bn.repo, "bn");
        assert_eq!(bn.refreshes, 1);
        assert_eq!(bn.leases, 0);
    }

    // ---- #125 personal baselines + anomaly flagging ----

    /// A bare scorecard with just `wip` set — enough to exercise the baseline pass without a full
    /// card. Other baselined metrics are `None`, so only `wip` produces a position.
    fn card_with_wip(wip: usize) -> Scorecard {
        Scorecard {
            window_days: 30,
            pr: compute(&[], 100 * DAY, 30),
            review_latency: ReviewLatency::default(),
            wip,
            open_branches: wip,
            branch_age_median_days: None,
            branch_age_max_days: None,
            close_out: CloseOut {
                closed_in_window: 0,
                lag_median_hours: None,
                lag_p90_hours: None,
            },
            flow: Flow {
                total_minutes: 0,
                longest_block_minutes: 0,
            },
            wip_guard: WipGuard::default(),
            fragmentation: Fragmentation::default(),
            impact: ImpactMix::default(),
            agent: AgentMetrics::default(),
            switches: Switches::default(),
            churn: Churn::default(),
            partial: PartialProgress::default(),
            baselines: Baselines::default(),
            pools: PoolActivity::default(),
        }
    }

    fn snap(date: &str, wip: usize) -> Snapshot {
        Snapshot {
            date: date.to_string(),
            card: card_with_wip(wip),
        }
    }

    #[test]
    fn baselines_flag_a_wip_spike_above_the_iqr() {
        // Four weekly snapshots of WIP 1,2,2,3 → median 2, p75 2. Current WIP 6 is way over p75 and
        // WIP is lower-is-better → an anomaly.
        let history = vec![
            snap("2026-05-01", 1),
            snap("2026-05-08", 2),
            snap("2026-05-15", 2),
            snap("2026-05-22", 3),
        ];
        let current = card_with_wip(6);
        let b = baselines(&history, &current, "2026-05-29", 56, 4);
        assert_eq!(b.samples, 4);
        assert!(b.sufficient, "4 samples meets the floor");
        let wip = b
            .positions
            .iter()
            .find(|p| p.key == "wip")
            .expect("wip baselined");
        assert_eq!(wip.median, 2.0);
        assert!(wip.anomaly, "WIP 6 over p75 is an anomaly");
        assert_eq!(wip.vs_median, Some(3.0), "6 / median 2 = 3×");
        assert_eq!(b.anomalies().len(), 1);
        assert!(wip.note.as_deref().unwrap().contains("unusually high"));
    }

    #[test]
    fn baselines_quiet_within_the_band() {
        let history = vec![
            snap("2026-05-01", 2),
            snap("2026-05-08", 3),
            snap("2026-05-15", 2),
            snap("2026-05-22", 3),
        ];
        // WIP 2 sits at/under the median → not an anomaly even though we have enough history.
        let b = baselines(&history, &card_with_wip(2), "2026-05-29", 56, 4);
        assert!(b.sufficient);
        let wip = b.positions.iter().find(|p| p.key == "wip").unwrap();
        assert!(!wip.anomaly, "within the IQR → quiet");
        assert!(b.anomalies().is_empty());
    }

    #[test]
    fn baselines_insufficient_history_stays_silent() {
        // Only two in-window snapshots, floor is 4 → not sufficient, so nothing is flagged even if
        // the current value is extreme.
        let history = vec![snap("2026-05-15", 2), snap("2026-05-22", 2)];
        let b = baselines(&history, &card_with_wip(20), "2026-05-29", 56, 4);
        assert_eq!(b.samples, 2);
        assert!(!b.sufficient, "below the min-samples floor");
        let wip = b.positions.iter().find(|p| p.key == "wip").unwrap();
        assert!(!wip.anomaly, "no anomalies spoken from thin history");
    }

    #[test]
    fn baselines_window_excludes_old_and_future_snapshots() {
        let history = vec![
            snap("2026-01-01", 1), // way outside the 56-day window
            snap("2026-05-01", 2),
            snap("2026-05-08", 2),
            snap("2026-05-15", 2),
            snap("2026-05-22", 2),
            snap("2026-06-30", 99), // after today → excluded
        ];
        let b = baselines(&history, &card_with_wip(2), "2026-05-29", 56, 4);
        assert_eq!(
            b.samples, 4,
            "only the four in-window, past snapshots count"
        );
    }
}
