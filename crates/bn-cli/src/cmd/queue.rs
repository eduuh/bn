//! `bn queue` — the machine-wide work queue.
//!
//! Reads and writes only; there is no dispatcher here yet. What this surface is *for* is answering
//! "what is waiting, what does it need, and is anything allowed to start" from one place, across
//! every repo and every kind of agent — a question the machine previously had no way to answer
//! because each producer kept its own admission logic to itself.
use crate::*;
use bn_core::backlog::{self, AddSpec, BacklogEntry, BacklogKind, BacklogStatus};
use bn_core::queue::kind::{ExecSpec, Requirements};

/// Dispatch a `bn queue` invocation.
pub(crate) fn route(cfg: &Config, action: QueueCmd, json: bool) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    if !cfg.queue_enabled_here() {
        crate::cmd::queue_install::disable();
        if matches!(&action, QueueCmd::Status { .. }) {
            crate::cmd::queue_scope::emit_not_enabled_status(cfg, json);
            return;
        }
        if matches!(&action, QueueCmd::Badge) {
            crate::cmd::queue_scope::emit_disabled_badge(cfg, json);
            return;
        }
        fail_json(
            json,
            "queue",
            "not_enabled",
            &format!(
                "bn queue: switched off on this machine ('{}'); remove queue = false from [machine] in {}",
                cfg.machine(),
                bn_core::resolver::machine_config_path().display()
            ),
        );
    }
    match action {
        QueueCmd::Status { repo } => {
            crate::cmd::queue_status::emit_status(cfg, repo.as_deref(), now, json)
        }
        QueueCmd::List {
            kind,
            repo,
            class_filter,
            status,
            all,
        } => emit_list(
            cfg,
            kind.as_deref(),
            repo.as_deref(),
            class_filter.as_deref(),
            &status,
            all,
            json,
        ),
        QueueCmd::Add(a) => emit_add(
            cfg,
            &a.item,
            a.repo.as_deref(),
            AddArgs {
                kind: &a.kind,
                priority: a.priority,
                skill: a.skill.as_deref(),
                skill_arg: a.skill_arg.as_deref(),
                name: a.name.as_deref(),
                yolo: a.yolo,
                job: a.job.as_deref(),
                job_repo: a.job_repo,
                job_branch: a.job_branch,
                shell: a.shell.as_deref(),
                harness: a.harness,
                requires: a.requires,
                excludes: a.excludes,
                touches: a.touches,
                est_secs: a.est_secs.unwrap_or_default(),
                recurrence: &a.recurrence,
            },
            now,
            json,
        ),
        QueueCmd::Cancel { id, repo } => emit_cancel(cfg, &id, repo.as_deref(), now, json),
        QueueCmd::Priority { id, priority, repo } => {
            emit_priority(cfg, &id, priority, repo.as_deref(), now, json)
        }
        QueueCmd::Snooze {
            id,
            for_duration,
            repo,
        } => emit_snooze(cfg, &id, &for_duration, repo.as_deref(), now, json),
        QueueCmd::Class { id, class, repo } => {
            emit_class(cfg, &id, &class, repo.as_deref(), now, json)
        }
        QueueCmd::Tick { dry_run } => crate::cmd::queue_tick::emit_tick(cfg, dry_run, json),
        QueueCmd::At { id, when, repo } => {
            crate::cmd::queue_when::emit_at(cfg, &id, &when, repo.as_deref(), now, json)
        }
        QueueCmd::Now { id, repo } => {
            crate::cmd::queue_when::emit_now(cfg, &id, repo.as_deref(), now, json)
        }
        QueueCmd::Away { for_duration, back } => {
            crate::cmd::queue_when::emit_away(&for_duration, back, now, json)
        }
        QueueCmd::Schedule { action } => match action {
            Some(action) => crate::cmd::queue_when::emit_schedule_edit(cfg, action, json),
            None => crate::cmd::queue_when::emit_schedule(cfg, now, json),
        },
        QueueCmd::Run { interval } => crate::cmd::queue_tick::emit_run(cfg, interval),
        QueueCmd::Stop => crate::cmd::queue_daemon::emit_stop(json),
        QueueCmd::Pause {
            scope,
            reason,
            for_,
        } => crate::cmd::queue_pause::emit_pause(&scope, &reason, for_.as_deref(), now, json),
        QueueCmd::Resume { scope } => crate::cmd::queue_pause::emit_resume(&scope, now, json),
        QueueCmd::Off { for_, reason } => {
            crate::cmd::queue_pause::emit_off(for_.as_deref(), &reason, now, json)
        }
        QueueCmd::On => crate::cmd::queue_pause::emit_on(now, json),
        QueueCmd::Why => crate::cmd::queue_tick::emit_why(json),
        QueueCmd::Badge => crate::cmd::queue_scope::emit_badge(cfg, now, json),
        QueueCmd::Log {
            since,
            limit,
            kind,
            repo,
        } => crate::cmd::queue_log::emit_log(
            cfg,
            &since,
            limit,
            kind.as_deref(),
            repo.as_deref(),
            now,
            json,
        ),
        QueueCmd::Timeline { date, class_filter } => crate::cmd::queue_timeline::emit_timeline(
            cfg,
            date.as_deref(),
            class_filter.as_deref(),
            json,
        ),
        QueueCmd::Finish {
            repo,
            id,
            exit_code,
        } => {
            crate::cmd::queue_scope::require_repo_allowed(cfg, &repo, json);
            crate::cmd::queue_tick::emit_finish(&repo, &id, exit_code, json);
        }
        QueueCmd::Nodes { skill } => {
            crate::cmd::queue_tick::emit_nodes(cfg, skill.as_deref(), json)
        }
    }
}

/// The `bn queue add` options, bundled so the handler signature stays readable.
struct AddArgs<'a> {
    kind: &'a str,
    priority: i64,
    skill: Option<&'a str>,
    skill_arg: Option<&'a str>,
    name: Option<&'a str>,
    yolo: bool,
    job: Option<&'a str>,
    job_repo: Option<String>,
    job_branch: Option<String>,
    shell: Option<&'a str>,
    harness: Vec<String>,
    requires: Vec<String>,
    excludes: Vec<String>,
    touches: Vec<String>,
    est_secs: i64,
    recurrence: &'a str,
}

/// `bn queue cancel` — take pending work back out.
fn emit_cancel(cfg: &Config, id: &str, repo: Option<&str>, now: i64, json: bool) {
    let repo = repo
        .map(str::to_string)
        .unwrap_or_else(|| resolve_or_exit(cfg).repo);
    crate::cmd::queue_scope::require_repo_allowed(cfg, &repo, json);
    let path = backlog::backlog_path();
    let done = backlog::cancel(&path, &repo, id, now)
        .unwrap_or_else(|e| fail(&format!("bn queue cancel: could not update backlog: {e}")));
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-cancel",
                serde_json::json!({ "repo": repo, "id": id, "cancelled": done }),
            )
            .to_json()
        );
        return;
    }
    if done {
        println!("cancelled {repo}/{id}");
        return;
    }
    // Say which of the two reasons it was, because they need different things from the reader.
    let bl = backlog::load(&path);
    match bl.entries().iter().find(|e| e.repo == repo && e.id == id) {
        Some(e) if e.status == BacklogStatus::Leased => {
            println!("{repo}/{id} is already running — `bn agent kill` stops it; the lease is returned after");
        }
        Some(e) => println!("{repo}/{id} is already {}", e.status.label()),
        None => println!("no queue entry {repo}/{id}"),
    }
}

/// `bn queue priority` — move queued work up or down.
fn emit_priority(cfg: &Config, id: &str, priority: i64, repo: Option<&str>, now: i64, json: bool) {
    let repo = repo
        .map(str::to_string)
        .unwrap_or_else(|| resolve_or_exit(cfg).repo);
    crate::cmd::queue_scope::require_repo_allowed(cfg, &repo, json);
    let path = backlog::backlog_path();
    let done = backlog::set_priority(&path, &repo, id, priority, now)
        .unwrap_or_else(|e| fail(&format!("bn queue priority: could not update backlog: {e}")));
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-priority",
                serde_json::json!({ "repo": repo, "id": id, "priority": priority, "changed": done }),
            )
            .to_json()
        );
    } else if done {
        println!("{repo}/{id} → priority {priority}");
    } else {
        println!("{repo}/{id} unchanged (absent, already terminal, or already at {priority})");
    }
}

fn emit_snooze(cfg: &Config, id: &str, dur: &str, repo: Option<&str>, now: i64, json: bool) {
    let Some(secs) = bn_core::focus::parse_duration(dur).filter(|s| *s > 0) else {
        fail("bn queue snooze: --for must be a positive duration like 30m, 2h, or 1d");
    };
    let until = now.saturating_add(secs);
    let repo = repo
        .map(str::to_string)
        .unwrap_or_else(|| resolve_or_exit(cfg).repo);
    crate::cmd::queue_scope::require_repo_allowed(cfg, &repo, json);
    let path = backlog::backlog_path();
    let done = backlog::snooze(&path, &repo, id, until, now)
        .unwrap_or_else(|e| fail(&format!("bn queue snooze: could not update backlog: {e}")));
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-snooze",
                serde_json::json!({ "repo": repo, "id": id, "until": until, "changed": done }),
            )
            .to_json()
        );
    } else if done {
        println!(
            "{repo}/{id} snoozed until {}",
            bn_core::util::clock_hm(until)
        );
    } else {
        println!("{repo}/{id} unchanged (absent, not pending, or already snoozed there)");
    }
}

fn emit_class(cfg: &Config, id: &str, class: &str, repo: Option<&str>, now: i64, json: bool) {
    let override_class = match class.trim().to_ascii_lowercase().as_str() {
        "clear" | "derived" | "auto-derived" => None,
        _ => match bn_core::backlog::ClassOverride::parse(class) {
            Some(c) => Some(c),
            None => fail("bn queue class: expected foreground/watched, autonomous, opportunistic/away, or clear"),
        },
    };
    let repo = repo
        .map(str::to_string)
        .unwrap_or_else(|| resolve_or_exit(cfg).repo);
    crate::cmd::queue_scope::require_repo_allowed(cfg, &repo, json);
    let path = backlog::backlog_path();
    let done = backlog::set_class_override(&path, &repo, id, override_class, now)
        .unwrap_or_else(|e| fail(&format!("bn queue class: could not update backlog: {e}")));
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-class",
                serde_json::json!({
                    "repo": repo,
                    "id": id,
                    "class": override_class.map(|c| c.label()),
                    "changed": done,
                }),
            )
            .to_json()
        );
    } else if done {
        let label = override_class.map(|c| c.label()).unwrap_or("derived");
        println!("{repo}/{id} class → {label}");
    } else {
        println!("{repo}/{id} unchanged (absent, not pending, or already set)");
    }
}

// ---------------------------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------------------------

/// `bn queue list` — entries with their kind, priority, executor and requirements.
fn emit_list(
    cfg: &Config,
    kind: Option<&str>,
    repo: Option<&str>,
    class_filter: Option<&str>,
    status: &str,
    all: bool,
    json: bool,
) {
    let want_kind = match kind {
        Some(k) => match BacklogKind::parse(k) {
            Some(k) => Some(k),
            None => fail(&format!(
                "bn queue list: unknown --kind '{k}' (expected one of: {})",
                kind_choices()
            )),
        },
        None => None,
    };
    let want_status = if all {
        None
    } else {
        match parse_status(status) {
            Some(s) => Some(s),
            None => fail(&format!(
                "bn queue list: unknown --status '{status}' (expected pending, leased, done or failed)"
            )),
        }
    };
    let want_class = class_filter.map(normalize_class_filter);
    let backlog = backlog::load(&backlog::backlog_path());
    let mut rows: Vec<&BacklogEntry> = backlog
        .entries()
        .iter()
        .filter(|e| cfg.repo_allowed_here(&e.repo))
        .filter(|e| want_kind.is_none_or(|k| e.kind == k))
        .filter(|e| want_status.is_none_or(|s| e.status == s))
        .filter(|e| repo.is_none_or(|r| e.repo.eq_ignore_ascii_case(r)))
        .filter(|e| want_class.is_none_or(|c| class_matches(e, c)))
        .collect();
    // Same order the dispatcher will pull in, so the list reads as the plan rather than as
    // storage order.
    rows.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then(a.added_at.cmp(&b.added_at))
            .then(a.id.cmp(&b.id))
    });

    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-list",
                serde_json::json!({
                    "count": rows.len(),
                    "entries": rows.iter().map(|e| {
                        serde_json::json!({
                            "entry": e,
                            "class": class_json(e),
                        })
                    }).collect::<Vec<_>>()
                })
            )
            .to_json()
        );
        return;
    }
    if rows.is_empty() {
        println!("no matching queue entries");
        return;
    }
    for e in rows {
        let req = describe_requirements(&e.requires);
        println!(
            "{:>5}  {:<12} {:<13} {:<8} {:<7} {:<20} {}{}",
            e.priority,
            e.kind.label(),
            class_label(e),
            e.exec.label(),
            e.status.label(),
            truncate(&e.repo, 20),
            truncate(&e.title(), 44),
            req
        );
    }

    fn normalize_class_filter(raw: &str) -> &str {
        let raw = raw.trim();
        match raw {
            "all" | "" => "",
            "foreground" | "autonomous" | "opportunistic" | "cron" | "warming" | "recovery"
            | "maintenance" => raw,
            _ => fail(&format!(
                "bn queue list: unknown --class '{raw}' (expected foreground, autonomous, opportunistic, cron, warming, recovery or maintenance)"
            )),
        }
    }
}

fn class_matches(entry: &BacklogEntry, want: &str) -> bool {
    want.is_empty() || {
        let class = bn_core::queue::classify::classify(entry);
        class.mode.label() == want || class.subtype.label() == want
    }
}

pub(super) fn class_label(entry: &BacklogEntry) -> &'static str {
    let class = bn_core::queue::classify::classify(entry);
    if class.subtype.label() != "foreground" {
        return class.subtype.label();
    }
    class.mode.label()
}

pub(super) fn class_json(entry: &BacklogEntry) -> serde_json::Value {
    let class = bn_core::queue::classify::classify(entry);
    serde_json::json!({
        "mode": class.mode.label(),
        "subtype": class.subtype.label(),
        "autonomous": class.mode.is_autonomous(),
        "opportunistic": class.mode == bn_core::queue::classify::QueueMode::Opportunistic,
    })
}

/// The requirements suffix on a list row — empty when the entry runs anywhere, so an
/// unconstrained queue stays quiet.
pub(super) fn describe_requirements(req: &Requirements) -> String {
    if req.is_empty() {
        return String::new();
    }
    let mut parts = Vec::new();
    if !req.skills.is_empty() {
        parts.push(format!("skill:{}", req.skills.join("+")));
    }
    if !req.harness.is_empty() {
        parts.push(format!("harness:{}", req.harness.join("|")));
    }
    if !req.mcps.is_empty() {
        parts.push(format!("mcp:{}", req.mcps.join("+")));
    }
    if !req.capabilities.is_empty() {
        parts.push(req.capabilities.join("+"));
    }
    if !req.excludes.is_empty() {
        parts.push(format!("!{}", req.excludes.join("!")));
    }
    format!("  [{}]", parts.join(" "))
}

// ---------------------------------------------------------------------------------------------
// add
// ---------------------------------------------------------------------------------------------

/// `bn queue add` — enqueue without dispatching.
fn emit_add(cfg: &Config, item: &str, repo: Option<&str>, args: AddArgs, now: i64, json: bool) {
    let Some(kind) = BacklogKind::parse(args.kind) else {
        fail(&format!(
            "bn queue add: unknown --kind '{}' (expected one of: {})",
            args.kind,
            kind_choices()
        ));
    };
    let repo = match repo {
        Some(r) => r.to_string(),
        None => resolve_or_exit(cfg).repo,
    };
    crate::cmd::queue_scope::require_repo_allowed(cfg, &repo, json);
    if args.job_repo.is_some() != args.job_branch.is_some() {
        fail("bn queue add: --job-repo and --job-branch must be given together");
    }
    if let Some(job_repo) = args.job_repo.as_deref() {
        crate::cmd::queue_scope::require_repo_allowed(cfg, job_repo, json);
    }
    // An explicit executor wins; otherwise the kind's default applies inside `BacklogEntry::new`,
    // which also folds the implied skill into the requirements.
    let exec = match (args.skill, args.job, args.shell) {
        (Some(skill), _, _) => ExecSpec::Skill {
            skill: skill.to_string(),
            arg: args.skill_arg.map(str::to_string),
        },
        (_, Some(job), _) => ExecSpec::Job {
            job: job.to_string(),
            repo: args.job_repo.clone(),
            branch: args.job_branch.clone(),
        },
        (_, _, Some(cmd)) => ExecSpec::Shell {
            cmd: cmd.to_string(),
        },
        _ => ExecSpec::Agent,
    };
    let Some(recurrence) = bn_core::backlog::Recurrence::parse(args.recurrence) else {
        fail(&format!(
            "bn queue add: unknown --recurrence '{}' (expected coalesce or skip)",
            args.recurrence
        ));
    };
    let spec = AddSpec {
        kind,
        priority: args.priority,
        recurrence,
        touches: args.touches,
        source: "queue".to_string(),
        controller_id: std::env::var("BN_CONTROLLER_ID").ok(),
        // One door, two kinds of work: `--job pool-warm` is the machine's own bookkeeping, a bare
        // `bn queue add "fix the header"` is yours. What it *runs* answers that for a job or a
        // shell command; for an agent it cannot, so the producer says so — an enhance or triage run
        // is an agent grooming the task store, and reads as a person's work by every other signal.
        task: match args.job.is_some()
            || args.shell.is_some()
            || crate::cmd::queue_producers::machine_owned()
        {
            true => crate::cmd::task_mint::task_for_machine_enqueue(cfg, &repo, item, "queue"),
            false => crate::cmd::task_mint::task_for_enqueue(cfg, &repo, item, "queue"),
        },
        machine: crate::cmd::queue_producers::machine_owned(),
        exec,
        requires: Requirements {
            harness: args.harness,
            capabilities: args.requires,
            excludes: args.excludes,
            ..Default::default()
        },
        // Carry the launch identity the caller declared. Without it a queued agent starts on a
        // branch slugified from its own subject, under a name no already-running guard would
        // recognise — the exact loss `AgentSpec` exists to prevent.
        agent: bn_core::backlog::AgentSpec {
            name: args.name.map(str::to_string),
            yolo: args.yolo,
            ..Default::default()
        },
        est_secs: args.est_secs,
        ..Default::default()
    };
    // A job is recovery or heavy because of what it *is*, not because of the door it came through.
    // Without this, `--job agent-supervise` produced an entry indistinguishable from the captured
    // one that was quietly not exempt from pause. An explicit `--priority` still wins.
    let mut spec = spec;
    if let Some(job) = args.job {
        bn_core::queue::jobs::classify(&mut spec, job, args.priority == 0);
    }
    let path = backlog::backlog_path();
    let added = backlog::add(
        &path,
        &repo,
        std::slice::from_ref(&item.to_string()),
        &spec,
        now,
    )
    .unwrap_or_else(|e| fail(&format!("bn queue add: could not update backlog: {e}")));

    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-add",
                serde_json::json!({ "added": added.len(), "entries": added })
            )
            .to_json()
        );
        return;
    }
    match added.first() {
        None => println!("already queued — {item}"),
        Some(id) => println!(
            "queued {repo}/{id} · {} · priority {}{}",
            spec.kind.label(),
            spec.priority,
            describe_requirements(&spec.requires)
        ),
    }
}

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

pub(super) fn kind_choices() -> String {
    BacklogKind::ALL
        .iter()
        .map(|k| k.label())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn parse_status(s: &str) -> Option<BacklogStatus> {
    match s.trim().to_ascii_lowercase().as_str() {
        "pending" => Some(BacklogStatus::Pending),
        "leased" => Some(BacklogStatus::Leased),
        "done" => Some(BacklogStatus::Done),
        "failed" => Some(BacklogStatus::Failed),
        _ => None,
    }
}

pub(super) fn repo_counts(pending: &[&BacklogEntry]) -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for e in pending {
        *counts.entry(e.repo.clone()).or_default() += 1;
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

pub(super) fn summarize(items: &[String], max: usize) -> String {
    if items.len() <= max {
        return items.join(", ");
    }
    format!("{}, +{} more", items[..max].join(", "), items.len() - max)
}

pub(super) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!(
        "{}…",
        s.chars().take(max.saturating_sub(1)).collect::<String>()
    )
}

pub(super) fn fmt_secs(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}
