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

/// Live signals used by [`classify_stuck_lane`]. The CLI gathers these from the existing
/// orchestrate tmux/worktree observation and [`agent_progress_for_cwd`] probe.
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

struct StuckPanePattern {
    marker: &'static str,
    reason: &'static str,
    suggested_unblock: &'static str,
}

/// Harness startup text that means MCP discovery itself is wedged. The table form keeps the set
/// extensible without spreading string checks across the supervisor.
const MCP_LOADING_PATTERNS: &[StuckPanePattern] = &[
    StuckPanePattern {
        marker: "loading… mcp servers",
        reason: "the harness is stuck loading MCP servers",
        suggested_unblock: "inspect or disable the failing MCP server, then resume the lane",
    },
    StuckPanePattern {
        marker: "loading... mcp servers",
        reason: "the harness is stuck loading MCP servers",
        suggested_unblock: "inspect or disable the failing MCP server, then resume the lane",
    },
    StuckPanePattern {
        marker: "loading mcp servers",
        reason: "the harness is stuck loading MCP servers",
        suggested_unblock: "inspect or disable the failing MCP server, then resume the lane",
    },
    StuckPanePattern {
        marker: "loading… servers",
        reason: "the harness is stuck loading MCP servers",
        suggested_unblock: "inspect or disable the failing MCP server, then resume the lane",
    },
];

const INTERACTIVE_BLOCK_PATTERNS: &[StuckPanePattern] = &[
    StuckPanePattern {
        marker: "yes, and add these directories to the allowed list",
        reason: "the harness is waiting for directory trust permission",
        suggested_unblock: "approve the allowed-directory option and resume the lane",
    },
    StuckPanePattern {
        marker: "yes, don't ask again",
        reason: "the harness is waiting for persistent permission",
        suggested_unblock: "approve the trusted permission option and resume the lane",
    },
    StuckPanePattern {
        marker: "yes, don’t ask again",
        reason: "the harness is waiting for persistent permission",
        suggested_unblock: "approve the trusted permission option and resume the lane",
    },
];

fn stuck_pane_pattern<'a>(
    tail: &[String],
    patterns: &'a [StuckPanePattern],
) -> Option<&'a StuckPanePattern> {
    patterns.iter().find(|pattern| {
        tail.iter()
            .any(|line| line.to_ascii_lowercase().contains(pattern.marker))
    })
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

/// Classify the controller-actionable stuck subset of an in-flight lane. Pane semantics are
/// delegated to the existing orchestrate classifier; this layer only adds the MCP startup pattern
/// and the durable no-change threshold.
pub fn classify_stuck_lane(obs: &StuckLaneObservation<'_>) -> Option<StuckLane> {
    use crate::orchestrate::{classify, PaneObservation, PaneState};

    let pane = PaneObservation {
        tail: obs.tail,
        has_window: obs.has_window,
        plane_a: obs.plane_a,
        has_wip: obs.has_wip,
    };
    let state = classify(&pane);

    if obs.has_process && obs.unchanged_secs > 0 {
        if let Some(pattern) = stuck_pane_pattern(obs.tail, MCP_LOADING_PATTERNS) {
            return Some(StuckLane {
                kind: StuckLaneKind::McpLoading,
                reason: pattern.reason.to_string(),
                suggested_unblock: pattern.suggested_unblock.to_string(),
            });
        }
        if let Some(pattern) = stuck_pane_pattern(obs.tail, INTERACTIVE_BLOCK_PATTERNS) {
            return Some(StuckLane {
                kind: StuckLaneKind::InteractivePrompt,
                reason: pattern.reason.to_string(),
                suggested_unblock: pattern.suggested_unblock.to_string(),
            });
        }
        let prompt = match state {
            PaneState::TrustMenu => Some((
                "the harness is waiting on a trust menu",
                "approve the trusted worktree or restart the lane with pre-trust enabled",
            )),
            PaneState::CommandApproval => Some((
                "the harness is waiting on command permission",
                "choose the session-scoped allow option and resume the lane",
            )),
            PaneState::QuestionMenu => Some((
                "the harness is waiting on an interactive choice",
                "answer the prompt in the lane and let the agent continue",
            )),
            _ => None,
        };
        if let Some((reason, suggested_unblock)) = prompt {
            return Some(StuckLane {
                kind: StuckLaneKind::InteractivePrompt,
                reason: reason.to_string(),
                suggested_unblock: suggested_unblock.to_string(),
            });
        }
    }

    if state == PaneState::Working && obs.unchanged_secs >= obs.idle_threshold_secs.max(1) {
        return Some(StuckLane {
            kind: StuckLaneKind::WorkingIdle,
            reason: "the lane still reports working beyond the configured idle threshold with no pane or agent activity".to_string(),
            suggested_unblock: "inspect the pane and nudge or restart the stalled agent".to_string(),
        });
    }

    (state == PaneState::IdleWithWip).then(|| StuckLane {
        kind: StuckLaneKind::IdleWithWip,
        reason: "the agent turn is idle while the worktree has uncommitted or unmerged work"
            .to_string(),
        suggested_unblock: "resume or recover the agent so it can finish and commit the work"
            .to_string(),
    })
}
