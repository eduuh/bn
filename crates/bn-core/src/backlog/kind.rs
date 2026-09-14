//! The backlog's vocabulary: **what** a piece of work is ([`BacklogKind`]) and the concurrency
//! **lane** it runs in ([`WorkClass`]), which is a projection of the kind.
use serde::{Deserialize, Serialize};

/// What kind of work a backlog entry represents — the **taxonomy** axis, orthogonal to the
/// concurrency lane ([`WorkClass`]). Serialized kebab-case.
///
/// `#[serde(other)]` is deliberately **not** used: an unrecognized kind must fail the line parse
/// loudly rather than silently degrade to `ship`. Every kind an older binary wrote stays listed so
/// its lines still fold.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum BacklogKind {
    /// A change that opens and lands a PR — the default and the pre-existing behavior.
    #[default]
    Ship,
    /// An investigation-first lane that produces findings and, when the fix is clear and bounded,
    /// may continue to a PR in the same session (#348 scout).
    Scout,
    /// A scoped read-only investigation that writes evidence into a branch note and hands off to
    /// plan (the `investigate` skill).
    Investigate,
    /// A deep research / answer agent — the ask lane.
    Research,
    /// A quality pass over existing code that opens a PR.
    Enhance,
    /// An automated review of a pull request (the `auto-review-pr` skill).
    ReviewPr,
    /// A scheduled headless agent or job.
    Cron,
    /// Housekeeping that reclaims a resource rather than producing one.
    Cleanup,
}

/// The **concurrency lane** an item is admitted against (#385) — a projection of
/// [`BacklogKind`], not a second taxonomy.
///
/// Defaults to [`Coding`](WorkClass::Coding) (the `ship` default), so a pre-#385 run's state — and
/// every `--queue` item, which has no kind — reads as a coding lane. Serialized kebab-case for the
/// status surface.
///
/// Lives here rather than in `orchestrate` because it is defined *by* the kind: keeping the enum
/// next to the mapping is what makes [`from_kind`](WorkClass::from_kind) exhaustive, so a new kind
/// cannot be added without deciding which pool pays for it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum WorkClass {
    /// A heavy coding agent that opens and lands a PR — the default lane.
    #[default]
    Coding,
    /// A light, read-only research/scout agent that produces findings, not a merge.
    Research,
}

impl WorkClass {
    /// The kebab-case label persisted on the run state + shown in status (`coding` / `research`).
    pub fn label(self) -> &'static str {
        match self {
            WorkClass::Coding => "coding",
            WorkClass::Research => "research",
        }
    }

    /// The lane an entry of `kind` belongs to.
    ///
    /// Anything that opens a PR is a coding agent and costs a coding slot; everything else is
    /// read-only and costs a research slot. Exhaustive on purpose — see the type's doc comment.
    pub fn from_kind(kind: BacklogKind) -> WorkClass {
        match kind {
            BacklogKind::Ship | BacklogKind::Enhance | BacklogKind::ReviewPr => WorkClass::Coding,
            BacklogKind::Scout
            | BacklogKind::Investigate
            | BacklogKind::Research
            | BacklogKind::Cron
            | BacklogKind::Cleanup => WorkClass::Research,
        }
    }
}
