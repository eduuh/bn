//! Strategic **"advise next"** ranking (#458) — the goals-agent's core.
//!
//! `bn next` ranks the *tactical* signal and `bn attn` lists raw attention, but nothing returns a
//! ranked shortlist **aligned to quarter goals**. This module is that strategic layer: given
//! candidate work, current attention pressure, the available capacity, and the quarter goals (with
//! their latest status), it returns a ranked, rationale'd shortlist tied to those goals.
//!
//! It is deliberately **pure** — no I/O, no clock, no globals — so it is unit-tested directly and
//! the CLI (`bn quarter advise`) / the orchestrator (#348) just feed it real inputs and render the
//! `--json`. Alignment is explicit-tag-first (`Candidate::goal`) with a keyword-overlap fallback
//! against each goal's text, so untagged backlog still finds the goal it advances.

use crate::backlog::BacklogKind;
use serde::Serialize;

/// One open quarter goal plus the status the latest impact review gave it — the strategic weight
/// of advancing it. `status` is one of [`crate::quarter::STATUSES`] or `unscored` (no review yet).
#[derive(Debug, Clone, PartialEq)]
pub struct GoalContext {
    pub key: String,
    pub text: String,
    pub status: String,
}

/// One candidate unit of work to weigh — a pending backlog entry, a `--q`-tagged todo, etc. The
/// pure ranker never reads a store, so the caller flattens its source into this shape.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Stable id within `repo` (the backlog `(repo, id)` key half).
    pub id: String,
    pub repo: String,
    /// The human subject used both for display and keyword alignment.
    pub title: String,
    pub kind: BacklogKind,
    /// The source's own priority — the base weight and the final tiebreak.
    pub priority: i64,
    /// An explicit quarter-goal key this work is tagged to, when the source knows one. Wins over
    /// keyword matching; `None` falls back to text overlap.
    pub goal: Option<String>,
}

/// An open attention item reduced to the pressure it puts on a repo — a candidate touching a repo
/// with an open urgent/warn item is more worth doing now.
#[derive(Debug, Clone, PartialEq)]
pub struct AttnSignal {
    pub repo: Option<String>,
    /// `info` | `warn` | `urgent` (anything else ranks as `info`).
    pub severity: String,
}

/// The full input set for one advise pass.
#[derive(Debug, Clone)]
pub struct AdviseInputs<'a> {
    /// The open quarter goals with their current status.
    pub goals: &'a [GoalContext],
    /// The candidate work to rank.
    pub candidates: &'a [Candidate],
    /// Open attention items, as repo pressure signals.
    pub attention: &'a [AttnSignal],
    /// How many slots are free — the shortlist size. The top `capacity` results are flagged
    /// [`Advice::within_capacity`]; everything ranked below still returns (the caller decides how
    /// much to show), so `capacity` shapes the shortlist without truncating the ranking.
    pub capacity: usize,
}

/// One ranked candidate with its rationale tied to the quarter goals. `Serialize` so the CLI/
/// orchestrator emit it straight into the `--json` envelope.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Advice {
    pub id: String,
    pub repo: String,
    pub title: String,
    /// The candidate kind label (`ship` / `scout`).
    pub kind: String,
    pub priority: i64,
    /// The quarter-goal key this candidate was aligned to, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// The composite rank score (higher first) — surfaced so the orchestrator can threshold.
    pub score: i64,
    /// 1-based rank in the returned order.
    pub rank: usize,
    /// Whether this candidate falls within the `capacity` shortlist (the first `capacity` ranks).
    pub within_capacity: bool,
    /// A one-line human rationale, tied to the matched goal (and attention/priority).
    pub rationale: String,
}

/// How a candidate matched a goal — an explicit `--q` tag (high confidence) or a keyword overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strength {
    Tagged,
    Keyword,
}

// --- Scoring weights ------------------------------------------------------------------------
// Alignment dominates (that is the strategic point); attention breaks near-ties within a status
// band; base priority is the final nudge. A candidate advancing an *at-risk* goal always outranks
// one advancing a healthy goal, which always outranks an unaligned candidate.

/// Weight of advancing a goal, by its latest-review status. A slipping goal is worth the most; a
/// `done` goal needs no more work, so it scores as unaligned.
fn status_weight(status: &str) -> i64 {
    match status {
        "off-track" => 100,
        "at-risk" => 80,
        "unscored" => 55,
        "on-track" => 40,
        _ => 0, // "done" or anything unrecognized: no strategic pull.
    }
}

/// Extra confidence for an explicitly `--q`-tagged match over an inferred keyword one.
const TAGGED_BONUS: i64 = 15;

/// Attention pressure a repo's open items add, by top severity.
fn attn_weight(severity: &str) -> i64 {
    match severity {
        "urgent" => 30,
        "warn" => 15,
        _ => 5,
    }
}

/// English stopwords dropped before keyword overlap so "the/and/with" don't fake an alignment.
const STOPWORDS: [&str; 16] = [
    "the", "and", "for", "with", "that", "this", "from", "into", "your", "have", "will", "when",
    "then", "than", "over", "user",
];

/// Tokenize into lowercased alphanumeric words of length ≥ 3 that aren't stopwords — the unit of
/// keyword overlap. Short words and stopwords are dropped so a match means a shared *topic*.
fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_ascii_lowercase())
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Pick the goal a candidate best aligns to: an explicit `--q` tag matching an open goal wins;
/// otherwise the goal sharing the most keyword tokens with the candidate title (≥ 1 shared token),
/// ties broken by goal order. `None` when nothing aligns.
fn align<'a>(cand: &Candidate, goals: &'a [GoalContext]) -> Option<(&'a GoalContext, Strength)> {
    if let Some(key) = cand.goal.as_deref().filter(|k| !k.is_empty()) {
        if let Some(g) = goals.iter().find(|g| g.key == key) {
            return Some((g, Strength::Tagged));
        }
    }
    let cand_tokens = tokens(&cand.title);
    if cand_tokens.is_empty() {
        return None;
    }
    let mut best: Option<(&GoalContext, usize)> = None;
    for g in goals {
        let goal_tokens = tokens(&format!("{} {}", g.text, g.key));
        let overlap = cand_tokens
            .iter()
            .filter(|t| goal_tokens.contains(t))
            .count();
        if overlap > 0 && best.is_none_or(|(_, b)| overlap > b) {
            best = Some((g, overlap));
        }
    }
    best.map(|(g, _)| (g, Strength::Keyword))
}

/// The best (highest-severity) attention weight for `repo`, or `0` when nothing presses on it.
fn attn_pressure(repo: &str, attention: &[AttnSignal]) -> i64 {
    attention
        .iter()
        .filter(|a| a.repo.as_deref() == Some(repo))
        .map(|a| attn_weight(&a.severity))
        .max()
        .unwrap_or(0)
}

/// The strategic verb for a rationale, by goal status: slipping goals are *protected*, healthy or
/// unjudged ones are *advanced*.
fn verb(status: &str) -> &'static str {
    match status {
        "off-track" | "at-risk" => "Protects",
        _ => "Advances",
    }
}

/// Rank the candidates into a goal-aligned shortlist. Pure over its inputs — the same
/// [`AdviseInputs`] always yields the same ordering, scores, and rationales.
pub fn advise(inputs: &AdviseInputs) -> Vec<Advice> {
    let mut scored: Vec<Advice> = inputs
        .candidates
        .iter()
        .map(|c| {
            let matched = align(c, inputs.goals);
            let pressure = attn_pressure(&c.repo, inputs.attention);
            let (goal_key, align_score, rationale) = match matched {
                Some((g, strength)) => {
                    let mut s = status_weight(&g.status);
                    if strength == Strength::Tagged {
                        s += TAGGED_BONUS;
                    }
                    let how = match strength {
                        Strength::Tagged => "tagged",
                        Strength::Keyword => "keyword match",
                    };
                    let mut r = format!(
                        "{} {} goal '{}' ({})",
                        verb(&g.status),
                        g.status,
                        g.key,
                        how
                    );
                    if pressure > 0 {
                        r.push_str(&attn_clause(&c.repo, inputs.attention));
                    }
                    (Some(g.key.clone()), s, r)
                }
                None => {
                    let mut r = format!(
                        "No quarter-goal alignment — ranked on base priority {}",
                        c.priority
                    );
                    if pressure > 0 {
                        r.push_str(&attn_clause(&c.repo, inputs.attention));
                    }
                    (None, 0, r)
                }
            };
            let score = align_score + pressure + c.priority;
            Advice {
                id: c.id.clone(),
                repo: c.repo.clone(),
                title: c.title.clone(),
                kind: c.kind.label().to_string(),
                priority: c.priority,
                goal: goal_key,
                score,
                rank: 0,
                within_capacity: false,
                rationale,
            }
        })
        .collect();

    // Higher score first, then higher priority, then repo/id for a total, deterministic order.
    scored.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.priority.cmp(&a.priority))
            .then(a.repo.cmp(&b.repo))
            .then(a.id.cmp(&b.id))
    });
    for (i, adv) in scored.iter_mut().enumerate() {
        adv.rank = i + 1;
        adv.within_capacity = i < inputs.capacity;
    }
    scored
}

/// The "; <severity> attention open in <repo>" tail appended to a rationale when the candidate's
/// repo has open attention.
fn attn_clause(repo: &str, attention: &[AttnSignal]) -> String {
    let sev = attention
        .iter()
        .filter(|a| a.repo.as_deref() == Some(repo))
        .max_by_key(|a| attn_weight(&a.severity))
        .map(|a| a.severity.as_str())
        .unwrap_or("info");
    format!("; {sev} attention open in {repo}")
}
