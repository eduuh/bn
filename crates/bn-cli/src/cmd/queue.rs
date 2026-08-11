//! `bn queue` — the machine-wide work queue.
//!
//! Reads and writes only; there is no dispatcher here yet. What this surface is *for* is answering
//! "what is waiting, what does it need, and is anything allowed to start" from one place, across
//! every repo and every kind of agent — a question the machine previously had no way to answer
//! because each producer kept its own admission logic to itself.
use crate::*;
use bn_core::backlog::{self, AddSpec, BacklogEntry, BacklogKind, BacklogStatus};
use bn_core::queue::kind::{ExecSpec, Requirements, WorkClass};

/// Dispatch a `bn queue` invocation.
pub(crate) fn route(cfg: &Config, action: QueueCmd, json: bool) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    match action {
        QueueCmd::Status { repo } => emit_status(cfg, repo.as_deref(), now, json),
        QueueCmd::List {
            kind,
            repo,
            class_filter,
            status,
            all,
        } => emit_list(
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
        QueueCmd::Run { interval } => crate::cmd::queue_tick::emit_run(cfg, interval),
        QueueCmd::Pause {
            scope,
            reason,
            for_,
        } => crate::cmd::queue_pause::emit_pause(&scope, &reason, for_.as_deref(), now, json),
        QueueCmd::Resume { scope } => crate::cmd::queue_pause::emit_resume(&scope, now, json),
        QueueCmd::Why => crate::cmd::queue_tick::emit_why(json),
        QueueCmd::Badge => emit_badge(now, json),
        QueueCmd::Log {
            since,
            limit,
            kind,
            repo,
        } => crate::cmd::queue_log::emit_log(
            &since,
            limit,
            kind.as_deref(),
            repo.as_deref(),
            now,
            json,
        ),
        QueueCmd::Timeline { date, class_filter } => {
            crate::cmd::queue_timeline::emit_timeline(
                cfg,
                date.as_deref(),
                class_filter.as_deref(),
                json,
            )
        }
        QueueCmd::Finish {
            repo,
            id,
            exit_code,
        } => crate::cmd::queue_tick::emit_finish(&repo, &id, exit_code, json),
        QueueCmd::Nodes { skill } => {
            crate::cmd::queue_tick::emit_nodes(cfg, skill.as_deref(), json)
        }
    }

    fn emit_badge(now: i64, json: bool) {
        let decision = bn_core::queue::decision::QueueDecision::read();
        let stale = decision
            .as_ref()
            .is_none_or(|d| d.dry_run || now.saturating_sub(d.at) > 180);
        let backlog = backlog::load(&backlog::backlog_path());
        let entries = backlog.entries();
        let failed = entries
            .iter()
            .filter(|e| e.status == BacklogStatus::Failed)
            .filter(|e| e.updated_at == 0 || now.saturating_sub(e.updated_at) <= 86_400)
            .count();
        let foreground_pending = entries
            .iter()
            .filter(|e| e.status == BacklogStatus::Pending)
            .filter(|e| !bn_core::queue::classify::classify(e).mode.is_autonomous())
            .count();
        let foreground_running = entries
            .iter()
            .filter(|e| e.status == BacklogStatus::Leased)
            .filter(|e| !bn_core::queue::classify::classify(e).mode.is_autonomous())
            .count();
        let held = decision
            .as_ref()
            .filter(|d| d.outcome == "held" && d.pending > 0)
            .and_then(attention_hold)
            .unwrap_or_default();
        let text = if failed > 0 {
            format!("Q !{failed}")
        } else if stale {
            "Q stale".to_string()
        } else if matches!(held, "no-capable-node" | "cant-merge" | "conflict") {
            format!("Q !{held}")
        } else if foreground_running > 0 || foreground_pending > 0 {
            let mut parts = Vec::new();
            if foreground_running > 0 {
                parts.push(format!("{foreground_running}▶"));
            }
            if foreground_pending > 0 {
                parts.push(format!("{foreground_pending}·fg"));
            }
            format!("Q {}", parts.join(" "))
        } else {
            String::new()
        };
        if json {
            println!(
                "{}",
                Envelope::success(
                    "queue-badge",
                    serde_json::json!({
                        "text": text,
                        "stale": stale,
                        "failed": failed,
                        "foreground_pending": foreground_pending,
                        "foreground_running": foreground_running,
                        "held": held,
                    }),
                )
                .to_json()
            );
            return;
        }
        if !text.is_empty() {
            println!("{text}");
        }
    }
}

fn attention_hold(d: &bn_core::queue::decision::QueueDecision) -> Option<&str> {
    d.bind
        .as_deref()
        .filter(|bind| matches!(*bind, "no-capable-node" | "cant-merge" | "conflict"))
        .or_else(|| {
            d.deferred
                .iter()
                .map(|row| row.reason.as_str())
                .find(|reason| matches!(*reason, "no-capable-node" | "cant-merge" | "conflict"))
        })
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
    let repo = repo.map(str::to_string).unwrap_or_else(|| resolve_or_exit(cfg).repo);
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
    let repo = repo.map(str::to_string).unwrap_or_else(|| resolve_or_exit(cfg).repo);
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
        println!("{repo}/{id} snoozed until {}", bn_core::util::clock_hm(until));
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
// status
// ---------------------------------------------------------------------------------------------

/// `bn queue status` — pending work by kind and lane, and what each node offers.
fn emit_status(cfg: &Config, repo: Option<&str>, now: i64, json: bool) {
    let backlog = backlog::load(&backlog::backlog_path());
    let pending: Vec<&BacklogEntry> = backlog
        .entries()
        .iter()
        .filter(|e| e.status == BacklogStatus::Pending)
        .filter(|e| repo.is_none_or(|r| e.repo.eq_ignore_ascii_case(r)))
        .collect();
    let active: Vec<&BacklogEntry> = backlog
        .entries()
        .iter()
        .filter(|e| e.status == BacklogStatus::Leased)
        .filter(|e| repo.is_none_or(|r| e.repo.eq_ignore_ascii_case(r)))
        .collect();
    let nodes = node_rows(cfg);

    if json {
        println!(
            "{}",
            Envelope::success("queue-status", status_json(cfg, &pending, &active, &nodes, now))
                .to_json()
        );
        return;
    }

    if backlog.unreadable > 0 {
        // Entries that could not be read are entries that have vanished. Loud, because a queue
        // quietly losing work is the failure that matters most.
        println!(
            "\u{f071} {} unreadable line(s) in backlog.jsonl — that many entries are lost",
            backlog.unreadable
        );
    }
    print_dispatcher(cfg, now);
    print_active(&active, now);
    print_pending(&pending);
    print_nodes(&nodes);
}

/// Whether the queue is actually the path work takes right now, and what bounds it.
///
/// The first thing to print, because everything below it is conditional on this: capture without a
/// live dispatcher means producers are quietly launching direct, and a reader who does not know
/// that will misread every number under it.
fn print_dispatcher(cfg: &Config, now: i64) {
    let alive = bn_core::resolver::dispatcher_alive(now);
    let (coding, research, why) = bn_core::queue::capacity::observe();
    let captured: Vec<&str> = bn_core::resolver::ALL_PRODUCERS
        .iter()
        .copied()
        .filter(|p| cfg.queue.captures(p))
        .collect();

    // A wedged dispatcher heartbeats like a healthy one — the heartbeat is a timer thread, so that
    // it keeps reporting alive while a pass is blocked is by design. The pass stamp is what tells
    // them apart, and the terminal says so in the same breath as the dashboard rather than leaving
    // the two surfaces to disagree about a live-but-useless daemon.
    let stalled = bn_core::resolver::dispatcher_pass_age(now)
        .is_some_and(|age| age > bn_core::resolver::DISPATCHER_STALL_SECS);
    if captured.is_empty() {
        println!("\u{f05e} capture off — every producer launches directly");
    } else if alive && stalled {
        println!(
            "\u{f071} dispatcher STUCK — alive, but no pass has finished in {}",
            fmt_secs(bn_core::resolver::dispatcher_pass_age(now).unwrap_or(0))
        );
        println!("    it is blocked inside something it started — `bn queue why` has the last pass");
    } else if alive {
        println!("\u{f0e7} dispatcher live · capturing {}", captured.join(", "));
    } else {
        println!(
            "\u{f071} dispatcher NOT running — {} would queue but are launching directly",
            captured.join(", ")
        );
        println!("    register it with `bn cron register` (queue-tick, every minute)");
    }
    let open = bn_core::queue::window::open(&cfg.queue.windows, now);
    if !open.is_empty() {
        for w in &open {
            let left = w
                .closes_in(now)
                .map(|s| format!(" · {} left", fmt_secs(s)))
                .unwrap_or_default();
            println!(
                "    window '{}' open ({:?}) — {:+} to {}{left}",
                w.name,
                w.mode,
                w.boost,
                w.matches.join(", ")
            );
        }
    }
    let idle = bn_core::idle::is_idle(cfg, now, cfg.queue.opportunistic_idle_mins);
    let away = cfg
        .queue
        .windows
        .iter()
        .any(|w| w.mode == bn_core::queue::window::WindowMode::Away && w.is_open(now));
    println!(
        "    presence: {}{} — opportunistic work {}",
        if idle { "idle" } else { "active" },
        if away { " · away window" } else { "" },
        if idle || away { "allowed" } else { "waiting" }
    );
    let (c, r) = cfg.queue.lanes();
    let derived = if cfg.queue.coding.is_some() || cfg.queue.research.is_some() {
        format!("configured (machine would give {coding}/{research})")
    } else {
        why
    };
    println!("    lanes: {c} coding · {r} research — {derived}");
    print_held(cfg);
    print_builds(now);
}

/// Report pipeline work waiting on **you**.
///
/// The point of `/queue` and `bn queue status` is answering "why is nothing running" (#1135), and a
/// held stage is now one of the real answers. Without this line a gate is indistinguishable from an
/// empty queue: the machine is idle, the lanes are free, and the reason is a review sitting in
/// someone's inbox. A hold nobody can see is the failure mode the gate must not have.
fn print_held(cfg: &Config) {
    let held: Vec<_> = crate::cmd::pipeline_sweep::sweep_tasks(cfg)
        .into_iter()
        .filter(|s| s.outcome == "held")
        .collect();
    if held.is_empty() {
        return;
    }
    println!(
        "    held: {} task(s) waiting on your plan review — `bn review` to decide",
        held.len()
    );
    for s in &held {
        println!(
            "      {} · {} · {}",
            s.task,
            s.repo,
            s.stage.as_deref().unwrap_or("-")
        );
    }
}

/// The build pool, printed beside the lanes because it is the same question — what bounds this
/// machine — asked about a different resource.
///
/// Separate from the lanes on purpose: a verify gate saturates every core, where an agent mostly
/// waits on the model, and a gate's caller blocks on the verdict where a dispatched agent's does
/// not. See [`bn_core::queue::build`] for why sharing the lanes would deadlock.
fn print_builds(now: i64) {
    let cap = bn_core::queue::build::observe_slots();
    let held = bn_core::queue::build::holders(now, &crate::cmd::queue_tick::pid_alive);
    if held.is_empty() {
        println!("    builds: 0/{cap} — no verify gate running");
        return;
    }
    let who = held
        .iter()
        .map(|h| format!("{}/{}", h.repo, h.label))
        .collect::<Vec<_>>()
        .join(", ");
    println!("    builds: {}/{cap} — {who}", held.len());
}

fn status_json(
    cfg: &Config,
    pending: &[&BacklogEntry],
    active: &[&BacklogEntry],
    nodes: &[NodeRow],
    now: i64,
) -> serde_json::Value {
    let idle = bn_core::idle::is_idle(cfg, now, cfg.queue.opportunistic_idle_mins);
    let away_window = cfg
        .queue
        .windows
        .iter()
        .any(|w| w.mode == bn_core::queue::window::WindowMode::Away && w.is_open(now));
    serde_json::json!({
        "now": now,
        "accepting": true,
        "pending": pending.len(),
        "kinds": BacklogKind::ALL.iter().map(|k| serde_json::json!({
            "kind": k.label(),
            "lane": WorkClass::from_kind(*k).label(),
            "pending": pending.iter().filter(|e| e.kind == *k).count(),
        })).collect::<Vec<_>>(),
        "active": active.iter().map(|e| serde_json::json!({
            "entry": e,
            "class": class_json(e),
            "age_secs": now.saturating_sub(e.updated_at),
        })).collect::<Vec<_>>(),
        "lanes": WorkClass::ALL.iter().map(|lane| serde_json::json!({
            "lane": lane.label(),
            "pending": pending.iter()
                .filter(|e| WorkClass::from_kind(e.kind) == *lane).count(),
        })).collect::<Vec<_>>(),
        "repos": repo_counts(pending).into_iter().map(|(repo, n)| serde_json::json!({
            "repo": repo,
            "pending": n,
        })).collect::<Vec<_>>(),
        "nodes": nodes,
        "builds": {
            "cap": bn_core::queue::build::observe_slots(),
            "held": bn_core::queue::build::holders(now, &crate::cmd::queue_tick::pid_alive),
        },
        "presence": {
            "idle": idle,
            "away_window": away_window,
            "opportunistic_allowed": idle || away_window,
            "opportunistic_idle_mins": cfg.queue.opportunistic_idle_mins,
        },
    })
}

fn print_active(active: &[&BacklogEntry], now: i64) {
    println!();
    if active.is_empty() {
        println!("\u{f04b} nothing running from the queue");
        return;
    }
    println!("\u{f04b} {} running", active.len());
    for e in active {
        let node = e.assigned_to.as_deref().unwrap_or("local");
        println!(
            "  {:<13} {:<8} {:<8} {:<18} {:>5}  {}",
            class_label(e),
            e.exec.label(),
            node,
            format!("{}/{}", truncate(&e.repo, 8), truncate(&e.id, 9)),
            fmt_secs(now.saturating_sub(e.updated_at)),
            truncate(&e.title(), 44),
        );
    }
}

fn print_pending(pending: &[&BacklogEntry]) {
    println!();
    if pending.is_empty() {
        println!("\u{f0ae} nothing pending");
        return;
    }
    println!("\u{f0ae} {} pending", pending.len());
    for kind in BacklogKind::ALL {
        let n = pending.iter().filter(|e| e.kind == kind).count();
        if n == 0 {
            continue;
        }
        let lane = WorkClass::from_kind(kind).label();
        println!("  {:<12} {n:>3}  ({lane})", kind.label());
    }
    let repos = repo_counts(pending);
    if repos.len() > 1 {
        println!(
            "  across {} repos: {}",
            repos.len(),
            repos
                .iter()
                .map(|(repo, n)| format!("{repo} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

fn print_nodes(nodes: &[NodeRow]) {
    println!();
    println!("\u{f013} {} node(s)", nodes.len());
    for node in nodes {
        println!(
            "  {:<18} {:<8} {:<14} slots {}",
            node.node_id, node.status, node.target, node.slots
        );
        if !node.harnesses.is_empty() {
            println!("    harness  {}", node.harnesses.join(", "));
        }
        if node.skills.is_empty() {
            println!("    skills   (none advertised)");
        } else {
            println!("    skills   {}", summarize(&node.skills, 8));
        }
    }
}

/// One registry node, flattened for display and JSON.
#[derive(Debug, Clone, serde::Serialize)]
struct NodeRow {
    node_id: String,
    target: String,
    status: String,
    slots: usize,
    /// Whether this is the box the command ran on.
    local: bool,
    skills: Vec<String>,
    harnesses: Vec<String>,
    mcps: Vec<String>,
    other: Vec<String>,
}

/// Every node the scheduler could place on, **including a synthesized row for this box**.
///
/// The local box has always been an implicit target with no registry record, which meant it
/// advertised nothing and could therefore never satisfy a requirement. Synthesizing a row is what
/// makes a single-machine setup — i.e. most of them — show its real skills instead of an empty
/// table. Stage 3 promotes this from a display concern to a placement one.
fn node_rows(cfg: &Config) -> Vec<NodeRow> {
    let base = std::env::current_dir().unwrap_or_default();
    let labels = bn_core::worker::advertise::advertised_labels(cfg, &base);
    let axes = bn_core::worker::advertise::split_labels(&labels);
    let mut rows = vec![NodeRow {
        node_id: cfg.machine().to_string(),
        target: "local".to_string(),
        status: "ready".to_string(),
        slots: 1,
        local: true,
        skills: axes.skills,
        harnesses: axes.harnesses,
        mcps: axes.mcps,
        other: axes.other,
    }];
    for node in &cfg.nodes {
        if node.node_id.eq_ignore_ascii_case(cfg.machine()) {
            continue;
        }
        let axes = bn_core::worker::advertise::split_labels(&node.labels);
        rows.push(NodeRow {
            node_id: node.node_id.clone(),
            target: node.target.label(),
            status: format!("{:?}", node.status).to_lowercase(),
            slots: 1,
            local: false,
            skills: axes.skills,
            harnesses: axes.harnesses,
            mcps: axes.mcps,
            other: axes.other,
        });
    }
    rows
}

// ---------------------------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------------------------

/// `bn queue list` — entries with their kind, priority, executor and requirements.
fn emit_list(
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
    want.is_empty()
        || {
            let class = bn_core::queue::classify::classify(entry);
            class.mode.label() == want || class.subtype.label() == want
        }
}

fn class_label(entry: &BacklogEntry) -> &'static str {
    let class = bn_core::queue::classify::classify(entry);
    if class.subtype.label() != "foreground" {
        return class.subtype.label();
    }
    class.mode.label()
}

fn class_json(entry: &BacklogEntry) -> serde_json::Value {
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
fn describe_requirements(req: &Requirements) -> String {
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
    if args.job_repo.is_some() != args.job_branch.is_some() {
        fail("bn queue add: --job-repo and --job-branch must be given together");
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
    let added = backlog::add(&path, &repo, std::slice::from_ref(&item.to_string()), &spec, now)
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

fn kind_choices() -> String {
    BacklogKind::ALL
        .iter()
        .map(|k| k.label())
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_status(s: &str) -> Option<BacklogStatus> {
    match s.trim().to_ascii_lowercase().as_str() {
        "pending" => Some(BacklogStatus::Pending),
        "leased" => Some(BacklogStatus::Leased),
        "done" => Some(BacklogStatus::Done),
        "failed" => Some(BacklogStatus::Failed),
        _ => None,
    }
}

fn repo_counts(pending: &[&BacklogEntry]) -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for e in pending {
        *counts.entry(e.repo.clone()).or_default() += 1;
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

fn summarize(items: &[String], max: usize) -> String {
    if items.len() <= max {
        return items.join(", ");
    }
    format!(
        "{}, +{} more",
        items[..max].join(", "),
        items.len() - max
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
}

fn fmt_secs(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}
