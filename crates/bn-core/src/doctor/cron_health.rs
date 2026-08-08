//! The cron lane of `bn doctor` — which failing cron jobs still count as broken.
//!
//! Split out of `doctor.rs` under the 1000-line ratchet, and it is the natural seam: every other
//! finding asks "is this note/item stale?", while this one has to answer a question none of them
//! do — whether the failing name still *exists*. Getting that wrong is what kept a retired job
//! alerting forever, so it earns its own file and its own tests.
use super::Finding;
use crate::crons::LiveJobs;
use crate::events;
use crate::resolver::Config;
use serde::{Deserialize, Serialize};

/// A cron job whose latest run failed — what's broken on the machine right now.
#[derive(Debug, Serialize, Deserialize)]
pub struct CronFinding {
    pub job: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    /// Total failed runs on record for this job (context for how chronic it is).
    pub failures: usize,
    /// Epoch seconds of the (failed) latest run.
    pub last_run: i64,
}

/// Cron jobs whose latest run failed, newest-failed first (the event log already sorts
/// `cron_status` most-recent-run first), minus the names that no longer exist
/// ([`LiveJobs::is_retired`]).
///
/// The retirement filter is not optional here: this is what `bn doctor` and `bn next` raise from,
/// and reading `cron_status` straight left a retired job's frozen failures alerting forever —
/// `bn cron retire task-improve` printed its tombstone while `bn next` kept the BREAKING row.
/// Unlike [`crate::crons::alerting_crons`] this keeps the one-failure blips: `bn doctor` is the
/// "what is red right now" view, so a cron's *first* failure must still show up.
pub(super) fn cron_findings(cfg: &Config) -> Vec<Finding> {
    let live = LiveJobs::discover(cfg, None);
    let now = crate::util::now_epoch_secs();
    events::cron_status()
        .into_iter()
        .filter(|j| raises_here(j, &live, now))
        .map(|j| {
            Finding::Cron(CronFinding {
                job: j.job,
                repo: j.repo,
                branch: j.branch,
                failures: j.failures,
                last_run: j.last_run,
            })
        })
        .collect()
}

/// Whether one cron's rolled-up status belongs on the attention list. Split out of
/// [`cron_findings`] as the pure seam: the fold around it reads the process-global event log, so
/// this is the half a test can pin without standing one up.
fn raises_here(job: &events::CronJobStatus, live: &LiveJobs, now: i64) -> bool {
    !job.ok && !live.is_retired(job, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn failing(job: &str, last_run: i64) -> events::CronJobStatus {
        events::CronJobStatus {
            job: job.to_string(),
            scope: "global".to_string(),
            repo: None,
            branch: None,
            last_run,
            last_exit: 1,
            last_duration_ms: 3,
            ok: false,
            runs: 7,
            failures: 7,
        }
    }

    /// The regression: `bn cron retire task-improve` wrote its tombstone and still `bn next` led
    /// with `BREAKING — fix cron task-improve · 7 failure(s) on record`, because this attention
    /// path read `cron_status` directly and never asked whether the name still existed.
    #[test]
    fn a_retired_cron_stops_raising_on_the_attention_list() {
        let live = LiveJobs::from_names(["task-enhance"]).with_tombstone("task-improve", NOW - 60);
        assert!(!raises_here(
            &failing("task-improve", NOW - 120),
            &live,
            NOW
        ));
    }

    /// The other direction, so the filter can't be widened into a mute: a job bn still resolves
    /// keeps raising however old its failures are.
    #[test]
    fn a_live_cron_still_raises() {
        let live = LiveJobs::from_names(["scout"]);
        assert!(raises_here(&failing("scout", NOW - 120), &live, NOW));
    }

    /// `bn doctor` is the "what is red right now" view, not the chronic-failure watchdog — unlike
    /// [`crate::crons::alerting_crons`] it must show a cron's *first* failure rather than waiting
    /// for `ALERT_MIN_FAILURES`.
    #[test]
    fn a_single_failure_still_raises_here() {
        let live = LiveJobs::from_names(["scout"]);
        let mut blip = failing("scout", NOW - 60);
        blip.runs = 1;
        blip.failures = 1;
        assert!(raises_here(&blip, &live, NOW));
    }

    /// A tombstone is a claim about the past: a run after it means the name is live again, and the
    /// attention list has to notice without an un-retire step.
    #[test]
    fn running_again_after_a_tombstone_raises_again() {
        let live =
            LiveJobs::from_names::<[&str; 0], &str>([]).with_tombstone("task-improve", NOW - 600);
        assert!(raises_here(&failing("task-improve", NOW - 60), &live, NOW));
    }
}
use super::{CronFinding, Finding};
use crate::events;
use crate::resolver::Config;

/// Cron jobs whose latest run failed, newest-failed first (the event log already sorts
/// `cron_status` most-recent-run first).
///
/// Routed through [`crate::crons::alerting_crons`] so a retired name — renamed, deleted, or
/// tombstoned by `bn cron retire` — stops reading as a live outage here too. `bn doctor` raises
/// cron health at URGENT, so skipping that filter left a job nothing on the machine could even
/// resolve pinned to the top of `bn next` forever.
///
/// Threshold of 1 rather than [`crate::crons::ALERT_MIN_FAILURES`]: this is the local
/// what's-broken-right-now list, deliberately more sensitive than the periodic watchdog that
/// escalates into the attention store, so a first failure is still worth showing the operator.
///
/// Discovery runs with no branch context — `doctor`'s repo argument carries a root and a name but
/// no branch, so there is no [`crate::resolver::Resolved`] to hand it — which means only the global
/// tier backs the live set. A repo-tier cron is therefore judged by the tombstone clause and the
/// 14-day recency horizon alone, the same fallback [`crate::crons::LiveJobs::is_retired`] already
/// relies on for natively-implemented jobs.
pub(super) fn cron_findings(cfg: &Config) -> Vec<Finding> {
    let runs = events::cron_status();
    let live = crate::crons::LiveJobs::discover(cfg, None);
    crate::crons::alerting_crons(&runs, 1, &live, crate::util::now_epoch_secs())
        .into_iter()
        .map(|j| {
            Finding::Cron(CronFinding {
                job: j.job.clone(),
                repo: j.repo.clone(),
                branch: j.branch.clone(),
                failures: j.failures,
                last_run: j.last_run,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    /// The regression this module exists for: `bn doctor` raises cron health at URGENT, so it has
    /// to answer the does-this-still-exist question like every other cron watchdog. It once
    /// filtered on `!ok` alone, which pinned `task-improve` — renamed to `task-enhance`, resolvable
    /// by nothing on the machine — to the top of `bn next` as BREAKING with no way to clear it.
    #[test]
    fn a_tombstoned_job_stops_being_a_doctor_finding_while_a_live_one_keeps_alerting() {
        let root = tempfile::tempdir().expect("create test notes directory");
        test_support::with_testhost_state_and_notes_dir(root.path(), || {
            let cfg = test_support::config(root.path());
            // Ran just now, so the 14-day recency clause cannot retire either job — this test is
            // about the tombstone clause, and a 1970 timestamp would retire both before it looked.
            let now = crate::util::now_epoch_secs();
            let fail = |job: &str| {
                events::log_cron_run(
                    job,
                    "global",
                    None,
                    None,
                    now,
                    4,
                    127,
                    "bn: command not found",
                )
                .expect("record a failing cron run")
            };
            fail("renamed-away");
            fail("still-broken");

            let jobs = |findings: Vec<Finding>| {
                findings
                    .into_iter()
                    .filter_map(|f| match f {
                        Finding::Cron(c) => Some(c.job),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            };

            // Both are failing and neither resolves, but nothing is tombstoned yet and both ran
            // just now — the recency clause cannot retire them, so both still alert.
            let before = jobs(cron_findings(&cfg));
            assert!(before.contains(&"renamed-away".to_string()), "{before:?}");
            assert!(before.contains(&"still-broken".to_string()), "{before:?}");

            events::retire_cron("renamed-away", Some("renamed")).expect("write the tombstone");

            let after = jobs(cron_findings(&cfg));
            assert!(!after.contains(&"renamed-away".to_string()), "{after:?}");
            assert!(after.contains(&"still-broken".to_string()), "{after:?}");
        });
    }
}
