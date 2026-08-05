//! Canonical ranking semantics shared by bn's work-selection surfaces.

use std::cmp::Ordering;

pub const URGENT_SEVERITY: &str = "urgent";
pub const WARN_SEVERITY: &str = "warn";
pub const INFO_SEVERITY: &str = "info";

/// A work signal's base rank and whether it may interrupt focused work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rank {
    priority: u8,
    urgent: bool,
}

impl Rank {
    const fn new(priority: u8, urgent: bool) -> Self {
        Self { priority, urgent }
    }

    /// Numeric priority used by existing serialized surface models.
    pub const fn priority(self) -> u8 {
        self.priority
    }

    /// Whether this signal represents work that needs immediate attention.
    pub const fn is_urgent(self) -> bool {
        self.urgent
    }

    /// Compare two base ranks with the most pressing signal first.
    pub fn compare(self, other: Self) -> Ordering {
        compare_priority(self.priority, other.priority)
    }
}

/// A flow-control guard that leads the list without interrupting focused work.
pub const GUARD: Rank = Rank::new(4, false);
/// Actively broken work or an explicitly urgent attention item.
pub const URGENT: Rank = Rank::new(3, true);
/// Active unfinished work waiting on an external dependency.
pub const BLOCKED: Rank = Rank::new(3, true);
/// Work that warrants attention but does not interrupt focused work.
pub const WARN: Rank = Rank::new(2, false);
/// Routine backlog or informational work.
pub const INFO: Rank = Rank::new(1, false);

/// Map the attention severity vocabulary to its canonical rank.
pub fn from_severity(severity: &str) -> Rank {
    match severity {
        URGENT_SEVERITY => URGENT,
        WARN_SEVERITY => WARN,
        _ => INFO,
    }
}

/// Compare serialized numeric priorities with the most pressing signal first.
pub fn compare_priority(left: u8, right: u8) -> Ordering {
    right.cmp(&left)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_mapping_defines_one_priority_scale() {
        assert_eq!(from_severity("urgent"), URGENT);
        assert_eq!(from_severity("warn"), WARN);
        assert_eq!(from_severity("info"), INFO);
        assert_eq!(from_severity("unknown"), INFO);
    }

    #[test]
    fn blocked_work_is_canonically_urgent() {
        assert_eq!(BLOCKED.priority(), URGENT.priority());
        assert!(BLOCKED.is_urgent());
    }

    #[test]
    fn base_comparator_orders_most_pressing_first() {
        let mut ranks = [INFO, BLOCKED, GUARD, WARN];
        ranks.sort_by(|a, b| a.compare(*b));
        assert_eq!(ranks, [GUARD, BLOCKED, WARN, INFO]);
    }

    #[test]
    fn guard_leads_without_becoming_an_interrupt() {
        assert_eq!(GUARD.priority(), 4);
        assert!(!GUARD.is_urgent());
    }
}
