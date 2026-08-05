//! The adversarial diff-only review gate (#607): before a dispatched *ship* PR is eligible to
//! land, ≥N independent reviewers must inspect the **PR diff only** — no branch-note reasoning, no
//! plan, no implementer transcript — assume the code is wrong, and enumerate why. This removes the
//! ship-bias of a self-reviewing author; the implementer never reviews and the reviewer never
//! implements.
//!
//! Inspired by Bun's Rust rewrite (≥2 diff-only reviewers per implementer): the class of bugs it
//! catches — use-after-free, eager `unwrap_or` panics, sign errors — all *compile clean and look
//! plausible*, so a passing `verify` gate is not enough. This gate is a distinct, post-implementation
//! correctness pass, complementary to the plan-review gate (#349, pre-implementation) and `bn gate`
//! (build/test).
//!
//! State lives in `review-gate.json` in the state dir, one [`Entry`] per PR (`repo#number`). Each
//! entry accumulates [`Review`]s (a reviewer's verdict at a specific head sha) and [`Finding`]s
//! (concrete defects, resolvable or waivable). The gate is **open** — the PR may land — only when
//! enough distinct reviewers *approve at the current head sha* AND no *blocking* finding is still
//! open. A force-push (new head sha) invalidates prior approvals (they no longer match the current
//! sha) but never auto-resolves an open finding — a real bug stays blocking until someone resolves
//! or explicitly waives it. Every mutation is a locked read-modify-write over an atomically-replaced
//! file, matching [`crate::plan_review`]/[`crate::orchestrate`], so a reviewer agent and the land
//! step never corrupt one another.

use crate::resolver::state_dir;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default number of independent approving reviews required before the gate opens. Two mirrors
/// Bun's "≥2 reviewers in separate context windows" discipline; overridable via config.
pub const DEFAULT_REQUIRED_REVIEWS: usize = 2;

/// A reviewer's recommendation on the diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The diff looks correct — counts toward the required-approvals threshold at its sha.
    Approve,
    /// The diff is buggy/incorrect — never counts toward approvals (the reviewer should also file
    /// the concrete findings that justify the block).
    Block,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Approve => "approve",
            Verdict::Block => "block",
        }
    }

    /// Parse a CLI verdict token (`approve`/`block`), case-insensitively. Also accepts the
    /// auto-review vocabulary (`reject` ⇒ `block`) so the two review surfaces share a language.
    pub fn parse(s: &str) -> Option<Verdict> {
        match s.trim().to_lowercase().as_str() {
            "approve" | "approved" | "ok" | "lgtm" => Some(Verdict::Approve),
            "block" | "reject" | "rejected" | "changes" => Some(Verdict::Block),
            _ => None,
        }
    }
}

/// How serious a finding is — only *blocking* findings hold the gate closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// A correctness defect that must be resolved or explicitly waived before the PR may land.
    Blocking,
    /// A non-blocking observation (style, nit, suggestion) — recorded, but never holds the gate.
    Advisory,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Blocking => "blocking",
            Severity::Advisory => "advisory",
        }
    }

    pub fn parse(s: &str) -> Option<Severity> {
        match s.trim().to_lowercase().as_str() {
            "blocking" | "block" | "high" | "critical" => Some(Severity::Blocking),
            "advisory" | "nit" | "low" | "minor" | "info" => Some(Severity::Advisory),
            _ => None,
        }
    }
}

/// Where a finding sits — open findings (if blocking) hold the gate closed until addressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    /// Unaddressed. A blocking open finding keeps the gate closed.
    Open,
    /// The author fixed it (typically on a follow-up push) — no longer blocks.
    Resolved,
    /// Deliberately accepted without a fix (with a reason) — no longer blocks.
    Waived,
}

impl FindingStatus {
    pub fn label(self) -> &'static str {
        match self {
            FindingStatus::Open => "open",
            FindingStatus::Resolved => "resolved",
            FindingStatus::Waived => "waived",
        }
    }
}

/// One concrete defect a reviewer enumerated against the diff.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// Stable id within its entry — `f<N>`, never reused even after resolve/waive.
    pub id: String,
    /// Who filed it (reviewer agent/session label).
    pub reviewer: String,
    pub severity: Severity,
    /// The defect itself — what is wrong and why.
    pub text: String,
    pub status: FindingStatus,
    /// The resolution/waiver reason, set when it leaves `open`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Head sha the reviewer saw when filing (context for whether a later push may have fixed it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    pub created: i64,
    pub updated: i64,
}

impl Finding {
    /// Whether this finding currently holds the gate closed: open AND blocking.
    pub fn is_blocking_open(&self) -> bool {
        self.status == FindingStatus::Open && self.severity == Severity::Blocking
    }
}

/// One reviewer's verdict, pinned to the head sha they reviewed. A verdict counts toward the gate
/// only while it matches the PR's *current* head — a force-push past it means the reviewer never
/// saw the shipped code, so their approval no longer applies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Review {
    /// Reviewer identity — an agent/session label, distinct per independent reviewer so the
    /// approval count is genuinely N *separate* reviewers, not one reviewer voting twice.
    pub reviewer: String,
    pub verdict: Verdict,
    /// The head sha (`git rev-parse HEAD`) the reviewer inspected.
    pub sha: String,
    /// A substantive summary of the review.
    #[serde(default)]
    pub summary: String,
    pub created: i64,
}

/// The gate state for one PR.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub repo: String,
    pub number: i64,
    #[serde(default)]
    pub url: String,
    /// Source branch (raw form), for cross-referencing the dispatched item / worktree.
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub reviews: Vec<Review>,
    #[serde(default)]
    pub findings: Vec<Finding>,
    pub created: i64,
    pub updated: i64,
}

impl Entry {
    /// Stable identity: `repo#number`.
    pub fn key(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }

    /// Next `f<N>` finding id — one past the highest ever issued, so ids are never reused.
    fn next_finding_id(&self) -> String {
        let max = self
            .findings
            .iter()
            .filter_map(|f| f.id.strip_prefix('f').and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        format!("f{}", max + 1)
    }

    /// Distinct reviewers who **approved at `current_sha`** — the approvals that count right now.
    /// De-duplicated by reviewer (case-insensitive) so one reviewer re-recording can't inflate the
    /// count; the latest verdict per reviewer at that sha wins.
    pub fn approvals_at(&self, current_sha: &str) -> Vec<String> {
        let mut seen: Vec<(String, Verdict)> = Vec::new();
        for r in &self.reviews {
            if r.sha != current_sha {
                continue;
            }
            let key = r.reviewer.to_lowercase();
            match seen.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = r.verdict, // latest verdict per reviewer wins
                None => seen.push((key, r.verdict)),
            }
        }
        seen.into_iter()
            .filter(|(_, v)| *v == Verdict::Approve)
            .map(|(k, _)| k)
            .collect()
    }

    /// Open blocking findings — the defects still holding the gate closed (any sha: a bug isn't
    /// auto-cleared by a later push, only by an explicit resolve/waive).
    pub fn open_blocking(&self) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|f| f.is_blocking_open())
            .collect()
    }

    /// Evaluate the gate for this PR at `current_sha`, requiring `required` distinct approvals.
    pub fn status(&self, current_sha: &str, required: usize) -> GateStatus {
        let approvals = self.approvals_at(current_sha).len();
        let blocking = self.open_blocking().len();
        GateStatus {
            approvals,
            required,
            open_blocking: blocking,
            open: approvals >= required && blocking == 0,
        }
    }
}

/// The evaluated gate verdict for a PR — the single "may it land?" answer plus the counts behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateStatus {
    /// Distinct approving reviewers at the current head sha.
    pub approvals: usize,
    /// How many were required.
    pub required: usize,
    /// Open blocking findings still holding the gate closed.
    pub open_blocking: usize,
    /// Whether the PR may land: enough approvals AND no open blocking findings.
    pub open: bool,
}

impl GateStatus {
    /// A one-line human summary of why the gate is (or isn't) open.
    pub fn summary(&self) -> String {
        if self.open {
            format!(
                "open — {}/{} approvals, no blocking findings",
                self.approvals, self.required
            )
        } else {
            format!(
                "blocked — {}/{} approvals, {} open blocking finding(s)",
                self.approvals, self.required, self.open_blocking
            )
        }
    }
}

/// The persisted set of per-PR gate entries.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Ledger {
    #[serde(default)]
    pub entries: Vec<Entry>,
}

impl Ledger {
    /// Load from `path`, or an empty ledger if absent/unparseable (the gate just hasn't run yet).
    pub fn load(path: &Path) -> Ledger {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Ledger::default(),
        }
    }

    /// Atomically persist (temp sibling + rename), creating the parent on demand.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        crate::store::save_json_atomic(path, self)
    }

    pub fn find(&self, repo: &str, number: i64) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.repo.eq_ignore_ascii_case(repo) && e.number == number)
    }

    fn find_mut(&mut self, repo: &str, number: i64) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .find(|e| e.repo.eq_ignore_ascii_case(repo) && e.number == number)
    }

    /// Get-or-create the entry for a PR, stamping identity on first sight.
    fn entry_mut(
        &mut self,
        repo: &str,
        number: i64,
        url: &str,
        branch: &str,
        now: i64,
    ) -> &mut Entry {
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.repo.eq_ignore_ascii_case(repo) && entry.number == number)
        {
            return &mut self.entries[index];
        }
        let index = self.entries.len();
        self.entries.push(Entry {
            repo: repo.to_string(),
            number,
            url: url.to_string(),
            branch: branch.to_string(),
            reviews: Vec::new(),
            findings: Vec::new(),
            created: now,
            updated: now,
        });
        &mut self.entries[index]
    }
}

/// The gate store file, beside the orchestrate/attention/plan-review state.
pub fn store_path() -> PathBuf {
    state_dir().join("review-gate.json")
}

/// A locked read-modify-write over the store — mirrors [`crate::plan_review::mutate`], serializing a
/// reviewer agent's `record` against the land step's read and a concurrent resolve/waive.
fn mutate<R>(f: impl FnOnce(&mut Ledger) -> (bool, R)) -> std::io::Result<R> {
    let path = store_path();
    let _lock = crate::store::FileLock::acquire(&path);
    let mut ledger = Ledger::load(&path);
    let (dirty, out) = f(&mut ledger);
    if dirty {
        ledger.save(&path)?;
    }
    Ok(out)
}

/// A finding to file alongside a review: its severity and text (the id is assigned on record).
pub struct NewFinding {
    pub severity: Severity,
    pub text: String,
}

/// Record one reviewer's verdict (and any findings) for a PR at head sha `sha`. Creates the entry
/// on first sight. Findings get stable `f<N>` ids and start `open`. Returns the updated entry.
#[allow(clippy::too_many_arguments)]
pub fn record_review(
    repo: &str,
    number: i64,
    url: &str,
    branch: &str,
    reviewer: &str,
    verdict: Verdict,
    sha: &str,
    summary: &str,
    findings: Vec<NewFinding>,
    now: i64,
) -> std::io::Result<Entry> {
    mutate(|led| {
        let entry = led.entry_mut(repo, number, url, branch, now);
        entry.reviews.push(Review {
            reviewer: reviewer.to_string(),
            verdict,
            sha: sha.to_string(),
            summary: summary.to_string(),
            created: now,
        });
        for nf in findings {
            let id = entry.next_finding_id();
            entry.findings.push(Finding {
                id,
                reviewer: reviewer.to_string(),
                severity: nf.severity,
                text: nf.text,
                status: FindingStatus::Open,
                note: None,
                sha: Some(sha.to_string()),
                created: now,
                updated: now,
            });
        }
        entry.updated = now;
        (true, entry.clone())
    })
}

/// Move a finding out of `open` (resolve or waive) with an optional note. Returns `Ok(true)` if the
/// finding existed and changed, `Ok(false)` if it was not found.
pub fn set_finding_status(
    repo: &str,
    number: i64,
    finding_id: &str,
    status: FindingStatus,
    note: Option<&str>,
    now: i64,
) -> std::io::Result<bool> {
    mutate(|led| {
        let Some(entry) = led.find_mut(repo, number) else {
            return (false, false);
        };
        let Some(f) = entry.findings.iter_mut().find(|f| f.id == finding_id) else {
            return (false, false);
        };
        f.status = status;
        f.note = note.map(str::to_string);
        f.updated = now;
        entry.updated = now;
        (true, true)
    })
}

/// The current gate verdict for a PR at `current_sha`, requiring `required` approvals. An untracked
/// PR (no reviews yet) is blocked (0 approvals) — a diff-only review must run first.
pub fn gate_status(repo: &str, number: i64, current_sha: &str, required: usize) -> GateStatus {
    let led = Ledger::load(&store_path());
    match led.find(repo, number) {
        Some(e) => e.status(current_sha, required),
        None => GateStatus {
            approvals: 0,
            required,
            open_blocking: 0,
            open: false,
        },
    }
}

/// The canonical **diff-only** adversarial reviewer prompt (#607). By construction it references
/// only the diff (`git diff origin/main...HEAD`) and explicitly forbids reading the branch note,
/// plan, or implementer transcript — so the reviewer's context provably excludes the implementer's
/// reasoning. `reviewer_label` names this independent reviewer; `required` states how many separate
/// reviewers the gate needs.
/// Semantic-regression trap checklist (#612): syntactically-identical-but-semantically-different
/// code that compiles clean and looks plausible. Inspired by Bun's Rust rewrite, where most
/// regressions were of this shape. Rendered into the adversarial reviewer prompt so every diff is
/// checked against these classes. **Extensible**: add a new `(name, cue)` line here as new
/// regression classes are found and it flows into the prompt (and the test) automatically.
pub const SEMANTIC_TRAPS: &[(&str, &str)] = &[
    (
        "debug_assert! side effects",
        "any call with side effects inside debug_assert!/assert! stripped in release — the effect never runs in prod",
    ),
    (
        "eager unwrap_or",
        "unwrap_or(expr)/ok_or(expr) evaluates expr eagerly (panics/allocates even on the Some/Ok path) — should be unwrap_or_else/ok_or_else",
    ),
    (
        "format/color-marker rewrite ordering",
        "color/format-marker or escaping rewrites applied over the substituted arguments instead of the template before substitution (or vice-versa)",
    ),
    (
        "release bounds/overflow drift",
        "arithmetic that overflow-panics in debug but silently wraps in release, or bounds checks relied on for correctness — plus placeholder/TODO constants left over from a port",
    ),
    (
        "odd-length slice-cast off-by-one",
        "casting/chunking a slice by a wider element type when the length isn't a multiple of the stride drops or reads past the tail",
    ),
];

/// Render the trap checklist as prompt bullet lines.
fn semantic_trap_lines() -> String {
    SEMANTIC_TRAPS
        .iter()
        .map(|(name, cue)| format!(" - {name}: {cue}."))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn reviewer_prompt(repo: &str, number: i64, reviewer_label: &str, required: usize) -> String {
    let traps = semantic_trap_lines();
    format!(
        "You are adversarial reviewer '{reviewer_label}', one of {required} INDEPENDENT reviewers of \
         {repo} PR #{number}. You review the DIFF ONLY. \
         Do NOT read the branch note, the plan, the implementer's reasoning, or any agent \
         transcript — you must judge the code purely on what it does, not what its author intended. \
         Look ONLY at: git diff origin/main...HEAD (read other files with `git show HEAD:<path>` if \
         needed to understand the change). \
         This is STRICTLY READ-ONLY: never edit, stage, commit, push, or comment on the PR itself. \
         Assume the code is WRONG and find how it is wrong: exhaustively enumerate correctness bugs \
         (use-after-free, panics on `unwrap`/`expect`/eager `unwrap_or`, sign/overflow errors, \
         off-by-one, race conditions, resource leaks, missing error handling, incorrect edge cases) \
         — the dangerous class is code that compiles clean and looks plausible. \
         Pay special attention to these semantic-regression traps — syntactically identical, \
         semantically different — and flag any diff line that matches one:{traps} \
         Then record your verdict with: \
         bn review-gate record {repo} {number} --reviewer {reviewer_label} --sha $(git rev-parse HEAD) \
         --verdict approve|block --summary '...' --finding 'blocking: <defect>' --finding 'advisory: <nit>'. \
         Record one --finding per concrete defect; use `block` if any blocking finding exists. This \
         `bn review-gate record` call is the ONLY write you may make."
    )
}
