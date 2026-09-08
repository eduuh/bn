//! Attention workflow: the deterministic failing-cron watchdog (`cron-health`) and the periodic
//! LLM `attention-agent` that reviews recent cron findings + cron health and writes a ranked
//! attention list. Both raise into the same attention store via `ingest(..., replace=true)`, so a
//! recovered cron's / dropped item's alert is auto-acked on the next run.
use crate::*;
use bn_core::backlog::{self, AddSpec, BacklogKind, Recurrence};
use bn_core::attention::{ingest, IngestItem};
use bn_core::queue::kind::ExecSpec;


/// The source stamped on everything the watchdog mints — the attention row, the repair task, and
/// the queued repair agent. Shared because [`close_recovered_repairs`] matches backlog entries on
/// it: a literal that drifted on one side would silently stop the sweep finding its own work.
const REPAIR_SOURCE: &str = "cron-health";

/// A short text summary of the crons currently in a chronic-failure state — the health half of the
/// attention agent's packet, and the watchdog's raise set.
fn failing_cron_lines(cfg: &Config, ctx: Option<&Resolved>) -> Vec<String> {
    let runs = bn_core::events::cron_status();
    let live = bn_core::crons::LiveJobs::discover(cfg, ctx);
    bn_core::crons::alerting_crons(
        &runs,
        bn_core::crons::ALERT_MIN_FAILURES,
        &live,
        bn_core::util::now_epoch_secs(),
    )
    .iter()
        .map(|s| {
            format!(
                "cron '{}' is failing — {}/{} runs failed (last exit {})",
                s.job, s.failures, s.runs, s.last_exit
            )
        })
        .collect()
}

fn repair_prompt(status: &bn_core::events::CronJobStatus, output_tail: Option<&str>) -> String {
    let mut prompt = format!(
        "Fix failing cron job '{}'. It has failed {}/{} recorded runs and the latest exit code was {}. \
Reproduce it with `bn run {}`, inspect the cron/job implementation, and land a fix so the next run is green.",
        status.job, status.failures, status.runs, status.last_exit, status.job
    );
    if let Some(tail) = output_tail.map(str::trim).filter(|tail| !tail.is_empty()) {
        prompt.push_str("\n\nLatest captured output:\n");
        prompt.push_str(tail);
    }
    prompt
}

fn looks_failure_shaped(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    [
        "error",
        "failed",
        "failure",
        "panic",
        "could not",
        "refusing",
        "refused",
        "not found",
        "no such",
        "no job",
        "invalid",
        "denied",
        "timed out",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn repair_task_line(output_tail: &str) -> Option<&str> {
    let mut first = None;
    for line in output_tail.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if first.is_none() {
            first = Some(line);
        }
        if looks_failure_shaped(line) {
            return Some(line);
        }
    }
    first
}

fn repair_task_text(status: &bn_core::events::CronJobStatus, output_tail: Option<&str>) -> String {
    let mut text = format!(
        "Fix failing cron {} repo:bn cron:{} failures:{} exit:{}",
        status.job, status.job, status.failures, status.last_exit
    );
    if let Some(tail) = output_tail.map(str::trim).filter(|tail| !tail.is_empty()) {
        let one_line = repair_task_line(tail).unwrap_or(tail).trim();
        if !one_line.is_empty() {
            text.push_str(" error:");
            text.push_str(&bn_core::todo::neutralize_meta(one_line));
        }
    }
    text
}

fn ensure_repair_task(
    cfg: &Config,
    status: &bn_core::events::CronJobStatus,
    output_tail: Option<&str>,
    today: &str,
) -> Option<String> {
    let path = cfg.tasks_file();
    let _lock = bn_core::todo::lock(&path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = bn_core::todo::load(&path).ok()?;
    let key = format!("cron-repair:{}", status.job);
    if let Some(existing) = file
        .tasks()
        .find(|task| task.is_open(today) && task.meta("key") == Some(key.as_str()))
        .and_then(|task| task.id())
    {
        return Some(existing.to_string());
    }

    let id = file.next_id();
    let mut task = bn_core::todo::Task::new_todo(
        &id,
        &repair_task_text(status, output_tail),
        Some('A'),
        "",
        REPAIR_SOURCE,
        today,
    );
    task.set_meta_raw("repo", "bn");
    task.set_meta_raw("key", &key);
    task.set_meta_raw("cron", &status.job);
    file.push_task(task);
    bn_core::todo::save(&path, &file).ok()?;
    Some(id)
}

/// Fold out the repair work for crons that are no longer alerting — the symmetric other half of
/// [`ensure_repair_task`], and the reason a recovered cron stops costing anything.
///
/// The attention row auto-acks itself, because [`ingest`] runs with `replace=true`. The repair
/// *task* and the queued repair *agent* have no such mechanism: without this, `scout` recovering
/// leaves `Fix failing cron scout` open in the task list forever, and any still-pending repair
/// agent goes on to burn a worktree reproducing a job that is already green.
///
/// `alerting` is the raise set this pass computed, so "recovered" and "retired" collapse into one
/// condition: a `cron-repair:<job>` task whose job the watchdog is no longer raising has nothing
/// left to do. The task is [`cancel`](bn_core::todo::Task::cancel)led rather than completed — no
/// one did the work, the cron stopped failing — which keeps it out of completion metrics. Only
/// `Pending` queue entries are withdrawn; [`backlog::cancel`] leaves a leased agent alone, since a
/// row flipped to cancelled under a live worktree would only misdescribe what is still running.
///
/// The withdrawal set is derived from the *backlog*, not from the tasks cancelled on this pass,
/// and that distinction is the whole bug it fixes. Keying it to this pass's cancellations made the
/// sweep one-shot: an entry that happened to be `Leased` at that moment was skipped, and because
/// its task was cancelled in the same breath it never appeared in a later pass's set either. The
/// entry then outlived every mechanism meant to reap it — released back to `Pending`, dispatched,
/// leased, released, forever. `task-improve` rode that loop through four dispatches after its cron
/// was tombstoned. Re-deriving from the store each pass makes the sweep idempotent, so an entry
/// caught mid-lease is simply withdrawn the next time it comes to rest.
///
/// Returns `(tasks_closed, agents_withdrawn)`.
fn close_recovered_repairs(
    cfg: &Config,
    alerting: &[&bn_core::events::CronJobStatus],
    today: &str,
) -> (usize, usize) {
    let raising: std::collections::HashSet<&str> =
        alerting.iter().map(|status| status.job.as_str()).collect();
    let path = cfg.tasks_file();
    // Cancelled repair tasks stay in the map: an entry withdrawn on a later pass still has to
    // resolve its job, and by then its task is exactly the one an earlier pass closed.
    let (closed, repair_jobs) = {
        let _lock = bn_core::todo::lock(&path);
        let Ok(mut file) = bn_core::todo::load(&path) else {
            return (0, 0);
        };
        let stale: Vec<String> = file
            .tasks()
            .filter(|task| task.is_open(today))
            .filter(|task| {
                task.meta("key")
                    .and_then(|key| key.strip_prefix("cron-repair:"))
                    .is_some_and(|job| !raising.contains(job))
            })
            .filter_map(|task| task.id().map(str::to_string))
            .collect();
        for id in &stale {
            file.cancel(id, today, Some("cron-recovered"));
        }
        if !stale.is_empty() && bn_core::todo::save(&path, &file).is_err() {
            return (0, 0);
        }
        let jobs: std::collections::HashMap<String, String> = file
            .tasks()
            .filter_map(|task| {
                let job = task.meta("key")?.strip_prefix("cron-repair:")?;
                Some((task.id()?.to_string(), job.to_string()))
            })
            .collect();
        (stale.len(), jobs)
    };

    let backlog_path = backlog::backlog_path();
    let now = bn_core::util::now_epoch_secs();
    let withdrawable: Vec<(String, String)> = backlog::load(&backlog_path)
        .entries()
        .iter()
        .filter(|entry| entry.status == backlog::BacklogStatus::Pending)
        .filter(|entry| entry.source == REPAIR_SOURCE)
        .filter(|entry| {
            repair_jobs
                .get(&entry.task)
                .is_some_and(|job| !raising.contains(job.as_str()))
        })
        .map(|entry| (entry.repo.clone(), entry.id.clone()))
        .collect();
    let withdrawn = withdrawable
        .iter()
        .filter(|(repo, id)| backlog::cancel(&backlog_path, repo, id, now).unwrap_or(false))
        .count();
    (closed, withdrawn)
}

fn enqueue_repair_agent(
    status: &bn_core::events::CronJobStatus,
    task_id: &str,
    output_tail: Option<&str>,
) -> Result<usize, String> {
    let prompt = repair_prompt(status, output_tail);
    let spec = AddSpec {
        kind: BacklogKind::Ship,
        priority: 4,
        recurrence: Recurrence::Coalesce,
        source: REPAIR_SOURCE.to_string(),
        task: task_id.to_string(),
        exec: ExecSpec::Agent,
        ..Default::default()
    };
    let added = backlog::add(
        &backlog::backlog_path(),
        "bn",
        std::slice::from_ref(&prompt),
        &spec,
        bn_core::util::now_epoch_secs(),
    )
    .map_err(|error| error.to_string())?;
    Ok(added.len())
}

/// `cron-health` — the deterministic failing-cron watchdog (no LLM). Raises an urgent attention
/// item for every cron in a chronic-failure state (see [`bn_core::crons::alerting_crons`]) with the
/// job name as the dedupe key, and — via `replace=true` — auto-acks the alert for any cron that has
/// since recovered. It also mints one keyed repair task and queues one autonomous repair agent per
/// failing cron, so the captured error turns into work instead of only a dashboard row — except
/// for a cron nothing can resolve, where the repair prompt's `bn run <job>` has nothing to run.
pub(crate) fn job_cron_health(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    use bn_core::clock::Clock;
    let runs = bn_core::events::cron_status();
    let live = bn_core::crons::LiveJobs::discover(cfg, ctx);
    let alerting = bn_core::crons::alerting_crons(
        &runs,
        bn_core::crons::ALERT_MIN_FAILURES,
        &live,
        bn_core::util::now_epoch_secs(),
    );
    let details: Vec<_> = alerting
        .iter()
        .filter_map(|status| bn_core::events::last_run(&status.job).map(|detail| (*status, detail)))
        .collect();
    let items: Vec<IngestItem> = alerting
        .iter()
        .map(|s| IngestItem {
            title: format!(
                "cron '{}' is failing — {}/{} runs failed (last exit {})",
                s.job, s.failures, s.runs, s.last_exit
            ),
            severity: Some("urgent".to_string()),
            source: None,
            key: Some(s.job.clone()),
            link: None,
            repo: None,
            branch: None,
            detail: bn_core::events::last_run(&s.job)
                .map(|d| d.output_tail.trim().to_string())
                .filter(|t| !t.is_empty()),
        })
        .collect();
    let n = items.len();
    let today = bn_core::clock::RealClock.today();
    let (closed_repairs, withdrawn_repairs) = close_recovered_repairs(cfg, &alerting, &today);
    let mut repair_tasks = 0usize;
    let mut queued_repairs = 0usize;
    let mut repair_errors = Vec::new();
    for (status, detail) in &details {
        let output_tail = detail.output_tail.trim();
        match ensure_repair_task(cfg, status, Some(output_tail), &today) {
            Some(task_id) => {
                repair_tasks += 1;
                // An unresolvable job is still worth a person's attention row, but dispatching an
                // autonomous agent at it only burns a worktree: the prompt tells that agent to
                // reproduce with `bn run <job>`, which cannot resolve, so it can only ever report
                // the same nothing. Retirement (the horizon) clears the row; this stops the spend.
                if !live.contains(&status.job) {
                    continue;
                }
                match enqueue_repair_agent(status, &task_id, Some(output_tail)) {
                    Ok(added) => queued_repairs += added,
                    Err(error) => repair_errors.push(format!("{}: {error}", status.job)),
                }
            }
            None => repair_errors.push(format!("{}: could not write repair task", status.job)),
        }
    }
    match ingest(&items, REPAIR_SOURCE, "warn", true, &today) {
        Ok(_) => {
            if n == 0 {
                println!("cron-health: all crons healthy");
            } else {
                println!(
                    "cron-health: {n} failing cron(s) raised/refreshed; {repair_tasks} repair task(s), {queued_repairs} queued agent(s)"
                );
                for error in repair_errors {
                    eprintln!("cron-health repair: {error}");
                }
            }
            if closed_repairs > 0 {
                println!(
                    "cron-health: {closed_repairs} repair task(s) closed on recovery, {withdrawn_repairs} queued agent(s) withdrawn"
                );
            }
            0
        }
        Err(e) => {
            eprintln!("cron-health: {e}");
            1
        }
    }
}


/// Whether the findings packet has no sources (nothing gathered) — the guardrail behind the
/// gather's decline, so no model is ever paid to look at an empty day.
/// How far back the attention gather reads the findings store.
const FINDINGS_LOOKBACK_DAYS: i64 = 7;

/// The findings packet the attention agent ranks: the latest run per source within the lookback,
/// newest first.
///
/// This was `bn_core::today::gather_packet_json` until the Today agent was retired. The packet
/// shape is unchanged — the agent prompt and `findings_packet_empty` both key off `sources` — and
/// it still reads the same [`bn_core::findings`] store the `gather-findings` cron writes.
fn findings_packet_json(today: &str, lookback_days: i64) -> String {
    let since = match bn_core::util::epoch_day(today) {
        Some(d) => bn_core::util::date_of((d - lookback_days.max(0)).max(0) * 86_400),
        None => today.to_string(),
    };
    // One run per source: a source that gathered twice today is described by its newest run.
    let mut latest: std::collections::HashMap<String, bn_core::findings::FindingsRun> =
        std::collections::HashMap::new();
    for run in bn_core::findings::query(None, Some(&since), 0) {
        latest
            .entry(run.source.clone())
            .and_modify(|cur| {
                if run.gathered_at > cur.gathered_at {
                    *cur = run.clone();
                }
            })
            .or_insert(run);
    }
    let mut runs: Vec<bn_core::findings::FindingsRun> = latest.into_values().collect();
    runs.sort_by(|a, b| {
        b.gathered_at
            .cmp(&a.gathered_at)
            .then(a.source.cmp(&b.source))
    });
    let packet = serde_json::json!({
        "generated_at": bn_core::util::now_epoch_secs(),
        "date": today,
        "sources": runs.iter().map(|r| serde_json::json!({
            "source": r.source,
            "run_id": r.run_id,
            "gathered_at": r.gathered_at,
            "items": r.items,
        })).collect::<Vec<_>>(),
    });
    serde_json::to_string(&packet).unwrap_or_else(|_| "{}".to_string())
}

fn findings_packet_empty(packet: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(packet)
        .ok()
        .and_then(|v| {
            v.get("sources")
                .and_then(|s| s.as_array())
                .map(|a| a.is_empty())
        })
        .unwrap_or(true)
}












/// `bn attn gather` — what the local automation noticed today, printed as JSON: the cron findings
/// packet plus the crons that are currently failing.
///
/// Declines (`{"skip": …}`) when there is nothing at all to look at. That guardrail is the reason
/// this is a command rather than two: the decision "is there anything here" needs both halves, and
/// splitting them would push it into a shell pipeline where it could not be made.
pub(crate) fn emit_attention_gather(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    use bn_core::clock::Clock;
    let today = bn_core::clock::RealClock.today();
    let findings = findings_packet_json(&today, FINDINGS_LOOKBACK_DAYS);
    let health = failing_cron_lines(cfg, ctx);
    if findings_packet_empty(&findings) && health.is_empty() {
        println!(
            "{}",
            serde_json::json!({ "skip": "no cron findings or failures to review" })
        );
        return 0;
    }
    let findings: serde_json::Value =
        serde_json::from_str(&findings).unwrap_or(serde_json::Value::Null);
    println!(
        "{}",
        serde_json::json!({
            "date": today,
            "findings": findings,
            "failing_crons": health,
        })
    );
    0
}

#[cfg(test)]
mod gather_tests {
    use super::findings_packet_empty;

    /// The guardrail behind the decline: a packet whose sources are empty means the gatherers found
    /// nothing, which is materially different from a packet that failed to parse — both must
    /// decline, because paying a model to rank nothing is the failure mode this prevents.
    #[test]
    fn an_empty_or_unreadable_packet_reads_as_nothing_to_review() {
        assert!(findings_packet_empty(r#"{"sources":[]}"#));
        assert!(findings_packet_empty("not json"));
        assert!(findings_packet_empty("{}"));
    }

    #[test]
    fn a_packet_with_any_source_is_worth_reviewing() {
        assert!(!findings_packet_empty(
            r#"{"sources":[{"source":"meetings","items":[]}]}"#
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::test_support;

    fn failing(job: &str) -> bn_core::events::CronJobStatus {
        bn_core::events::CronJobStatus {
            job: job.to_string(),
            scope: "global".to_string(),
            repo: None,
            branch: None,
            last_run: bn_core::util::now_epoch_secs(),
            ok: false,
            last_exit: 127,
            runs: 7,
            failures: 7,
            last_duration_ms: 4,
        }
    }

    /// The four-dispatch loop, in miniature.
    ///
    /// `task-improve` was renamed away and tombstoned, so the watchdog stopped raising it — but its
    /// queued repair agent happened to be *leased* on the pass that cancelled its task. The old
    /// sweep drew its withdrawal set from that pass's cancellations, so the leased entry was
    /// skipped once and then never reconsidered: its task was already closed, so no later pass
    /// could rediscover it. The entry cycled leased → pending → dispatched forever.
    ///
    /// Asserting the *second* pass is the whole point. A sweep that only withdraws on the
    /// transition pass leaves the entry pending here, and this fails.
    #[test]
    fn a_repair_agent_leased_when_its_cron_retires_is_withdrawn_on_a_later_pass() {
        let root = tempfile::tempdir().expect("create test notes directory");
        test_support::with_testhost_state_and_notes_dir(root.path(), || {
            let cfg = test_support::config(root.path());
            let today = "2026-08-09";
            let status = failing("renamed-away");

            let task_id = ensure_repair_task(&cfg, &status, Some("bn: command not found"), today)
                .expect("mint the repair task");
            enqueue_repair_agent(&status, &task_id, None).expect("queue the repair agent");

            let backlog_path = backlog::backlog_path();
            let now = bn_core::util::now_epoch_secs();
            let leased = backlog::lease(&backlog_path, "bn", 1, now).expect("lease the entry");
            let entry_id = leased.first().expect("one entry was queued").id.clone();

            // Pass 1: the cron is no longer raising. The task closes; the leased entry is left
            // alone on purpose, because cancelling a row under a live worktree would misdescribe
            // work that is still running.
            let (closed, withdrawn) = close_recovered_repairs(&cfg, &[], today);
            assert_eq!(closed, 1, "the repair task should be cancelled");
            assert_eq!(withdrawn, 0, "a leased entry is not withdrawn mid-flight");

            // The dispatch ends and the entry comes to rest.
            backlog::release_many(&backlog_path, "bn", &[entry_id.as_str()], now)
                .expect("release the lease");

            // Pass 2: nothing was cancelled this pass, and the task is long closed. The entry must
            // still be reaped — that is the regression.
            let (closed, withdrawn) = close_recovered_repairs(&cfg, &[], today);
            assert_eq!(closed, 0, "the task was already closed on pass 1");
            assert_eq!(withdrawn, 1, "the released entry must be withdrawn, not re-dispatched");

            let status = backlog::load(&backlog_path)
                .get("bn", &entry_id)
                .map(|entry| entry.status)
                .expect("the entry is still on record");
            assert_eq!(status, backlog::BacklogStatus::Cancelled, "{status:?}");
        });
    }

    /// The other direction, so the sweep cannot be "fixed" by withdrawing everything: a cron that
    /// is still failing keeps its repair task and its queued agent.
    #[test]
    fn a_still_failing_cron_keeps_its_repair_task_and_queued_agent() {
        let root = tempfile::tempdir().expect("create test notes directory");
        test_support::with_testhost_state_and_notes_dir(root.path(), || {
            let cfg = test_support::config(root.path());
            let today = "2026-08-09";
            let status = failing("still-broken");

            let task_id =
                ensure_repair_task(&cfg, &status, None, today).expect("mint the repair task");
            enqueue_repair_agent(&status, &task_id, None).expect("queue the repair agent");

            let (closed, withdrawn) = close_recovered_repairs(&cfg, &[&status], today);

            assert_eq!(closed, 0, "a still-failing cron keeps its repair task");
            assert_eq!(withdrawn, 0, "and keeps its queued repair agent");
        });
    }

    #[test]
    fn repair_task_text_prefers_a_failure_line_over_earlier_progress() {
        let tail = "main-sync: BigMono: main worktree → origin/main\n\
                    main-sync: BigMono: main slot has uncommitted work — left alone\n\
                    main-sync: WebApp: could not create main worktree\n\
                    main-sync: telemetry-service: fetch failed (offline?)";
        assert_eq!(
            repair_task_line(tail),
            Some("main-sync: WebApp: could not create main worktree")
        );

        let text = repair_task_text(&failing("main-sync"), Some(tail));
        assert!(text.contains("could not create main worktree"));
        assert!(!text.contains("error:main-sync- BigMono: main worktree → origin/main"));
    }

    #[test]
    fn repair_task_text_falls_back_to_the_first_line_when_none_look_failing() {
        let text = repair_task_text(
            &failing("main-sync"),
            Some(
                "main-sync: BigMono: main worktree → origin/main\n\
                 main-sync: BigMono: main slot has uncommitted work — left alone",
            ),
        );

        assert!(text.contains("error:main-sync- BigMono: main worktree → origin/main"));
    }

}
