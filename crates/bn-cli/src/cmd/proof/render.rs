//! Human rendering for `bn proof`.

use bn_core::proof::{ItemState, Listing, Proof, Readiness, Verdict};

pub(super) fn print_readiness(r: &Readiness, head: Option<&str>, stray: &[String], bad: usize) {
    if r.items.is_empty() {
        println!("No done-when criteria on this branch. Add one: bn add done-when \"…\"");
    }
    for i in &r.items {
        let (mark, what) = match &i.state {
            ItemState::Accepted { proof, judge } => ("✓", format!("accepted — {proof} by {judge}")),
            ItemState::Stale { proof, sha } => (
                "~",
                format!("stale — {proof} was at {}, head moved", short(sha)),
            ),
            ItemState::Attached { proofs } => ("…", format!("awaiting a verdict — {}", proofs.join(", "))),
            ItemState::Rejected { proof, judge } => ("✗", format!("rejected — {proof} by {judge}")),
            ItemState::Unproven => ("·", "no proof".to_string()),
        };
        let tick = if i.hand_ticked { "  [ticked by hand]" } else { "" };
        println!("{mark} {:<4} {}  — {what}{tick}", i.id, i.text);
    }
    if !r.items.is_empty() {
        let at = head.map(|h| format!(" at {}", short(h))).unwrap_or_default();
        if r.ready {
            println!("\nReady{at}: every criterion has accepted evidence. That says what was accepted, not that the work is good.");
        } else {
            println!("\nNot ready{at}.");
        }
    }
    if !stray.is_empty() {
        println!("Unregistered in proof/ (never counted): {}", stray.join(", "));
    }
    if bad > 0 {
        println!("{bad} unreadable line(s) in proof/verdicts.jsonl were skipped.");
    }
}

pub(super) fn print_list(listing: &Listing, verdicts: &[Verdict]) {
    if listing.proofs.is_empty() {
        println!("No proof yet.");
    }
    for p in &listing.proofs {
        let last = verdicts
            .iter()
            .rev()
            .find(|v| v.proof == p.id)
            .map(|v| format!("{} by {}", v.verdict.as_str(), v.judge))
            .unwrap_or_else(|| "no verdict".into());
        let exit = p.exit.map(|c| format!(" exit {c}")).unwrap_or_default();
        println!(
            "{:<4} {:<10} {:<24} {}{exit}  [{last}]  {}",
            p.id,
            p.kind,
            if p.for_.is_empty() { "-".to_string() } else { p.for_.join(",") },
            p.sha.as_deref().map(short).unwrap_or("-"),
            p.title
        );
    }
    if !listing.unregistered.is_empty() {
        println!("Unregistered (never counted): {}", listing.unregistered.join(", "));
    }
}

pub(super) fn print_proof(p: &Proof, verdicts: &[Verdict]) {
    println!("{} — {} · {}", p.id, p.kind, p.title);
    println!("  for:     {}", if p.for_.is_empty() { "-".into() } else { p.for_.join(", ") });
    println!("  by:      {} via {}", p.author, p.source);
    println!("  at:      {}{}", p.sha.as_deref().map(short).unwrap_or("-"), if p.dirty { " (dirty)" } else { "" });
    if let Some(c) = &p.cmd {
        println!("  cmd:     {c}{}", p.exit.map(|e| format!("  → exit {e}")).unwrap_or_default());
    }
    if let Some(a) = &p.attachment {
        let t = if p.truncated { ", truncated" } else { "" };
        println!("  file:    proof/{a} ({} bytes{t})", p.bytes.unwrap_or(0));
    }
    println!("  created: {}", p.created);
    for v in verdicts.iter().filter(|v| v.proof == p.id) {
        println!("  {} by {} — {}", v.verdict.as_str(), v.judge, v.reason);
    }
    if !p.body.is_empty() {
        println!();
        for l in p.body.lines() {
            println!("  {l}");
        }
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}
