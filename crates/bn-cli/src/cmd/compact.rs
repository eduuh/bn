//! `bn compact` — compaction discipline. `plan` reads every live agent's context use; the prep,
//! audit and run steps build on it.

use crate::cli::coord_group::CompactTarget;
use crate::*;
use bn_core::agent::context_usage::{band, Band, ContextUsage};
use serde::Serialize;
use serde_json::json;

pub(crate) const CMD: &str = "compact";

mod prep;
mod run;

/// One agent in the plan.
#[derive(Debug, Serialize)]
pub(crate) struct PlanRow {
    pub(crate) target: String,
    pub(crate) harness: String,
    pub(crate) context: Option<ContextUsage>,
    pub(crate) band: Band,
    /// `compact` | `recap-first` | `ok` | `unknown`.
    pub(crate) advice: &'static str,
}

pub(crate) fn emit_compact(cfg: &Config, action: Option<CompactCmd>, json: bool) {
    match action.unwrap_or(CompactCmd::Plan { threshold: None }) {
        CompactCmd::Plan { threshold } => {
            let rows = plan(cfg, threshold);
            if json {
                println!("{}", Envelope::success(CMD, json!({"agents": rows})).to_json());
            } else {
                print_plan(&rows);
            }
        }
        CompactCmd::Recap {
            text,
            stdin,
            target,
        } => {
            let ctx = ctx_for(cfg, &target, json, false);
            let words = if stdin {
                let mut buf = String::new();
                let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf);
                buf
            } else {
                text.join(" ")
            };
            let path = prep::recap(cfg, &ctx, &words, None)
                .unwrap_or_else(|e| fail_json(json, CMD, "io_error", &e.to_string()));
            if json {
                println!("{}", Envelope::success(CMD, json!({"recap": path})).to_json());
            } else {
                println!("Recap written: {}", path.display());
            }
        }
        CompactCmd::Prep {
            text,
            dry_run,
            hook,
            target,
        } => prep::run_prep(cfg, &ctx_for(cfg, &target, json, hook), text, dry_run, hook, json),
        CompactCmd::Audit { hook, target } => {
            prep::run_audit(cfg, &ctx_for(cfg, &target, json, hook), hook, json)
        }
        CompactCmd::Run {
            seat,
            focus,
            timeout,
            dry_run,
        } => run::run(cfg, &seat, focus.as_deref(), timeout, dry_run, json),
        CompactCmd::Log { target } => {
            let ctx = ctx_for(cfg, &target, json, false);
            let log = bn_core::compact::entries(cfg, &ctx);
            if json {
                println!("{}", Envelope::success(CMD, json!({"entries": log})).to_json());
            } else if log.is_empty() {
                println!("No compactions recorded for {}/{}.", ctx.repo, ctx.branch);
            } else {
                for e in log {
                    let outcome = e
                        .outcome
                        .and_then(|o| serde_json::to_value(o).ok())
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default();
                    println!(
                        "{}  {:<8} {:<11} {}{}",
                        bn_core::util::fmt_local_short(e.ts),
                        if e.phase == bn_core::compact::Phase::Prep { "prep" } else { "restore" },
                        e.trigger.as_deref().unwrap_or(""),
                        e.ctx_pct.map(|p| format!("{p}% ")).unwrap_or_default(),
                        outcome
                    );
                }
            }
        }
    }
}

/// The note to act on. In hook mode a missing repo is a quiet exit, never an error: a hook must
/// not break the harness it runs in.
fn ctx_for(cfg: &Config, t: &CompactTarget, json: bool, hook: bool) -> Resolved {
    match target_ctx(cfg, t.repo.clone(), t.branch.clone()) {
        Some(ctx) => ctx,
        None if hook => std::process::exit(0),
        None => fail_json(json, CMD, "no_target", "not in a git repo — pass --repo/--branch"),
    }
}

/// What a band means for the agent in it.
pub(crate) fn advice(b: Band) -> &'static str {
    match b {
        Band::Compact => "compact",
        Band::Warn => "recap-first",
        Band::Ok => "ok",
        Band::Unknown => "unknown",
    }
}

/// Every live agent, fullest context first; `threshold` keeps only those at or above it.
pub(crate) fn plan(cfg: &Config, threshold: Option<u8>) -> Vec<PlanRow> {
    let fleet = bn_core::agent::supervise(
        cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    );
    let mut rows: Vec<PlanRow> = fleet
        .agents
        .into_iter()
        .map(|a| {
            let b = a.context.as_ref().map_or(Band::Unknown, band);
            PlanRow {
                target: a.target,
                harness: a.harness,
                context: a.context,
                band: b,
                advice: advice(b),
            }
        })
        .filter(|r| {
            threshold.is_none_or(|t| r.context.as_ref().and_then(|c| c.pct).is_some_and(|p| p >= t))
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.context.as_ref().and_then(|c| c.pct)));
    rows
}

fn print_plan(rows: &[PlanRow]) {
    if rows.is_empty() {
        println!("No live agents to plan for.");
        return;
    }
    for r in rows {
        let ctx = match r.context.as_ref() {
            Some(c) if c.pct.is_some() => format!(
                "{:>3}% of {}k",
                c.pct.unwrap_or(0),
                c.window_tokens / 1000
            ),
            Some(c) if c.state == bn_core::agent::context_usage::UsageState::JustCompacted => {
                "just compacted".into()
            }
            _ => "unknown".into(),
        };
        let what = match r.advice {
            "compact" => "compact deliberately: bn compact run (write the recap first)",
            "recap-first" => "write a recap now, compact soon",
            "ok" => "",
            _ => "can't measure (Copilot, or no usage yet)",
        };
        println!("{:<30} {:<8} {:<16} {what}", r.target, r.harness, ctx);
    }
}

/// The post-compaction audit for `ctx`, printed for the session — for a caller that has already
/// consumed the hook's stdin (the SessionStart memory hook).
pub(crate) fn audit_quietly(cfg: &Config, ctx: &Resolved) {
    prep::run_audit(cfg, ctx, false, false);
}
