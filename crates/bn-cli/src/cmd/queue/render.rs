//! Human rendering for `bn queue`.

use bn_core::coord::{QItem, QState, Transition};
use bn_core::util::fmt_local_short;

/// One line per row: id, state, seat, tier and deadline, then the first line of the body.
pub(super) fn print_rows(rows: &[QItem], seat: Option<&str>, now: i64) {
    if rows.is_empty() {
        match seat {
            Some(s) => println!("No open queue rows for {s}."),
            None => println!("No open queue rows."),
        }
        return;
    }
    for q in rows {
        let who = q.owner.as_deref().unwrap_or(&q.dest);
        println!(
            "{:<5} {:<11} {:<28} {:<8} {}{}",
            q.id,
            q.state,
            who,
            q.tier,
            first_line(&q.body),
            due(q, now)
        );
    }
}

/// A row in full.
pub(super) fn print_item(q: &QItem, now: i64) {
    println!("{} — {}", q.id, q.state);
    println!("  for:     {}", q.dest);
    if let Some(owner) = &q.owner {
        println!("  owner:   {owner}");
    }
    println!("  from:    {}", q.source);
    println!("  tier:    {}{}", q.tier, due(q, now));
    if let Some(r) = &q.ref_ {
        println!("  ref:     {r}");
    }
    if let Some(p) = &q.parent {
        println!("  parent:  {p}");
    }
    if let Some(n) = &q.next {
        println!("  next:    {n}");
    }
    if let Some(note) = &q.closure_note {
        println!("  closed:  {note}");
    }
    if let Some(todo) = &q.mirror_todo {
        println!("  todo:    {todo} ({})", q.mirror_state);
    }
    println!("  opened:  {}", fmt_local_short(q.created_at));
    println!();
    for line in q.body.lines() {
        println!("  {line}");
    }
}

/// A row's history, oldest first.
pub(super) fn print_transitions(history: &[Transition]) {
    for t in history {
        let arrow = match (&t.from, &t.to) {
            (Some(f), Some(to)) => format!("{f} → {to}"),
            (None, Some(to)) => format!("→ {to}"),
            _ => String::new(),
        };
        let note = t.note.as_deref().map(|n| format!("  {n}")).unwrap_or_default();
        println!(
            "{}  {:<22} {:<20} {:<26}{note}",
            fmt_local_short(t.ts),
            t.actor,
            t.verb,
            arrow
        );
    }
}

fn due(q: &QItem, now: i64) -> String {
    match q.closure_required_at {
        Some(d) if q.qstate() == QState::InProgress => {
            let late = if d < now { " OVERDUE" } else { "" };
            format!("  due {}{late}", fmt_local_short(d))
        }
        _ => String::new(),
    }
}

fn first_line(body: &str) -> &str {
    body.lines().next().unwrap_or("").trim()
}

/// `bn queue sweep --dry-run`.
pub(super) fn print_plan(plan: &bn_core::coord::sweep::Plan) {
    let ids = |v: &[i64]| {
        v.iter()
            .map(|n| bn_core::coord::qid(*n))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if plan == &bn_core::coord::sweep::Plan::default() {
        println!("Nothing to do.");
        return;
    }
    if !plan.nudge.is_empty() {
        println!("nudge:  {}", ids(&plan.nudge));
    }
    for a in &plan.raise {
        println!("raise:  [{}] {}", a.severity, a.title);
    }
    for k in &plan.ack {
        println!("clear:  {k}");
    }
    if !plan.mirror_retry.is_empty() {
        println!("mirror: {}", ids(&plan.mirror_retry));
    }
}
