//! Machine-level cron enablement and registration filtering.

use crate::*;

/// Health of the WSL keepalive Windows task that keeps user timers alive.
pub(super) fn keepalive_status(wintasks: &[winsched::WinTask]) -> Option<&'static str> {
    if !winsched::on_wsl() {
        return None;
    }
    match wintasks.iter().find(|task| task.job == "ensure-tmux") {
        Some(task) if task.disabled() => Some("disabled"),
        Some(_) => None,
        None => Some("missing"),
    }
}

/// The compact running-job badge — `⟳ <job>` / `⟳ <job>+N`, `None` when this machine runs no cron
/// or nothing is in flight. A read_dir of `state_dir()/running`: no subprocess, no network. Split
/// out of `emit_cron_running` so `bn tat`'s status header can reuse it in-process (#1349).
pub(crate) fn running_badge(cfg: &Config) -> Option<String> {
    if !cfg.cron_enabled_here() {
        return None;
    }
    let jobs = bn_core::events::running_jobs();
    match jobs.split_first() {
        None => None,
        Some((first, [])) => Some(format!("⟳ {first}")),
        Some((first, rest)) => Some(format!("⟳ {first}+{}", rest.len())),
    }
}

/// Render the compact running-job badge used by tmux.
pub(crate) fn emit_cron_running(cfg: &Config) {
    if let Some(badge) = running_badge(cfg) {
        println!("{badge}");
    }
}

pub(super) fn run_enabled(cfg: &Config) -> bool {
    if cfg.cron_enabled_here() {
        return true;
    }
    eprintln!(
        "bn run: cron is not enabled on this machine ('{}')",
        cfg.machine()
    );
    false
}

/// Emit the disabled dashboard and report whether the caller should stop.
pub(super) fn emit_disabled_dashboard(cfg: &Config, as_json: bool) -> bool {
    if cfg.cron_enabled_here() {
        return false;
    }
    let message = format!("cron is not enabled on this machine ('{}')", cfg.machine());
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "cron",
                serde_json::json!({
                    "state": "not_enabled",
                    "machine": cfg.machine(),
                    "message": message,
                    "jobs": [],
                }),
            )
            .to_json()
        );
    } else {
        println!("{message}");
    }
    true
}

pub(super) fn desired_crons(
    cfg: &Config,
    ctx: Option<&Resolved>,
) -> Vec<bn_core::crons::ResolvedCron> {
    if !cfg.cron_enabled_here() {
        return Vec::new();
    }
    bn_core::crons::discover(cfg, ctx)
        .into_iter()
        .filter(|cron| {
            cfg.job_allowed_here(&cron.entry.job)
                && cron
                    .repo
                    .as_deref()
                    .is_none_or(|repo| cfg.repo_available_here(repo))
                && (cron.entry.only_on.is_empty()
                    || cron
                        .entry
                        .only_on
                        .iter()
                        .any(|machine| machine == cfg.machine()))
        })
        .collect()
}
