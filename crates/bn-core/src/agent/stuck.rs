//! The stuck-lane shape: "is this in-flight lane wedged, and on what?".
//!
//! Split out of `supervise.rs` (the live-fleet view) so both stay under the 1000-line ratchet.
//! This is the controller-actionable subset of agent health, kept separate from the broader
//! `AgentHealth` idle thresholds and from the orchestrator's own recovery states.
//!
//! Re-exported by [`crate::agent`], so `agent::StuckLane` keeps resolving unchanged.

use serde::{Deserialize, Serialize};

/// Default seconds without pane or transcript activity before a lane still reporting `working`
/// is considered stuck.
pub const DEFAULT_STUCK_LANE_IDLE_SECS: i64 = 600;

/// Why an in-flight worktree lane is stuck. Kept separate from the broader agent health and
/// orchestrator recovery states: this is the controller-actionable subset that should wake an
/// owning controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StuckLaneKind {
    InteractivePrompt,
    McpLoading,
    WorkingIdle,
    IdleWithWip,
}

impl StuckLaneKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::InteractivePrompt => "interactive-prompt",
            Self::McpLoading => "mcp-loading",
            Self::WorkingIdle => "working-idle",
            Self::IdleWithWip => "idle-with-wip",
        }
    }
}

/// A controller-actionable stuck state, persisted on a lane's [`crate::orchestrate::QueueItem`] in
/// its run file so status and event emission share one source of truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StuckLane {
    pub kind: StuckLaneKind,
    pub reason: String,
    pub suggested_unblock: String,
}

impl StuckLane {
    pub fn key(&self) -> String {
        match self.kind {
            StuckLaneKind::InteractivePrompt => {
                format!("{}:{}", self.kind.label(), self.reason)
            }
            _ => self.kind.label().to_string(),
        }
    }

    pub fn summary(&self, lane: &str) -> String {
        format!(
            "{lane} is blocked: {}; suggested unblock: {}",
            self.reason, self.suggested_unblock
        )
    }
}

#[cfg(test)]
#[path = "stuck_tests.rs"]
mod tests;
