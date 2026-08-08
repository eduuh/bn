//! Taking a cron out of service — the two different things that can mean.
//!
//! `unregister` stops the *timer*: the job still exists and `bn run <job>` still works, it just no
//! longer fires on its own. `retire` tombstones the *name*: the job is gone for good and its old
//! failures are history, not an outage. Split out of `cron.rs` so both live next to each other,
//! since picking the wrong one is the easy mistake.
use crate::*;

pub(crate) fn emit_cron_unregister(job: &str, as_json: bool) {
    let Some(timer) = timers::registered().into_iter().find(|t| t.job == job) else {
        // No systemd timer. A launchd agent (macOS) unloads + deletes here directly.
        if let Some(agent) = launchd::registered().into_iter().find(|a| a.job == job) {
            match launchd::bootout(&agent.label) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&agent.path);
                    if as_json {
                        println!(
                            "{}",
                            Envelope::success(
                                "cron_unregister",
                                serde_json::json!({
                                    "job": job,
                                    "unregistered": true,
                                    "launchd_label": agent.label,
                                }),
                            )
                            .to_json()
                        );
                    } else {
                        println!(
                            "Unregistered {} ({job}) — unloaded and removed",
                            agent.label
                        );
                    }
                }
                Err(e) => fail(&format!("bn cron unregister: {e}")),
            }
            return;
        }
        // If a Windows task drives it, unregistering lives on the Windows side.
        let win = winsched::registered().into_iter().find(|w| w.job == job);
        if as_json {
            println!(
                "{}",
                Envelope::success(
                    "cron_unregister",
                    serde_json::json!({
                        "job": job,
                        "unregistered": false,
                        "windows_task": win.map(|w| w.task_name),
                    }),
                )
                .to_json()
            );
        } else if let Some(w) = win {
            println!(
                "'{job}' is scheduled via Windows Task Scheduler (task '{}').\n\
                 Disable it from Windows: schtasks.exe /Change /TN \"{}\" /DISABLE",
                w.task_name, w.task_name
            );
        } else {
            println!("No registered timer for '{job}' — `bn cron` lists what's scheduled");
        }
        return;
    };

    match timers::unregister(&timer.timer_unit) {
        Ok(()) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success(
                        "cron_unregister",
                        serde_json::json!({
                            "job": job,
                            "unregistered": true,
                            "timer_unit": timer.timer_unit,
                        }),
                    )
                    .to_json()
                );
            } else {
                println!(
                    "Unregistered {} ({job}) — stopped and disabled",
                    timer.timer_unit
                );
            }
        }
        Err(e) => fail(&format!("bn cron unregister: {e}")),
    }
}


/// `bn cron retire <job> [reason…]` — record that a cron name is gone on purpose.
///
/// Refuses a job that still resolves, because that is always the operator meaning something else:
/// a job with a manifest behind it *will* run again, so tombstoning it would only mute it until it
/// next did. Delete or rename the manifest first, then retire the name it left behind.
pub(crate) fn emit_cron_retire(
    cfg: &Config,
    ctx: Option<&Resolved>,
    job: &str,
    reason: &str,
    as_json: bool,
) {
    let live = bn_core::crons::LiveJobs::discover(cfg, ctx);
    if live.contains(job) {
        fail(&format!(
            "bn cron retire: '{job}' still resolves (a crons.toml entry, agent manifest, or \
             <job>.sh backs it) — remove that first, or use `bn cron unregister {job}` to stop \
             only its timer"
        ));
    }
    if let Err(e) = bn_core::events::retire_cron(job, Some(reason)) {
        fail(&format!("bn cron retire: {e}"));
    }
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "cron_retire",
                serde_json::json!({ "job": job, "retired": true, "reason": reason }),
            )
            .to_json()
        );
    } else {
        println!("Retired '{job}' — its recorded failures no longer read as an outage.");
        println!("Running it again revives it; there is nothing to undo.");
    }
}
