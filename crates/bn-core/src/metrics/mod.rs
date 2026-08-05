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
