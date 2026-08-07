//! The `/api/queue` viewmodel.
//!
//! Split out of [`super`] when that file hit the 1000-line ratchet, and it is the right seam
//! anyway: this is the only payload that folds the dispatcher's own state — capacity, liveness,
//! the build pool — rather than reshaping a note or a run.
use serde_json::Value;

/// `GET /api/queue` / `bn queue status --json` — the machine-wide queue view.
///
/// One builder for both surfaces: a dashboard that computes its own version of "what is queued"
/// is a dashboard that will eventually disagree with the command line about it.
pub fn queue_data(cfg: &crate::resolver::Config, now: i64) -> Value {
    use crate::queue::kind::{BacklogKind, WorkClass};
    let snap = crate::queue::snapshot::Snapshot::capture(cfg, now);
    serde_json::json!({
        "now": now,
        "paused": snap.pauses.is_paused(),
        "fully_paused": snap.pauses.is_fully_paused(),
        "pauses": snap.pauses.pauses,
        "pending": snap.pending.len(),
        "kinds": BacklogKind::ALL.iter().map(|k| serde_json::json!({
            "kind": k.label(),
            "lane": WorkClass::from_kind(*k).label(),
            "pending": snap.pending.iter().filter(|e| e.kind == *k).count(),
        })).collect::<Vec<_>>(),
        "entries": snap.pending,
        "nodes": snap.nodes.iter().map(|n| node_data(n, now)).collect::<Vec<_>>(),
        // The state of the machine itself — what the page is *for*.
        //
        // "Why is nothing running" is answered by four facts, and until now the payload carried
        // none of them: whether a dispatcher is even alive (if not, nothing below it matters and
        // producers are launching direct), how full each lane is, how full the build pool is, and
        // which priority windows are open. They were computed for `bn queue status` and nowhere
        // else, so the terminal could answer the question and the dashboard could not.
        "machine": {
            "dispatcher_alive": crate::resolver::dispatcher_alive(now),
            "captured": crate::resolver::ALL_PRODUCERS.iter()
                .filter(|p| cfg.queue.captures(p))
                .collect::<Vec<_>>(),
            "lanes": WorkClass::ALL.iter().map(|lane| {
                let (coding, research) = cfg.queue.lanes();
                serde_json::json!({
                    "lane": lane.label(),
                    "cap": if *lane == WorkClass::Coding { coding } else { research },
                    "used": snap.in_flight.iter().filter(|f| f.work_class == *lane).count(),
                })
            }).collect::<Vec<_>>(),
            "lanes_why": crate::queue::capacity::observe().2,
            "builds": {
                "cap": crate::queue::build::observe_slots(),
                "held": crate::queue::build::holders_live(now),
            },
            "windows": crate::queue::window::open(&cfg.queue.windows, now),
        },
        "decision": crate::queue::decision::QueueDecision::read(),
        // The reservoir the backlog draws from — the half of the queue this payload never carried.
        //
        // Since #1001 the refill lands as *tasks* and only reaches the backlog once it has passed
        // the SMART gate, so a page fed by the backlog alone shows an empty queue no matter how
        // much work is waiting: `keep-warm` tops the store up, `bn task pull` mints, and none of it
        // is visible until something dispatches. That is the gap that made this page read as broken.
        "reservoir": reservoir(cfg, now),
    })
}

/// One worker node as the queue dashboard needs to see it.
///
/// The persisted registry carries both scheduler slots and a point-in-time resource sample. Keep
/// them distinct in the payload: lane caps are global queue limits, while `free` is this node's
/// worker headroom. A lapsed heartbeat is rendered offline even if the last persisted status still
/// says ready, while `reported_status` preserves that last observation for diagnosis.
pub fn node_data(node: &crate::hosts::NodeRecord, now: i64) -> Value {
    let axes = crate::worker::advertise::split_labels(&node.labels);
    let stale = crate::hosts::is_stale(node, now, crate::hosts::DEFAULT_HEARTBEAT_TTL_SECS);
    let reported_status = format!("{:?}", node.status).to_lowercase();
    let slots = node.slots.max(1);
    serde_json::json!({
        "node_id": node.node_id,
        "target": node.target.label(),
        "status": if stale { "offline" } else { reported_status.as_str() },
        "reported_status": reported_status,
        "stale": stale,
        "last_heartbeat": node.last_heartbeat,
        "heartbeat_age_secs": (node.last_heartbeat > 0)
            .then(|| now.saturating_sub(node.last_heartbeat)),
        "slots": slots,
        "busy": node.active_attempts,
        "free": slots.saturating_sub(node.active_attempts),
        "skills": axes.skills,
        "harnesses": axes.harnesses,
        "labels": node.labels,
        "owned_repos": node.owned_repos,
        "ram_total_mb": node.ram_total_mb,
        "ram_reserved_mb": node.ram_reserved_mb,
        "capacity": node.capacity,
    })
}

/// Open tasks grouped by how far they are from being executable.
///
/// Counts only — the list is `/api/tasks`, which already filters and is asked for far less often.
/// This rides the SSE frame on every dispatcher change, so it stays four integers rather than a
/// growing array.
///
/// An unreadable store is reported as `null` rather than zeroes. "No tasks" and "could not read the
/// task store" are different facts, and a page that renders the second as the first is exactly the
/// kind of quiet emptiness this whole payload exists to stop telling.
fn reservoir(cfg: &crate::resolver::Config, now: i64) -> Value {
    let Ok(file) = crate::todo::load(&cfg.tasks_file()) else {
        return Value::Null;
    };
    reservoir_of(&file, &crate::util::local_date_of(now))
}

/// The reservoir fold itself, over an already-loaded store.
///
/// Split from [`reservoir`] so the classification — the part with the actual semantics — can be
/// tested without a `Config`, a tasks file on disk, or the process/disk probes a full
/// [`queue_data`] does. `today` decides what counts as open: a snoozed task is deliberately not
/// waiting on anything.
pub fn reservoir_of(file: &crate::todo::TodoFile, today: &str) -> Value {
    let (mut needs_scoping, mut ready, mut stuck, mut failed) = (0usize, 0usize, 0usize, 0usize);
    for task in file.tasks().filter(|t| t.is_open(today)) {
        // `smart:` absent means unjudged, which is where a freshly pulled task starts and where
        // the overwhelming majority of a healthy reservoir sits.
        match task.meta(crate::pipeline::gate::SMART_KEY) {
            Some("ok") => ready += 1,
            Some("stuck") => stuck += 1,
            Some("fail") => failed += 1,
            _ => needs_scoping += 1,
        }
    }
    serde_json::json!({
        "needs_scoping": needs_scoping,
        "ready": ready,
        "failed": failed,
        // Waiting on a person: the improve agent has given up on these.
        "stuck": stuck,
        "total": needs_scoping + ready + failed + stuck,
    })
}
