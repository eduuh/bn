//! Stuck-lane classification and the pane activity fingerprint.
//!
//! Split out of `supervise.rs` (the live-fleet view) so both stay under the 1000-line ratchet.
//! This half answers "is this in-flight lane wedged, and on what?" — the controller-actionable
//! subset of agent health, kept separate from the broader `AgentHealth` idle thresholds and from
//! the orchestrator's own recovery states.
//!
//! Re-exported by [`crate::agent`], so `agent::StuckLane`, `agent::classify_stuck_lane` and
//! `agent::pane_activity_fingerprint` keep resolving unchanged.

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

/// A controller-actionable stuck state, persisted on an orchestrate queue item so status and event
/// emission share one source of truth.
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

/// Live signals used by `classify_stuck_lane`. The CLI gathers these from the existing
/// orchestrate tmux/worktree observation and `agent_progress_for_cwd` probe.
#[derive(Debug, Clone, Copy)]
pub struct StuckLaneObservation<'a> {
    pub tail: &'a [String],
    pub has_window: bool,
    pub has_process: bool,
    pub plane_a: &'a str,
    pub has_wip: bool,
    /// Seconds since either the pane fingerprint or harness transcript last changed.
    pub unchanged_secs: i64,
    pub idle_threshold_secs: i64,
}

/// Stable fingerprint for the pane/activity clock persisted by the orchestrator. Plane-A is part
/// of the key so a lifecycle transition counts as activity even when the visible tail is unchanged.
pub fn pane_activity_fingerprint(tail: &[String], plane_a: &str) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in plane_a
        .as_bytes()
        .iter()
        .copied()
        .chain(std::iter::once(0))
        .chain(tail.iter().flat_map(|line| {
            line.as_bytes()
                .iter()
                .copied()
                .chain(std::iter::once(b'\n'))
        }))
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}
