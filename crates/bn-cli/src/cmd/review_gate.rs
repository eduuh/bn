//! `bn review-gate` — the adversarial diff-only review gate CLI (#607): the reviewer's `record`
//! write path, the author/human `resolve`/`waive` of findings, the `status`/`list` reads, and
//! `prompt` (emit the canonical diff-only reviewer seed). The gate policy + store live in
//! [`bn_core::review_gate`]; this is the thin CLI shell over it.
use crate::*;
use bn_core::review_gate::{self, FindingStatus, GateStatus, NewFinding, Severity, Verdict};

/// Parse a `--finding` token: `"<severity>: <text>"` (severity = `blocking`/`advisory`, plus the
/// aliases [`Severity::parse`] accepts), or bare `"<text>"` defaulting to *blocking* — a reviewer
/// filing a defect without a label means it. An empty text is rejected.
fn parse_finding(raw: &str) -> NewFinding {
    let (sev, text) = match raw.split_once(':') {
        Some((head, rest)) if Severity::parse(head).is_some() => {
            (Severity::parse(head).unwrap(), rest.trim())
        }
        _ => (Severity::Blocking, raw.trim()),
    };
    if text.is_empty() {
        fail("bn review-gate record: a --finding must have text (e.g. 'blocking: eager unwrap panics')");
    }
    NewFinding {
        severity: sev,
        text: text.to_string(),
    }
}

fn print_status(gs: &GateStatus, key: &str, as_json: bool) {
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "review-gate",
                serde_json::json!({
                    "pr": key,
                    "open": gs.open,
                    "approvals": gs.approvals,
                    "required": gs.required,
                    "open_blocking": gs.open_blocking,
                }),
            )
            .to_json()
        );
    } else {
        println!("{key}: {}", gs.summary());
    }
}

/// `bn review-gate record <repo> <number> --reviewer R --verdict approve|block --sha SHA
/// [--summary ..] [--finding 'blocking: ..']…` — one reviewer's diff-only verdict + findings.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_review_gate_record(
    repo: &str,
    number: i64,
    reviewer: &str,
    verdict: &str,
    sha: &str,
    url: Option<&str>,
    branch: Option<&str>,
    summary: Option<&str>,
    findings: &[String],
    required: usize,
    as_json: bool,
) {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    let Some(v) = Verdict::parse(verdict) else {
        fail(&format!(
            "bn review-gate record: --verdict must be approve|block (got '{verdict}')"
        ));
    };
    if sha.trim().is_empty() {
        fail(
            "bn review-gate record: --sha is required (the head you reviewed: git rev-parse HEAD)",
        );
    }
    let new_findings: Vec<NewFinding> = findings.iter().map(|f| parse_finding(f)).collect();
    let now = bn_core::util::now_epoch_secs();
    match review_gate::record_review(
        repo,
        number,
        url.unwrap_or(""),
        branch.unwrap_or(""),
        reviewer,
        v,
        sha,
        summary.unwrap_or(""),
        new_findings,
        now,
    ) {
        Ok(entry) => {
            bn_core::log::record(
                bn_core::log::LogLevel::Info,
                "review-gate",
                "record",
                serde_json::json!({
                    "repo": repo,
                    "number": number,
                    "reviewer": reviewer,
                    "verdict": verdict,
                    "required": required,
                    "json": as_json,
                }),
                serde_json::json!({
                    "head": sha,
                    "findings": findings.len(),
                }),
                timer.as_ref().map(|t| t.elapsed_ms()),
            );
            let gs = entry.status(sha, required);
            if !as_json {
                println!(
                    "review-gate: recorded {} by {reviewer} on {}#{number} @ {}",
                    v.label(),
                    repo,
                    &sha[..sha.len().min(8)]
                );
            }
            print_status(&gs, &format!("{repo}#{number}"), as_json);
        }
        Err(e) => fail(&format!("bn review-gate record: {e}")),
    }
}

/// Shared body for `resolve`/`waive`: move a finding out of `open`.
fn set_finding(
    repo: &str,
    number: i64,
    finding_id: &str,
    status: FindingStatus,
    note: Option<&str>,
    as_json: bool,
) {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    let now = bn_core::util::now_epoch_secs();
    match review_gate::set_finding_status(repo, number, finding_id, status, note, now) {
        Ok(true) => {
            bn_core::log::record(
                bn_core::log::LogLevel::Info,
                "review-gate",
                "set_finding",
                serde_json::json!({
                    "repo": repo,
                    "number": number,
                    "finding_id": finding_id,
                    "status": status.label(),
                    "json": as_json,
                }),
                serde_json::json!({ "ok": true }),
                timer.as_ref().map(|t| t.elapsed_ms()),
            );
            if as_json {
                println!(
                    "{}",
                    Envelope::success(
                        "review-gate",
                        serde_json::json!({
                            "pr": format!("{repo}#{number}"),
                            "finding": finding_id,
                            "status": status.label(),
                        }),
                    )
                    .to_json()
                );
            } else {
                println!(
                    "review-gate: {}#{number} finding {finding_id} → {}",
                    repo,
                    status.label()
                );
            }
        }
        Ok(false) => fail(&format!(
            "bn review-gate: no finding '{finding_id}' on {repo}#{number}"
        )),
        Err(e) => fail(&format!("bn review-gate: {e}")),
    }
}

pub(crate) fn emit_review_gate_resolve(
    repo: &str,
    number: i64,
    finding_id: &str,
    note: Option<&str>,
    as_json: bool,
) {
    set_finding(
        repo,
        number,
        finding_id,
        FindingStatus::Resolved,
        note,
        as_json,
    );
}

pub(crate) fn emit_review_gate_waive(
    repo: &str,
    number: i64,
    finding_id: &str,
    reason: &str,
    as_json: bool,
) {
    set_finding(
        repo,
        number,
        finding_id,
        FindingStatus::Waived,
        Some(reason),
        as_json,
    );
}

/// `bn review-gate status <repo> <number> [--sha SHA]` — the gate verdict. Without `--sha` it
/// evaluates against the most recently reviewed head (the "as reviewed" view); pass `--sha` (the
/// PR's current head) for the true land-eligibility answer.
pub(crate) fn emit_review_gate_status(
    repo: &str,
    number: i64,
    sha: Option<&str>,
    required: usize,
    as_json: bool,
) {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    let led = review_gate::Ledger::load(&review_gate::store_path());
    let Some(entry) = led.find(repo, number) else {
        let gs = review_gate::gate_status(repo, number, sha.unwrap_or(""), required);
        print_status(&gs, &format!("{repo}#{number}"), as_json);
        if !as_json {
            println!("  (no diff-only reviews recorded yet — run `bn review-gate prompt {repo} {number}`)");
        }
        return;
    };
    let head = sha
        .map(str::to_string)
        .or_else(|| entry.reviews.last().map(|r| r.sha.clone()))
        .unwrap_or_default();
    let gs = entry.status(&head, required);
    print_status(&gs, &format!("{repo}#{number}"), as_json);
    if !as_json {
        for f in &entry.findings {
            println!(
                "  [{}] {} ({}) — {}",
                f.id,
                f.severity.label(),
                f.status.label(),
                f.text
            );
        }
        bn_core::log::record(
            bn_core::log::LogLevel::Debug,
            "review-gate",
            "status",
            serde_json::json!({
                "repo": repo,
                "number": number,
                "sha": sha,
                "required": required,
                "json": as_json,
            }),
            serde_json::json!({ "open": gs.open, "approvals": gs.approvals }),
            timer.as_ref().map(|t| t.elapsed_ms()),
        );
    }
}

/// `bn review-gate list` — every tracked PR with its gate verdict (evaluated at each PR's most
/// recently reviewed head).
pub(crate) fn emit_review_gate_list(required: usize, as_json: bool) {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    let led = review_gate::Ledger::load(&review_gate::store_path());
    if as_json {
        println!(
            "{}",
            Envelope::success("review-gate", serde_json::json!({ "entries": led.entries }),)
                .to_json()
        );
        return;
    }
    if led.entries.is_empty() {
        println!("No PRs tracked by the diff-only review gate yet.");
        return;
    }
    for e in &led.entries {
        let head = e.reviews.last().map(|r| r.sha.as_str()).unwrap_or("");
        println!("{}  {}", e.key(), e.status(head, required).summary());
    }
    bn_core::log::record(
        bn_core::log::LogLevel::Debug,
        "review-gate",
        "list",
        serde_json::json!({ "required": required, "json": as_json }),
        serde_json::json!({ "count": led.entries.len() }),
        timer.as_ref().map(|t| t.elapsed_ms()),
    );
}

/// `bn review-gate prompt <repo> <number> [--reviewer R]` — emit the canonical diff-only reviewer
/// seed. An orchestrator (or human) dispatches an independent reviewer agent with exactly this
/// prompt, whose context provably excludes the implementer's reasoning.
pub(crate) fn emit_review_gate_prompt(
    cfg: &Config,
    repo: &str,
    number: i64,
    reviewer: Option<&str>,
    required: usize,
    as_json: bool,
) {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    let label = reviewer.unwrap_or("rev1");
    let base = std::env::current_dir().unwrap_or_default();
    let mut args = serde_json::Map::new();
    for (name, value) in [
        ("reviewer", label.to_string()),
        ("required", required.to_string()),
        ("repo", repo.to_string()),
        ("number", number.to_string()),
    ] {
        args.insert(name.into(), serde_json::Value::String(value));
    }
    let prompt =
        bn_core::skills::render_required(cfg, &base, "review-gate-critic", Some(&args))
            .unwrap_or_else(|error| fail(&format!("bn review-gate prompt: {error}")));
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "review-gate",
                serde_json::json!({ "reviewer": label, "prompt": prompt }),
            )
            .to_json()
        );
    } else {
        println!("{prompt}");
    }
    bn_core::log::record(
        bn_core::log::LogLevel::Debug,
        "review-gate",
        "prompt",
        serde_json::json!({
            "repo": repo,
            "number": number,
            "reviewer": label,
            "required": required,
            "json": as_json,
        }),
        serde_json::json!({ "prompt_len": prompt.len() }),
        timer.as_ref().map(|t| t.elapsed_ms()),
    );
}
