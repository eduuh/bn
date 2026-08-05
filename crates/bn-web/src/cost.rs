//! `GET /api/cost` — the unified token + **dual-cost** fold (design §3 `CostView`).
//!
//! #381 landed the cost-accounting core in [`bn_core::cost`], so this endpoint no longer re-derives
//! the fold itself: it calls [`bn_core::cost::rollup`] — the *same* reusable core the `bn cost` CLI
//! runs — and serializes its [`bn_core::cost::CostView`] verbatim. That hands the dashboard the
//! **real** surface, reported in two clearly-labeled cost units:
//!
//! * **AIC** (AI Credits) — the *metered Copilot bill* (`aic`), `null` for a group with no Copilot
//!   runs (raw-Claude / non-Copilot work carries no AIC).
//! * **$ (API-equiv)** — a *cache-aware benchmark* (`cost_usd`) from the per-model price table,
//!   `null` when nothing in the group had a priceable model.
//!
//! Broken down by work-class `category`, `machine`, `repo`, `model`, `day`, plus the
//! `by_day_category` (day × category) cross-tab the stacked-bar chart consumes. The whole view is
//! always computed, so the UI switches the table's breakdown dimension client-side without a
//! re-fetch (only the window changes the query).

use bn_core::cost::{self, CostFilter};
use bn_core::resolver::Config;
use serde_json::Value;

/// Fold the trailing `window_days` of agent runs into the `CostView` (design §3) by delegating to
/// [`bn_core::cost::rollup`], the shared core behind `bn cost`. `now` anchors the window
/// (overridable for tests). `by` is accepted for API compatibility but ignored: the view carries
/// every breakdown at once, so the client switches dimensions locally rather than re-fetching.
pub fn cost_view(cfg: &Config, window_days: i64, now: i64, _by: &str) -> Value {
    let view = cost::rollup(
        cfg,
        &CostFilter {
            window_days,
            now,
            repo: None,
            window_secs: None,
        },
    );
    serde_json::to_value(&view).unwrap_or(Value::Null)
}
