use super::{AttnFinding, Finding, StaffFinding};
use crate::resolver::Config;

/// Staff agents whose most recent scheduled run is missed (#273) — a dead brief with no ledger row
/// at or after its slot. Read-only: it joins the declared roster schedule with the run ledger and
/// raises nothing, so it clears the moment a run of any status lands.
pub(super) fn staff_findings(cfg: &Config) -> Vec<Finding> {
    let agents = match crate::staff::discover(cfg, None) {
        Ok(agents) => agents,
        Err(error) => {
            return vec![Finding::Attn(AttnFinding {
                id: "agent-catalog-invalid".to_string(),
                title: "Agent catalog is invalid".to_string(),
                severity: "urgent".to_string(),
                source: "agent-catalog".to_string(),
                repo: None,
                branch: None,
                link: None,
                detail: Some(error.to_string()),
            })]
        }
    };
    if agents.is_empty() {
        return Vec::new();
    }

    let last = crate::staff::last_runs();
    let now = crate::util::now_epoch_secs();
    crate::staff_schedule::missed_runs(
        &agents,
        &last,
        now,
        crate::staff_schedule::DEFAULT_MISS_GRACE_SECS,
    )
    .into_iter()
    .map(|missed| {
        Finding::Staff(StaffFinding {
            agent: missed.agent,
            on_calendar: missed.on_calendar,
            scheduled: missed.scheduled,
            last_run: missed.last_run,
            overdue_secs: now.saturating_sub(missed.scheduled),
        })
    })
    .collect()
}
