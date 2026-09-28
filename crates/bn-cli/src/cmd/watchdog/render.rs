//! Human rendering for `bn watchdog`.

use bn_core::coord::watchdog::{Eval, Watchdog};
use bn_core::util::fmt_local_short;

pub(super) fn print_list(rows: &[Watchdog]) {
    if rows.is_empty() {
        println!("No active watchdogs.");
        return;
    }
    for w in rows {
        let every = w
            .every_secs
            .map(|e| format!("every {}", secs(e)))
            .unwrap_or_else(|| "once".into());
        println!(
            "{:<6} {:<10} {:<8} {:<26} next {}  {every}  {}",
            w.id,
            w.kind,
            w.state,
            w.target,
            fmt_local_short(w.next_due_at),
            w.text.as_deref().or(w.row.as_deref()).unwrap_or("")
        );
    }
}

pub(super) fn print_one(w: &Watchdog) {
    println!("{} — {} ({})", w.id, w.kind, w.state);
    println!("  target:  {}", w.target);
    if w.is_active() {
        println!("  next:    {}", fmt_local_short(w.next_due_at));
    }
    if let Some(e) = w.every_secs {
        println!("  every:   {}", secs(e));
    }
    if let Some(t) = &w.text {
        println!("  text:    {t}");
    }
    if let Some(r) = &w.row {
        println!("  row:     {r}");
    }
    if let Some(p) = w.spec.threshold_pct {
        println!("  at:      {p}% context");
    }
    if let Some(i) = w.spec.idle_secs {
        println!("  idle:    {}", secs(i));
    }
    let cap = w.max_fires.map(|m| format!(" of {m}")).unwrap_or_default();
    println!("  fired:   {}{cap}", w.fires);
    if w.defer_count > 0 {
        println!("  deferred {} time(s) in a row", w.defer_count);
    }
    if let Some(why) = &w.stop_reason {
        println!("  ended:   {why}");
    }
    println!("  by:      {}", w.created_by);
}

pub(super) fn print_evals(evals: &[Eval]) {
    if evals.is_empty() {
        println!("  (not evaluated yet)");
        return;
    }
    println!();
    for e in evals {
        println!(
            "  {}  {:<11} {}",
            fmt_local_short(e.ts),
            e.outcome,
            e.detail.as_deref().unwrap_or("")
        );
    }
}

fn secs(s: i64) -> String {
    match s {
        s if s % 86_400 == 0 => format!("{}d", s / 86_400),
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s if s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}
