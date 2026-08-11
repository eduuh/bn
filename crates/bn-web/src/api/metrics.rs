//! The reporting surface: metrics, the weekly digests, and the brag list.
//!
//! Split out of `api.rs` under the file-size ratchet. These belong together because they share one
//! window query and one cost profile — each shells git/gh or folds the whole event log, so every
//! one of them runs on the blocking pool.

use crate::state::AppState;
use axum::{
    extract::{Query, State},
    response::Json,
};
use bn_core::render::Envelope;
use serde::Deserialize;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Insights & metrics (#507) — the reporting surface: `bn metrics` (velocity/throughput +
// agent telemetry), `bn insights`/`weekly` (the weekly digests), and `bn brag`
// (accomplishments). All native in-process `bn-core` reads wrapped in an envelope, matching
// the CLI's `--json` shape. `metrics`/`brag`/`weekly` shell git/gh (or read the whole event
// log) so they run on a blocking thread to keep the async executor free.
// ---------------------------------------------------------------------------

/// Trailing-window / since query shared by the metrics + digest reads.
#[derive(Debug, Deserialize)]
pub struct InsightsQuery {
    /// Trailing window in days for the metrics scorecard (default 30).
    pub window: Option<i64>,
    /// Rolling window in days for the insights/weekly/brag digests; `None` uses each report's
    /// anchored default window.
    pub since: Option<i64>,
}

/// `GET /api/metrics?window=30` — the velocity + throughput scorecard (mirrors `bn metrics`):
/// PR flow, review latency, WIP, close-out lag, and the AI-agent session telemetry the dashboard
/// tiles read. Native in-process; shells git for the commit-mix, so it runs on a blocking thread.
pub async fn metrics(
    State(state): State<AppState>,
    Query(q): Query<InsightsQuery>,
) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let window = q.window.unwrap_or(30);
    let data = tokio::task::spawn_blocking(move || {
        use bn_core::clock::Clock;
        let clock = bn_core::clock::RealClock;
        let today = clock.today();
        let s = bn_core::metrics::scorecard(
            &cfg,
            &bn_core::runner::RealRunner,
            clock.now_epoch_secs(),
            window,
            &today,
        );
        bn_core::payload::scorecard_data(&s)
    })
    .await
    .unwrap_or_else(|_| json!({}));
    Json(Envelope::success("metrics", data))
}

/// `GET /api/metrics/trend` — the snapshot history behind the trend chart: one row per recorded
/// snapshot (`date`, `wip`, `merged`, `close_out_lag_median_hours`), mirroring `bn metrics --trend
/// --json`. Snapshots are written by the weekly job / `bn metrics --snapshot`, so a fresh install
/// returns an empty list (the chart then renders its empty state).
pub async fn metrics_trend(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let data = tokio::task::spawn_blocking(move || {
        let snaps = bn_core::metrics::read_snapshots(&cfg);
        let rows: Vec<Value> = snaps
            .iter()
            .map(|s| {
                json!({
                    "date": s.date,
                    "wip": s.card.wip,
                    "merged": s.card.pr.merged,
                    "close_out_lag_median_hours": s.card.close_out.lag_median_hours,
                })
            })
            .collect();
        json!({ "snapshots": rows })
    })
    .await
    .unwrap_or_else(|_| json!({ "snapshots": [] }));
    Json(Envelope::success("metrics_trend", data))
}

/// `GET /api/insights?since=` — the weekly auto-retro (mirrors `bn insights`): the mined telemetry
/// (PR cycle, deep-work ratio, scoreboard) plus the one suggestion, with a `rendered` text field.
/// Read-only: never writes a baseline or the weekly file (those stay `bn` CLI actions).
pub async fn insights(
    State(state): State<AppState>,
    Query(q): Query<InsightsQuery>,
) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let since = q.since;
    let data = tokio::task::spawn_blocking(move || {
        let ins = bn_core::insights::insights(&cfg, &bn_core::util::today(), since);
        bn_core::payload::insights_data(&ins)
    })
    .await
    .unwrap_or_else(|_| json!({}));
    Json(Envelope::success("insights", data))
}

/// `GET /api/weekly?since=` — the weekly 1-1 review digest (mirrors `bn weekly`): goals completed
/// since the anchor weekday (or `--since N` days), plus a `rendered` text field for display.
pub async fn weekly(
    State(state): State<AppState>,
    Query(q): Query<InsightsQuery>,
) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let since = q.since;
    let data = tokio::task::spawn_blocking(move || {
        let dxi = bn_core::resolver::JobConfig::load().dxi_enabled;
        let w = bn_core::reports::weekly(&cfg, &bn_core::util::today(), since, dxi);
        bn_core::payload::weekly_data(&w)
    })
    .await
    .unwrap_or_else(|_| json!({}));
    Json(Envelope::success("weekly", data))
}

/// `GET /api/brag?since=` — the accomplishments / impact packet (mirrors `bn brag`): shipped work
/// over the window, plus a `rendered` markdown field. Shells git/gh through the real runner, so it
/// runs on a blocking thread. `since` is a free-form window string (e.g. `30`, `2w`); absent uses
/// brag's default window.
pub async fn brag(State(state): State<AppState>, Query(q): Query<InsightsQuery>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let since = q.since.map(|n| n.to_string());
    let data = tokio::task::spawn_blocking(move || {
        let b = bn_core::brag::brag(
            &cfg,
            &bn_core::util::today(),
            since.as_deref(),
            &bn_core::runner::RealRunner,
        );
        bn_core::payload::brag_data(&b)
    })
    .await
    .unwrap_or_else(|_| json!({}));
    Json(Envelope::success("brag", data))
}
