//! Cron-result agents: the deterministic failing-cron watchdog (`cron-health`) and the periodic
//! LLM `attention-agent` that reviews recent cron findings + cron health and writes a ranked
//! attention list. Both raise into the same attention store via `ingest(..., replace=true)`, so a
//! recovered cron's / dropped item's alert is auto-acked on the next run.
use crate::*;
use bn_core::backlog::{self, AddSpec, BacklogKind, Recurrence};
use bn_core::attention::{ingest, IngestItem};
use bn_core::queue::kind::ExecSpec;

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
        "cron-health",
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
/// Returns `(tasks_closed, agents_withdrawn)`.
fn close_recovered_repairs(
    cfg: &Config,
    alerting: &[&bn_core::events::CronJobStatus],
    today: &str,
) -> (usize, usize) {
    let raising: std::collections::HashSet<&str> =
        alerting.iter().map(|status| status.job.as_str()).collect();
    let path = cfg.tasks_file();
    let recovered: Vec<String> = {
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
        stale
    };
    if recovered.is_empty() {
        return (0, 0);
    }

    let backlog_path = backlog::backlog_path();
    let now = bn_core::util::now_epoch_secs();
    let pending: Vec<(String, String)> = backlog::load(&backlog_path)
        .entries()
        .iter()
        .filter(|entry| entry.status == backlog::BacklogStatus::Pending)
        .filter(|entry| recovered.contains(&entry.task))
        .map(|entry| (entry.repo.clone(), entry.id.clone()))
        .collect();
    let withdrawn = pending
        .iter()
        .filter(|(repo, id)| backlog::cancel(&backlog_path, repo, id, now).unwrap_or(false))
        .count();
    (recovered.len(), withdrawn)
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
        source: "cron-health".to_string(),
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
    match ingest(&items, "cron-health", "warn", true, &today) {
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
    args.insert("findings".into(), serde_json::Value::String(findings));
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
    let (ok, stdout, combined) =
        crate::cmd::staff::dispatch_capture(cmd, Some(std::time::Duration::from_secs(240)));
    if !ok {
        eprintln!("attention-agent: harness run failed — {}", combined.trim());
        return 1;
    }

    let ranked = parse_ranked(&stdout);
    let items: Vec<IngestItem> = ranked
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
        .collect();
    let n = items.len();
    match ingest(&items, "attention-agent", "info", true, &today) {
        Ok(_) => {
            if n == 0 {
                println!("attention-agent: nothing worth the user's attention right now");
            } else {
                println!("attention-agent: ranked {n} attention item(s):");
                for (i, r) in ranked.iter().enumerate() {
                    println!(
                        "  {}. [{}] {}",
                        i + 1,
                        normalize_severity(&r.severity),
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
