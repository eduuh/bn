//! How long a claimed row may stay open before it counts as overdue.

use std::fmt;

/// A row's urgency. Claiming sets `closure_required_at = claimed_at + window()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tier {
    Fast,
    #[default]
    Routine,
    Deep,
    Critical,
}

impl Tier {
    pub const ALL: [Tier; 4] = [Tier::Fast, Tier::Routine, Tier::Deep, Tier::Critical];

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Fast => "fast",
            Tier::Routine => "routine",
            Tier::Deep => "deep",
            Tier::Critical => "critical",
        }
    }

    pub fn parse(s: &str) -> Option<Tier> {
        Tier::ALL.into_iter().find(|t| t.as_str() == s.trim())
    }

    /// Seconds a claimed row of this tier may stay open. Critical is short because it matters,
    /// not because it is small: an hour without a word on critical work is already a problem.
    pub fn window_secs(self) -> i64 {
        match self {
            Tier::Fast => 30 * 60,
            Tier::Critical => 60 * 60,
            Tier::Routine => 4 * 60 * 60,
            Tier::Deep => 24 * 60 * 60,
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
