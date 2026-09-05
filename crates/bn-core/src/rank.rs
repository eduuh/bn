//! The shared ranking vocabulary: which **band** a piece of work sits in, and how it is ordered
//! *within* that band by time, strategy and operator intent.
//!
//! bn had two rankers with two different shapes. `bn next` compares **lexicographically** — band,
//! then section, then momentum, then age — so nothing cosy can ever outrank a failing cron.
//! `bn queue` compares an **additive score**: goal alignment plus attention pressure plus the
//! entry's own `priority`, summed. The first two terms are bounded (0..=115 and 0..=30); `priority`
//! is an unbounded `i64`. Adding an unbounded quantity to bounded ones means the scales fight, and
//! on this machine they did:
//!
//! - `bn queue add --priority 200` erased the strategic layer entirely — work advancing nothing
//!   outranked work protecting an off-track goal;
//! - an open priority window adds `+100`, which outranks every goal alignment there is;
//! - a PR review's priority *is its waiting hours* ([`crate::queue::jobs`] and `capture_review`),
//!   so a review ignored for 200 hours outranked a person's typed `bn dispatch` at 100. A quantity
//!   and a category were being stored in the same integer.
//!
//! This module takes the lexicographic shape for the part that must not be crossable, and keeps
//! addition only *inside* a band. A [`Band`] is a statement about what **kind** of claim the work
//! has on the machine — somebody is blocked, a person asked, the schedule said so. Everything else
//! — how long it has waited, how close its deadline is, which goal it advances, the operator's
//! nudge — competes for order within that claim and can never escape it.
//!
//! Pure and integer-only: no clock, no floats. The dispatcher's decision has to be reproducible
//! from a captured snapshot, which is what makes `bn queue why` and the timeline's replay able to
//! tell the same story twice.
use serde::{Deserialize, Serialize};

/// What kind of claim a piece of work has on the machine. Ordered most-pressing first by `ord`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Band {
    /// Warming, releases, anything whose whole purpose is to use time nobody else wants.
    Background,
    /// The schedule said so: crons, the default ship, ordinary backlog.
    Scheduled,
    /// Keeps the machine able to do anything at all — reclaiming leases, freeing slots.
    ///
    /// Above routine scheduled work and below anything a person is waiting on, which is exactly
    /// where [`crate::queue::jobs::RECOVERY_PRIORITY`] has always put it: "exempting recovery from
    /// pause but leaving it at the back of a long cron backlog would be an exemption in name only".
    Recovery,
    /// Work a person reviewed and accepted — an accepted Today suggestion, a pipeline stage.
    Planned,
    /// A person typed this and is waiting for it.
    Asked,
    /// Somebody is blocked right now — an urgent attention item, an ad-hoc ask.
    Interrupt,
    /// The escape hatch. Set only by an explicit `bn queue pin`, and deliberately the only way to
    /// bypass the model.
    Pinned,
}

/// The legacy `priority` at which each band begins.
///
/// These are not invented: they are the numbers already in the tree, read back as the categories
/// they were always standing in for. `1000` is `INTERRUPT_PRIORITY`, `100` is a typed dispatch,
/// `80`/`81` is an accepted Today suggestion, `50` is `RECOVERY_PRIORITY`, `-10` is
/// `CRON_PRIORITY`, `-50` is `HEAVY_PRIORITY`. So the mapping is a naming exercise over data that
/// already exists, not a conversion of it.
const INTERRUPT_FLOOR: i64 = 1000;
const ASKED_FLOOR: i64 = 100;
const PLANNED_FLOOR: i64 = 60;
const RECOVERY_FLOOR: i64 = 45;
const SCHEDULED_FLOOR: i64 = -40;
const BACKGROUND_FLOOR: i64 = -100;

/// How far apart two bands sit in the flat score.
///
/// Every within-band term is clamped so their sum cannot reach this. That is the whole contract:
/// aging, urgency, goal alignment and an operator's nudge reorder work inside its band and can
/// never lift it out of one. See [`MAX_WITHIN`], which is asserted against this in the tests.
pub const BAND_STRIDE: i64 = 1_000;

/// Ceilings on each within-band term. Their sum is [`MAX_WITHIN`], which must stay below
/// [`BAND_STRIDE`].
pub const NUDGE_MAX: i64 = 99;
pub const ALIGN_MAX: i64 = 115;
pub const PRESSURE_MAX: i64 = 30;
pub const URGENCY_MAX: i64 = 400;
pub const AGE_MAX: i64 = 250;

/// The largest a within-band score can be.
pub const MAX_WITHIN: i64 = NUDGE_MAX + ALIGN_MAX + PRESSURE_MAX + URGENCY_MAX + AGE_MAX;

/// The band model's one load-bearing invariant, checked by the compiler rather than by a test:
/// everything competing inside a band must not sum to a band. Add a term or raise a ceiling past
/// the stride and this fails the build, which is the only place it can fail safely — a runtime
/// check would report a silent reordering after the fact.
const _: () = assert!(
    MAX_WITHIN < BAND_STRIDE,
    "within-band terms must never reach the next band"
);

/// How long work waits before its age term is at full weight.
///
/// A day, so that within one band the thing that has been waiting since yesterday leads. Shorter
/// and every queue older than an hour would be flat again; longer and starvation takes a week to
/// correct.
pub const AGE_SATURATION_SECS: i64 = 24 * 3600;

/// How much slack a deadline needs before it stops being urgent.
///
/// Slack, not proximity: the term measures `not_after - now - expected_duration`, so a four-hour
/// job due in five hours is already more urgent than a five-second job due in one. Time-to-deadline
/// alone cannot say that, and it is the thing you actually need to know.
pub const URGENCY_HORIZON_SECS: i64 = 4 * 3600;

impl Band {
    /// Rank order, higher first.
    pub fn ord(self) -> i64 {
        match self {
            Band::Background => 0,
            Band::Scheduled => 1,
            Band::Recovery => 2,
            Band::Planned => 3,
            Band::Asked => 4,
            Band::Interrupt => 5,
            Band::Pinned => 6,
        }
    }

    /// The kebab-case label for the decision file and the status surfaces.
    pub fn label(self) -> &'static str {
        match self {
            Band::Background => "background",
            Band::Scheduled => "scheduled",
            Band::Recovery => "recovery",
            Band::Planned => "planned",
            Band::Asked => "asked",
            Band::Interrupt => "interrupt",
            Band::Pinned => "pinned",
        }
    }

    pub fn parse(s: &str) -> Option<Band> {
        match s.trim().to_ascii_lowercase().as_str() {
            "background" => Some(Band::Background),
            "scheduled" => Some(Band::Scheduled),
            "recovery" => Some(Band::Recovery),
            "planned" => Some(Band::Planned),
            "asked" => Some(Band::Asked),
            "interrupt" => Some(Band::Interrupt),
            "pinned" => Some(Band::Pinned),
            _ => None,
        }
    }

    /// The band a legacy `priority` integer was standing in for.
    ///
    /// Total over every `i64`, so an entry already on disk keeps working without a migration and
    /// without a rewrite of the append-only store. `Pinned` is deliberately unreachable from here:
    /// it is a person's explicit statement, never something inferred from a number.
    pub fn from_legacy_priority(priority: i64) -> Band {
        if priority >= INTERRUPT_FLOOR {
            Band::Interrupt
        } else if priority >= ASKED_FLOOR {
            Band::Asked
        } else if priority >= PLANNED_FLOOR {
            Band::Planned
        } else if priority >= RECOVERY_FLOOR {
            Band::Recovery
        } else if priority >= SCHEDULED_FLOOR {
            Band::Scheduled
        } else {
            Band::Background
        }
    }

    /// The band an attention-list action sits in.
    ///
    /// `bn next` has always had bands — [`crate::action_rank`] calls itself "canonical ranking
    /// semantics shared by bn's work-selection surfaces" and was, until now, shared with nothing.
    /// Its four `u8` levels are the same claims this enum names, so mapping them is what actually
    /// makes the vocabulary shared: `bn next` and `bn queue` stop being two ranking models that
    /// happen to agree and become one that is stated once.
    ///
    /// The mapping preserves `bn next`'s order exactly. It does not change how that list sorts —
    /// section and momentum still do that — it just lets both surfaces say "this is an interrupt"
    /// and mean the same thing.
    pub fn from_action_priority(priority: u8) -> Band {
        match priority {
            // The WIP/focus guard and actively-broken work share a band. They are distinguished in
            // `bn next` by `Rank::is_urgent`, which decides whether they may interrupt *focused
            // work* — a different question from what claim they have, and one this enum does not
            // try to answer. Both are saying "deal with this before you start anything else".
            3 | 4 => Band::Interrupt,
            // Warrants attention without interrupting.
            2 => Band::Planned,
            _ => Band::Scheduled,
        }
    }

    /// The legacy priority at which this band begins — the anchor a band writes back so an older
    /// binary reading the same store still sorts it correctly.
    pub fn legacy_floor(self) -> i64 {
        match self {
            Band::Background => BACKGROUND_FLOOR,
            Band::Scheduled => SCHEDULED_FLOOR,
            Band::Recovery => RECOVERY_FLOOR,
            Band::Planned => PLANNED_FLOOR,
            Band::Asked => ASKED_FLOOR,
            Band::Interrupt => INTERRUPT_FLOOR,
            Band::Pinned => INTERRUPT_FLOOR,
        }
    }

    /// The within-band nudge a legacy priority carries — how far above its band's floor it sat.
    ///
    /// Saturating and clamped, because `queue_tick_policy` deliberately ranks an `i64::MAX`
    /// priority to prove the arithmetic does not wrap, and a plain subtraction there would.
    pub fn nudge_from_legacy(self, priority: i64) -> i64 {
        priority
            .saturating_sub(self.legacy_floor())
            .clamp(0, NUDGE_MAX)
    }
}

/// How urgent a deadline makes this work, `0..=URGENCY_MAX`.
///
/// Measured in *slack* — the time that would be left over after the work finished — so the term
/// answers "can this still make it?" rather than "is the deadline soon?". Work with no deadline is
/// not urgent, which is most of it: a code change is not less valuable an hour later.
pub fn urgency(not_after: Option<i64>, now: i64, expected_secs: i64) -> i64 {
    let Some(deadline) = not_after else {
        return 0;
    };
    let slack = deadline
        .saturating_sub(now)
        .saturating_sub(expected_secs.max(0));
    if slack <= 0 {
        return URGENCY_MAX;
    }
    if slack >= URGENCY_HORIZON_SECS {
        return 0;
    }
    // Integer throughout: the numerator is bounded by URGENCY_MAX * URGENCY_HORIZON_SECS, which is
    // nowhere near i64, so this cannot overflow and needs no float.
    URGENCY_MAX * (URGENCY_HORIZON_SECS - slack) / URGENCY_HORIZON_SECS
}

/// How much having waited lifts this work, `0..=AGE_MAX`.
///
/// This is the anti-starvation term, and the reason it is bounded is the whole design: it cures
/// starvation *inside* a band without ever letting a week-old cron outrank a person. `tick.rs`
/// captures a `waiting_since` field and calls it "the starvation alarm"; nothing read it until now.
pub fn age_bonus(added_at: i64, now: i64) -> i64 {
    let waited = now.saturating_sub(added_at).max(0);
    if waited >= AGE_SATURATION_SECS {
        return AGE_MAX;
    }
    AGE_MAX * waited / AGE_SATURATION_SECS
}

/// Everything that decides one item's place in the order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankInput {
    pub band: Band,
    /// Operator intent within the band.
    pub nudge: i64,
    /// Goal alignment, from [`crate::advise`].
    pub align: i64,
    /// Open attention pressure on this work's repo.
    pub pressure: i64,
    /// Deadline pressure — see [`urgency`].
    pub urgency: i64,
    /// How long it has waited — see [`age_bonus`].
    pub age: i64,
}

/// A computed rank, with the terms kept so a surface can explain the number rather than assert it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Score {
    pub total: i64,
    pub band: Band,
    pub within: i64,
    pub nudge: i64,
    pub align: i64,
    pub pressure: i64,
    pub urgency: i64,
    pub age: i64,
}

/// Compose the terms into one comparable number.
///
/// Every term is clamped on the way in, so a caller cannot widen a band by passing a larger value:
/// the guarantee that within-band terms never cross a boundary is enforced here, not documented and
/// hoped for.
pub fn score(input: &RankInput) -> Score {
    let nudge = input.nudge.clamp(0, NUDGE_MAX);
    let align = input.align.clamp(0, ALIGN_MAX);
    let pressure = input.pressure.clamp(0, PRESSURE_MAX);
    let urgency = input.urgency.clamp(0, URGENCY_MAX);
    let age = input.age.clamp(0, AGE_MAX);
    let within = nudge + align + pressure + urgency + age;
    Score {
        total: input.band.ord() * BAND_STRIDE + within,
        band: input.band,
        within,
        nudge,
        align,
        pressure,
        urgency,
        age,
    }
}

impl Score {
    /// A one-line breakdown for `bn queue why` — the arithmetic in the order it happened.
    pub fn explain(&self) -> String {
        let mut parts = vec![format!("band {}", self.band.label())];
        for (label, value) in [
            ("nudge", self.nudge),
            ("goal", self.align),
            ("attn", self.pressure),
            ("deadline", self.urgency),
            ("waited", self.age),
        ] {
            if value != 0 {
                parts.push(format!("{label} +{value}"));
            }
        }
        format!("{} = {}", parts.join(" · "), self.total)
    }
}
