//! `bn` Debt Sentinel wiring (#259): the native `sentinel` cron job and the `bn staff gather
//! sentinel` packet builder. All the deterministic logic + tests live in
//! `bn_core::staff::sentinel`; this file is the thin I/O edge — resolve config, run the offline
//! collectors, and hand off to the core patrol / gather.
use crate::*;
use bn_core::runner::RealRunner;
use bn_core::staff::sentinel;

/// Resolve the patrol tunables from the opt-in job config (`[sentinel]`, defaulting the allowlist
/// to `[night] repos`).
fn patrol_config(jc: &resolver::JobConfig) -> sentinel::PatrolConfig {
    sentinel::PatrolConfig {
        repos: jc.sentinel_repos.clone(),
        todo_age_days: jc.sentinel_todo_age_days,
    }
}

/// `config/crons.toml` job `sentinel` — the 22:30 estate patrol. Collects the estate offline, lands
/// findings via `attn ingest --replace`, promotes the ≤ 2 best picks for the night shift, and
/// writes the trend card. A no-op when nothing is allowlisted (the opt-in boundary).
pub(crate) fn job_sentinel(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let jc = resolver::JobConfig::load();
    let pcfg = patrol_config(&jc);
    if pcfg.repos.is_empty() {
        println!(
            "sentinel: no repos allowlisted ([sentinel] repos / [night] repos) — patrol is a no-op"
        );
        return 0;
    }
    let today = today();
    let inputs = sentinel::collect_inputs(cfg, &RealRunner, &pcfg, &today);
    match sentinel::patrol(cfg, &inputs, &pcfg, &today) {
        Ok(r) => {
            println!(
                "sentinel: {} — {} surfaced ({} raised, {} resolved), {} promoted",
                r.headline,
                r.findings.len(),
                r.raised,
                r.resolved,
                r.promotions.len()
            );
            0
        }
        Err(e) => {
            eprintln!("sentinel: patrol failed: {e}");
            1
        }
    }
}

/// `bn staff gather sentinel` — the side-effect-free packet the triage agent (and `bn staff plan`)
/// reads: the surfaced findings, the trend, and the deterministic promotion picks. Lands nothing.
pub(crate) fn emit_sentinel_gather(cfg: &Config) -> i32 {
    let jc = resolver::JobConfig::load();
    let pcfg = patrol_config(&jc);
    let today = today();
    let inputs = sentinel::collect_inputs(cfg, &RealRunner, &pcfg, &today);
    println!("{}", sentinel::gather_packet(&inputs, &pcfg, &today));
    0
}
