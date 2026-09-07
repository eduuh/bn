//! Machine-scope guards and status rendering for `bn queue`.

use crate::*;
use bn_core::backlog::{self, BacklogEntry, BacklogStatus};

pub(crate) fn disable_if_authorized(cfg: &Config, disable: impl FnOnce()) {
    if cfg.destructive_reconcile_allowed() {
        disable();
    }
}

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

pub(crate) fn emit_badge(cfg: &Config, now: i64, json: bool) {
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
        .unwrap_or_default();
    let text = if failed > 0 {
        format!("Q !{failed}")
    } else if stale {
        "Q stale".to_string()
    } else if matches!(held, "no-capable-node" | "cant-merge" | "conflict") {
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
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-badge",
                serde_json::json!({
                    "text": text,
                    "stale": stale,
                    "failed": failed,
                    "foreground_pending": foreground_pending,
                    "foreground_running": foreground_running,
                    "held": held,
                }),
            )
            .to_json()
        );
    } else if !text.is_empty() {
        println!("{text}");
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn malformed_config_does_not_disable_queue_service() {
        let _lock = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        std::fs::write(&path, "[machines.broken\nqueue = true").expect("write malformed config");
        let _config = bn_core::test_support::EnvGuard::set("BN_CONFIG", &path);
        let _hostname = bn_core::test_support::EnvGuard::set("BN_HOSTNAME", "broken");
        let cfg = Config::load();
        let disabled = Cell::new(false);

        disable_if_authorized(&cfg, || disabled.set(true));

        assert!(!cfg.queue_enabled_here());
        assert!(!disabled.get());
    }

    #[test]
    fn unreadable_config_does_not_disable_queue_service() {
        let _lock = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("missing-config.toml");
        let _config = bn_core::test_support::EnvGuard::set("BN_CONFIG", &path);
        let _hostname = bn_core::test_support::EnvGuard::set("BN_HOSTNAME", "missing");
        let cfg = Config::load();
        let disabled = Cell::new(false);

        disable_if_authorized(&cfg, || disabled.set(true));

        assert!(!cfg.queue_enabled_here());
        assert!(!disabled.get());
    }

    #[test]
    fn loaded_disabled_config_reconciles_queue_service() {
        let cfg = Config {
            machine_profile: None,
            config_file_loaded: true,
            ..Config::default()
        };
        let disabled = Cell::new(false);

        disable_if_authorized(&cfg, || disabled.set(true));

        assert!(disabled.get());
    }
}
