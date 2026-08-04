//! Pure primitives for the investigate-first pipeline stage (PIPE-B, EPIC #746).
//!
//! The CLI owns agent launch, note writes, attention, and stage transitions. This module keeps the
//! stage contract testable without any store: fresh-context seed builders, the decision between
//! planning / focused fan-out / human escalation, and deterministic findings merging.

use std::collections::HashSet;

/// One focused sub-investigator's answer to a remaining unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubInvestigation {
    pub question: String,
    pub findings: Vec<String>,
}

/// What the pipeline should do after an investigator reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvestigationNext {
    /// The problem is scoped: persist the findings and hand them to planning.
    Plan { findings: Vec<String> },
    /// Concrete unknowns remain: launch one focused fresh-context investigator per unknown.
    FanOut {
        findings: Vec<String>,
        unknowns: Vec<String>,
    },
    /// The task cannot be scoped without a person: pause and raise planning attention.
    Escalate { reason: String },
}

/// Decide whether investigation is ready for planning, needs focused fan-out, or needs a human.
///
/// A non-empty `blocked_reason` always wins: fan-out must not guess around a missing human
/// decision. Blank and duplicate findings/unknowns are removed while preserving first-seen order.
pub fn next_after_investigation(
    findings: &[String],
    unknowns: &[String],
    blocked_reason: Option<&str>,
) -> InvestigationNext {
    if let Some(reason) = blocked_reason
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
    {
        return InvestigationNext::Escalate {
            reason: reason.to_string(),
        };
    }

    let findings = normalized(findings.iter().map(String::as_str));
    let unknowns = normalized(unknowns.iter().map(String::as_str));
    if unknowns.is_empty() {
        InvestigationNext::Plan { findings }
    } else {
        InvestigationNext::FanOut { findings, unknowns }
    }
}

/// Merge the primary investigation with focused answers in a stable, duplicate-free order.
///
/// Primary findings stay first. Sub-investigations are ordered by their trimmed question so the
/// same set of asynchronously completed agents produces the same note regardless of finish order.
pub fn merge_findings(primary: &[String], sub_investigations: &[SubInvestigation]) -> Vec<String> {
    let mut ordered = sub_investigations.to_vec();
    ordered.sort_by(|a, b| a.question.trim().cmp(b.question.trim()));

    normalized(
        primary.iter().map(String::as_str).chain(
            ordered
                .iter()
                .flat_map(|sub| sub.findings.iter().map(String::as_str)),
        ),
    )
}

/// Build the fresh-context seed for the primary investigator.
///
/// Model/effort selection belongs to PIPE-C; this builder only guarantees that the investigator
/// receives the task, source, and explicit read-only/reporting protocol.
pub fn investigate_seed(goal: &str, source_url: Option<&str>) -> String {
    let mut seed = String::from(
        "You are the FRESH-CONTEXT INVESTIGATE stage of an autonomous task pipeline. Research the \
         problem and the relevant codebase before anyone plans or edits code. Work read-only: do \
         not implement a fix, modify source, or commit. Follow evidence past the first plausible \
         answer and cite concrete paths, symbols, commands, and behavior.\n\n",
    );
    seed.push_str("## Task\n");
    seed.push_str(goal.trim());
    seed.push('\n');
    if let Some(url) = source_url.map(str::trim).filter(|url| !url.is_empty()) {
        seed.push_str(&format!("\nSource: {url}\n"));
    }
    seed.push_str(
        "\n## Required outcome\n\
         - Record each concrete finding with `bn add research \"<finding with evidence>\"`.\n\
         - If focused unknowns remain, report each one separately with \
         `bn pipeline investigate finish --unknown \"<question>\"`; the driver will fan them out \
         to fresh sub-investigators.\n\
         - If the task genuinely cannot be scoped without a person, run \
         `bn pipeline investigate finish --blocked \"<specific input needed>\"` instead of \
         guessing.\n\
         - Otherwise run `bn pipeline investigate finish` after recording the findings so the \
         pipeline can hand them to planning.\n",
    );
    seed
}

/// Build a fresh-context seed for one focused sub-investigator.
pub fn sub_investigator_seed(goal: &str, question: &str) -> String {
    format!(
        "You are a FRESH-CONTEXT SUB-INVESTIGATOR. Research exactly one unresolved question for \
         the task below. Work read-only; do not plan the full solution or edit code. Return only \
         evidence-backed findings that answer this question, including concrete paths, symbols, \
         commands, and behavior. If the evidence is inconclusive, say what remains unknown rather \
         than guessing.\n\n## Task\n{}\n\n## Focused question\n{}\n",
        goal.trim(),
        question.trim()
    )
}

/// Build the fresh-context plan-stage seed from the task and persisted investigation findings.
pub fn plan_seed(goal: &str, findings: &[String]) -> String {
    let findings = normalized(findings.iter().map(String::as_str));
    let mut seed = String::from(
        "You are the FRESH-CONTEXT PLAN stage of an autonomous task pipeline. The investigate \
         stage has already researched this task. Consume its evidence below, inspect only what is \
         needed to validate the plan, and write no product code. Turn the findings into observable \
         done-when criteria and ordered implementation todos in the branch note.\n\n## Task\n",
    );
    seed.push_str(goal.trim());
    seed.push_str("\n\n## Investigation findings\n");
    if findings.is_empty() {
        seed.push_str(
            "(none recorded — do not invent facts; record any missing evidence as research)\n",
        );
    } else {
        for (index, finding) in findings.iter().enumerate() {
            seed.push_str(&format!("{}. {finding}\n", index + 1));
        }
    }
    seed.push_str(
        "\nStart by running `bn --cat`, then follow the plan workflow. Preserve the evidence in the \
         note so the executor and critic can trace every todo and done-when criterion back to the \
         investigation.\n",
    );
    seed
}

fn normalized<'a>(items: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        let item = item.trim();
        if item.is_empty() || !seen.insert(item.to_string()) {
            continue;
        }
        out.push(item.to_string());
    }
    out
}


