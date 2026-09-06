//! Applying the auto-approval policy to a live lane, and recording every decision it makes.
//!
//! [`bn_core::agent::approval`] decides; this realizes. Kept out of `cmd/orchestrate.rs` (which is
//! at its file-size ratchet) so the supervisor's arm stays a few lines, and so the *same* policy
//! path is reachable from outside an orchestrate wave — an agent started by a plain `bn dispatch`
//! used to get no policy at all.
//!
//! Everything decided here is appended to `<state_dir>/approvals.jsonl`, whether it was approved or
//! escalated. An unattended approval you cannot audit afterwards is not a feature, it is a hole:
//! the log is what makes "what did bn say yes to while I was asleep?" a question with an answer.

use bn_core::agent::{
    decide_with_builtin_default, ApprovalDecision, ApprovalPolicy, LaneKind, PendingPrompt,
    PolicyCtx,
};
use bn_core::resolver::Config;

/// One recorded decision.
#[derive(serde::Serialize)]
struct AuditEntry<'a> {
    ts: i64,
    target: &'a str,
    kind: &'a str,
    question: &'a str,
    subject: Option<&'a str>,
    /// Every option the dialog offered, so the log shows what bn *could* have picked, not only what
    /// it did. Reviewing an approval means seeing the choice it faced.
    options: Vec<String>,
    approved: bool,
    option: Option<u8>,
    reason: &'a str,
}

fn audit_path() -> std::path::PathBuf {
    bn_core::resolver::state_dir().join("approvals.jsonl")
}

/// Append one decision to the audit log. Best-effort: a log that cannot be written must not stop a
/// supervision pass, but it is reported once rather than swallowed.
fn record(target: &str, prompt: &PendingPrompt, decision: &ApprovalDecision) {
    let (approved, option) = match decision {
        ApprovalDecision::Auto { option, .. } => (true, Some(*option)),
        ApprovalDecision::Escalate { .. } => (false, None),
    };
    let entry = AuditEntry {
        ts: bn_core::util::now_epoch_secs(),
        target,
        kind: prompt.kind.label(),
        question: &prompt.question,
        subject: prompt.subject.as_deref(),
        options: prompt
            .options
            .iter()
            .map(|o| format!("{}. {} [{}]", o.number, o.label, o.semantic.label()))
            .collect(),
        approved,
        option,
        reason: decision.reason(),
    };
    let Ok(line) = serde_json::to_string(&entry) else {
        return;
    };
    let path = audit_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    use std::io::Write;
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => {
            let _ = writeln!(f, "{line}");
        }
        Err(e) => eprintln!("bn: could not record approval to {} ({e})", path.display()),
    }
}

/// Run the policy over every live agent that is currently blocked on a dialog.
///
/// The orchestrate supervisor has always applied its rule to its own lanes. An agent started by a
/// plain `bn dispatch` — or one you launched by hand — sat on its permission prompt until a human
/// noticed, because nothing outside a wave was watching. This is that pass, and it is where a
/// `scope` of `dispatched` or `all` actually takes effect.
///
/// Returns `(answered, escalated)`. `dry_run` decides nothing differently — it just does not send
/// the keystroke, so you can see what a policy *would* do before enabling it.
pub(crate) fn sweep(cfg: &Config, dry_run: bool, as_json: bool) -> (usize, usize) {
    use bn_core::render::Envelope;
    let fleet = bn_core::agent::supervise(
        cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    );
    let mut answered = 0usize;
    let mut escalated = 0usize;
    let mut rows: Vec<serde_json::Value> = Vec::new();

    for agent in &fleet.agents {
        let target = agent.target.clone();
        let Some(prompt) = crate::cmd::agent::read_prompt(
            cfg,
            &target,
            bn_core::agent::PROMPT_SCAN_LINES,
        ) else {
            continue;
        };
        // Outside a wave bn has no trust contract to appeal to, so `trusted` is false and the
        // built-in default never fires here — only an explicit, enabled policy can approve.
        let ctx = PolicyCtx { lane: LaneKind::Dispatched, trusted: false };
        let policy = ApprovalPolicy::load();
        let decision = decide_with_builtin_default(&prompt, &ctx, &policy);
        record(&target, &prompt, &decision);
        match &decision {
            ApprovalDecision::Auto { option, reason } => {
                answered += 1;
                if !dry_run {
                    crate::cmd::agent::answer_option(cfg, &target, *option);
                }
                rows.push(serde_json::json!({
                    "target": target, "action": "approved", "option": option,
                    "reason": reason, "question": prompt.question, "dry_run": dry_run,
                }));
            }
            ApprovalDecision::Escalate { reason } => {
                escalated += 1;
                rows.push(serde_json::json!({
                    "target": target, "action": "escalated",
                    "reason": reason, "question": prompt.question,
                }));
            }
        }
    }

    if as_json {
        let data = serde_json::json!({
            "answered": answered, "escalated": escalated,
            "dry_run": dry_run, "decisions": rows,
        });
        println!("{}", Envelope::success("agent_autoapprove", data).to_json());
    } else if rows.is_empty() {
        println!("No agent is waiting on a dialog.");
    } else {
        for r in &rows {
            let mark = if r["action"] == "approved" { "\u{2713}" } else { "\u{2191}" };
            let dry = if dry_run && r["action"] == "approved" { " (dry run)" } else { "" };
            println!(
                "  {mark} {}{dry} — {} :: {}",
                r["target"].as_str().unwrap_or("?"),
                r["question"].as_str().unwrap_or("?"),
                r["reason"].as_str().unwrap_or("?")
            );
        }
        println!("\n{answered} answered, {escalated} left for you.");
    }
    (answered, escalated)
}

/// `bn agent approvals` — show the effective policy and the recent decisions it produced.
pub(crate) fn emit_approvals(_cfg: &Config, init: bool, limit: usize, as_json: bool) {
    use bn_core::render::Envelope;
    let path = bn_core::resolver::approvals_file_path();

    if init {
        if path.exists() {
            eprintln!("bn agent approvals: {} already exists", path.display());
            std::process::exit(1);
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(&path, ApprovalPolicy::template()) {
            Ok(()) => println!("\u{2713} wrote {}", path.display()),
            Err(e) => {
                eprintln!("bn agent approvals: could not write {} ({e})", path.display());
                std::process::exit(1);
            }
        }
        return;
    }

    let policy = ApprovalPolicy::load();
    let recent: Vec<serde_json::Value> = std::fs::read_to_string(audit_path())
        .unwrap_or_default()
        .lines()
        .rev()
        .take(limit)
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    if as_json {
        let data = serde_json::json!({
            "policy_file": path.display().to_string(),
            "exists": path.exists(),
            "enabled": policy.approve.enabled,
            "scope": policy.approve.scope,
            "allow_rules": policy.approve.rules.len(),
            "deny_rules": policy.deny.rules.len(),
            "recent": recent,
        });
        println!("{}", Envelope::success("agent_approvals", data).to_json());
        return;
    }

    println!("Policy: {}", path.display());
    if !path.exists() {
        println!("  (no file — bn's built-in default applies: an orchestrated --yolo ship lane's");
        println!("   command approvals are cleared with the session-scoped allow, nothing else)");
        println!("  write one with `bn agent approvals --init`");
    } else {
        println!(
            "  enabled: {}  scope: {:?}  allow rules: {}  deny rules: {}",
            policy.approve.enabled,
            policy.approve.scope,
            policy.approve.rules.len(),
            policy.deny.rules.len()
        );
        if !policy.approve.enabled {
            println!("  (not enabled — only the built-in default applies)");
        }
    }
    if recent.is_empty() {
        println!("\nNo decisions recorded yet.");
        return;
    }
    println!("\nRecent decisions (newest first):");
    for e in &recent {
        let mark = if e["approved"].as_bool().unwrap_or(false) {
            "\u{2713}"
        } else {
            "\u{2191}"
        };
        println!(
            "  {mark} {} — {} :: {}",
            e["target"].as_str().unwrap_or("?"),
            e["question"].as_str().unwrap_or("?"),
            e["reason"].as_str().unwrap_or("?")
        );
    }
}
