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

#[cfg(test)]
mod tests {
    use super::*;

    /// The endpoint must surface the full #381 `CostView` shape — both cost units (`aic` +
    /// `cost_usd`) and every breakdown, including the `by_day_category` cross-tab the chart needs —
    /// so the typed frontend contract holds. Structural only (no disk assumptions), so it is stable
    /// whether or not the machine has any recorded runs.
    #[test]
    fn cost_view_serializes_the_full_dual_cost_shape() {
        // A far-future anchor with a zero window captures no runs, so the fold is empty and fast
        // regardless of the machine's recorded history — this asserts the shape, not the data.
        let v = cost_view(&Config::from_env(), 0, i64::MAX / 2, "");
        let obj = v.as_object().expect("cost view is a JSON object");
        for k in [
            "window_days",
            "totals",
            "by_category",
            "by_machine",
            "by_repo",
            "by_model",
            "by_day",
            "by_day_category",
        ] {
            assert!(obj.contains_key(k), "cost view missing `{k}`");
        }
        let totals = obj["totals"].as_object().expect("totals is an object");
        for k in [
            "sessions",
            "input_tokens",
            "cache_read_tokens",
            "output_tokens",
            "aic",
            "cost_usd",
        ] {
            assert!(totals.contains_key(k), "totals missing `{k}`");
        }
    }
}
