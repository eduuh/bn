//! `bn compact run` — a deliberate compaction of another live agent: plan, prep, compact, wait for
//! the restore, report.

use super::prep::prep_now;
use super::CMD;
use crate::cmd::agent::{try_deliver_live, LiveDelivery};
use crate::*;
use bn_core::compact::{self, Entry, Outcome, Phase};
use serde_json::json;

/// How often to look for the restore while waiting.
const POLL_SECS: u64 = 5;

pub(super) fn run(
    cfg: &Config,
    seat: &str,
    focus: Option<&str>,
    timeout: u64,
    dry_run: bool,
    json: bool,
) {
    let seat = bn_core::coord::normalize_seat(seat)
        .unwrap_or_else(|e| fail_json(json, CMD, "invalid_seat", &e.to_string()));
    let Some((repo, branch)) = bn_core::coord::agent_target(&seat) else {
        fail_json(json, CMD, "invalid_seat", "a person can't be compacted — use repo:branch")
    };
    let ctx = bn_core::resolver::resolve_target(
        cfg,
        std::path::Path::new("."),
        Some(repo.to_string()),
        Some(branch.to_string()),
    )
    .unwrap_or_else(|_| fail_json(json, CMD, "no_target", &format!("can't resolve {seat}")));

    // Only a live Claude session can be compacted this way.
    let agent = super::plan(cfg, None).into_iter().find(|r| {
        bn_core::coord::normalize_seat(&r.target).ok().as_deref() == Some(seat.as_str())
    });
    let Some(agent) = agent else {
        fail_json(json, CMD, "not_live", &format!("{seat} has no live session"))
    };
    if agent.harness != "claude" {
        fail_json(
            json,
            CMD,
            "unsupported",
            &format!("{seat} runs {}; only Claude has /compact", agent.harness),
        );
    }
    let pct = agent.context.as_ref().and_then(|c| c.pct);
    let command = match focus.map(str::trim).filter(|f| !f.is_empty()) {
        Some(f) => format!("/compact {f}"),
        None => "/compact".to_string(),
    };
    if dry_run {
        let steps = [
            format!("write {seat}'s recap and handoff, log a deliberate prep"),
            format!("type `{command}` into its pane"),
            format!("wait up to {timeout}s for it to come back and audit itself"),
        ];
        if json {
            let data = json!({"seat": seat, "ctx_pct": pct, "advice": agent.advice, "steps": steps});
            println!("{}", Envelope::success(CMD, data).to_json());
        } else {
            let at = pct.map(|p| format!(" at {p}%")).unwrap_or_default();
            println!("Plan for {seat}{at} ({}):", agent.advice);
            for (i, s) in steps.iter().enumerate() {
                println!("  {}. {s}", i + 1);
            }
        }
        return;
    }

    let started = bn_core::util::now_epoch_secs();
    let (_, problems) = prep_now(cfg, &ctx, "", None, "deliberate", pct);
    if !problems.is_empty() {
        fail_json(json, CMD, "prep_failed", &format!("not compacting: {}", problems.join("; ")));
    }
    match try_deliver_live(cfg, &seat, &command) {
        LiveDelivery::Delivered => {}
        other => fail_json(
            json,
            CMD,
            "not_delivered",
            &format!("prepped, but couldn't type into {seat}'s pane ({other:?}); nothing compacted"),
        ),
    }
    if !json {
        println!("{command} sent to {seat}; waiting for it to come back…");
    }
    let deadline = started + timeout as i64;
    let outcome = loop {
        let restored = compact::entries(cfg, &ctx)
            .into_iter()
            .rev()
            .find(|e| e.phase == Phase::Restore && e.ts >= started)
            .and_then(|e| e.outcome);
        if let Some(o) = restored {
            break o;
        }
        if bn_core::util::now_epoch_secs() >= deadline {
            let waiting = Entry {
                ts: bn_core::util::now_epoch_secs(),
                phase: Phase::Restore,
                session: None,
                trigger: Some("deliberate".into()),
                ctx_pct: None,
                recap: false,
                outcome: Some(Outcome::Waiting),
            };
            bn_core::util::best_effort(compact::append(cfg, &ctx, &waiting), "compact run: log");
            break Outcome::Waiting;
        }
        std::thread::sleep(std::time::Duration::from_secs(POLL_SECS));
    };
    let name = serde_json::to_value(outcome)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    if json {
        println!("{}", Envelope::success(CMD, json!({"seat": seat, "outcome": name})).to_json());
    } else {
        println!("{seat}: {name}");
    }
}
