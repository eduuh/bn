//! Cron-result agents: the deterministic failing-cron watchdog (`cron-health`) and the periodic
//! LLM `attention-agent` that reviews recent cron findings + cron health and writes a ranked
//! attention list. Both raise into the same attention store via `ingest(..., replace=true)`, so a
//! recovered cron's / dropped item's alert is auto-acked on the next run.
use crate::*;
use bn_core::backlog::{self, AddSpec, BacklogKind, Recurrence};
use bn_core::attention::{ingest, IngestItem};
use bn_core::queue::kind::ExecSpec;

/// Keep the headless harness under the outer cron budget so a slow model call can still fall back
/// and finish green instead of being killed by the scheduler.
const HARNESS_WALL_SECS: u64 = 90;

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

fn repair_task_text(status: &bn_core::events::CronJobStatus, output_tail: Option<&str>) -> String {
    let mut text = format!(
        "Fix failing cron {} repo:bn cron:{} failures:{} exit:{}",
        status.job, status.job, status.failures, status.last_exit
    );
    if let Some(tail) = output_tail.map(str::trim).filter(|tail| !tail.is_empty()) {
        let one_line = tail.lines().next().unwrap_or(tail).trim();
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

/// One ranked item the LLM returns. Tolerant of the usual field-name drift (`why`/`reason`,
/// `link`/`url`); an absent severity folds to `info`.
#[derive(Debug, serde::Deserialize)]
struct RankedItem {
    title: String,
    #[serde(default)]
    severity: String,
    #[serde(default, alias = "reason", alias = "rationale")]
    why: Option<String>,
    #[serde(default, alias = "url")]
    link: Option<String>,
}

/// Whether the findings packet has no sources (nothing gathered) — a guardrail so the agent never
/// spends a model call on an empty packet.
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

/// Extract the first top-level JSON array from a model reply, which is often wrapped in prose or a
/// ```json fence. Returns the `[..]` slice, or `None` when there's no balanced array.
fn extract_json_array(s: &str) -> Option<&str> {
    let start = s.find('[')?;
    let end = s.rfind(']')?;
    if end > start {
        Some(&s[start..=end])
    } else {
        None
    }
}

/// Parse the model's ranked attention list from its (possibly prose-wrapped) stdout. Returns an
/// empty vec on any parse failure — a bad reply is a no-op, never a crash.
fn parse_ranked(stdout: &str) -> Vec<RankedItem> {
    extract_json_array(stdout)
        .and_then(|j| serde_json::from_str::<Vec<RankedItem>>(j).ok())
        .unwrap_or_default()
}

#[derive(Debug, serde::Deserialize)]
struct FindingsPacket {
    sources: Vec<FindingsPacketSource>,
}

#[derive(Debug, serde::Deserialize)]
struct FindingsPacketSource {
    source: String,
    items: Vec<FindingsPacketItem>,
}

#[derive(Debug, serde::Deserialize)]
struct FindingsPacketItem {
    text: String,
    #[serde(default)]
    url: String,
}

fn health_items(health: &[String]) -> Vec<IngestItem> {
    let mut items = Vec::new();
    for line in health {
        let title = line.trim();
        if title.is_empty() {
            continue;
        }
        items.push(IngestItem {
            title: title.to_string(),
            severity: Some("urgent".to_string()),
            source: None,
            key: Some(slug(title)),
            link: None,
            repo: None,
            branch: None,
            detail: None,
        });
        if items.len() >= 10 {
            break;
        }
    }
    items
}

fn fallback_attention_items(findings: &str, health: &[String]) -> Vec<IngestItem> {
    let mut items = health_items(health);
    let Ok(packet) = serde_json::from_str::<FindingsPacket>(findings) else {
        return items;
    };

    for source in packet.sources {
        for item in source.items {
            let title = item.text.trim();
            if title.is_empty() {
                continue;
            }
            items.push(IngestItem {
                title: title.to_string(),
                severity: Some("info".to_string()),
                source: None,
                key: Some(slug(title)),
                link: {
                    let url = item.url.trim();
                    if url.is_empty() {
                        None
                    } else {
                        Some(url.to_string())
                    }
                },
                repo: None,
                branch: None,
                detail: Some(format!(
                    "Fallback from attention-agent after a harness failure; source {}.",
                    source.source
                )),
            });
            if items.len() >= 10 {
                return items;
            }
        }
    }

    items
}

/// Normalize a model-supplied severity to one of `info|warn|urgent`; anything else folds to `info`.
fn normalize_severity(s: &str) -> &'static str {
    match s.trim().to_ascii_lowercase().as_str() {
        "urgent" | "critical" | "high" => "urgent",
        "warn" | "warning" | "medium" => "warn",
        _ => "info",
    }
}

/// A stable dedupe slug from a title: lowercase, non-alphanumerics collapsed to single dashes,
/// bounded length. Keeps the attention key steady across runs so re-ranking refreshes rather than
/// duplicates the item.
fn slug(title: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in title.trim().to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    out.trim_matches('-').to_string()
}

/// `attention-agent` — the periodic LLM cron (#): fold recent cron findings + cron health into a
/// ranked attention list via a headless copilot run, then raise the items (source
/// `attention-agent`, `replace=true` so a dropped item is auto-acked). Opt-in via
/// `[attention_agent].enabled`; a clean no-op when off or when there is nothing to review.
pub(crate) fn job_attention_agent(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    use bn_core::clock::Clock;
    if !resolver::JobConfig::load().attention_agent_enabled {
        println!("attention-agent: disabled — set [attention_agent].enabled to opt in");
        return 0;
    }
    let today = bn_core::clock::RealClock.today();
    let findings = bn_core::today_agent::gather_packet_json(
        &today,
        bn_core::today_agent::DEFAULT_LOOKBACK_DAYS,
    );
    let health = failing_cron_lines(cfg, ctx);

    // Guardrail: never spend a model call on an empty packet.
    if findings_packet_empty(&findings) && health.is_empty() {
        println!("attention-agent: no cron findings or failures to review — skipping");
        return 0;
    }

    let health_block = if health.is_empty() {
        "(none)".to_string()
    } else {
        health.join("\n")
    };
    let catalog = match bn_core::agents::discover(cfg, ctx) {
        Ok(catalog) => catalog,
        Err(error) => {
            eprintln!("attention-agent: {error}");
            return 1;
        }
    };
    let agent = match catalog.require("attention-agent") {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("attention-agent: {error}");
            return 1;
        }
    };
    let mut args = serde_json::Map::new();
    args.insert("date".into(), serde_json::Value::String(today.clone()));
    args.insert("findings".into(), serde_json::Value::String(findings.clone()));
    args.insert(
        "cron_health".into(),
        serde_json::Value::String(health_block),
    );
    let prompt = match agent.render(Some(&args)) {
        Ok(prompt) => prompt,
        Err(error) => {
            eprintln!("attention-agent: {error}");
            return 1;
        }
    };

    // Claude is org-disabled in this environment, so copilot is the default headless harness.
    let harness = "copilot";
    let Some(cmd) = crate::cmd::staff::staff_dispatch_command(harness, &prompt, None, None) else {
        eprintln!("attention-agent: unknown harness '{harness}'");
        return 1;
    };
    let (ok, stdout, combined) = crate::cmd::staff::dispatch_capture(
        cmd,
        Some(std::time::Duration::from_secs(HARNESS_WALL_SECS)),
    );
    let mut used_fallback = false;
    let items: Vec<IngestItem> = if ok {
        parse_ranked(&stdout)
            .iter()
            .map(|r| IngestItem {
                title: r.title.clone(),
                severity: Some(normalize_severity(&r.severity).to_string()),
                source: None,
                key: Some(slug(&r.title)),
                link: r.link.clone(),
                repo: None,
                branch: None,
                detail: r.why.clone(),
            })
            .filter(|i| !i.title.trim().is_empty())
            .collect()
    } else {
        eprintln!("attention-agent: harness run failed — {}", combined.trim());
        used_fallback = true;
        fallback_attention_items(&findings, &health)
    };
    let n = items.len();
    match ingest(&items, "attention-agent", "info", true, &today) {
        Ok(_) => {
            if n == 0 {
                println!("attention-agent: nothing worth the user's attention right now");
            } else {
                if used_fallback {
                    println!(
                        "attention-agent: harness failed; using deterministic fallback attention items"
                    );
                }
                println!("attention-agent: ranked {n} attention item(s):");
                for (i, r) in items.iter().enumerate() {
                    println!(
                        "  {}. [{}] {}",
                        i + 1,
                        normalize_severity(r.severity.as_deref().unwrap_or("info")),
                        r.title.trim()
                    );
                }
            }
            0
        }
        Err(e) => {
            eprintln!("attention-agent: {e}");
            1
        }
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
    fn harness_failure_falls_back_to_findings_and_health_items() {
        let findings = serde_json::json!({
            "generated_at": 1,
            "date": "2026-08-20",
            "sources": [{
                "source": "meetings",
                "run_id": "run-1",
                "gathered_at": 1,
                "items": [
                    {"key": "k1", "text": "First finding", "url": "https://example.com/1"},
                    {"key": "k2", "text": "   ", "url": "https://example.com/2"}
                ]
            }]
        })
        .to_string();
        let health = vec!["cron 'attention-agent' is failing — 6/62 runs failed (last exit 1)".to_string()];

        let items = fallback_attention_items(&findings, &health);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, health[0]);
        assert_eq!(items[0].severity.as_deref(), Some("urgent"));
        assert_eq!(items[1].title, "First finding");
        assert_eq!(items[1].severity.as_deref(), Some("info"));
        assert_eq!(items[1].link.as_deref(), Some("https://example.com/1"));
    }
}
