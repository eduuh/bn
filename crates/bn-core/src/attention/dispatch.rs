//! The attn → orchestrate task pipeline (#460, part of #348): promote a raised attention item into
//! the durable orchestrate backlog as a new task, reprioritize the work if it's already queued, or
//! route an urgent item onto the #378 priority/ask lane.
//!
//! The classification is a **pure policy** ([`classify`]) tested in isolation; the wiring
//! ([`promote_to_backlog`]) turns an [`AttnItem`] + the current [`Backlog`] view into the
//! [`BacklogEntry`] line to append (a fresh entry, or a bumped clone of the existing one). The CLI
//! does the file I/O and folds the item off the attention list via `attn_dispatched`.

use super::model::AttnItem;
use crate::backlog::{AddSpec, Backlog, BacklogEntry, BacklogKind};
use crate::orchestrate::derive_id;

/// Priority band an *interrupt* (urgent) attn promotion rides — the #378 ad-hoc priority/ask lane.
/// Mirrors the CLI's `ASK_PRIORITY`, well above the default-`0` ship floor, so an urgent item
/// leases ahead of the whole ship backlog and dispatches on the next active poll.
pub const INTERRUPT_PRIORITY: i64 = 1000;
/// The band a `warn` start-new promotion enters at — above the default ship floor so a
/// human-flagged concern outranks routine backlog adds, but well below an interrupt.
pub const WARN_PRIORITY: i64 = 100;
/// A routine `info` start-new promotion enters at the default ship floor.
pub const INFO_PRIORITY: i64 = 0;

/// How an attention item enters the orchestrate queue — the three lanes of the #460 pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionAction {
    /// No matching active backlog entry exists — enqueue a brand-new pending task.
    StartNew,
    /// A matching active entry is already queued/leased — bump its priority instead of duplicating.
    Reprioritize,
    /// An urgent item with no queued twin — enqueue onto the #378 priority lane so it leases first.
    Interrupt,
}

impl PromotionAction {
    /// The kebab-case label persisted on the `attn_dispatched` trail + shown to the user.
    pub fn label(self) -> &'static str {
        match self {
            PromotionAction::StartNew => "start-new",
            PromotionAction::Reprioritize => "reprioritize",
            PromotionAction::Interrupt => "interrupt",
        }
    }
}

/// The pure classification decision: which lane, and at what priority the backlog entry lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Promotion {
    pub action: PromotionAction,
    pub priority: i64,
}

/// The severity → priority-band mapping: `urgent` rides the interrupt band, `warn` a middle band,
/// everything else the default floor. Pure so the whole policy is unit-testable.
pub fn severity_band(severity: &str) -> i64 {
    match severity {
        "urgent" => INTERRUPT_PRIORITY,
        "warn" => WARN_PRIORITY,
        _ => INFO_PRIORITY,
    }
}

/// The pure promotion policy: classify how an attn item of `severity` should enter the backlog,
/// given the current priority of a matching **active** entry (`existing_priority`), if one exists.
///
/// - An already-queued twin → **reprioritize**: bump to at least its severity band, but always a
///   strict raise (`cur + 1` floor) so a re-flagged item actually moves up, never a duplicate.
/// - No twin + `urgent` → **interrupt**: the #378 priority lane.
/// - No twin, otherwise → **start-new** at the severity band.
pub fn classify(severity: &str, existing_priority: Option<i64>) -> Promotion {
    let band = severity_band(severity);
    match existing_priority {
        Some(cur) => Promotion {
            action: PromotionAction::Reprioritize,
            priority: band.max(cur + 1),
        },
        None if severity == "urgent" => Promotion {
            action: PromotionAction::Interrupt,
            priority: band,
        },
        None => Promotion {
            action: PromotionAction::StartNew,
            priority: band,
        },
    }
}

/// The dispatch subject to enqueue for an attn item: its link when that resolves to an issue/PR
/// number (so the agent gets the issue directly), else the item's title as free text.
pub fn dispatch_subject(item: &AttnItem) -> String {
    if let Some(link) = &item.link {
        if derive_id(link).chars().all(|c| c.is_ascii_digit()) {
            return link.clone();
        }
    }
    item.title.clone()
}

/// The outcome of promoting an attn item into the backlog: the chosen lane, the backlog entry line
/// to append (a fresh pending entry for start-new/interrupt, or the bumped clone for reprioritize),
/// and the derived `(subject, backlog_id)` for the trail.
#[derive(Debug, Clone, PartialEq)]
pub struct DispatchOutcome {
    pub action: PromotionAction,
    pub entry: BacklogEntry,
    pub subject: String,
    pub backlog_id: String,
}

/// Turn an attn `item` into the orchestrate backlog entry to enqueue in `repo`, applying the pure
/// [`classify`] policy against the current `backlog` view. Pure: it reads the backlog by reference
/// and returns the line to append — the caller ([`crate::backlog::add`]/[`reprioritize`]) does the
/// I/O and then folds the item off the attention list. The entry is a `ship` (coding) task sourced
/// `attn:<attn-id>` so the trail points back at the raised item.
pub fn promote_to_backlog(
    item: &AttnItem,
    backlog: &Backlog,
    repo: &str,
    now: i64,
) -> DispatchOutcome {
    let subject = dispatch_subject(item);
    let backlog_id = derive_id(&subject);
    let existing = backlog
        .get(repo, &backlog_id)
        .filter(|entry| entry.status.is_active());
    let promo = classify(&item.severity, existing.map(|entry| entry.priority));

    let entry = if let Some(existing) = existing {
        // The log-structured store folds the bumped clone over the existing (repo, id).
        let mut entry = existing.clone();
        entry.priority = promo.priority;
        entry.updated_at = now;
        entry
    } else {
        let spec = AddSpec {
            kind: BacklogKind::Ship,
            priority: promo.priority,
            touches: Vec::new(),
            substrate: Default::default(),
            source: format!("attn:{}", item.id),
            controller_id: None,
            // An attention item promoted to the backlog carries the task it names, when it names
            // one (#1001). A hand-raised item legitimately has none — it *is* the record.
            task: crate::attention::task_id_of(item)
                .unwrap_or_default()
                .to_string(),
        };
        BacklogEntry::new(repo, &subject, &spec, now)
    };

    DispatchOutcome {
        action: promo.action,
        entry,
        subject,
        backlog_id,
    }
}


