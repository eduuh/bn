//! Fold-level tests for the cost surface: the event → [`RunSample`] normalization and the
//! groupings [`fold`] builds from it. Pricing itself is tested next door in `price.rs`.
//!
//! Split into its own file rather than an inline `#[cfg(test)]` module because `mod.rs` sits
//! close to the 1000-line ratchet, and tests are the part that should be free to grow.

use super::*;
use serde_json::json;

/// An `agent_session` event as [`crate::agent::session::AgentSession::to_fields`] writes one,
/// with the token shape of a real cached Claude Code run: a trivial fresh-input figure against
/// a large cache read and a 1-hour cache write.
fn claude_event(fields: serde_json::Value) -> Event {
    let mut f = serde_json::Map::new();
    f.insert("session_id".into(), json!("S-1"));
    f.insert("harness".into(), json!("claude"));
    f.insert("repo".into(), json!("bn"));
    f.insert("models".into(), json!("claude-opus-5"));
    f.insert("work_class".into(), json!("coding"));
    if let serde_json::Value::Object(extra) = fields {
        f.extend(extra);
    }
    Event {
        kind: "agent_session".into(),
        ts: 1_000,
        fields: f,
    }
}

fn sample_of(fields: serde_json::Value) -> RunSample {
    sample_from_agent_session(&claude_event(fields), "mac", &BTreeMap::new())
}

#[test]
fn a_claude_sessions_cache_writes_reach_the_priced_sample() {
    let s = sample_of(json!({
        "input_tokens": 2,
        "cache_read_tokens": 56_575,
        "cache_write_1h_tokens": 35_163,
        "output_tokens": 197,
    }));
    assert_eq!(s.usage.cache_write_1h_tokens, 35_163);
    assert_eq!(s.usage.cache_write_tokens(), 35_163);
    // The writes are the dominant term: without them this run prices at well under half.
    let without = sample_of(json!({
        "input_tokens": 2,
        "cache_read_tokens": 56_575,
        "output_tokens": 197,
    }));
    assert!(s.cost_usd.unwrap() > without.cost_usd.unwrap() * 2.0);
}

#[test]
fn an_event_recorded_before_cache_writes_existed_still_folds() {
    // Zero writes, not a missing sample: history stays foldable and simply under-reports.
    let s = sample_of(json!({ "input_tokens": 10, "output_tokens": 20 }));
    assert_eq!(s.usage.cache_write_tokens(), 0);
    assert!(s.cost_usd.is_some());
}

#[test]
fn a_copilot_events_input_is_net_of_both_cache_buckets() {
    let mut e = claude_event(json!({
        "input_tokens": 156_985,
        "cache_read_tokens": 111_104,
        "cache_write_5m_tokens": 20_000,
        "output_tokens": 1_910,
    }));
    e.fields.insert("harness".into(), json!("copilot"));
    e.fields.insert("models".into(), json!("gpt-5.4-mini"));
    let s = sample_from_agent_session(&e, "mac", &BTreeMap::new());
    assert_eq!(s.usage.input_tokens, 25_881);
    // Disjointness is the invariant pricing depends on: the buckets must re-sum to the total.
    assert_eq!(s.usage.prompt_tokens(), 156_985);
}

#[test]
fn the_fold_sums_cache_writes_into_every_breakdown() {
    let view = fold(
        &[
            sample_of(json!({ "cache_write_1h_tokens": 1_000 })),
            sample_of(json!({ "cache_write_5m_tokens": 500 })),
        ],
        7,
    );
    assert_eq!(view.totals.cache_write_tokens, 1_500);
    assert_eq!(view.by_category[0].cache_write_tokens, 1_500);
    assert_eq!(view.by_repo[0].cache_write_tokens, 1_500);
    assert_eq!(view.by_day_category[0].cache_write_tokens, 1_500);
}

#[test]
fn the_two_harnesses_fold_into_their_own_rows() {
    let mut copilot =
        claude_event(json!({ "output_tokens": 100, "cost_nanoaiu": 2_000_000_000i64 }));
    copilot.fields.insert("harness".into(), json!("copilot"));
    copilot.fields.insert("models".into(), json!("gpt-5.4"));
    let view = fold(
        &[
            sample_of(json!({ "output_tokens": 200 })),
            sample_from_agent_session(&copilot, "mac", &BTreeMap::new()),
        ],
        7,
    );
    let row = |k: &str| {
        view.by_harness
            .iter()
            .find(|r| r.key == k)
            .unwrap_or_else(|| panic!("no {k} row in {:?}", view.by_harness))
    };
    assert_eq!(view.by_harness.len(), 2);
    assert_eq!(row("claude").sessions, 1);
    assert_eq!(row("copilot").sessions, 1);
}

#[test]
fn a_claude_row_reports_no_aic_which_is_why_the_comparison_reads_the_dollar_column() {
    // The trap this breakdown exists to survive: AIC is metered by Copilot only, so a Claude row's
    // `—` means "not measured here", not "cost nothing". Both harnesses do carry a $ benchmark.
    let view = fold(&[sample_of(json!({ "output_tokens": 200 }))], 7);
    let claude = &view.by_harness[0];
    assert_eq!(claude.key, "claude");
    assert_eq!(claude.aic, None);
    assert!(claude.cost_usd.is_some_and(|d| d > 0.0));
}

#[test]
fn a_source_that_records_no_harness_gets_its_own_row_rather_than_someone_elses() {
    // Staff runs dispatch headless and log no harness; they must not silently inflate either
    // product's row in a comparison.
    let staff = Event {
        kind: "staff_run".into(),
        ts: 1_000,
        fields: serde_json::Map::from_iter([("tokens".to_string(), json!(500))]),
    };
    let view = fold(&[sample_from_staff_run(&staff, "mac")], 7);
    assert_eq!(view.by_harness.len(), 1);
    assert_eq!(view.by_harness[0].key, "—");
}

#[test]
fn harness_tokens_get_their_product_names_and_unknowns_keep_their_own() {
    assert_eq!(harness_label("claude"), "Claude Code");
    assert_eq!(harness_label("copilot"), "Copilot");
    assert_eq!(harness_label(""), "—");
    // A harness bn learns about later shows up under its own name, not folded into an existing row.
    assert_eq!(harness_label("codex"), "codex");
}

#[test]
fn billed_tokens_meters_writes_but_not_reads() {
    let totals = CostTotals {
        input_tokens: 100,
        cache_read_tokens: 1_000_000,
        cache_write_tokens: 50,
        output_tokens: 200,
        ..CostTotals::default()
    };
    // Reads dominate the raw count and bill at a tenth of input; a rate that counted them would
    // throttle on the cheapest traffic there is. Writes bill at 1.25-2x, so they must count.
    assert_eq!(totals.billed_tokens(), 350);
}
