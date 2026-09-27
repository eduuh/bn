//! Attention workflow: the deterministic failing-cron watchdog (`cron-health`) and the periodic
//! LLM `attention-agent` that reviews recent cron findings + cron health and writes a ranked
//! attention list. Both raise into the same attention store via `ingest(..., replace=true)`, so a
//! recovered cron's / dropped item's alert is auto-acked on the next run.
use crate::*;
use bn_core::attention::{ingest, IngestItem};

/// The source stamped on everything the watchdog mints: the attention row and the repair task.
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

fn repair_task_key(status: &bn_core::events::CronJobStatus) -> String {
    let mut key = format!("cron-repair:{}", status.job);
    let repo = status.repo.as_deref().map(str::trim).filter(|r| !r.is_empty());
    let branch = status
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty());
    if repo.is_some() || branch.is_some() {
        key.push(':');
        key.push_str(&repo.unwrap_or_default().to_ascii_lowercase());
        if let Some(branch) = branch {
            key.push(':');
            key.push_str(&branch.to_ascii_lowercase());
        }
    }
    key
}

fn repair_task_parts(key: &str) -> Option<(&str, Option<&str>, Option<&str>)> {
    let key = key.strip_prefix("cron-repair:")?;
    let mut parts = key.split(':');
    let job = parts.next()?;
    let repo = parts.next();
    let branch = parts.next();
    Some((job, repo, branch))
}

fn repair_task_matches_key(key: &str, status: &bn_core::events::CronJobStatus) -> bool {
    let Some((job, repo, branch)) = repair_task_parts(key) else {
        return false;
    };
    if job != status.job {
        return false;
    }
    if let Some(status_repo) = status.repo.as_deref() {
        if let Some(task_repo) = repo {
            if !task_repo.eq_ignore_ascii_case(status_repo) {
                return false;
            }
        }
    }
    if let Some(status_branch) = status.branch.as_deref() {
        if let Some(task_branch) = branch {
            if !task_branch.eq_ignore_ascii_case(status_branch) {
                return false;
            }
        }
    }
    true
}

fn recovered_repair_task_job(key: &str) -> Option<&str> {
    repair_task_parts(key).map(|(job, _, _)| job)
}

/// The open `cron-repair:<job>` task for a failing cron, minting it when none is open. Returns the
/// task id and whether this call minted it, which is what limits a repair agent to one per task.
fn ensure_repair_task(
    cfg: &Config,
    status: &bn_core::events::CronJobStatus,
    output_tail: Option<&str>,
    today: &str,
) -> Option<(String, bool)> {
    let store = crate::cmd::task::task_store(cfg);
    let _lock = store.lock();
    let mut file = store.load().ok()?;
    let mut open: Vec<String> = file
        .tasks()
        .filter(|task| task.is_open(today))
        .filter(|task| {
            task.meta("key")
                .is_some_and(|key| repair_task_matches_key(key, status))
        })
        .filter_map(|task| task.id().map(str::to_string))
        .collect();
    if let Some(existing) = open.first().cloned() {
        let had_duplicates = open.len() > 1;
        for stale in open.drain(1..) {
            let _ = file.cancel(&stale, today, Some("cron-duplicate"));
        }
        if had_duplicates && store.save(&file).is_err() {
            return None;
        }
        return Some((existing, false));
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
    task.set_meta_raw("key", &repair_task_key(status));
    task.set_meta_raw("cron", &status.job);
    file.push_task(task);
    store.save(&file).ok()?;
    Some((id, true))
}

/// Fold out the repair tasks for crons that are no longer alerting — the symmetric other half of
/// [`ensure_repair_task`], and the reason a recovered cron stops costing anything.
///
/// The attention row auto-acks itself, because [`ingest`] runs with `replace=true`. The repair
/// *task* has no such mechanism: without this, `scout` recovering leaves `Fix failing cron scout`
/// open in the task list forever.
///
/// `alerting` is the raise set this pass computed, so "recovered" and "retired" collapse into one
/// condition: a `cron-repair:<job>` task whose job the watchdog is no longer raising has nothing
/// left to do. The task is [`cancel`](bn_core::todo::Task::cancel)led rather than completed — no
/// one did the work, the cron stopped failing — which keeps it out of completion metrics. A repair
/// agent already running is left alone; it is a live worktree a person can read and stop.
///
/// Returns how many tasks were closed.
fn close_recovered_repairs(
    cfg: &Config,
    alerting: &[&bn_core::events::CronJobStatus],
    today: &str,
) -> usize {
    let raising: std::collections::HashSet<&str> =
        alerting.iter().map(|status| status.job.as_str()).collect();
    let store = crate::cmd::task::task_store(cfg);
    let _lock = store.lock();
    let Ok(mut file) = store.load() else {
        return 0;
    };
    let stale: Vec<String> = file
        .tasks()
        .filter(|task| task.is_open(today))
        .filter(|task| task.meta("key").and_then(recovered_repair_task_job).is_some_and(|job| !raising.contains(job)))
        .filter_map(|task| task.id().map(str::to_string))
        .collect();
    for id in &stale {
        file.cancel(id, today, Some("cron-recovered"));
    }
    if !stale.is_empty() && store.save(&file).is_err() {
        return 0;
    }
    stale.len()
}

/// Start the autonomous repair agent for a freshly minted repair task, through `bn dispatch`.
fn start_repair_agent(
    status: &bn_core::events::CronJobStatus,
    task_id: &str,
    output_tail: Option<&str>,
) -> Result<(), String> {
    let prompt = repair_prompt(status, output_tail);
    let name = crate::wt::sanitize_branch(&format!("cron-repair-{}", status.job));
    crate::cmd::dispatch::DispatchInvocation::new(&prompt, "bn", "auto")
        .name(Some(&name))
        .task(Some(task_id))
        .launch()
}

fn should_dispatch_repair_agents() -> bool {
    use std::io::IsTerminal;
    !std::io::stdin().is_terminal()
}

fn job_cron_health_with_dispatch(
    cfg: &Config,
    ctx: Option<&Resolved>,
    dispatch_repair_agents: bool,
) -> i32 {
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
    let closed_repairs = close_recovered_repairs(cfg, &alerting, &today);
    let mut repair_tasks = 0usize;
    let mut started_repairs = 0usize;
    let mut repair_errors = Vec::new();
    for (status, detail) in &details {
        let output_tail = detail.output_tail.trim();
        match ensure_repair_task(cfg, status, Some(output_tail), &today) {
            Some((task_id, minted)) => {
                repair_tasks += 1;
                // A health check run by hand should not launch fresh repair agents unless the
                // caller is headless. The alert row still refreshes, but the repair work stays
                // queued for the cron scheduler rather than spawning from a terminal.
                if !dispatch_repair_agents || !minted || !live.contains(&status.job) {
                    continue;
                }
                match start_repair_agent(status, &task_id, Some(output_tail)) {
                    Ok(()) => started_repairs += 1,
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
                    "cron-health: {n} failing cron(s) raised/refreshed; {repair_tasks} repair task(s), {started_repairs} agent(s) started"
                );
                for error in repair_errors {
                    eprintln!("cron-health repair: {error}");
                }
            }
            if closed_repairs > 0 {
                println!("cron-health: {closed_repairs} repair task(s) closed on recovery");
            }
            0
        }
        Err(e) => {
            eprintln!("cron-health: {e}");
            1
        }
    }
}

/// `cron-health` — the deterministic failing-cron watchdog (no LLM). Raises an urgent attention
/// item for every cron in a chronic-failure state (see [`bn_core::crons::alerting_crons`]) with the
/// job name as the dedupe key, and — via `replace=true` — auto-acks the alert for any cron that has
/// since recovered. It also mints one keyed repair task per failing cron and starts one autonomous
/// repair agent when that task is minted, so the captured error turns into work instead of only a
/// dashboard row — except for a cron nothing can resolve, where the repair prompt's `bn run <job>`
/// has nothing to run. The agent starts once per task, not once per pass: the watchdog runs every
/// fifteen minutes, and an open repair task already says the work is in hand.
pub(crate) fn job_cron_health(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    job_cron_health_with_dispatch(cfg, ctx, should_dispatch_repair_agents())
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

    fn failing_scoped(
        job: &str,
        repo: Option<&str>,
        branch: Option<&str>,
    ) -> bn_core::events::CronJobStatus {
        bn_core::events::CronJobStatus {
            job: job.to_string(),
            scope: "global".to_string(),
            repo: repo.map(str::to_string),
            branch: branch.map(str::to_string),
            last_run: bn_core::util::now_epoch_secs(),
            ok: false,
            last_exit: 127,
            runs: 7,
            failures: 7,
            last_duration_ms: 4,
        }
    }

    /// A cron that stops raising closes its repair task once, and a later pass finds nothing left.
    #[test]
    fn a_recovered_crons_repair_task_is_closed_once() {
        let root = tempfile::tempdir().expect("create test notes directory");
        test_support::with_testhost_state_and_notes_dir(root.path(), || {
            let cfg = test_support::config(root.path());
            let today = "2026-08-09";
            let status = failing("renamed-away");

            let (_, minted) =
                ensure_repair_task(&cfg, &status, Some("bn: command not found"), today)
                    .expect("mint the repair task");
            assert!(minted, "the first pass mints the task");

            assert_eq!(close_recovered_repairs(&cfg, &[], today), 1);
            assert_eq!(close_recovered_repairs(&cfg, &[], today), 0);
        });
    }

    /// The other direction, so the sweep cannot be "fixed" by closing everything: a cron that is
    /// still failing keeps its repair task, and a second pass reuses it rather than minting another.
    #[test]
    fn a_still_failing_cron_keeps_its_repair_task() {
        let root = tempfile::tempdir().expect("create test notes directory");
        test_support::with_testhost_state_and_notes_dir(root.path(), || {
            let cfg = test_support::config(root.path());
            let today = "2026-08-09";
            let status = failing("still-broken");

            let (first, _) =
                ensure_repair_task(&cfg, &status, None, today).expect("mint the repair task");
            assert_eq!(close_recovered_repairs(&cfg, &[&status], today), 0);
            let (again, minted) =
                ensure_repair_task(&cfg, &status, None, today).expect("find the repair task");
            assert_eq!(first, again);
            assert!(!minted, "an open repair task is reused, so no second agent starts");
        });
    }

    /// Repo names can arrive in different case from different hosts, but the repair key must stay
    /// canonical so the same failing cron does not mint duplicate tasks.
    #[test]
    fn a_case_only_repo_variant_reuses_the_same_repair_task() {
        let root = tempfile::tempdir().expect("create test notes directory");
        test_support::with_testhost_state_and_notes_dir(root.path(), || {
            let cfg = test_support::config(root.path());
            let today = "2026-08-09";
            let upper = failing_scoped("pool-warm", Some("BigMono"), Some("main"));
            let lower = failing_scoped("pool-warm", Some("bigmono"), Some("main"));

            let (first, minted) =
                ensure_repair_task(&cfg, &upper, None, today).expect("mint the repair task");
            assert!(minted, "the first pass mints the task");

            let (again, minted) =
                ensure_repair_task(&cfg, &lower, None, today).expect("find the repair task");
            assert_eq!(first, again);
            assert!(!minted, "case-only repo variants must reuse the same task");

            let tasks = bn_core::todo::load(&cfg.tasks_file()).expect("tasks");
            assert_eq!(
                tasks
                    .tasks()
                    .filter(|task| task.meta("key") == Some(&repair_task_key(&upper)))
                    .count(),
                1
            );
        });
    }

    /// A hand-run health check should refresh the alert rows without firing repair agents.
    #[test]
    fn a_manual_health_check_can_skip_dispatch() {
        let _lock = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", temp.path().join("state"));
        let _jobs = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", temp.path().join("jobs"));
        let _agents = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", temp.path().join("agents"));
        let cfg = Config {
            personal_root: temp.path().join("personal"),
            work_root: temp.path().join("work"),
            hostname: "test-host".to_string(),
            ..Default::default()
        };
        let now = bn_core::util::now_epoch_secs();
        for i in 0..3 {
            bn_core::events::log_cron_run(
                "broken-cron",
                "global",
                None,
                None,
                now - (3 - i) * 60,
                10,
                7,
                "panic: missing config\nstack tail",
            )
            .expect("cron event");
        }

        assert_eq!(
            job_cron_health_with_dispatch(&cfg, None, false),
            0
        );
        assert!(
            crate::cmd::dispatch::invocation::take_launched().is_empty(),
            "manual health checks should not launch repair agents"
        );
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
