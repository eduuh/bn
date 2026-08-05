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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog::{Backlog, BacklogEntry, BacklogStatus};

    fn item(severity: &str, title: &str, link: Option<&str>) -> AttnItem {
        AttnItem {
            id: "a1".into(),
            source: "manual".into(),
            title: title.into(),
            severity: severity.into(),
            repo: Some("bn".into()),
            branch: None,
            link: link.map(|s| s.into()),
            key: None,
            detail: None,
            mode: None,
            prompt: None,
            task_type: None,
            worktree_path: None,
            uncommitted_files: Vec::new(),
            created: 100,
        }
    }

    #[test]
    fn no_twin_info_starts_new_at_floor() {
        let p = classify("info", None);
        assert_eq!(p.action, PromotionAction::StartNew);
        assert_eq!(p.priority, INFO_PRIORITY);
    }

    #[test]
    fn no_twin_warn_starts_new_at_warn_band() {
        let p = classify("warn", None);
        assert_eq!(p.action, PromotionAction::StartNew);
        assert_eq!(p.priority, WARN_PRIORITY);
    }

    #[test]
    fn no_twin_urgent_interrupts_on_priority_lane() {
        let p = classify("urgent", None);
        assert_eq!(p.action, PromotionAction::Interrupt);
        assert_eq!(p.priority, INTERRUPT_PRIORITY);
    }

    #[test]
    fn existing_twin_reprioritizes_never_duplicates() {
        // A warn re-flag of already-queued default-floor work bumps to the warn band.
        let p = classify("warn", Some(0));
        assert_eq!(p.action, PromotionAction::Reprioritize);
        assert_eq!(p.priority, WARN_PRIORITY);
    }

    #[test]
    fn reprioritize_is_always_a_strict_raise() {
        // Re-flagging at the same severity band still nudges the entry up (cur + 1 floor), so a
        // repeated flag keeps moving stalled work forward rather than being a no-op.
        let p = classify("warn", Some(WARN_PRIORITY));
        assert_eq!(p.action, PromotionAction::Reprioritize);
        assert_eq!(p.priority, WARN_PRIORITY + 1);
    }

    #[test]
    fn urgent_reprioritize_bumps_to_interrupt_band() {
        let p = classify("urgent", Some(50));
        assert_eq!(p.action, PromotionAction::Reprioritize);
        assert_eq!(p.priority, INTERRUPT_PRIORITY);
    }

    #[test]
    fn subject_prefers_issue_link_over_title() {
        let it = item(
            "warn",
            "prod is down",
            Some("https://github.com/eduuh/bn/issues/460"),
        );
        assert_eq!(
            dispatch_subject(&it),
            "https://github.com/eduuh/bn/issues/460"
        );
        // A non-issue link falls back to the title.
        let it = item(
            "warn",
            "prod is down",
            Some("https://example.com/dashboard"),
        );
        assert_eq!(dispatch_subject(&it), "prod is down");
        // No link → the title.
        let it = item("warn", "fix the flaky test", None);
        assert_eq!(dispatch_subject(&it), "fix the flaky test");
    }

    #[test]
    fn promote_start_new_builds_pending_ship_sourced_to_attn() {
        let bl = Backlog::new();
        let it = item("warn", "fix the flaky test", None);
        let out = promote_to_backlog(&it, &bl, "bn", 1000);
        assert_eq!(out.action, PromotionAction::StartNew);
        assert_eq!(out.entry.status, BacklogStatus::Pending);
        assert_eq!(out.entry.kind, BacklogKind::Ship);
        assert_eq!(out.entry.priority, WARN_PRIORITY);
        assert_eq!(out.entry.repo, "bn");
        assert_eq!(out.entry.source, "attn:a1");
        assert_eq!(out.backlog_id, out.entry.id);
    }

    #[test]
    fn promote_urgent_interrupts() {
        let bl = Backlog::new();
        let it = item("urgent", "database on fire", None);
        let out = promote_to_backlog(&it, &bl, "bn", 1000);
        assert_eq!(out.action, PromotionAction::Interrupt);
        assert_eq!(out.entry.priority, INTERRUPT_PRIORITY);
    }

    #[test]
    fn promote_existing_active_entry_reprioritizes_same_id() {
        let mut bl = Backlog::new();
        let spec = AddSpec {
            source: "triage".into(),
            ..Default::default()
        };
        bl.apply(BacklogEntry::new("bn", "fix the flaky test", &spec, 10));
        let it = item("warn", "fix the flaky test", None);
        let out = promote_to_backlog(&it, &bl, "bn", 2000);
        assert_eq!(out.action, PromotionAction::Reprioritize);
        assert_eq!(out.entry.priority, WARN_PRIORITY);
        // Same (repo, id) — a supersede, not a duplicate — and the original provenance is kept.
        assert_eq!(out.entry.id, derive_id("fix the flaky test"));
        assert_eq!(out.entry.source, "triage");
        assert_eq!(out.entry.updated_at, 2000);
    }

    #[test]
    fn promote_ignores_a_terminal_twin_and_starts_new() {
        // A done/failed entry for the same subject is not "active" — the item starts fresh.
        let mut bl = Backlog::new();
        let mut e = BacklogEntry::new("bn", "fix the flaky test", &AddSpec::default(), 10);
        e.status = BacklogStatus::Done;
        bl.apply(e);
        let it = item("warn", "fix the flaky test", None);
        let out = promote_to_backlog(&it, &bl, "bn", 2000);
        assert_eq!(out.action, PromotionAction::StartNew);
        assert_eq!(out.entry.status, BacklogStatus::Pending);
    }
}
