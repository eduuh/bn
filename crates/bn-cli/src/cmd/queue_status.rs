//! `bn queue status` — the machine-wide picture, and the answer to "why is nothing running".
//!
//! Split out of [`super::queue`] when that file crossed the 1000-line ratchet. It is the right seam
//! rather than a convenient one: the rest of `queue.rs` reads and writes single entries, while
//! everything here renders *the machine* — whether work is being filed at all, what is holding it,
//! how full the lanes and the build pool are, and which nodes could take any of it.
//!
//! The order the sections print in is the order the questions get asked, and it is deliberate:
//! whether the queue is even the path work takes comes first, because every number below it is
//! conditional on that answer.
use crate::*;
use bn_core::backlog::{self, BacklogEntry, BacklogKind, BacklogStatus};
use bn_core::queue::kind::WorkClass;
// The row vocabulary the two surfaces share: a status line and a `list` row must not disagree
// about what class an entry is in, so both spell it from the same helpers.
use super::queue::{class_json, class_label, fmt_secs, repo_counts, summarize, truncate};

/// `bn queue status` — pending work by kind and lane, and what each node offers.
pub(super) fn emit_status(cfg: &Config, repo: Option<&str>, now: i64, json: bool) {
    let backlog = backlog::load(&backlog::backlog_path());
    let pending: Vec<&BacklogEntry> = backlog
        .entries()
        .iter()
        .filter(|e| cfg.repo_allowed_here(&e.repo))
        .filter(|e| e.status == BacklogStatus::Pending)
        .filter(|e| repo.is_none_or(|r| e.repo.eq_ignore_ascii_case(r)))
        .collect();
    let active: Vec<&BacklogEntry> = backlog
        .entries()
        .iter()
        .filter(|e| cfg.repo_allowed_here(&e.repo))
        .filter(|e| e.status == BacklogStatus::Leased)
        .filter(|e| repo.is_none_or(|r| e.repo.eq_ignore_ascii_case(r)))
        .collect();
    let nodes = node_rows(cfg);

    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-status",
                status_json(cfg, &pending, &active, &nodes, now)
            )
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

    // A wedged dispatcher looks exactly as alive as a healthy one: it is holding its lock, and the
    // kernel holds that for as long as the process exists however blocked it is. The pass stamp is
    // what tells them apart, and the terminal says so in the same breath as the dashboard rather
    // than leaving the two surfaces to disagree about a live-but-useless daemon.
    let stalled = bn_core::resolver::dispatcher_pass_age(now)
        .is_some_and(|age| age > bn_core::resolver::DISPATCHER_STALL_SECS);
    if let Some(off) = bn_core::queue::off::active(now) {
        // Named apart from `capture off` below because the remedy differs: this one is a switch
        // somebody flipped and can flip back, and it is the likelier of the two to be a surprise.
        let why = match off.reason.is_empty() {
            true => String::new(),
            false => format!(" — {}", off.reason),
        };
        println!("\u{f05e} queue OFF{why} — every trigger runs immediately, nothing is filed");
        match off.remaining(now) {
            Some(left) => println!(
                "    back on in {} (or `bn queue on`) · queued work still drains",
                fmt_secs(left)
            ),
            None => println!("    `bn queue on` to file work again · queued work still drains"),
        }
    } else if captured.is_empty() {
        println!("\u{f05e} capture off — every producer launches directly");
    } else if alive && stalled {
        println!(
            "\u{f071} dispatcher STUCK — alive, but no pass has finished in {}",
            fmt_secs(bn_core::resolver::dispatcher_pass_age(now).unwrap_or(0))
        );
        println!(
            "    it is blocked inside something it started — `bn queue why` has the last pass"
        );
    } else if alive {
        // The pid is advisory (the flock is what owns the machine), but it is the thing an operator
        // reaches for next — to `kill` it, or to find it in `ps`.
        match bn_core::queue::daemon::holder_pid() {
            Some(pid) => println!(
                "\u{f0e7} dispatcher live (pid {pid}) · capturing {}",
                captured.join(", ")
            ),
            None => println!(
                "\u{f0e7} dispatcher live · capturing {}",
                captured.join(", ")
            ),
        }
    } else {
        println!(
            "\u{f071} dispatcher NOT running — {} would queue but are launching directly",
            captured.join(", ")
        );
        println!("    start one with `bn queue run` — or any `bn` command will start it for you");
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
