//! Agent/session metrics, partial progress, and harness-switch counts.

use super::flow::DAY;
use crate::events::Event;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// In-progress work across open branch todos: items advanced with `bn done --pct N` but not yet
/// closed (#213). Binary done/not-done hides hours of progress on long-running goals; this block
/// makes it count for something on the card.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PartialProgress {
    /// Open todos carrying a recorded percentage.
    pub items: usize,
    /// Mean recorded percentage across them; `None` when there are none.
    pub avg_pct: Option<f64>,
}

/// Fold the open todos of the given notes' items into [`PartialProgress`]. Pure over the pcts.
pub fn partial_progress(pcts: &[u8]) -> PartialProgress {
    let items = pcts.len();
    let avg_pct = (items > 0).then(|| pcts.iter().map(|&p| p as f64).sum::<f64>() / items as f64);
    PartialProgress { items, avg_pct }
}

/// AI-agent activity over the window, folded from `agent_session` events (one per finished Claude
/// or Copilot session). Cost is Copilot-only (`cost_nanoaiu`), so it's surfaced as such rather
/// than blended with Claude, which records no cost.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AgentMetrics {
    pub sessions: usize,
    pub total_minutes: i64,
    /// Session count per harness ("claude"/"copilot"), most-used first.
    pub by_harness: Vec<(String, usize)>,
    /// Session count per model, most-used first.
    pub by_model: Vec<(String, usize)>,
    /// Σ cost_nanoaiu / 1e9 — Copilot AI Units (Claude omitted; it carries no cost).
    pub cost_aiu: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub lines_added: i64,
    pub lines_removed: i64,
}

/// Fold `agent_session` events in the trailing window into [`AgentMetrics`]. Pure over the slice.
pub fn agent_metrics(events: &[Event], now: i64, window_days: i64) -> AgentMetrics {
    let cutoff = now - window_days.max(0) * DAY;
    let mut m = AgentMetrics::default();
    let mut harness: HashMap<String, usize> = HashMap::new();
    let mut model: HashMap<String, usize> = HashMap::new();
    let mut cost_nano: i64 = 0;
    let mut total_secs: i64 = 0;
    for ev in crate::agent::latest_session_events(events) {
        if ev.ts < cutoff {
            continue;
        }
        m.sessions += 1;
        let f = |k: &str| ev.fields.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        // Sum raw seconds and divide once at the end; dividing per-event truncates each
        // short session down to a whole minute, undercounting total focus time.
        total_secs += f("duration_secs");
        m.input_tokens += f("input_tokens");
        m.output_tokens += f("output_tokens");
        m.lines_added += f("lines_added");
        m.lines_removed += f("lines_removed");
        cost_nano += f("cost_nanoaiu");
        if let Some(h) = ev.fields.get("harness").and_then(|v| v.as_str()) {
            *harness.entry(h.to_string()).or_default() += 1;
        }
        if let Some(models) = ev.fields.get("models").and_then(|v| v.as_str()) {
            for name in models.split(", ").filter(|s| !s.is_empty()) {
                *model.entry(name.to_string()).or_default() += 1;
            }
        }
    }
    m.total_minutes = total_secs / 60;
    m.cost_aiu = cost_nano as f64 / 1e9;
    m.by_harness = sorted_counts(harness);
    m.by_model = sorted_counts(model);
    m
}

/// A count map as a vec sorted by count desc, then key asc (stable display order).
fn sorted_counts(map: HashMap<String, usize>) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = map.into_iter().collect();
    // count desc, then key asc — a stable, deterministic display order.
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

/// Harness-switch activity over the window, folded from `harness_switch` events (one per
/// Claude↔Copilot switch, emitted on every path: `bn switch`, the fleet auto-switch, and
/// orchestrator relaunch). How often we hop harnesses — and why — is a rate-limit-pressure and
/// harness-reliability signal (#326): a spike in `limit`-reason switches flags a capped harness
/// that new dispatches should bias away from.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Switches {
    /// harness_switch events in the window.
    pub total: usize,
    /// Count per reason ("limit"/"manual"/"stuck"), most-frequent first.
    pub by_reason: Vec<(String, usize)>,
    /// Count per from→to direction ("claude→copilot"), most-frequent first.
    pub by_direction: Vec<(String, usize)>,
    /// The harness most often *left* by a `limit`-reason switch — the rate-limit source, so new
    /// dispatches can bias away from it. Counted by `from`, most-frequent first.
    pub limit_source: Vec<(String, usize)>,
}

/// Fold `harness_switch` events in the trailing window into [`Switches`]. Pure over the slice —
/// the caller picks the source (this machine's log or the unioned whole-person view).
pub fn switches(events: &[Event], now: i64, window_days: i64) -> Switches {
    let cutoff = now - window_days.max(0) * DAY;
    let mut total = 0usize;
    let mut reason: HashMap<String, usize> = HashMap::new();
    let mut direction: HashMap<String, usize> = HashMap::new();
    let mut limit_source: HashMap<String, usize> = HashMap::new();
    for ev in events {
        if ev.kind != "harness_switch" || ev.ts < cutoff {
            continue;
        }
        total += 1;
        let field = |k: &str| ev.fields.get(k).and_then(Value::as_str);
        let r = field("reason").unwrap_or("manual").to_string();
        let from = field("from").unwrap_or("?");
        let to = field("to").unwrap_or("?");
        if r == "limit" && from != "?" {
            *limit_source.entry(from.to_string()).or_default() += 1;
        }
        *reason.entry(r).or_default() += 1;
        *direction.entry(format!("{from}→{to}")).or_default() += 1;
    }
    Switches {
        total,
        by_reason: sorted_counts(reason),
        by_direction: sorted_counts(direction),
        limit_source: sorted_counts(limit_source),
    }
}
