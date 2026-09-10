//! Machine-scope guards and status rendering for `bn queue`.

use crate::*;
use bn_core::backlog::{self, BacklogEntry, BacklogStatus};

pub(crate) fn require_repo_allowed(cfg: &Config, repo: &str, json: bool) {
    if !cfg.repo_allowed_here(repo) {
        fail_json(
            json,
            "queue",
            "repo_not_enabled",
            &format!(
                "bn queue: repository '{repo}' is not enabled on this machine ('{}')",
                cfg.machine()
            ),
        );
    }
}

pub(crate) fn emit_not_enabled_status(cfg: &Config, json: bool) {
    let message = format!("queue is not enabled on this machine ('{}')", cfg.machine());
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-status",
                serde_json::json!({
                    "state": "not_enabled",
                    "machine": cfg.machine(),
                    "message": message,
                }),
            )
            .to_json()
        );
    } else {
        println!("{message}");
    }
}

pub(crate) fn emit_disabled_badge(cfg: &Config, json: bool) {
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-badge",
                serde_json::json!({
                    "state": "not_enabled",
                    "machine": cfg.machine(),
                    "text": "",
                }),
            )
            .to_json()
        );
    }
}

/// The queue badge's computed state: the compact text plus the counters `--json` reports.
pub(crate) struct QueueBadge {
    pub(crate) text: String,
    stale: bool,
    failed: usize,
    foreground_pending: usize,
    foreground_running: usize,
    held: String,
}

/// Compute the queue badge — `Q !3` (failed) / `Q stale` / `Q !conflict` / `Q 2▶ 1·fg`, empty when
/// the queue has nothing worth saying. Split out of `emit_badge` so `bn tat`'s status header can
/// render the same text without shelling out to `bn queue badge` (#1349).
pub(crate) fn badge(cfg: &Config, now: i64) -> QueueBadge {
    let decision = bn_core::queue::decision::QueueDecision::read();
    let stale = decision
        .as_ref()
        .is_none_or(|d| d.dry_run || now.saturating_sub(d.at) > 180);
    let backlog = backlog::load(&backlog::backlog_path());
    let entries: Vec<&BacklogEntry> = backlog
        .entries()
        .iter()
        .filter(|entry| cfg.repo_allowed_here(&entry.repo))
        .collect();
    let failed = entries
        .iter()
        .filter(|e| e.status == BacklogStatus::Failed)
        .filter(|e| e.updated_at == 0 || now.saturating_sub(e.updated_at) <= 86_400)
        .count();
    let foreground_pending = entries
        .iter()
        .filter(|e| e.status == BacklogStatus::Pending)
        .filter(|e| !bn_core::queue::classify::classify(e).mode.is_autonomous())
        .count();
    let foreground_running = entries
        .iter()
        .filter(|e| e.status == BacklogStatus::Leased)
        .filter(|e| !bn_core::queue::classify::classify(e).mode.is_autonomous())
        .count();
    let held = decision
        .as_ref()
        .filter(|d| d.outcome == "held" && d.pending > 0)
        .and_then(attention_hold)
        .unwrap_or_default()
        .to_string();
    let text = if failed > 0 {
        format!("Q !{failed}")
    } else if stale {
        "Q stale".to_string()
    } else if matches!(held.as_str(), "no-capable-node" | "cant-merge" | "conflict") {
        format!("Q !{held}")
    } else if foreground_running > 0 || foreground_pending > 0 {
        let mut parts = Vec::new();
        if foreground_running > 0 {
            parts.push(format!("{foreground_running}▶"));
        }
        if foreground_pending > 0 {
            parts.push(format!("{foreground_pending}·fg"));
        }
        format!("Q {}", parts.join(" "))
    } else {
        String::new()
    };
    QueueBadge {
        text,
        stale,
        failed,
        foreground_pending,
        foreground_running,
        held,
    }
}

pub(crate) fn emit_badge(cfg: &Config, now: i64, json: bool) {
    let b = badge(cfg, now);
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-badge",
                serde_json::json!({
                    "text": b.text,
                    "stale": b.stale,
                    "failed": b.failed,
                    "foreground_pending": b.foreground_pending,
                    "foreground_running": b.foreground_running,
                    "held": b.held,
                }),
            )
            .to_json()
        );
    } else if !b.text.is_empty() {
        println!("{}", b.text);
    }
}

fn attention_hold(decision: &bn_core::queue::decision::QueueDecision) -> Option<&str> {
    decision
        .bind
        .as_deref()
        .filter(|bind| matches!(*bind, "no-capable-node" | "cant-merge" | "conflict"))
        .or_else(|| {
            decision
                .deferred
                .iter()
                .map(|row| row.reason.as_str())
                .find(|reason| matches!(*reason, "no-capable-node" | "cant-merge" | "conflict"))
        })
}
