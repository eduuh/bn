//! How a dispatcher pass reads on a terminal.
//!
//! Split from the tick itself so the pass's logic and its presentation can each grow without
//! crowding the other — the file-size ratchet made the seam mandatory, but it was always the right
//! line: nothing below decides anything, it only says what was decided.

use super::observe_nodes;
use crate::*;
use bn_core::queue::decision::QueueDecision;

pub(super) fn print_decision(d: &QueueDecision) {
    let dry = if d.dry_run { " (dry run)" } else { "" };
    match d.outcome.as_str() {
        "started" => println!("\u{f0e7} started {}{dry}", d.started),
        "empty" => println!("nothing pending{dry}"),
        _ => println!("\u{f017} started nothing — {} pending{dry}", d.pending),
    }
    if let Some(bind) = &d.bind {
        let why = d.reason.as_deref().unwrap_or("");
        println!(
            "  bind: {bind}{}",
            if why.is_empty() {
                String::new()
            } else {
                format!(" — {why}")
            }
        );
    }
    for row in &d.dispatched {
        println!(
            "  → {}/{} · {} · {} on {}{}",
            row.repo,
            row.id,
            row.kind,
            row.exec,
            row.target,
            row.harness
                .as_ref()
                .map(|h| format!(" ({h})"))
                .unwrap_or_default()
        );
    }
    if !d.deferred.is_empty() {
        println!("  held {}:", d.deferred.len());
        for row in d.deferred.iter().take(8) {
            let detail = if row.detail.is_empty() {
                String::new()
            } else {
                format!(" — {}", row.detail)
            };
            println!("    {}/{} · {}{detail}", row.repo, row.id, row.reason);
        }
        if d.deferred.len() > 8 {
            println!("    …and {} more", d.deferred.len() - 8);
        }
    }
    for lane in &d.lanes {
        println!(
            "  lane {:<9} {}/{} in flight · {} pending{}",
            lane.lane,
            lane.in_flight,
            lane.cap,
            lane.pending,
            if lane.saturated { " (full)" } else { "" }
        );
    }
}

/// `bn queue nodes` — the node × capability matrix.
///
/// The direct answer to a `no-capable-node` deferral. A queue that can only say "nothing could run
/// this" is a queue you have to guess at; this says which boxes exist, which are actually alive,
/// and exactly what each one serves.
pub(crate) fn emit_nodes(cfg: &Config, skill: Option<&str>, json: bool) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let ttl = cfg.queue.heartbeat_ttl();
    let nodes = observe_nodes(cfg, now);
    let rows: Vec<serde_json::Value> = nodes
        .iter()
        .map(|n| {
            let axes = bn_core::worker::advertise::split_labels(&n.labels);
            serde_json::json!({
                "node_id": n.node_id,
                "target": n.target.label(),
                "status": format!("{:?}", n.status).to_lowercase(),
                "slots": n.slots.max(1),
                "busy": n.active_attempts,
                "free": n.slots.max(1).saturating_sub(n.active_attempts),
                "stale": bn_core::hosts::is_stale(n, now, ttl),
                "heartbeat_age_secs": (n.last_heartbeat > 0)
                    .then(|| now.saturating_sub(n.last_heartbeat)),
                "skills": axes.skills,
                "harnesses": axes.harnesses,
                "mcps": axes.mcps,
                "labels": axes.other,
            })
        })
        .filter(|row| match skill {
            None => true,
            Some(want) => row["skills"]
                .as_array()
                .is_some_and(|s| s.iter().any(|v| v.as_str() == Some(want))),
        })
        .collect();

    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-nodes",
                serde_json::json!({ "count": rows.len(), "nodes": rows })
            )
            .to_json()
        );
        return;
    }
    if rows.is_empty() {
        match skill {
            Some(want) => println!("no node serves skill '{want}'"),
            None => println!("no nodes"),
        }
        return;
    }
    for row in &rows {
        let stale = if row["stale"].as_bool().unwrap_or(false) {
            "  (heartbeat lapsed)"
        } else {
            ""
        };
        println!(
            "{:<18} {:<9} {:<14} {}/{} free{stale}",
            row["node_id"].as_str().unwrap_or(""),
            row["status"].as_str().unwrap_or(""),
            row["target"].as_str().unwrap_or(""),
            row["free"].as_u64().unwrap_or(0),
            row["slots"].as_u64().unwrap_or(0),
        );
        for (label, key) in [
            ("harness", "harnesses"),
            ("mcp", "mcps"),
            ("skills", "skills"),
        ] {
            let items: Vec<&str> = row[key]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            if items.is_empty() {
                continue;
            }
            println!("  {label:<8} {}", items.join(", "));
        }
    }
}
