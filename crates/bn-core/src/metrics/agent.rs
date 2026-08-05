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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::ev;
    use serde_json::json;

    #[test]
    fn agent_metrics_folds_sessions_in_window() {
        let day = 86_400;
        let now = 100 * day;
        let evs = vec![
            ev(
                "agent_session",
                95 * day,
                &[
                    ("harness", json!("claude")),
                    ("models", json!("claude-opus-4.8")),
                    ("duration_secs", json!(1800)),
                    ("input_tokens", json!(1000)),
                    ("lines_added", json!(40)),
                ],
            ),
            ev(
                "agent_session",
                96 * day,
                &[
                    ("harness", json!("copilot")),
                    ("models", json!("claude-opus-4.8")),
                    ("duration_secs", json!(600)),
                    ("cost_nanoaiu", json!(2_000_000_000i64)),
                ],
            ),
            // Outside the window → ignored.
            ev("agent_session", 10 * day, &[("harness", json!("claude"))]),
        ];
        let m = agent_metrics(&evs, now, 30);
        assert_eq!(m.sessions, 2);
        assert_eq!(m.total_minutes, 40); // 30 + 10
        assert_eq!(m.input_tokens, 1000);
        assert_eq!(m.lines_added, 40);
        assert_eq!(m.cost_aiu, 2.0);
        assert_eq!(m.by_model, vec![("claude-opus-4.8".to_string(), 2)]);
        assert_eq!(m.by_harness.len(), 2);
    }

    #[test]
    fn total_minutes_sums_seconds_before_dividing() {
        let day = 86_400;
        let now = 100 * day;
        // Two sub-minute sessions: 50s + 70s = 120s = 2 min. Dividing per event would
        // truncate each (0 + 1 = 1), undercounting; summing first gives the true 2.
        let evs = vec![
            ev("agent_session", 99 * day, &[("duration_secs", json!(50))]),
            ev("agent_session", 99 * day, &[("duration_secs", json!(70))]),
        ];
        let m = agent_metrics(&evs, now, 30);
        assert_eq!(
            m.total_minutes, 2,
            "seconds must be summed before the minute division"
        );
    }

    #[test]
    fn partial_progress_averages_open_pcts() {
        let p = partial_progress(&[40, 80]);
        assert_eq!(p.items, 2);
        assert_eq!(p.avg_pct, Some(60.0));
        let none = partial_progress(&[]);
        assert_eq!((none.items, none.avg_pct), (0, None));
    }

    // ---- #326 harness-switch frequency ----

    #[test]
    fn switches_fold_counts_reason_direction_and_limit_source() {
        let day = 86_400;
        let now = 100 * day;
        let evs = vec![
            ev(
                "harness_switch",
                99 * day,
                &[
                    ("from", json!("claude")),
                    ("to", json!("copilot")),
                    ("reason", json!("limit")),
                ],
            ),
            ev(
                "harness_switch",
                98 * day,
                &[
                    ("from", json!("claude")),
                    ("to", json!("copilot")),
                    ("reason", json!("limit")),
                ],
            ),
            ev(
                "harness_switch",
                97 * day,
                &[
                    ("from", json!("copilot")),
                    ("to", json!("claude")),
                    ("reason", json!("manual")),
                ],
            ),
            // Missing reason → defaults to manual; missing from → excluded from limit source.
            ev("harness_switch", 96 * day, &[("to", json!("claude"))]),
            // Outside the window → ignored.
            ev(
                "harness_switch",
                10 * day,
                &[("from", json!("claude")), ("to", json!("copilot"))],
            ),
            // A different event kind → ignored.
            ev("agent_session", 99 * day, &[("harness", json!("claude"))]),
        ];
        let s = switches(&evs, now, 30);
        assert_eq!(s.total, 4, "four switches inside the window");
        assert_eq!(
            s.by_reason,
            vec![("limit".to_string(), 2), ("manual".to_string(), 2)],
            "two limit, two manual (one via the default)"
        );
        assert_eq!(
            s.by_direction,
            vec![
                ("claude→copilot".to_string(), 2),
                ("?→claude".to_string(), 1),
                ("copilot→claude".to_string(), 1),
            ],
            "count desc then key asc; missing from renders as ?"
        );
        assert_eq!(
            s.limit_source,
            vec![("claude".to_string(), 2)],
            "only limit-reason switches with a known from count as a limit source"
        );
    }

    #[test]
    fn switches_fold_empty_when_no_switch_events() {
        let s = switches(&[], 100 * 86_400, 30);
        assert_eq!(s.total, 0);
        assert!(s.by_reason.is_empty());
        assert!(s.by_direction.is_empty());
        assert!(s.limit_source.is_empty());
    }
}
