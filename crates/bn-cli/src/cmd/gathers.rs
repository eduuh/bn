//! The remaining built-in gathers, each behind its own command.
//!
//! These used to live in one `bn staff gather <agent>` match on agent names — the last place an
//! agent's *identity* decided which Rust ran. Each is now a command named for what it collects, so
//! a manifest's `gather:` line is the only thing that connects the two.
use crate::*;

/// `bn radar gather` — the outside world's week: upstream issues in subsystems this person works
/// in, dependency releases and advisories, tooling changelogs. The one gather here that touches
/// the network; each of its three collectors degrades independently and says so.
pub(crate) fn emit_radar_gather(cfg: &Config) -> i32 {
    println!(
        "{}",
        bn_core::scout::gather_packet_json(cfg, &bn_core::runner::RealRunner, &today())
    );
    0
}

/// `bn goals align --gather` — Command Center activity against the daily and quarter goals: what
/// the week actually went into, and what it was supposed to go into.
pub(crate) fn emit_goals_align_gather(cfg: &Config) -> i32 {
    println!(
        "{}",
        bn_core::goals_align::gather_packet_json(cfg, &today())
    );
    0
}

/// `bn standup --gather` — yesterday's completed work and today's plan, as the agent packet rather
/// than the rendered view `bn standup` prints.
pub(crate) fn emit_standup_gather(cfg: &Config) -> i32 {
    println!("{}", bn_core::standup::gather_packet_json(cfg, &today()));
    0
}

/// `bn staff demo-packet` — the echo-grade fixture the `demo` agent gathers, so the whole
/// gather → dispatch → validate → route path can be exercised without a real data source.
pub(crate) fn emit_demo_packet(cfg: &Config) -> i32 {
    println!("{}", bn_core::staff::demo_packet(cfg, &today()));
    0
}
