//! Autonomous next-wave **selection** (#459) — the pure policy that turns a pool of candidate work
//! into the concrete wave the orchestrator starts this cycle, with **no human picking issues**.
//!
//! `bn orchestrate` today refills its working set FIFO/priority-first from the durable backlog; the
//! human still shapes *what* is in that backlog and, implicitly, which waves run. #348 wants the
//! daemon to decide autonomously. This module is that decision, composing the three shipped layers
//! into one pure function so the choice is deterministic and unit-tested:
//!   - **#458 `advise`** ranks the candidates by which quarter goal they advance (slipping goals
//!     first), lifted by open attention on their repo — the *strategic order* to consider them in;
//!   - **#376 conflict-awareness** ([`crate::orchestrate::conflicts`]) holds a candidate whose
//!     predicted touch-set would overlap an in-flight (or already-picked-this-wave) one, so parallel
//!     lanes don't collide and pile up rebases at land time;
//!   - **#377 capacity** caps the wave to the free local slots the caller has this cycle;
//!   - a **don't-start-what-can't-merge** filter drops a candidate with no path to a clean merge.
//!
//! It is deliberately **pure** — no I/O, clock, or globals — so the whole select→skip decision is
//! unit-tested directly here (the acceptance's "pure + unit-tested" requirement). The CLI gathers
//! the live candidates (`bn next` + `bn attn` + the open backlog/issues), computes the free
//! capacity + the in-flight conflict map, calls [`select_wave`], leases the chosen ids, and lets the
//! existing dispatch loop (the #377 scheduler) realize them.

use crate::advise::{self, AdviseInputs, AttnSignal, Candidate, GoalContext};
use crate::backlog::BacklogKind;
use crate::orchestrate::conflicts;
use serde::Serialize;

/// One candidate unit of work to weigh for the next wave — the advise ranking fields plus the two
/// guardrail signals selection needs on top of them: the #376 predicted touch-set and whether the
/// work can currently reach a clean merge. The caller flattens its source (a pending backlog entry,
/// an open issue) into this shape; the pure policy never reads a store.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectCandidate {
    /// Stable id within `repo` (the backlog `(repo, id)` key half).
    pub id: String,
    pub repo: String,
    /// The human subject, used for display and advise keyword alignment.
    pub title: String,
    pub kind: BacklogKind,
    /// The source's own priority — read as a band plus a within-band nudge.
    pub priority: i64,
    /// An explicit band, when the source states one; `None` derives it from `priority`.
    pub band: Option<crate::rank::Band>,
    /// When the work arrived — the age term and the FIFO tiebreak.
    pub added_at: i64,
    /// Its deadline, if it has one — what the urgency term measures slack against.
    pub not_after: Option<i64>,
    /// Its expected duration, so urgency asks whether the work can still finish in time.
    pub est_secs: i64,
    /// An explicit quarter-goal key this work is tagged to, when the source knows one (advise wins
    /// over keyword matching on it); `None` falls back to title/keyword overlap.
    pub goal: Option<String>,
    /// Declared touch globs (#376) for conflict prediction. Empty = no hint, which conflicts with
    /// nothing (the item stays schedulable; land-time serialization remains the safety net).
    pub touches: Vec<String>,
    /// Whether this item has a path to a clean merge right now. `false` drops it from the wave
    /// (don't-start-what-can't-merge) — e.g. a blocked issue, or one already owning an in-flight
    /// lane it would only duplicate. The caller computes it; the policy just honors it.
    pub mergeable: bool,
}

/// The full input for one selection pass.
pub struct SelectionInputs<'a> {
    /// The open quarter goals with their current status — the strategic weights advise ranks by.
    pub goals: &'a [GoalContext],
    /// The candidate work to rank + gate.
    pub candidates: &'a [SelectCandidate],
    /// Open attention items, as repo pressure signals (advise lifts a candidate whose repo presses).
    pub attention: &'a [AttnSignal],
    /// Free local slots to fill this cycle (#377) — typically the total local cap minus the working
    /// set. The wave never exceeds this; `0` selects nothing (everything otherwise-pickable is
    /// skipped [`SkipReason::Capacity`]).
    pub capacity: usize,
    /// Wall clock for this pass, epoch secs — handed to the ranker's time terms.
    pub now: i64,
    /// Touch-sets of the items already committed to the run (in-flight + queued) — the #376 conflict
    /// map a new pick must not overlap. Each pick then joins it, so two picked candidates never
    /// collide with each other either.
    pub inflight_touches: &'a [Vec<String>],
}

/// Why a ranked candidate did not make the wave — surfaced so `bn orchestrate` can explain the
/// autonomous pick (the "held: …" / "skipped: …" status line) without re-deriving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkipReason {
    /// No path to a clean merge (blocked / would only duplicate an in-flight lane) —
    /// don't-start-what-can't-merge. An *intrinsic* skip: the item is unpickable regardless of room.
    CantMerge,
    /// Predicted file overlap (#376) with an in-flight, queued, or already-picked-this-wave
    /// touch-set. Intrinsic: it would serialize behind its holder anyway, so it isn't started now.
    Conflict,
    /// Otherwise pickable, but the wave already filled the free [`SelectionInputs::capacity`]
    /// (#377). *Extrinsic*: it's next in line and starts as soon as a slot frees.
    Capacity,
}

impl SkipReason {
    /// The kebab-case label for the status surface / events (`cant-merge` / `conflict` / `capacity`).
    pub fn label(self) -> &'static str {
        match self {
            SkipReason::CantMerge => "cant-merge",
            SkipReason::Conflict => "conflict",
            SkipReason::Capacity => "capacity",
        }
    }
}

/// One chosen candidate, carrying the advise rationale that justified starting it now. `Serialize`
/// so the CLI/web emit the wave straight into a `--json` envelope.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Chosen {
    pub id: String,
    pub repo: String,
    pub title: String,
    /// The candidate kind label (`ship` / `scout`).
    pub kind: String,
    /// The quarter-goal key this candidate was aligned to, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// The advise composite score (higher = more strategic pull).
    pub score: i64,
    /// The advise rank this candidate held (1-based over the full ranked pool, before gating).
    pub rank: usize,
    /// The one-line advise rationale, tied to the matched goal / attention / priority.
    pub rationale: String,
}

/// One skipped candidate plus why it didn't start this cycle.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SkippedItem {
    pub id: String,
    pub repo: String,
    pub title: String,
    pub reason: SkipReason,
}

/// The outcome of one selection pass: the wave to dispatch (in advise-rank order) and every skipped
/// candidate with its reason. Pure over the [`SelectionInputs`] — the same inputs always yield the
/// same wave, order, and skip reasons.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Selection {
    /// The chosen wave, in the order to dispatch (advise rank).
    pub wave: Vec<Chosen>,
    /// The candidates not started this cycle, each tagged with why (in advise-rank order).
    pub skipped: Vec<SkippedItem>,
}

impl Selection {
    /// The chosen `(repo, id)` keys in wave order — what the CLI leases + dispatches.
    pub fn ids(&self) -> Vec<(String, String)> {
        self.wave
            .iter()
            .map(|c| (c.repo.clone(), c.id.clone()))
            .collect()
    }

    /// How many candidates were skipped for `reason` — the per-reason tally for the status line.
    pub fn skipped_for(&self, reason: SkipReason) -> usize {
        self.skipped.iter().filter(|s| s.reason == reason).count()
    }
}

/// Select the next wave from the candidate pool (#459) — the pure autonomous-selection policy.
///
/// Ranks the candidates strategically via [`advise::advise`] (#458), then walks them in that rank
/// order and applies the guardrails, taking each candidate that clears all three into the wave until
/// the free capacity is full:
///   1. **can't-merge** — a candidate with no path to a clean merge is dropped
///      ([`SkipReason::CantMerge`]);
///   2. **conflict** (#376) — a candidate whose declared touches overlap an already-committed or
///      already-picked-this-wave touch-set is held ([`SkipReason::Conflict`]) and does *not* occupy
///      a touch area (it isn't started), so a later disjoint candidate still fills the slot;
///   3. **capacity** (#377) — an otherwise-pickable candidate that arrives after the wave is full is
///      deferred ([`SkipReason::Capacity`]).
///
/// The intrinsic reasons (1, 2) are checked before capacity so a blocked/conflicting item is always
/// labeled by *why it can't run* rather than merely *no room* — more useful for the status surface,
/// and correct even when `capacity == 0`. Pure: no IO, clock, or globals.
pub fn select_wave(inputs: &SelectionInputs) -> Selection {
    // 1. Strategic ranking (#458). advise is pure + total and preserves each candidate's (repo, id),
    //    so we rank on the advise view and rejoin the guardrail signals by that key below.
    let advise_cands: Vec<Candidate> = inputs
        .candidates
        .iter()
        .map(|c| Candidate {
            id: c.id.clone(),
            repo: c.repo.clone(),
            title: c.title.clone(),
            kind: c.kind,
            priority: c.priority,
            goal: c.goal.clone(),
            band: c.band,
            added_at: c.added_at,
            not_after: c.not_after,
            est_secs: c.est_secs,
        })
        .collect();
    let ranked = advise::advise(&AdviseInputs {
        goals: inputs.goals,
        candidates: &advise_cands,
        attention: inputs.attention,
        capacity: inputs.capacity,
        now: inputs.now,
    });

    // 2. Walk in rank order applying the guardrails, growing the conflict map as we pick.
    let mut inflight = inputs.inflight_touches.to_vec();
    let mut sel = Selection::default();
    for adv in &ranked {
        // Rejoin the candidate's guardrail signals (touches / mergeable) by its (repo, id) key.
        let Some(cand) = inputs
            .candidates
            .iter()
            .find(|c| c.repo == adv.repo && c.id == adv.id)
        else {
            continue; // unreachable: advise only emits candidates it was given.
        };
        let skip = |reason: SkipReason| SkippedItem {
            id: adv.id.clone(),
            repo: adv.repo.clone(),
            title: adv.title.clone(),
            reason,
        };
        if !cand.mergeable {
            sel.skipped.push(skip(SkipReason::CantMerge));
            continue;
        }
        if !cand.touches.is_empty() && conflicts(&cand.touches, &inflight) {
            sel.skipped.push(skip(SkipReason::Conflict));
            continue;
        }
        if sel.wave.len() >= inputs.capacity {
            sel.skipped.push(skip(SkipReason::Capacity));
            continue;
        }
        // Cleared every guard: start it, and add its touch area to the conflict map so a later
        // overlapping candidate this same wave is held rather than co-scheduled.
        inflight.push(cand.touches.clone());
        sel.wave.push(Chosen {
            id: adv.id.clone(),
            repo: adv.repo.clone(),
            title: adv.title.clone(),
            kind: adv.kind.clone(),
            goal: adv.goal.clone(),
            score: adv.score,
            rank: adv.rank,
            rationale: adv.rationale.clone(),
        });
    }
    sel
}
