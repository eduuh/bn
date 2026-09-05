//! What a staff run's outcome means to the process that ran it.
//!
//! Split from `staff.rs` so the rule lives beside its own tests rather than in the CLI, where it
//! was a private helper that only `bn staff run` could reach — and where the file-size ratchet had
//! no room left for it.

use super::{RunReport, RunStatus};

impl RunReport {
    /// The process exit code this run should produce.
    ///
    /// `Skipped` and `Off` are deliberate no-ops, not failures: an agent switched off, or one shed
    /// under backpressure, did exactly what it was told to. Exiting 1 for those made every
    /// scheduled run of a disabled agent look like a broken one on the health board.
    pub fn exit_code(&self) -> i32 {
        if self.ok || self.status == RunStatus::Skipped || self.status == RunStatus::Off {
            0
        } else {
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::staff::RouteSummary;

    fn report(status: RunStatus, ok: bool) -> RunReport {
        RunReport {
            agent: "demo".into(),
            ok,
            status,
            error: None,
            card_path: None,
            routed: RouteSummary::default(),
            rerun: false,
            tokens: 0,
            wall_ms: 0,
            note: None,
            silent: false,
        }
    }

    /// A deliberate off run is a clean no-op, not a failure to be surfaced as exit 1.
    #[test]
    fn a_deliberate_no_op_run_exits_successfully() {
        assert_eq!(report(RunStatus::Off, false).exit_code(), 0);
        assert_eq!(report(RunStatus::Skipped, false).exit_code(), 0);
    }

    #[test]
    fn a_real_failure_still_exits_nonzero() {
        assert_eq!(report(RunStatus::Fail, false).exit_code(), 1);
    }

    #[test]
    fn a_successful_run_exits_zero() {
        assert_eq!(report(RunStatus::Ok, true).exit_code(), 0);
    }
}
