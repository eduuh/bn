//! The reads the app shell makes on *every* navigation, all served through [`crate::view_cache`].
//!
//! These five are grouped by who asks for them rather than by what they describe. The rail and the
//! topbar are mounted on every screen, so opening any page fires all of them at once — which made
//! them, collectively, the page load. Three of them (`fleet`, `machine`, `nodes`) each ran a full
//! `agent::supervise`; `/api/repos` ran a fourth. One navigation supervised the live fleet four
//! times over.
//!
//! They live together here because they share one property that matters: each is a pure read whose
//! cost is paid per navigation rather than per change, which is exactly the shape a TTL cache
//! fixes. The per-view windows and the reasoning behind each are in [`crate::view_cache`].

use crate::state::AppState;
use axum::{extract::State, response::Json};
use bn_core::render::Envelope;
use serde_json::json;

/// The failure envelope for a view whose computation panicked on the blocking pool. Rare, and
/// deliberately distinct from a view that legitimately answered "nothing" — a handler that
/// swallowed this would report an empty fleet as though the machine were idle.
fn view_panicked(command: &'static str, message: &'static str) -> Json<Envelope> {
    Json(Envelope::failure(command, "view_panicked", message))
}

/// `GET /api/fleet` — the live agent fleet + health counts (design §2.1), plus the machine's
/// harness availability (#501) so the dashboard's per-agent switcher can render each option
/// usable/disabled without a second read.
pub async fn fleet(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let data = state
        .views
        .get_or_compute(crate::view_cache::FLEET, move || {
            let fleet = bn_core::agent::supervise(
                &cfg,
                bn_core::agent::DEFAULT_IDLE_MINS,
                bn_core::agent::DEFAULT_STUCK_MINS,
            );
            let mut data = bn_core::payload::agent_fleet_data(&fleet);
            super::enrich_fleet_plans(&cfg, &mut data);
            data["harnesses"] = json!(bn_core::harness::availability(&cfg));
            data
        })
        .await;
    match data {
        Some(data) => Json(Envelope::success("fleet", (*data).clone())),
        None => view_panicked("fleet", "supervising the fleet failed"),
    }
}

/// `GET /api/machine` (#525) — a cheap, cross-platform snapshot of the host the fleet runs on:
/// CPU load (1/5/15 + cores + a normalized per-core gauge), RAM (free/used/total + swap-used, the
/// real OOM signal), the live active-agent count, and a derived green/amber/red saturation state.
/// Surfaces "near saturation" *before* the machine melts (this session it saturated twice), and
/// shares thresholds with #503 dispatch backpressure.
pub async fn machine(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let data = state
        .views
        .get_or_compute(crate::view_cache::MACHINE, move || {
            let snapshot = bn_core::machine::sample();
            let fleet = bn_core::agent::supervise(
                &cfg,
                bn_core::agent::DEFAULT_IDLE_MINS,
                bn_core::agent::DEFAULT_STUCK_MINS,
            );
            bn_core::payload::machine_health_data(&snapshot, fleet.running_count())
        })
        .await;
    match data {
        Some(data) => Json(Envelope::success("machine", (*data).clone())),
        None => view_panicked("machine", "sampling the machine failed"),
    }
}

/// `GET /api/nodes` — the fleet's machines as a *list*. Today that is exactly one entry — the
/// local host — but the shape is a list from day one so registered remote nodes can join without
/// a UI contract change: each entry is the `/api/machine` snapshot plus identity (`host`,
/// `platform`, `arch`), a reachability `status`, and a `local` marker for the node running this
/// server. Remote entries will carry `status: "unreachable"` when their heartbeat lapses.
pub async fn nodes(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let data = state
        .views
        .get_or_compute(crate::view_cache::NODES, move || {
            let snapshot = bn_core::machine::sample();
            let fleet = bn_core::agent::supervise(
                &cfg,
                bn_core::agent::DEFAULT_IDLE_MINS,
                bn_core::agent::DEFAULT_STUCK_MINS,
            );
            let mut node = bn_core::payload::machine_health_data(&snapshot, fleet.running_count());
            node["host"] = json!(cfg.hostname);
            node["platform"] = json!(std::env::consts::OS);
            node["arch"] = json!(std::env::consts::ARCH);
            node["status"] = json!("online");
            node["local"] = json!(true);
            json!({ "nodes": [node] })
        })
        .await;
    match data {
        Some(data) => Json(Envelope::success("nodes", (*data).clone())),
        None => view_panicked("nodes", "listing the nodes failed"),
    }
}

/// `GET /api/attn` — open attention items for today (includes future plan-review requests).
///
/// Invalidated by every attention write (`attn_action`), so acking or snoozing an item clears it
/// from the list and the rail's badge at once rather than at the end of a window the user cannot
/// see.
pub async fn attn(State(state): State<AppState>) -> Json<Envelope> {
    let data = state
        .views
        .get_or_compute(crate::view_cache::ATTN, || {
            let items = bn_core::attention::open_items(&bn_core::util::today());
            bn_core::payload::attn_list_data(&items)
        })
        .await;
    match data {
        Some(data) => Json(Envelope::success("attn", (*data).clone())),
        None => view_panicked("attn", "reading attention items failed"),
    }
}

/// `GET /api/cron` — the machine-wide cron registry: every declared cron (`crons.toml`) joined
/// with its last-run health from the event log, plus run-only orphans. Manifest-owned schedules are
/// nested beneath `agent-scheduler`; malformed agent catalogs return an explicit failure.
///
/// The slowest read the shell makes — ~780ms, on the rail's critical path for *every* screen — and
/// one whose sources (a config file and a run log) move on the order of minutes, so it gets the
/// longest window of the five. The failure is cached alongside the success: re-reading a broken
/// catalog on every poll fixes nothing, and the envelope has to survive the round trip either way.
pub async fn cron(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let view = state
        .views
        .get_or_compute(crate::view_cache::CRON, move || {
            match bn_core::crons::status_views_with_agents(&cfg, None) {
                Ok(views) => json!({ "ok": bn_core::payload::crons_view_data(&views) }),
                Err(error) => json!({ "err": error.to_string() }),
            }
        })
        .await;

    // Discriminate on *which key is present*, never on the shape of the value — `crons_view_data`
    // is free to return whatever JSON it likes without that being mistaken for an error.
    let Some(view) = view.as_deref() else {
        return view_panicked("cron", "building the cron registry failed");
    };
    match (view.get("ok"), view.get("err")) {
        (Some(data), _) => Json(Envelope::success("cron", data.clone())),
        (None, Some(err)) => Json(Envelope::failure(
            "cron",
            "invalid_catalog",
            err.as_str().unwrap_or("invalid cron catalog").to_string(),
        )),
        (None, None) => view_panicked("cron", "building the cron registry failed"),
    }
}
