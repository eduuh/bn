//! Cron-result agents: the deterministic failing-cron watchdog (`cron-health`) and the periodic
//! LLM `attention-agent` that reviews recent cron findings + cron health and writes a ranked
//! attention list. Both raise into the same attention store via `ingest(..., replace=true)`, so a
//! recovered cron's / dropped item's alert is auto-acked on the next run.
use crate::*;
use bn_core::attention::{ingest, IngestItem};

/// A short text summary of the crons currently in a chronic-failure state — the health half of the
/// attention agent's packet, and the watchdog's raise set.
fn failing_cron_lines() -> Vec<String> {
    let runs = bn_core::events::cron_status();
    bn_core::crons::alerting_crons(&runs, bn_core::crons::ALERT_MIN_FAILURES)
        .iter()
        .map(|s| {
            format!(
                "cron '{}' is failing — {}/{} runs failed (last exit {})",
                s.job, s.failures, s.runs, s.last_exit
            )
        })
        .collect()
}

/// `cron-health` — the deterministic failing-cron watchdog (no LLM). Raises an urgent attention
/// item for every cron in a chronic-failure state (see [`bn_core::crons::alerting_crons`]) with the
/// job name as the dedupe key, and — via `replace=true` — auto-acks the alert for any cron that has
/// since recovered. This is what turns a silent 539/540-failure cron into something `notify` toasts.
pub(crate) fn job_cron_health(_cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use bn_core::clock::Clock;
    let runs = bn_core::events::cron_status();
    let alerting = bn_core::crons::alerting_crons(&runs, bn_core::crons::ALERT_MIN_FAILURES);
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
    match ingest(&items, "cron-health", "warn", true, &today) {
        Ok(_) => {
            if n == 0 {
                println!("cron-health: all crons healthy");
            } else {
                println!("cron-health: {n} failing cron(s) raised/refreshed");
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

/// Build the attention-agent prompt from the findings packet + cron health lines.
fn build_prompt(today: &str, findings: &str, health: &[String]) -> String {
    let health_block = if health.is_empty() {
        "(none)".to_string()
    } else {
        health.join("\n")
    };
    format!(
        "You are the bn attention agent. Review what the local automation (cron jobs) has gathered \
today ({today}) and produce a single ranked attention list for the user to prioritize.\n\n\
CRON FINDINGS (JSON: sources[].items[] = {{key,text,url}}):\n{findings}\n\n\
CRON HEALTH (jobs currently failing):\n{health_block}\n\n\
Output ONLY a JSON array (no prose, no code fence), most-important first, at most 10 items. Each \
element: {{\"title\": string (one actionable line), \"severity\": \"info\"|\"warn\"|\"urgent\", \
\"why\": string (one sentence of rationale), \"link\": string (optional url)}}. Merge duplicates, \
drop noise, and put genuinely time-sensitive or failing-system items first. If there is nothing \
worth the user's attention, output []."
    )
}

/// `attention-agent` — the periodic LLM cron (#): fold recent cron findings + cron health into a
/// ranked attention list via a headless copilot run, then raise the items (source
/// `attention-agent`, `replace=true` so a dropped item is auto-acked). Opt-in via
/// `[attention_agent].enabled`; a clean no-op when off or when there is nothing to review.
pub(crate) fn job_attention_agent(_cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
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
    let health = failing_cron_lines();

    // Guardrail: never spend a model call on an empty packet.
    if findings_packet_empty(&findings) && health.is_empty() {
        println!("attention-agent: no cron findings or failures to review — skipping");
        return 0;
    }

    // Claude is org-disabled in this environment, so copilot is the default headless harness.
    let harness = "copilot";
    let prompt = build_prompt(&today, &findings, &health);
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


