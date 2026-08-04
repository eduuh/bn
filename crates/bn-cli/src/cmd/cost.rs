//! `bn cost` — the unified token-usage + $ cost report (#381).
//!
//! Rolls every recorded agent run up by work-class into one board, in two clearly-labeled cost
//! units: **AIC** (the real Copilot bill) and **$ (API-equiv)** (a cache-aware price-table
//! benchmark — what the same tokens would cost at public per-model API list prices). The heavy
//! lifting lives in [`bn_core::cost::rollup`] so bn-web (#391/#389) can reuse the same fold.

use crate::cmd::staff::fmt_tokens;
use crate::Config;
use crate::Envelope;
use bn_core::cost::{self, CostRow, CostView, LiveCost, WorkClass};

/// `bn cost [--by category|repo|model|machine|day] [--window <days>] [--live] [--json]`. Defaults
/// to a unified current month-to-date report: category totals plus compact repo/model/day sections.
pub(crate) fn emit_cost(
    cfg: &Config,
    by: Option<&str>,
    window: Option<i64>,
    live: bool,
    as_json: bool,
) {
    if live {
        emit_live(cfg, as_json);
        return;
    }

    let dim = match by {
        Some(by) => match Dimension::parse(by) {
            Some(dim) => Some(dim),
            None => {
                let msg =
                    format!("unknown --by '{by}' (use: category | repo | model | machine | day)");
                if as_json {
                    println!(
                        "{}",
                        Envelope::failure("cost", "bad-dimension", msg).to_json()
                    );
                } else {
                    eprintln!("bn cost: {msg}");
                }
                return;
            }
        },
        None => None,
    };

    let window_days = window.unwrap_or_else(default_window_days).max(0);
    let view = cost::rollup(cfg, &cost::CostFilter::window(window_days));

    if as_json {
        let data = serde_json::to_value(&view).unwrap_or(serde_json::Value::Null);
        println!("{}", Envelope::success("cost", data).to_json());
        return;
    }

    match dim {
        Some(dim) => render_view(&view, dim),
        None => render_unified(&view),
    }
}

/// Which breakdown the CLI shows. The [`CostView`] always computes every dimension; this only
/// picks which one to render.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Dimension {
    Category,
    Repo,
    Model,
    Machine,
    Day,
}

impl Dimension {
    fn parse(s: &str) -> Option<Dimension> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "category" | "class" | "work-class" => Dimension::Category,
            "repo" => Dimension::Repo,
            "model" => Dimension::Model,
            "machine" => Dimension::Machine,
            "day" => Dimension::Day,
            _ => return None,
        })
    }

    fn header(self) -> &'static str {
        match self {
            Dimension::Category => "CATEGORY",
            Dimension::Repo => "REPO",
            Dimension::Model => "MODEL",
            Dimension::Machine => "MACHINE",
            Dimension::Day => "DAY",
        }
    }

    /// The pre-computed rows for this dimension.
    fn rows(self, view: &CostView) -> &[CostRow] {
        match self {
            Dimension::Category => &view.by_category,
            Dimension::Repo => &view.by_repo,
            Dimension::Model => &view.by_model,
            Dimension::Machine => &view.by_machine,
            Dimension::Day => &view.by_day,
        }
    }

    /// Human label for a row key (categories render title-case).
    fn key_label(self, key: &str) -> String {
        match self {
            Dimension::Category => {
                WorkClass::parse(key).map_or_else(|| key.to_string(), |c| c.label().to_string())
            }
            _ => key.to_string(),
        }
    }
}

/// AIC (the real Copilot bill) — `—` when a group has no Copilot runs.
fn fmt_aic(aic: Option<f64>) -> String {
    aic.map_or_else(|| "—".to_string(), |v| format!("{v:.2}"))
}

/// $ (API-equiv) benchmark — `—` when nothing in the group was priceable.
fn fmt_usd(usd: Option<f64>) -> String {
    usd.map_or_else(|| "—".to_string(), |v| format!("${v:.2}"))
}

/// Input tokens with the cached subset in parentheses, e.g. `4.2m(3.9m)`.
fn fmt_input(input: i64, cache: i64) -> String {
    format!(
        "{}({})",
        fmt_tokens(input.max(0) as u64),
        fmt_tokens(cache.max(0) as u64)
    )
}

fn render_view(view: &CostView, dim: Dimension) {
    println!(
        "COST · last {}d · by {}    (AIC = real Copilot bill · $ = API-equiv benchmark)",
        view.window_days,
        dim.header().to_ascii_lowercase()
    );
    let rows = dim.rows(view);
    if rows.is_empty() {
        println!("  no recorded agent runs in this window.");
        return;
    }
    print_rows(dim.header(), rows, dim, None);
    print_total(view);
    println!(
        "  $ (API-equiv) is a cache-aware benchmark, not a bill; AIC is the metered Copilot cost."
    );
}

fn render_unified(view: &CostView) {
    println!(
        "COST · last {}d · unified    (AIC = real Copilot bill · $ = API-equiv benchmark)",
        view.window_days
    );
    if view.totals.sessions == 0 {
        println!("  no recorded agent runs in this window.");
        return;
    }
    print_rows("CATEGORY", &view.by_category, Dimension::Category, None);
    print_total(view);
    print_rows("TOP REPOS", &view.by_repo, Dimension::Repo, Some(5));
    print_rows("TOP MODELS", &view.by_model, Dimension::Model, Some(5));
    print_rows("LAST 7 DAYS", &view.by_day, Dimension::Day, Some(7));
    println!(
        "  $ (API-equiv) is a cache-aware benchmark, not a bill; AIC is the metered Copilot cost."
    );
}

fn print_rows(header: &str, rows: &[CostRow], dim: Dimension, top: Option<usize>) {
    if rows.is_empty() {
        return;
    }
    println!();
    println!(
        "  {:<18} {:>5} {:>14} {:>9} {:>10} {:>13}",
        header, "SESS", "INPUT(cache)", "OUTPUT", "AIC", "$ (API-eq)"
    );
    for r in rows.iter().take(top.unwrap_or(usize::MAX)) {
        println!(
            "  {:<18} {:>5} {:>14} {:>9} {:>10} {:>13}",
            truncate(&dim.key_label(&r.key), 18),
            r.sessions,
            fmt_input(r.input_tokens, r.cache_read_tokens),
            fmt_tokens(r.output_tokens.max(0) as u64),
            fmt_aic(r.aic),
            fmt_usd(r.cost_usd),
        );
    }
}

fn print_total(view: &CostView) {
    let t = &view.totals;
    println!(
        "  {:<18} {:>5} {:>14} {:>9} {:>10} {:>13}",
        "total",
        t.sessions,
        fmt_input(t.input_tokens, t.cache_read_tokens),
        fmt_tokens(t.output_tokens.max(0) as u64),
        fmt_aic(t.aic),
        fmt_usd(t.cost_usd),
    );
}

pub(crate) fn emit_live(cfg: &Config, as_json: bool) {
    let live = cost::live_rollup(cfg);
    if as_json {
        let data = serde_json::to_value(&live).unwrap_or(serde_json::Value::Null);
        println!("{}", Envelope::success("cost", data).to_json());
        return;
    }
    render_live(&live);
}

fn render_live(live: &LiveCost) {
    println!(
        "COST · live (in-flight) · {} agent{} · {} output tokens so far",
        live.agents,
        if live.agents == 1 { "" } else { "s" },
        fmt_tokens(live.output_tokens.max(0) as u64),
    );
    if live.by_category.is_empty() {
        println!("  no agents currently running.");
        return;
    }
    println!("  {:<18} {:>7} {:>10}", "CATEGORY", "AGENTS", "OUTPUT");
    for r in &live.by_category {
        println!(
            "  {:<18} {:>7} {:>10}",
            truncate(
                &WorkClass::parse(&r.key).map_or_else(|| r.key.clone(), |c| c.label().to_string()),
                18
            ),
            r.sessions,
            fmt_tokens(r.output_tokens.max(0) as u64),
        );
    }
    println!("  $ / AIC land once each run finishes and is ingested (bn cost).");
}

/// Default window when `--window` is omitted: the current month-to-date, i.e. today's day-of-month
/// (so on the 8th the last 8 days cover the 1st onward). Falls back to 30 if the date is unparsable.
fn default_window_days() -> i64 {
    bn_core::util::today()
        .get(8..10)
        .and_then(|d| d.parse::<i64>().ok())
        .filter(|d| *d > 0)
        .unwrap_or(30)
}

/// Clip a display key to `max` chars (categories/repos are short; a long model id would break the
/// column).
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}


