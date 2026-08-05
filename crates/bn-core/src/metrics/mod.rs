//! Productivity metrics — pure aggregation over the event log. The PR observer feeds
//! pr_opened/pr_merged/pr_closed events; this turns them into a completion rate, cycle
//! times, and throughput. No collection here and no network: the script gathers, the log
//! records, this just counts. The north star — read, don't re-fetch.
//!
//! Split into focused submodules; the public types and functions are re-exported
//! so callers keep using `metrics::compute`, `metrics::scorecard`, … as before.

mod agent;
mod churn;
mod flow;
mod scorecard;
mod snapshot;
mod visibility;
mod wip;

pub use agent::*;
pub use churn::*;
pub use flow::*;
pub use scorecard::*;
pub use snapshot::*;
pub use visibility::*;
pub use wip::*;

#[cfg(test)]
use crate::events::Event;
#[cfg(test)]
use serde_json::{Map, Value};

/// Build an in-memory event without touching disk — the pure helpers take a slice.
#[cfg(test)]
fn ev(kind: &str, ts: i64, fields: &[(&str, Value)]) -> Event {
    let mut m = Map::new();
    for (k, v) in fields {
        m.insert((*k).to_string(), v.clone());
    }
    Event {
        kind: kind.to_string(),
        ts,
        fields: m,
    }
}
