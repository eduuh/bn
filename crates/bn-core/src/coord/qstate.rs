//! A row's lifecycle state.

use std::fmt;

/// Where a row is. `pending` waits for its seat to claim it; `in-progress` is owned; `blocked` is
/// parked with a wake trigger. The rest are terminal: `handed-off` means the work continues on the
/// row its `next_id` points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QState {
    Pending,
    InProgress,
    Blocked,
    Done,
    Failed,
    Denied,
    Canceled,
    HandedOff,
}

impl QState {
    pub const ALL: [QState; 8] = [
        QState::Pending,
        QState::InProgress,
        QState::Blocked,
        QState::Done,
        QState::Failed,
        QState::Denied,
        QState::Canceled,
        QState::HandedOff,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            QState::Pending => "pending",
            QState::InProgress => "in-progress",
            QState::Blocked => "blocked",
            QState::Done => "done",
            QState::Failed => "failed",
            QState::Denied => "denied",
            QState::Canceled => "canceled",
            QState::HandedOff => "handed-off",
        }
    }

    pub fn parse(s: &str) -> Option<QState> {
        QState::ALL.into_iter().find(|q| q.as_str() == s.trim())
    }

    /// No verb moves a row out of a terminal state.
    pub fn is_terminal(self) -> bool {
        !matches!(self, QState::Pending | QState::InProgress | QState::Blocked)
    }
}

impl fmt::Display for QState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
