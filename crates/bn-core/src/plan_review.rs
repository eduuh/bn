//! The plan-review gate (#349): a dispatched agent's *plan* awaits human approval before it ships.
//!
//! The user's keystone workflow — "start some work → the agent runs in plan mode → I get a link
//! with the plan → I review/edit → I approve → the agent implements + opens the PR". Autonomy for
//! the mechanics, a human gate for the plan (and especially for `gh pr create`).
//!
//! A plan-review is a persisted record — `plan-reviews.json` in the state dir — carried through a
//! four-state lifecycle: `pending` (the agent submitted a plan and is paused) → `approved` (unblock:
//! implement + open the PR), `changes-requested` (the human's notes go back; the agent revises and
//! re-submits, cycling to `pending` again), or `rejected` (stop, ship nothing). The store is the
//! source of truth for the plan text, status, and reviewer notes; every mutation is a locked
//! read-modify-write (see [`crate::store::FileLock`]) over an atomically-replaced file, so the
//! agent's poll loop and a concurrent human/web approval never corrupt or lose one another (the
//! same discipline `orchestrate` uses for its per-run state).
//!
//! A plan-review is *also* an attention item (`mode = "review"`), mirrored on submit so a pending
//! plan surfaces in `bn attn` and the dashboard, and acked when the review leaves `pending`. The
//! record is the store; the attention item is a view onto it.

use crate::resolver::state_dir;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_REVIEWER_COUNT: usize = 3;

const HIGH_RISK_LABELS: &[&str] = &["auth", "tenant", "payments"];
const SENSITIVE_PATH_GLOBS: &[&str] = &[
    "**/auth/**",
    "**/tenant/**",
    "**/tenants/**",
    "**/payment/**",
    "**/payments/**",
    "**/billing/**",
    "**/secrets/**",
    "**/credentials/**",
    "**/migrations/**",
];

/// The lifecycle status of a plan-review as it flows submit → decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewStatus {
    /// The agent submitted a plan and is paused, waiting on the human. The one state that blocks
    /// the agent's poll loop; every other state releases it.
    Pending,
    /// Approved — the agent may proceed to implement and open the PR.
    Approved,
    /// The human returned notes; the agent revises the plan and re-submits (cycling to `Pending`).
    ChangesRequested,
    /// Rejected — the agent stops and ships nothing.
    Rejected,
}

impl ReviewStatus {
    pub fn label(self) -> &'static str {
        match self {
            ReviewStatus::Pending => "pending",
            ReviewStatus::Approved => "approved",
            ReviewStatus::ChangesRequested => "changes-requested",
            ReviewStatus::Rejected => "rejected",
        }
    }

    /// Whether the agent is still paused on this review — the poll loop keeps waiting only while
    /// `Pending`. Every other status is a decision the agent acts on.
    pub fn is_pending(self) -> bool {
        matches!(self, ReviewStatus::Pending)
    }

    /// Whether this status ends the review for good. `Approved`/`Rejected` are terminal; `Pending`
    /// (waiting) and `ChangesRequested` (the agent will revise + re-submit) are not.
    pub fn is_terminal(self) -> bool {
        matches!(self, ReviewStatus::Approved | ReviewStatus::Rejected)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConsensusStatus {
    HumanOnly,
    AwaitingVotes,
    Approved,
    HumanRequired,
    NoConsensus,
}

impl ConsensusStatus {
    pub fn label(self) -> &'static str {
        match self {
            ConsensusStatus::HumanOnly => "human-only",
            ConsensusStatus::AwaitingVotes => "awaiting-votes",
            ConsensusStatus::Approved => "approved",
            ConsensusStatus::HumanRequired => "human-required",
            ConsensusStatus::NoConsensus => "no-consensus",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VoteVerdict {
    Approve,
    Reject,
}

impl VoteVerdict {
    pub fn label(self) -> &'static str {
        match self {
            VoteVerdict::Approve => "approve",
            VoteVerdict::Reject => "reject",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "approve" | "approved" => Some(Self::Approve),
            "reject" | "rejected" => Some(Self::Reject),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerVote {
    pub reviewer: String,
    pub verdict: VoteVerdict,
    /// Reviewer-reported confidence as a percentage. Optional for votes written by older clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risks: Vec<String>,
    pub created: i64,
}

/// One persisted plan-review record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Review {
    /// Stable id — `r<N>`, one past the highest ever issued (ids are never reused, even after a
    /// review reaches a terminal state and stays as history).
    pub id: String,
    pub repo: String,
    /// The dispatched branch the plan is for (raw form, e.g. `dispatch/349`).
    pub branch: String,
    /// The agent/harness session that submitted the plan, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The plan itself as markdown — approach, files-to-touch, risks.
    pub plan: String,
    pub status: ReviewStatus,
    /// The reviewer's notes — set on `changes-requested` (what to change), optional otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// The mirrored attention item's id, so a decision can ack it off the attention list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attn_id: Option<String>,
    /// Number of fresh, independent reviewer identities expected. Zero preserves the original
    /// human-only plan gate for existing callers and persisted records.
    #[serde(default)]
    pub reviewer_count: usize,
    /// Approvals required to reach consensus. Clamped to 1..=reviewer_count on submission.
    #[serde(default)]
    pub quorum: usize,
    #[serde(default)]
    pub votes: Vec<ReviewerVote>,
    #[serde(default)]
    pub high_risk: bool,
    #[serde(default)]
    pub risk_reasons: Vec<String>,
    #[serde(default = "human_only_consensus")]
    pub consensus: ConsensusStatus,
    /// A distinct attention item raised when all reviewers vote without an approval quorum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consensus_attn_id: Option<String>,
    /// Epoch secs the review was first submitted.
    pub created: i64,
    /// Epoch secs of the last change (re-submit or decision).
    pub updated: i64,
}

fn human_only_consensus() -> ConsensusStatus {
    ConsensusStatus::HumanOnly
}

impl Review {
    /// Whether this review is still *open* — awaiting the human (`pending`) or awaiting the agent's
    /// revision (`changes-requested`). An open review is the one a re-submit reuses; terminal ones
    /// are history. The agent is considered *waiting-for-review* only while `pending`.
    pub fn is_open(&self) -> bool {
        matches!(
            self.status,
            ReviewStatus::Pending | ReviewStatus::ChangesRequested
        )
    }

    pub fn approvals(&self) -> usize {
        self.votes
            .iter()
            .filter(|vote| vote.verdict == VoteVerdict::Approve)
            .count()
    }

    pub fn reviewer_view(&self) -> Review {
        let mut review = self.clone();
        review.votes.clear();
        review
    }
}

/// The human's (or web's) decision on a pending plan-review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approve,
    RequestChanges,
    Reject,
}

impl Decision {
    /// Parse the CLI / web action verb. Accepts the hyphen and underscore spellings so
    /// `request-changes` (CLI) and `request_changes` (JSON body) both map through.
    pub fn parse(s: &str) -> Option<Decision> {
        match s.trim().to_lowercase().replace('_', "-").as_str() {
            "approve" | "approved" => Some(Decision::Approve),
            "request-changes" | "changes" | "changes-requested" => Some(Decision::RequestChanges),
            "reject" | "rejected" => Some(Decision::Reject),
            _ => None,
        }
    }

    /// The status a decision moves a pending review into.
    fn target_status(self) -> ReviewStatus {
        match self {
            Decision::Approve => ReviewStatus::Approved,
            Decision::RequestChanges => ReviewStatus::ChangesRequested,
            Decision::Reject => ReviewStatus::Rejected,
        }
    }
}

/// Why a [`decide`] failed.
#[derive(Debug)]
pub enum DecideError {
    /// No review carries that id.
    NotFound(String),
    /// The review isn't `pending`, so there's nothing to decide (already approved/rejected, or
    /// awaiting the agent's revision after a change request).
    NotPending {
        id: String,
        status: ReviewStatus,
    },
    /// `request-changes` needs notes telling the agent what to change.
    MissingNotes,
    /// An edit tried to save an empty plan — a review must always carry a plan.
    EmptyPlan,
    Io(std::io::Error),
}

#[derive(Debug)]
pub enum VoteError {
    NotFound(String),
    NotPending {
        id: String,
        status: ReviewStatus,
    },
    NotConsensusReview(String),
    DuplicateReviewer(String),
    VotingClosed(String),
    EmptyReviewer,
    InvalidConfidence(u8),
    /// `add_reviewers` was asked to grow the panel by zero.
    NothingToAdd(String),
    Io(std::io::Error),
}

impl std::fmt::Display for VoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VoteError::NotFound(id) => write!(f, "no plan-review '{id}'"),
            VoteError::NotPending { id, status } => {
                write!(f, "plan-review '{id}' is {} — not pending", status.label())
            }
            VoteError::NotConsensusReview(id) => {
                write!(
                    f,
                    "plan-review '{id}' is human-only and does not accept agent votes"
                )
            }
            VoteError::DuplicateReviewer(reviewer) => {
                write!(f, "reviewer '{reviewer}' already voted")
            }
            VoteError::VotingClosed(id) => {
                write!(f, "plan-review '{id}' already has all configured ballots")
            }
            VoteError::EmptyReviewer => write!(f, "reviewer identity cannot be empty"),
            VoteError::InvalidConfidence(confidence) => {
                write!(
                    f,
                    "reviewer confidence must be between 0 and 100, got {confidence}"
                )
            }
            VoteError::NothingToAdd(id) => {
                write!(
                    f,
                    "plan-review '{id}': nothing to add — request at least one reviewer"
                )
            }
            VoteError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for VoteError {}

impl std::fmt::Display for DecideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecideError::NotFound(id) => write!(f, "no plan-review '{id}'"),
            DecideError::NotPending { id, status } => {
                write!(f, "plan-review '{id}' is {} — not pending", status.label())
            }
            DecideError::MissingNotes => {
                write!(f, "request-changes needs --notes describing the change")
            }
            DecideError::EmptyPlan => write!(f, "the edited plan is empty — nothing to save"),
            DecideError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DecideError {}

/// The persisted set of plan-reviews (newest appended last).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ReviewStore {
    #[serde(default)]
    pub reviews: Vec<Review>,
}

impl ReviewStore {
    /// Load the store from `path`, or an empty store if absent/unparseable (nothing has been
    /// submitted yet), never an error.
    pub fn load(path: &Path) -> ReviewStore {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => ReviewStore::default(),
        }
    }

    /// Atomically persist to `path` (temp sibling + fsync + rename), creating the parent on demand,
    /// so a concurrent poll never reads a torn write.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        crate::store::save_json_atomic(path, self)
    }

    /// Next `r<N>` id: one past the highest `r<N>` ever issued, so terminal reviews left as history
    /// never have their id reused.
    pub fn next_id(&self) -> String {
        let max = self
            .reviews
            .iter()
            .filter_map(|r| r.id.strip_prefix('r').and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        format!("r{}", max + 1)
    }

    pub fn get(&self, id: &str) -> Option<&Review> {
        self.reviews.iter().find(|r| r.id == id)
    }

    /// The open (pending or changes-requested) review for a repo+branch, if any — the record a
    /// re-submit reuses. Matched case-insensitively on repo and on the sanitized branch (so the raw
    /// `dispatch/349` and window-form `dispatch-349` reconcile).
    fn open_for(&self, repo: &str, branch: &str) -> Option<usize> {
        let (r, b) = (repo.to_lowercase(), sanitize_branch(branch));
        self.reviews.iter().position(|rv| {
            rv.is_open() && rv.repo.to_lowercase() == r && sanitize_branch(&rv.branch) == b
        })
    }
}

/// Sanitize a branch to its window/key form (`/` → `-`) — matching how dispatch names an agent's
/// tmux window, so a review keyed on the raw branch reconciles with the orchestrator's item.
pub fn sanitize_branch(branch: &str) -> String {
    branch.replace('/', "-")
}

/// The plan-review store file, beside the orchestrate/attention state.
pub fn store_path() -> PathBuf {
    state_dir().join("plan-reviews.json")
}

/// Run a locked read-modify-write over the store: acquire the advisory [`FileLock`], load, apply
/// `f`, and (when `f` asks) save. Mirrors [`crate::orchestrate::mutate`] — the guard serializes the
/// agent's poll-driven re-submit against a concurrent human/web decision.
fn mutate<R>(f: impl FnOnce(&mut ReviewStore) -> (bool, R)) -> std::io::Result<R> {
    let path = store_path();
    let _lock = crate::store::FileLock::acquire(&path);
    let mut store = ReviewStore::load(&path);
    let (dirty, out) = f(&mut store);
    if dirty {
        store.save(&path)?;
    }
    Ok(out)
}

/// Submit (or re-submit) a plan for review. If an open review already exists for this repo+branch —
/// the change-request revise loop — its plan is replaced and it cycles back to `pending`; otherwise
/// a fresh `pending` review is created. Either way the mirrored attention item is (re-)raised so the
/// plan surfaces in `bn attn` and the dashboard. Returns the stored record.
pub fn submit(
    repo: &str,
    branch: &str,
    session: Option<&str>,
    plan: &str,
    now: i64,
) -> std::io::Result<Review> {
    submit_with_consensus(repo, branch, session, plan, 0, 0, &[], &[], now)
}

#[allow(clippy::too_many_arguments)]
pub fn submit_with_consensus(
    repo: &str,
    branch: &str,
    session: Option<&str>,
    plan: &str,
    reviewer_count: usize,
    quorum: usize,
    labels: &[String],
    paths: &[String],
    now: i64,
) -> std::io::Result<Review> {
    let today = crate::util::today();
    let quorum = if reviewer_count == 0 {
        0
    } else {
        quorum.clamp(1, reviewer_count)
    };
    let risk_reasons = if reviewer_count == 0 {
        Vec::new()
    } else {
        risk_reasons(labels, paths)
    };
    let high_risk = !risk_reasons.is_empty();
    let consensus = if reviewer_count == 0 {
        ConsensusStatus::HumanOnly
    } else {
        ConsensusStatus::AwaitingVotes
    };
    mutate(|store| {
        let review = match store.open_for(repo, branch) {
            Some(idx) => {
                // Revise-and-re-submit: reuse the record, back to pending with the new plan.
                let r = &mut store.reviews[idx];
                if let Some(aid) = r.consensus_attn_id.take() {
                    crate::util::best_effort(
                        crate::attention::ack(&aid),
                        "plan-review: ack stale consensus attn",
                    );
                }
                r.plan = plan.to_string();
                r.status = ReviewStatus::Pending;
                r.reviewer_count = reviewer_count;
                r.quorum = quorum;
                r.votes.clear();
                r.high_risk = high_risk;
                r.risk_reasons = risk_reasons.clone();
                r.consensus = consensus;
                r.consensus_attn_id = None;
                r.updated = now;
                if let Some(s) = session {
                    r.session = Some(s.to_string());
                }
                r.clone()
            }
            None => {
                let id = store.next_id();
                let r = Review {
                    id,
                    repo: repo.to_string(),
                    branch: branch.to_string(),
                    session: session.map(str::to_string),
                    plan: plan.to_string(),
                    status: ReviewStatus::Pending,
                    notes: None,
                    attn_id: None,
                    reviewer_count,
                    quorum,
                    votes: Vec::new(),
                    high_risk,
                    risk_reasons: risk_reasons.clone(),
                    consensus,
                    consensus_attn_id: None,
                    created: now,
                    updated: now,
                };
                store.reviews.push(r.clone());
                r
            }
        };
        // Mirror onto the attention list (best-effort — a failed mirror must not lose the record).
        // Re-raise reuses the open item by key, or creates a fresh one after a prior ack.
        let attn_id = raise_attn(&review, &today);
        if let Some(aid) = &attn_id {
            if let Some(slot) = store.reviews.iter_mut().find(|r| r.id == review.id) {
                slot.attn_id = Some(aid.clone());
            }
        }
        let mut stored = review;
        stored.attn_id = attn_id;
        (true, stored)
    })
}

pub fn majority_quorum(reviewer_count: usize) -> usize {
    reviewer_count / 2 + 1
}

pub fn risk_reasons(labels: &[String], paths: &[String]) -> Vec<String> {
    let mut reasons = Vec::new();
    for label in labels {
        if HIGH_RISK_LABELS
            .iter()
            .any(|sensitive| label.eq_ignore_ascii_case(sensitive))
        {
            reasons.push(format!("label:{label}"));
        }
    }
    for path in paths {
        if SENSITIVE_PATH_GLOBS
            .iter()
            .any(|glob| crate::orchestrate::glob_overlap(glob, path))
        {
            reasons.push(format!("path:{path}"));
        }
    }
    reasons.sort();
    reasons.dedup();
    reasons
}

/// Pull `additional` more reviewers into an open, consensus plan-review — the on-demand
/// scale-up (#656). Existing ballots are kept; `reviewer_count` grows by `additional` and the
/// quorum is recomputed to a strict majority of the enlarged panel, so adding reviewers
/// *tightens* the bar rather than resetting the round. Voting reopens (`AwaitingVotes`) and any
/// stale no-consensus attention raised by the previous full panel is acked off. Requires the
/// review to be pending and consensus-style (`reviewer_count > 0`).
pub fn add_reviewers(id: &str, additional: usize, now: i64) -> Result<Review, VoteError> {
    if additional == 0 {
        return Err(VoteError::NothingToAdd(id.to_string()));
    }
    let result = mutate(|store| {
        let Some(slot) = store.reviews.iter_mut().find(|r| r.id == id) else {
            return (false, Err(VoteError::NotFound(id.to_string())));
        };
        if !slot.status.is_pending() {
            return (
                false,
                Err(VoteError::NotPending {
                    id: id.to_string(),
                    status: slot.status,
                }),
            );
        }
        if slot.reviewer_count == 0 {
            return (false, Err(VoteError::NotConsensusReview(id.to_string())));
        }
        slot.reviewer_count += additional;
        slot.quorum = majority_quorum(slot.reviewer_count);
        slot.consensus = ConsensusStatus::AwaitingVotes;
        // A prior full panel may have raised a no-consensus attention item; growing the panel
        // reopens the decision, so retire that stale escalation.
        if let Some(aid) = slot.consensus_attn_id.take() {
            crate::util::best_effort(
                crate::attention::ack(&aid),
                "plan-review: ack stale consensus attn on grow",
            );
        }
        slot.updated = now;
        (true, Ok(slot.clone()))
    });
    match result {
        Ok(inner) => inner,
        Err(e) => Err(VoteError::Io(e)),
    }
}

pub fn vote(
    id: &str,
    reviewer: &str,
    verdict: VoteVerdict,
    notes: Option<&str>,
    now: i64,
) -> Result<Review, VoteError> {
    vote_with_confidence(id, reviewer, verdict, None, notes, now)
}

pub fn vote_with_confidence(
    id: &str,
    reviewer: &str,
    verdict: VoteVerdict,
    confidence: Option<u8>,
    notes: Option<&str>,
    now: i64,
) -> Result<Review, VoteError> {
    vote_with_feedback(id, reviewer, verdict, confidence, notes, &[], now)
}

#[allow(clippy::too_many_arguments)]
pub fn vote_with_feedback(
    id: &str,
    reviewer: &str,
    verdict: VoteVerdict,
    confidence: Option<u8>,
    notes: Option<&str>,
    risks: &[String],
    now: i64,
) -> Result<Review, VoteError> {
    let reviewer = reviewer.trim();
    if reviewer.is_empty() {
        return Err(VoteError::EmptyReviewer);
    }
    if let Some(value) = confidence {
        if value > 100 {
            return Err(VoteError::InvalidConfidence(value));
        }
    }
    let notes = notes.map(str::trim).filter(|s| !s.is_empty());
    let risks: Vec<String> = risks
        .iter()
        .map(|risk| risk.trim())
        .filter(|risk| !risk.is_empty())
        .map(str::to_string)
        .collect();
    let today = crate::util::today();
    let result = mutate(|store| {
        let Some(slot) = store.reviews.iter_mut().find(|r| r.id == id) else {
            return (false, Err(VoteError::NotFound(id.to_string())));
        };
        if !slot.status.is_pending() {
            return (
                false,
                Err(VoteError::NotPending {
                    id: id.to_string(),
                    status: slot.status,
                }),
            );
        }
        if slot.reviewer_count == 0 {
            return (false, Err(VoteError::NotConsensusReview(id.to_string())));
        }
        if slot
            .votes
            .iter()
            .any(|vote| vote.reviewer.eq_ignore_ascii_case(reviewer))
        {
            return (
                false,
                Err(VoteError::DuplicateReviewer(reviewer.to_string())),
            );
        }
        if slot.votes.len() >= slot.reviewer_count {
            return (false, Err(VoteError::VotingClosed(id.to_string())));
        }
        slot.votes.push(ReviewerVote {
            reviewer: reviewer.to_string(),
            verdict,
            confidence,
            notes: notes.map(str::to_string),
            risks,
            created: now,
        });
        slot.updated = now;

        if slot.votes.len() == slot.reviewer_count {
            if slot.approvals() >= slot.quorum {
                if slot.high_risk {
                    slot.consensus = ConsensusStatus::HumanRequired;
                } else {
                    slot.consensus = ConsensusStatus::Approved;
                    slot.status = ReviewStatus::Approved;
                    if let Some(aid) = slot.attn_id.clone() {
                        crate::util::best_effort(
                            crate::attention::ack(&aid),
                            "plan-review: ack consensus-approved attn",
                        );
                    }
                }
            } else {
                slot.consensus = ConsensusStatus::NoConsensus;
                if let Some(aid) = raise_consensus_attn(slot, &today) {
                    slot.consensus_attn_id = Some(aid);
                }
            }
        }
        (true, Ok(slot.clone()))
    });
    match result {
        Ok(inner) => inner,
        Err(e) => Err(VoteError::Io(e)),
    }
}

/// Apply a human/web decision to a *pending* review. `approve` unblocks the agent, `request-changes`
/// records the notes for its revise loop, `reject` stops it — and the mirrored attention item is
/// acked off the list. Errors if the id is unknown, the review isn't pending, or notes are missing
/// on a change request.
pub fn decide(
    id: &str,
    decision: Decision,
    notes: Option<&str>,
    now: i64,
) -> Result<Review, DecideError> {
    let notes = notes.map(str::trim).filter(|s| !s.is_empty());
    if decision == Decision::RequestChanges && notes.is_none() {
        return Err(DecideError::MissingNotes);
    }
    let result = mutate(|store| {
        let Some(slot) = store.reviews.iter_mut().find(|r| r.id == id) else {
            return (false, Err(DecideError::NotFound(id.to_string())));
        };
        if !slot.status.is_pending() {
            return (
                false,
                Err(DecideError::NotPending {
                    id: id.to_string(),
                    status: slot.status,
                }),
            );
        }
        slot.status = decision.target_status();
        if let Some(n) = notes {
            slot.notes = Some(n.to_string());
        }
        slot.updated = now;
        let attn_id = slot.attn_id.clone();
        let consensus_attn_id = slot.consensus_attn_id.take();
        let review = slot.clone();
        // The human has acted → resolve the mirrored attention item (best-effort).
        if let Some(aid) = attn_id {
            crate::util::best_effort(crate::attention::ack(&aid), "plan-review: ack attn");
        }
        if let Some(aid) = consensus_attn_id {
            crate::util::best_effort(
                crate::attention::ack(&aid),
                "plan-review: ack consensus attn",
            );
        }
        (true, Ok(review))
    });
    match result {
        Ok(inner) => inner,
        Err(e) => Err(DecideError::Io(e)),
    }
}

/// Replace the plan text of an *open* review (pending or changes-requested) — the human's
/// edit-in-place from `bn review edit`. Keeps the review `pending` (so the human still decides) and
/// re-syncs the mirrored attention item's detail to the new plan. Errors if the id is unknown, the
/// review is terminal (approved/rejected), or the new plan is empty.
pub fn update_plan(id: &str, plan: &str, now: i64) -> Result<Review, DecideError> {
    let plan = plan.trim_end().to_string();
    if plan.trim().is_empty() {
        return Err(DecideError::EmptyPlan);
    }
    let today = crate::util::today();
    let result = mutate(|store| {
        let Some(slot) = store.reviews.iter_mut().find(|r| r.id == id) else {
            return (false, Err(DecideError::NotFound(id.to_string())));
        };
        if slot.status.is_terminal() {
            return (
                false,
                Err(DecideError::NotPending {
                    id: id.to_string(),
                    status: slot.status,
                }),
            );
        }
        slot.plan = plan.clone();
        slot.status = ReviewStatus::Pending;
        slot.votes.clear();
        slot.consensus = if slot.reviewer_count == 0 {
            ConsensusStatus::HumanOnly
        } else {
            ConsensusStatus::AwaitingVotes
        };
        if let Some(aid) = slot.consensus_attn_id.take() {
            crate::util::best_effort(
                crate::attention::ack(&aid),
                "plan-review: ack edited consensus attn",
            );
        }
        slot.updated = now;
        // Keep the mirrored attention item's detail in sync with the edited plan (best-effort —
        // reused by review-id key; only adopt a fresh id, never clobber the existing one with None).
        if let Some(aid) = raise_attn(slot, &today) {
            slot.attn_id = Some(aid);
        }
        (true, Ok(slot.clone()))
    });
    match result {
        Ok(inner) => inner,
        Err(e) => Err(DecideError::Io(e)),
    }
}

/// Review `id`, if any.
pub fn get(id: &str) -> Option<Review> {
    ReviewStore::load(&store_path()).get(id).cloned()
}

/// Every review, newest-submitted first — the `bn review list` / dashboard feed.
pub fn list() -> Vec<Review> {
    let mut all = ReviewStore::load(&store_path()).reviews;
    all.sort_by_key(|r| std::cmp::Reverse(r.created));
    all
}

/// The open (pending or changes-requested) reviews — the ones still needing attention, newest
/// first. Used to annotate orchestrate items with their waiting-for-review state.
pub fn open_reviews() -> Vec<Review> {
    let mut open: Vec<Review> = ReviewStore::load(&store_path())
        .reviews
        .into_iter()
        .filter(Review::is_open)
        .collect();
    open.sort_by_key(|r| std::cmp::Reverse(r.created));
    open
}

/// The pending review for a repo+branch, if the agent is currently waiting-for-review. Matched
/// case-insensitively on repo and on the sanitized branch (raw `dispatch/349` ~ window `dispatch-349`).
pub fn pending_for<'a>(reviews: &'a [Review], repo: &str, branch: &str) -> Option<&'a Review> {
    let (r, b) = (repo.to_lowercase(), sanitize_branch(branch));
    reviews.iter().find(|rv| {
        rv.status.is_pending() && rv.repo.to_lowercase() == r && sanitize_branch(&rv.branch) == b
    })
}

/// Raise (or reuse) the attention item mirroring a pending review. `mode = "review"` is the signal
/// the #394 dashboard panel keys on; the plan rides in `detail` and the item is tagged
/// `task_type = "planning"` so it surfaces under `bn attn --type planning` (#649 Phase 2). Keyed by
/// review id so a re-submit
/// after a prior ack raises a fresh item rather than duplicating an open one. Best-effort: returns
/// the (new or existing) attention id, or `None` if the append failed.
fn raise_attn(review: &Review, today: &str) -> Option<String> {
    let title = format!("Plan review: {}/{}", review.repo, review.branch);
    let outcome = crate::attention::add_new(
        &crate::attention::NewAttn {
            source: "plan-review",
            title: &title,
            severity: "warn",
            repo: Some(&review.repo),
            branch: Some(&review.branch),
            key: Some(&review.id),
            detail: Some(&review.plan),
            mode: Some("review"),
            task_type: Some("planning"),
            ..Default::default()
        },
        today,
    );
    match outcome {
        Ok(o) => Some(o.id),
        Err(e) => {
            crate::util::best_effort::<(), _>(Err(e), "plan-review: raise attn");
            None
        }
    }
}

fn raise_consensus_attn(review: &Review, today: &str) -> Option<String> {
    let title = format!("Plan consensus needed: {}/{}", review.repo, review.branch);
    let detail = format!(
        "{}/{} reviewers approved; human decision required.\n\n{}",
        review.approvals(),
        review.votes.len(),
        review.plan
    );
    let key = format!("{}:consensus", review.id);
    let outcome = crate::attention::add_new(
        &crate::attention::NewAttn {
            source: "plan-review",
            title: &title,
            severity: "warn",
            repo: Some(&review.repo),
            branch: Some(&review.branch),
            key: Some(&key),
            detail: Some(&detail),
            mode: Some("review"),
            task_type: Some("planning"),
            ..Default::default()
        },
        today,
    );
    match outcome {
        Ok(o) => Some(o.id),
        Err(e) => {
            crate::util::best_effort::<(), _>(Err(e), "plan-review: raise consensus attn");
            None
        }
    }
}
