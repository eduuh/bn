//! `bn estate gather` — the offline estate sweep, printed as JSON.
//!
//! The thin I/O edge over [`bn_core::estate`]: resolve the scope from config, sweep, print. It
//! decides nothing and lands nothing, which is the point — whoever reads the packet (today the
//! `sentinel` agent's prompt) owns the judgment.
use crate::*;
use bn_core::estate;
use bn_core::runner::RealRunner;

/// Resolve the sweep scope from the opt-in job config (`[sentinel]`, defaulting the allowlist to
/// `[night] repos`).
fn scope(jc: &resolver::JobConfig) -> estate::Scope {
    estate::Scope {
        repos: jc.sentinel_repos.clone(),
        todo_age_days: jc.sentinel_todo_age_days,
    }
}

/// `bn estate gather [--carried-from <attention source>]`. Exits 0 on a printed packet, always —
/// an empty allowlist is a legitimately empty estate, not a failure, and the packet says so.
pub(crate) fn emit_estate_gather(cfg: &Config, carried_from: Option<&str>) -> i32 {
    let jc = resolver::JobConfig::load();
    let scope = scope(&jc);
    // The opt-in boundary. An empty allowlist means there is legitimately nothing to sweep, which
    // is not a failure and not worth a model's time — decline the run so a reader that is a staff
    // agent records itself as off rather than dispatching over an empty estate.
    if scope.is_empty() {
        println!(
            "{}",
            serde_json::json!({
                "skip": "no repos allowlisted ([sentinel] repos / [night] repos)"
            })
        );
        return 0;
    }
    println!(
        "{}",
        estate::gather_packet(cfg, &RealRunner, &scope, carried_from, &today())
    );
    0
}
