use super::{discover, ledger_entries, month_of, RunStatus};
use crate::resolver::{Config, Resolved};
use serde::Serialize;
use std::collections::BTreeMap;

/// One agent's line in the [`CostReport`].
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentCost {
    pub agent: String,
    pub runs: u64,
    pub skipped: u64,
    pub tokens: u64,
    pub wall_ms: u64,
}

/// `bn staff cost` rolled up: per-agent spend for a month.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CostReport {
    /// The `YYYY-MM` month this rolls up.
    pub month: String,
    pub agents: Vec<AgentCost>,
    pub total_tokens: u64,
    pub total_wall_ms: u64,
}

/// Roll the ledger up for `month` (`YYYY-MM`). Rows sort by tokens descending, then by name.
pub fn cost_report(
    cfg: &Config,
    ctx: Option<&Resolved>,
    month: &str,
) -> Result<CostReport, crate::agents::AgentCatalogError> {
    let roster = discover(cfg, ctx)?;

    #[derive(Default)]
    struct Agg {
        runs: u64,
        skipped: u64,
        tokens: u64,
        wall_ms: u64,
    }

    let mut map: BTreeMap<String, Agg> = BTreeMap::new();
    for agent in &roster {
        map.entry(agent.name.clone()).or_default();
    }
    for entry in ledger_entries() {
        if month_of(&entry.date) != month {
            continue;
        }
        let aggregate = map.entry(entry.agent.clone()).or_default();
        if entry.status == RunStatus::Skipped {
            aggregate.skipped = aggregate.skipped.saturating_add(1);
        } else {
            aggregate.runs = aggregate.runs.saturating_add(1);
        }
        aggregate.tokens = aggregate.tokens.saturating_add(entry.tokens);
        aggregate.wall_ms = aggregate.wall_ms.saturating_add(entry.wall_ms);
    }

    let mut agents: Vec<AgentCost> = map
        .into_iter()
        .map(|(agent, aggregate)| AgentCost {
            agent,
            runs: aggregate.runs,
            skipped: aggregate.skipped,
            tokens: aggregate.tokens,
            wall_ms: aggregate.wall_ms,
        })
        .collect();
    agents.sort_by(|left, right| {
        right
            .tokens
            .cmp(&left.tokens)
            .then(left.agent.cmp(&right.agent))
    });
    let total_tokens = agents.iter().map(|agent| agent.tokens).sum();
    let total_wall_ms = agents.iter().map(|agent| agent.wall_ms).sum();
    Ok(CostReport {
        month: month.to_string(),
        agents,
        total_tokens,
        total_wall_ms,
    })
}
