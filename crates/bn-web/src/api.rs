//! The typed read API and the SSE live stream (design §2.1–§2.2).
//!
//! Every handler is a native in-process `bn-core` call wrapped in a [`render::Envelope`], so the
//! web API, the CLI `--json`, and MCP all speak one shape. `GET /api/stream` subscribes a client to
//! the shared [`crate::watch`] broadcast (one watcher feeds N clients). All reads; the write layer
//! lands in v2.

use crate::cost;
use crate::state::AppState;
use crate::watch::{self, StreamFrame};
use crate::{actions, bn_runner};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event as SseEvent, KeepAlive, Sse},
        IntoResponse, Json, Response,
    },
};
use bn_core::render::Envelope;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

/// Trim a query param to `None` when absent or empty, so `?repo=` reads the same as an omitted one.
fn nonempty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty())
}

/// A 404 carrying the standard failure envelope (JSON), for "no such live agent / note" reads.
fn not_found(command: &str, message: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(Envelope::failure(command, "not_found", message)),
    )
        .into_response()
}

/// A 400 carrying the standard failure envelope (JSON), for a read whose query params are missing
/// or malformed before any store is touched.
fn bad_request(command: &str, code: &str, message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(Envelope::failure(command, code, message)),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Fleet
// ---------------------------------------------------------------------------

/// `GET /api/bearings` — one bounded current-state artifact shared with `bn bearings` and the
/// live stream.
pub async fn bearings(State(state): State<AppState>) -> Json<Envelope> {
    let snapshot = bn_core::bearings::BearingsSnapshot::gather(
        &state.cfg,
        bn_core::util::now_epoch_secs(),
        &bn_core::util::today(),
    );
    Json(Envelope::success(
        "bearings",
        watch::bearings_data(&snapshot),
    ))
}

/// `GET /api/fleet` — the live agent fleet + health counts (design §2.1), plus the machine's
/// harness availability (#501) so the dashboard's per-agent switcher can render each option
/// usable/disabled without a second read.
pub async fn fleet(State(state): State<AppState>) -> Json<Envelope> {
    let fleet = bn_core::agent::supervise(
        &state.cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    );
    let mut data = bn_core::payload::agent_fleet_data(&fleet);
    data["harnesses"] = json!(bn_core::harness::availability(&state.cfg));
    Json(Envelope::success("fleet", data))
}

/// `GET /api/machine` (#525) — a cheap, cross-platform snapshot of the host the fleet runs on:
/// CPU load (1/5/15 + cores + a normalized per-core gauge), RAM (free/used/total + swap-used, the
/// real OOM signal), the live active-agent count, and a derived green/amber/red saturation state.
/// Surfaces "near saturation" *before* the machine melts (this session it saturated twice), and
/// shares thresholds with #503 dispatch backpressure.
pub async fn machine(State(state): State<AppState>) -> Json<Envelope> {
    let snapshot = bn_core::machine::sample();
    let fleet = bn_core::agent::supervise(
        &state.cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    );
    let data = bn_core::payload::machine_health_data(&snapshot, fleet.agents.len());
    Json(Envelope::success("machine", data))
}

/// `GET /api/nodes` — the fleet's machines as a *list*. Today that is exactly one entry — the
/// local host — but the shape is a list from day one so registered remote nodes can join without
/// a UI contract change: each entry is the `/api/machine` snapshot plus identity (`host`,
/// `platform`, `arch`), a reachability `status`, and a `local` marker for the node running this
/// server. Remote entries will carry `status: "unreachable"` when their heartbeat lapses.
pub async fn nodes(State(state): State<AppState>) -> Json<Envelope> {
    let snapshot = bn_core::machine::sample();
    let fleet = bn_core::agent::supervise(
        &state.cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    );
    let mut node = bn_core::payload::machine_health_data(&snapshot, fleet.agents.len());
    node["host"] = json!(state.cfg.hostname);
    node["platform"] = json!(std::env::consts::OS);
    node["arch"] = json!(std::env::consts::ARCH);
    node["status"] = json!("online");
    node["local"] = json!(true);
    Json(Envelope::success("nodes", json!({ "nodes": [node] })))
}

/// `GET /api/agents/{session_id}` — one live agent's detail: the fleet entry plus its most-recent
/// recorded session, last request text, the rendered branch `note` (typed frontmatter + prose
/// body + goal), and the machine's harness availability (#501, for the detail switcher) so the
/// dashboard can show the full session context in one read. `note` is `null` when the branch has
/// no note.
pub async fn agent_detail(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Response {
    let fleet = bn_core::agent::supervise(
        &state.cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    );
    let Some(agent) = fleet.agents.iter().find(|a| a.session_id == session_id) else {
        return not_found("agent", &format!("no live agent with session {session_id}"));
    };
    let latest = bn_core::agent::latest_session_for(&state.cfg, &agent.repo, &agent.branch);
    let last_request = bn_core::agent::last_request_for(&agent.session_id, &agent.harness);
    let note = resolve_note(&state, &agent.repo, &agent.branch);
    let data = json!({
        "agent": agent,
        "latest_session": latest,
        "last_request": last_request,
        "note": note,
        "harnesses": bn_core::harness::availability(&state.cfg),
    });
    Json(Envelope::success("agent", data)).into_response()
}

/// Read the branch note for a live agent's `repo`+`branch`, returning the same detail shape the
/// notes endpoint serves (or `Value::Null` when the branch has no note). The live fleet reports a
/// git branch (slashes), so it's normalized to the note key via [`resolve_target`] — the canonical
/// `(repo, branch)` → note-dir mapping — rather than duplicating the slash→dash rule here.
fn resolve_note(state: &AppState, repo: &str, branch: &str) -> Value {
    let Ok(ctx) = bn_core::resolver::resolve_target(
        &state.cfg,
        std::path::Path::new("."),
        Some(repo.to_string()),
        Some(branch.to_string()),
    ) else {
        return Value::Null;
    };
    match bn_core::store::read(&state.cfg.note_file(&ctx)) {
        Ok(note) => bn_core::payload::note_detail_data(&ctx.repo, &ctx.branch, &note),
        Err(_) => Value::Null,
    }
}

#[derive(Debug, Deserialize)]
pub struct PaneQuery {
    /// How many trailing pane lines to return (default 40, capped at 400).
    pub lines: Option<usize>,
}

/// `GET /api/agents/{target}/pane?lines=40` — the raw tmux-pane tail for one agent.
///
/// No longer what the dashboard reads: [`agent_activity`] replaced the pane mirror, because a
/// ~200-column pane can't be rendered in a browser-width grid and says nothing about an agent whose
/// window has closed. This is kept as the escape hatch the transcript genuinely can't cover — an
/// agent sitting on a TUI permission prompt or trust menu, which never reaches the transcript at
/// all. Same shape as `bn agent pane`, so a `curl` answers "what is it actually stuck on?".
///
/// The capture is a tmux spawn, so it runs through the CLI boundary (`bn agent pane` via
/// [`crate::bn_runner`]) rather than bn-web shelling tmux directly (design §5 blast-radius).
/// `target` is `repo:branch`; a validated, argv-interpolated field — never a shell.
pub async fn agent_pane(Path(target): Path<String>, Query(q): Query<PaneQuery>) -> Response {
    if !actions::valid_target(&target) {
        return (
            StatusCode::BAD_REQUEST,
            Json(Envelope::failure(
                "agent_pane",
                "invalid_target",
                "target must be repo:branch",
            )),
        )
            .into_response();
    }
    let lines = q.lines.unwrap_or(40).clamp(1, 400);
    let args = [
        "agent".to_string(),
        "pane".to_string(),
        target,
        "--lines".to_string(),
        lines.to_string(),
    ];
    let result = tokio::task::spawn_blocking(move || {
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        bn_runner::run(&refs, None)
    })
    .await;
    match result {
        Ok(rr) => (StatusCode::OK, Json(rr.envelope)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(Envelope::failure("agent_pane", "join_error", e.to_string())),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct ActivityQuery {
    /// How many trailing timeline entries to return (default 40, capped at 200).
    pub limit: Option<usize>,
}

/// `GET /api/agents/{session_id}/activity?limit=40` — the agent-detail page's activity feed: the
/// session transcript folded into prompts, agent prose, and collapsed tool runs. The read that
/// replaced the pane mirror as the primary supervise surface, because it reads a file on disk and
/// so still answers for an agent whose tmux window has closed.
///
/// Keyed by *session id*, not `repo:branch` — the transcript is per-session, and the detail page
/// already has the id. Runs through the CLI boundary (`bn agent activity` via
/// [`crate::bn_runner`]) like every other read. A session id is a harness-issued uuid; it's
/// validated here so a path segment can never widen into a filesystem traversal.
pub async fn agent_activity(
    Path(session_id): Path<String>,
    Query(q): Query<ActivityQuery>,
) -> Response {
    if !valid_session_id(&session_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(Envelope::failure(
                "agent_activity",
                "invalid_session",
                "session id must be alphanumeric with dashes",
            )),
        )
            .into_response();
    }
    let limit = q.limit.unwrap_or(40).clamp(1, 200);
    let args = [
        "agent".to_string(),
        "activity".to_string(),
        session_id,
        "--limit".to_string(),
        limit.to_string(),
    ];
    let result = tokio::task::spawn_blocking(move || {
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        bn_runner::run(&refs, None)
    })
    .await;
    match result {
        Ok(rr) => (StatusCode::OK, Json(rr.envelope)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(Envelope::failure(
                "agent_activity",
                "join_error",
                e.to_string(),
            )),
        )
            .into_response(),
    }
}

/// Whether `s` is shaped like a harness session id: a non-empty run of ASCII alphanumerics and
/// dashes, length-bounded. Both harnesses issue uuids; this rejects path separators, dots, and
/// anything else that could escape a transcript directory. Pure.
fn valid_session_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

// ---------------------------------------------------------------------------
// Orchestrate
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct OrchestrateQuery {
    /// Restrict to one repo's run (mirrors `bn orchestrate status --repo`); default = every run.
    pub repo: Option<String>,
}

/// `GET /api/orchestrate` — the `{runs:[…]}` viewmodel, byte-for-byte identical to
/// `bn orchestrate status --json`'s `data` (both build it from `payload::orchestrate_status_data`).
pub async fn orchestrate(Query(q): Query<OrchestrateQuery>) -> Json<Envelope> {
    let data = watch::orchestrate_runs_json(nonempty(q.repo).as_deref());
    Json(Envelope::success("orchestrate", data))
}

#[derive(Debug, Deserialize)]
pub struct RecoveryQuery {
    pub limit: Option<usize>,
    pub repo: Option<String>,
}

/// `GET /api/orchestrate/recovery?limit=50` — recovery events newest-first, across every run (or a
/// single `?repo=`), each row annotated with its `repo`.
pub async fn orchestrate_recovery(Query(q): Query<RecoveryQuery>) -> Json<Envelope> {
    let limit = q.limit.unwrap_or(50);
    let paths = match nonempty(q.repo) {
        Some(r) => vec![bn_core::orchestrate::state_path_for(&r)],
        None => bn_core::orchestrate::run_paths(),
    };
    let mut rows: Vec<Value> = Vec::new();
    for p in paths {
        if let Some(st) = bn_core::orchestrate::OrchestrateState::load(&p) {
            for ev in st.recovery.iter().rev() {
                if let Ok(Value::Object(mut m)) = serde_json::to_value(ev) {
                    m.insert("repo".into(), json!(st.repo));
                    rows.push(Value::Object(m));
                }
            }
        }
    }
    rows.sort_by(|a, b| {
        b["ts"]
            .as_i64()
            .unwrap_or(0)
            .cmp(&a["ts"].as_i64().unwrap_or(0))
    });
    rows.truncate(limit);
    Json(Envelope::success(
        "orchestrate-recovery",
        json!({ "recovery": rows }),
    ))
}

// ---------------------------------------------------------------------------
// Cost
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CostQuery {
    /// Trailing window in days (default 30).
    pub window: Option<i64>,
    /// Requested breakdown order, echoed back; all folds are always computed (default all dims).
    pub by: Option<String>,
}

/// `GET /api/cost?window=30&by=…` — the v1 cost/usage fold (design §3, [`crate::cost`]).
pub async fn cost(State(state): State<AppState>, Query(q): Query<CostQuery>) -> Json<Envelope> {
    let window = q.window.unwrap_or(30);
    let by = nonempty(q.by).unwrap_or_else(|| "category,machine,repo,model,day".to_string());
    let data = cost::cost_view(&state.cfg, window, bn_core::util::now_epoch_secs(), &by);
    Json(Envelope::success("cost", data))
}

// ---------------------------------------------------------------------------
// Insights & metrics (#507) — the reporting surface: `bn metrics` (velocity/throughput +
// agent telemetry), `bn insights`/`weekly` (the weekly digests), and `bn brag`
// (accomplishments). All native in-process `bn-core` reads wrapped in an envelope, matching
// the CLI's `--json` shape. `metrics`/`brag`/`weekly` shell git/gh (or read the whole event
// log) so they run on a blocking thread to keep the async executor free.
// ---------------------------------------------------------------------------

/// Trailing-window / since query shared by the metrics + digest reads.
#[derive(Debug, Deserialize)]
pub struct InsightsQuery {
    /// Trailing window in days for the metrics scorecard (default 30).
    pub window: Option<i64>,
    /// Rolling window in days for the insights/weekly/brag digests; `None` uses each report's
    /// anchored default window.
    pub since: Option<i64>,
}

/// `GET /api/metrics?window=30` — the velocity + throughput scorecard (mirrors `bn metrics`):
/// PR flow, review latency, WIP, close-out lag, and the AI-agent session telemetry the dashboard
/// tiles read. Native in-process; shells git for the commit-mix, so it runs on a blocking thread.
pub async fn metrics(
    State(state): State<AppState>,
    Query(q): Query<InsightsQuery>,
) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let window = q.window.unwrap_or(30);
    let data = tokio::task::spawn_blocking(move || {
        use bn_core::clock::Clock;
        let clock = bn_core::clock::RealClock;
        let today = clock.today();
        let s = bn_core::metrics::scorecard(
            &cfg,
            &bn_core::runner::RealRunner,
            clock.now_epoch_secs(),
            window,
            &today,
        );
        bn_core::payload::scorecard_data(&s)
    })
    .await
    .unwrap_or_else(|_| json!({}));
    Json(Envelope::success("metrics", data))
}

/// `GET /api/metrics/trend` — the snapshot history behind the trend chart: one row per recorded
/// snapshot (`date`, `wip`, `merged`, `close_out_lag_median_hours`), mirroring `bn metrics --trend
/// --json`. Snapshots are written by the weekly job / `bn metrics --snapshot`, so a fresh install
/// returns an empty list (the chart then renders its empty state).
pub async fn metrics_trend(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let data = tokio::task::spawn_blocking(move || {
        let snaps = bn_core::metrics::read_snapshots(&cfg);
        let rows: Vec<Value> = snaps
            .iter()
            .map(|s| {
                json!({
                    "date": s.date,
                    "wip": s.card.wip,
                    "merged": s.card.pr.merged,
                    "close_out_lag_median_hours": s.card.close_out.lag_median_hours,
                })
            })
            .collect();
        json!({ "snapshots": rows })
    })
    .await
    .unwrap_or_else(|_| json!({ "snapshots": [] }));
    Json(Envelope::success("metrics_trend", data))
}

/// `GET /api/insights?since=` — the weekly auto-retro (mirrors `bn insights`): the mined telemetry
/// (PR cycle, deep-work ratio, scoreboard) plus the one suggestion, with a `rendered` text field.
/// Read-only: never writes a baseline or the weekly file (those stay `bn` CLI actions).
pub async fn insights(
    State(state): State<AppState>,
    Query(q): Query<InsightsQuery>,
) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let since = q.since;
    let data = tokio::task::spawn_blocking(move || {
        let ins = bn_core::insights::insights(&cfg, &bn_core::util::today(), since);
        bn_core::payload::insights_data(&ins)
    })
    .await
    .unwrap_or_else(|_| json!({}));
    Json(Envelope::success("insights", data))
}

/// `GET /api/weekly?since=` — the weekly 1-1 review digest (mirrors `bn weekly`): goals completed
/// since the anchor weekday (or `--since N` days), plus a `rendered` text field for display.
pub async fn weekly(
    State(state): State<AppState>,
    Query(q): Query<InsightsQuery>,
) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let since = q.since;
    let data = tokio::task::spawn_blocking(move || {
        let dxi = bn_core::resolver::JobConfig::load().dxi_enabled;
        let w = bn_core::reports::weekly(&cfg, &bn_core::util::today(), since, dxi);
        bn_core::payload::weekly_data(&w)
    })
    .await
    .unwrap_or_else(|_| json!({}));
    Json(Envelope::success("weekly", data))
}

/// `GET /api/brag?since=` — the accomplishments / impact packet (mirrors `bn brag`): shipped work
/// over the window, plus a `rendered` markdown field. Shells git/gh through the real runner, so it
/// runs on a blocking thread. `since` is a free-form window string (e.g. `30`, `2w`); absent uses
/// brag's default window.
pub async fn brag(State(state): State<AppState>, Query(q): Query<InsightsQuery>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let since = q.since.map(|n| n.to_string());
    let data = tokio::task::spawn_blocking(move || {
        let b = bn_core::brag::brag(
            &cfg,
            &bn_core::util::today(),
            since.as_deref(),
            &bn_core::runner::RealRunner,
        );
        bn_core::payload::brag_data(&b)
    })
    .await
    .unwrap_or_else(|_| json!({}));
    Json(Envelope::success("brag", data))
}

// ---------------------------------------------------------------------------
// Repos (multi-repo navigation, #495)
// ---------------------------------------------------------------------------

/// `GET /api/repos` — the multi-repo navigation rollup: every repo bn knows (its bare clones on this
/// machine, the managed-repos config, the branch-note store, the live fleet, the orchestrate runs),
/// each with a per-repo rollup (active agents, open PRs, session count, last activity, cost) and
/// its drill-in session list. Delegates to [`crate::repos::repos_view`], which reuses the same
/// Notes / Fleet / Cost rollups the other reads build from, so the Repos surface is one contract
/// with the rest of the dashboard.
pub async fn repos(State(state): State<AppState>) -> Json<Envelope> {
    Json(Envelope::success(
        "repos",
        crate::repos::repos_view(&state.cfg),
    ))
}

/// `GET /api/repos/{repo}/issues` — one repo's open GitHub issues (#556).
///
/// Migrated onto the #999 cache (#1005). It used to shell `gh issue list` on **every request**,
/// unbounded and uncached, and `.unwrap_or_default()` any failure into an empty list — so a
/// missing `gh`, an expired login, or a rate limit rendered as "this repo has no open issues".
///
/// Now: a TTL cache in front, a bounded fetch behind, and `stale` / `error` on the response so the
/// page can say *"12 minutes old, gh is unavailable"* instead of quietly lying. The `issues` key
/// keeps its shape for the existing per-repo view; the new fields are additive.
pub async fn repo_issues(State(state): State<AppState>, Path(repo): Path<String>) -> Response {
    let cmd = "repo-issues";
    let repo = repo.trim().to_ascii_lowercase();
    if !actions::valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }
    let cfg = state.cfg.clone();
    let name = repo.clone();
    let found = tokio::task::spawn_blocking(move || {
        let runner = bn_core::runner::RealRunner;
        bn_core::issues::for_repo(&cfg, &runner, &name, bn_core::util::now_epoch_secs())
    })
    .await
    .ok();

    match found {
        Some(r) => Json(Envelope::success(
            cmd,
            json!({
                "repo": r.repo,
                "slug": r.slug,
                "issues": r.issues,
                "fetched_at": r.fetched_at,
                "stale": r.stale,
                "error": r.error,
            }),
        ))
        .into_response(),
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(Envelope::failure(
                cmd,
                "fetch_panicked",
                "issue fetch failed",
            )),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// Notes
// ---------------------------------------------------------------------------

/// `GET /api/notes` — the branch-note rollup (repo/branch with open todos + blockers).
pub async fn notes(State(state): State<AppState>) -> Json<Envelope> {
    let entries = bn_core::rollups::summary(&state.cfg);
    Json(Envelope::success(
        "notes",
        bn_core::payload::summary_data(&entries),
    ))
}

/// `GET /api/notes/{repo}/{branch}` — one full note: typed frontmatter, prose body, and derived
/// goal line. `branch` is the note-key form (slashes already normalized to dashes by the rollup).
pub async fn note_detail(
    State(state): State<AppState>,
    Path((repo, branch)): Path<(String, String)>,
) -> Response {
    let ctx = bn_core::resolver::Resolved {
        repo: repo.to_lowercase(),
        branch,
    };
    let path = state.cfg.note_file(&ctx);
    match bn_core::store::read(&path) {
        Ok(note) => Json(Envelope::success(
            "note",
            bn_core::payload::note_detail_data(&ctx.repo, &ctx.branch, &note),
        ))
        .into_response(),
        Err(_) => not_found("note", &format!("no note for {}/{}", ctx.repo, ctx.branch)),
    }
}

// ---------------------------------------------------------------------------
// Handoff (#509)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct HandoffQuery {
    pub repo: Option<String>,
    pub branch: Option<String>,
}

/// `GET /api/handoff?repo=&branch=` — the per-branch session resume packet (#509). Prefers the
/// written `handoff.json`; when none exists it composes one live from the branch note, mirroring
/// `bn handoff read` (so a fresh branch still orients). The response carries the structured packet,
/// the human-readable markdown (`handoff.md` mirror), and the one-line orientation banner. `400`
/// when `repo`/`branch` are missing; `404` when the branch has neither a handoff nor a note.
pub async fn handoff(State(state): State<AppState>, Query(q): Query<HandoffQuery>) -> Response {
    let (Some(repo), Some(branch)) = (nonempty(q.repo), nonempty(q.branch)) else {
        return bad_request(
            "handoff",
            "missing_target",
            "repo and branch query params are required",
        );
    };
    let Ok(ctx) = bn_core::resolver::resolve_target(
        &state.cfg,
        std::path::Path::new("."),
        Some(repo.clone()),
        Some(branch.clone()),
    ) else {
        return bad_request("handoff", "invalid_target", "could not resolve repo/branch");
    };

    let packet = bn_core::handoff::read(&state.cfg, &ctx).or_else(|| {
        bn_core::store::read(&state.cfg.note_file(&ctx))
            .ok()
            .map(|note| {
                bn_core::handoff::compose(&note, &bn_core::util::today(), &Default::default())
            })
    });

    match packet {
        Some(h) => {
            let data = json!({
                "handoff": h,
                "markdown": bn_core::handoff::render_markdown(&h),
                "rendered": bn_core::handoff::render_orientation(&h),
            });
            Json(Envelope::success("handoff", data)).into_response()
        }
        None => not_found("handoff", &format!("no note for {repo}/{branch}")),
    }
}

// ---------------------------------------------------------------------------
// Learnings
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct LearningsQuery {
    /// Rolling window in days; `None` uses the anchored weekly window.
    pub since: Option<i64>,
    /// Widen to the whole daily history.
    pub all: Option<bool>,
    pub repo: Option<String>,
    pub skill: Option<String>,
    /// `agent` | `mine`; anything else applies no author filter.
    pub agent: Option<String>,
}

/// `GET /api/learnings?since=7&repo=&skill=&agent=` — the reflections/learnings inspection view.
pub async fn learnings(
    State(state): State<AppState>,
    Query(q): Query<LearningsQuery>,
) -> Json<Envelope> {
    use bn_core::learnings::Author;
    let author = match nonempty(q.agent).as_deref() {
        Some("agent") => Author::Agent,
        Some("mine") => Author::Mine,
        _ => Author::Any,
    };
    let filter = bn_core::learnings::Filter {
        since: q.since,
        all: q.all.unwrap_or(false),
        repo: nonempty(q.repo),
        skill: nonempty(q.skill),
        author,
    };
    let view = bn_core::learnings::collect(&state.cfg, &bn_core::util::today(), &filter);
    Json(Envelope::success("learnings", json!(view)))
}

// ---------------------------------------------------------------------------
// Goals (daily + quarter)
// ---------------------------------------------------------------------------

/// `GET /api/goals` — today's daily goals plus the current quarter. Read-only: a dashboard GET
/// never auto-creates or rolls the day/quarter forward (that stays a `bn` CLI action), so a not-yet
/// -materialized day returns the documented empty shape.
pub async fn goals(State(state): State<AppState>) -> Json<Envelope> {
    let today = bn_core::util::today();

    // #1004: goals are a *projection* over the task store, so this read no longer depends on the
    // CLI having written today's file first. `ensure_today` (the carry-forward) is a write, and a
    // GET could never call it — so before this, carried-forward goals were invisible in bn-web
    // until something else touched the day, and the rail's Goals badge read 0.
    let daily = {
        let d = bn_core::daily::view(&state.cfg, &today);
        let upcoming = bn_core::daily::scheduled_pending(&state.cfg, &today);
        bn_core::payload::daily_data(&d, &upcoming)
    };

    let quarter = match bn_core::quarter::quarter_id(&today).and_then(|qid| {
        bn_core::quarter::read_quarter(&state.cfg, &qid)
            .ok()
            .flatten()
    }) {
        Some(q) => bn_core::payload::quarter_data(&q, &today),
        None => json!({
            "quarter": bn_core::quarter::quarter_id(&today),
            "goals": [], "open": 0, "done": 0, "on_track": 0, "total": 0, "reviews": []
        }),
    };

    Json(Envelope::success(
        "goals",
        json!({ "daily": daily, "quarter": quarter }),
    ))
}

// ---------------------------------------------------------------------------
// Wrap — today's "achieved" digest (#490)
// ---------------------------------------------------------------------------

/// `GET /api/wrap` — the end-of-day "achieved today" digest (mirrors `bn wrap`): branches touched,
/// un-noted commits, PRs merged today, still-open todos, and tomorrow's carry-forward. It shells
/// git/gh through the real runner, so the subprocess work runs on a blocking thread rather than
/// stalling the async executor. Read-only: uses [`bn_core::reports::wrap_readonly`], so a dashboard
/// GET never materializes/rolls today's day file (matching the read-only `/api/goals`). The
/// note-activity marker (for "un-noted commits") is resolved from the server's cwd like the CLI,
/// falling back to `today` when no note is in scope.
pub async fn wrap(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let data = tokio::task::spawn_blocking(move || {
        let today = bn_core::util::today();
        let note_since =
            bn_core::resolver::resolve_target(&cfg, std::path::Path::new("."), None, None)
                .ok()
                .and_then(|ctx| bn_core::store::read(&cfg.note_file(&ctx)).ok())
                .map(|note| bn_core::reports::note_last_activity(&note))
                .unwrap_or_else(|| today.clone());
        let w = bn_core::reports::wrap_readonly(
            &cfg,
            &today,
            &note_since,
            &bn_core::runner::RealRunner,
        );
        bn_core::payload::wrap_data(&w)
    })
    .await
    .unwrap_or_else(|_| json!({}));
    Json(Envelope::success("wrap", data))
}

// ---------------------------------------------------------------------------
// Attention
// ---------------------------------------------------------------------------

/// `GET /api/attn` — open attention items for today (includes future plan-review requests).
pub async fn attn() -> Json<Envelope> {
    let items = bn_core::attention::open_items(&bn_core::util::today());
    Json(Envelope::success(
        "attn",
        bn_core::payload::attn_list_data(&items),
    ))
}

// ---------------------------------------------------------------------------
// Tasks, issues, pipeline (#1005, EPIC #993) — the read side of the task-first model
// ---------------------------------------------------------------------------

/// Query params for `GET /api/tasks`.
#[derive(Debug, Deserialize, Default)]
pub struct TasksQuery {
    /// `today` (the default view) or a `YYYY-MM-DD` date.
    pub day: Option<String>,
    pub repo: Option<String>,
    /// Filter on the SMART verdict: `ok`, `fail`, `stuck`, or `unjudged`.
    pub smart: Option<String>,
    pub stage: Option<String>,
    /// `1` to include the whole backlog rather than just what's actionable now.
    pub all: Option<String>,
    /// `open` (the default) or `done` — the finished record for a day.
    ///
    /// Without this the web UI could not see a completed task at all: every path below reads
    /// `open_sorted`, so "what did I finish today" had no answer on the wire.
    pub status: Option<String>,
}

/// `GET /api/tasks/:id` — one task, plus everything already joined to it.
///
/// A task had no address of its own: the list endpoint could show it in a row, but nothing could
/// link *to* it — so the fleet's "this agent is working on t42" had nowhere to go. This is that
/// address.
///
/// The `agent` join is the same `(lowercase repo, lowercase branch)` key, and the same five fields,
/// that [`pipeline_stages`] attaches — deliberately, so a task and the thing working on it can
/// never render out of sync, and so the existing `TaskAgent` type is reused rather than forked.
/// Runs on `spawn_blocking`: it folds the task store *and* a supervisor pass.
pub async fn task_detail(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let cmd = "task";
    let id = id.trim().to_string();
    // Same rule `actions::task_action` enforces, so a link that works for a read works for a write.
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return bad_request(cmd, "invalid_id", "task id must be alphanumeric (e.g. t42)");
    }

    let id_for_msg = id.clone();
    let st = state.clone();
    let found = tokio::task::spawn_blocking(move || {
        let today = bn_core::util::today();
        // An unreadable store is a real failure, not a missing task — a caller must be able to
        // tell "no such task" from "couldn't read the store".
        let file = bn_core::todo::load(&st.cfg.tasks_file()).map_err(|e| e.to_string())?;
        let Some(task) = file.tasks().find(|t| t.id() == Some(id.as_str())).cloned() else {
            return Ok(None);
        };
        let repo = task.meta("repo").map(str::to_string);
        let branch = task.meta("branch").map(str::to_string);

        // The live agent on this task's worktree, if any.
        let agent = match (&repo, &branch) {
            (Some(r), Some(b)) => {
                let fleet = bn_core::agent::supervise(
                    &st.cfg,
                    bn_core::agent::DEFAULT_IDLE_MINS,
                    bn_core::agent::DEFAULT_STUCK_MINS,
                );
                fleet
                    .agents
                    .iter()
                    .find(|a| a.repo.eq_ignore_ascii_case(r) && a.branch.eq_ignore_ascii_case(b))
                    .map(|a| {
                        json!({
                            "session_id": a.session_id,
                            "target": a.target,
                            "harness": a.harness,
                            "health": a.health,
                            "idle_secs": a.idle_secs,
                        })
                    })
            }
            _ => None,
        };
        let note = match (&repo, &branch) {
            (Some(r), Some(b)) => resolve_note(&st, r, b),
            _ => Value::Null,
        };
        let doc = bn_core::task_doc::read(&st.cfg, &task)
            .map(|d| json!({ "path": d.path, "body": d.text, "version": d.version() }));

        Ok::<_, String>(Some(json!({
            "today": today,
            "task": task.to_json(&today),
            "agent": agent,
            "note": note,
            "doc": doc,
        })))
    })
    .await;

    match found {
        Ok(Ok(Some(data))) => Json(Envelope::success(cmd, data)).into_response(),
        Ok(Ok(None)) => (
            StatusCode::NOT_FOUND,
            Json(Envelope::failure(
                cmd,
                "not_found",
                format!("no task '{id_for_msg}'"),
            )),
        )
            .into_response(),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(Envelope::failure(
                cmd,
                "store_unreadable",
                format!("could not read the task store: {e}"),
            )),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(Envelope::failure(cmd, "join_failed", e.to_string())),
        )
            .into_response(),
    }
}

/// `GET /api/tasks` — the task store (#996), filtered.
///
/// Served **natively** from `bn_core::todo` rather than by shelling `bn task --json`: it is a file
/// read, and `bn_runner` has no timeout, so a subprocess here would put an unbounded call on an
/// axum worker for no gain.
///
/// The default view mirrors the CLI's: `day:` today-or-earlier plus unscheduled work. `?all=1`
/// widens to the whole backlog. `?day=<date>` pins a specific day, which is what the Today page
/// and #1004's goal projection read.
pub async fn tasks(State(state): State<AppState>, Query(q): Query<TasksQuery>) -> Response {
    let cmd = "tasks";
    let today = bn_core::util::today();
    let all = q.all.as_deref().is_some_and(|v| v == "1" || v == "true");
    let repo = nonempty(q.repo).map(|r| r.to_ascii_lowercase());
    let smart = nonempty(q.smart).map(|s| s.to_ascii_lowercase());
    let stage = nonempty(q.stage).map(|s| s.to_ascii_lowercase());
    let day = nonempty(q.day);
    let status = nonempty(q.status).map(|s| s.to_ascii_lowercase());

    if let Some(d) = day.as_deref() {
        if d != "today" && bn_core::util::epoch_day(d).is_none() {
            return bad_request(cmd, "invalid_day", "day must be `today` or YYYY-MM-DD");
        }
    }
    if let Some(s) = status.as_deref() {
        if s != "open" && s != "done" {
            return bad_request(cmd, "invalid_status", "status must be `open` or `done`");
        }
    }
    let day_filter = day.as_deref().map(|d| {
        if d == "today" {
            today.clone()
        } else {
            d.to_string()
        }
    });

    let file = match bn_core::todo::load(&state.cfg.tasks_file()) {
        Ok(f) => f,
        // An unreadable store is a real failure, not an empty list — the same honesty rule the
        // issue cache follows. A caller must be able to tell "no tasks" from "couldn't read".
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(
                    cmd,
                    "store_unreadable",
                    format!("could not read the task store: {e}"),
                )),
            )
                .into_response()
        }
    };

    // The finished record for a day. Cancelled work is *listed but never conflated* with a real
    // completion — the store keeps them apart on purpose (`x <date> cancelled:1`, `todo.rs`), so
    // folding them into one count here would let abandoned work inflate a day's output.
    if status.as_deref() == Some("done") {
        let want_day = day_filter.unwrap_or_else(|| today.clone());
        let mut done = 0usize;
        let mut cancelled = 0usize;
        let mut rows: Vec<Value> = Vec::new();

        for t in file.tasks() {
            if t.completion.as_deref() != Some(want_day.as_str()) {
                continue;
            }
            if repo
                .as_deref()
                .is_some_and(|want| !t.meta("repo").is_some_and(|r| r.eq_ignore_ascii_case(want)))
            {
                continue;
            }
            match t.status(&today) {
                bn_core::todo::Status::Cancelled => cancelled += 1,
                bn_core::todo::Status::Done => done += 1,
                // Not finished on this day — a `completion` with an open status is a malformed
                // line, and guessing at it would be worse than leaving it out.
                _ => continue,
            }
            let mut row = t.to_json(&today);
            row["reason"] = json!(t.meta("reason"));
            rows.push(row);
        }

        return Json(Envelope::success(
            cmd,
            json!({
                "today": today,
                "day": want_day,
                "repo": repo,
                "status": "done",
                "count": rows.len(),
                "done": done,
                "cancelled": cancelled,
                "tasks": rows,
            }),
        ))
        .into_response();
    }

    let rows: Vec<Value> = file
        .open_sorted(&today, None)
        .into_iter()
        .filter(|t| {
            repo.as_deref()
                .is_none_or(|want| t.meta("repo").is_some_and(|r| r.eq_ignore_ascii_case(want)))
        })
        .filter(|t| {
            stage.as_deref().is_none_or(|want| {
                t.meta("stage")
                    .is_some_and(|s| s.eq_ignore_ascii_case(want))
            })
        })
        .filter(|t| match smart.as_deref() {
            None => true,
            // `unjudged` is a first-class state, not a missing value: a pulled task (#1000) has
            // never been through the gate, which is exactly what the improve queue looks for.
            Some("unjudged") => t.meta("smart").is_none(),
            Some(want) => t
                .meta("smart")
                .is_some_and(|s| s.eq_ignore_ascii_case(want)),
        })
        .filter(|t| match (&day_filter, all) {
            (Some(d), _) => t.meta("day") == Some(d.as_str()),
            (None, true) => true,
            // The bare view: actionable now — dated today or earlier (so overdue work stays
            // visible), plus everything unscheduled.
            (None, false) => match t
                .meta("day")
                .filter(|d| bn_core::util::epoch_day(d).is_some())
            {
                Some(d) => d <= today.as_str(),
                None => true,
            },
        })
        .map(|t| t.to_json(&today))
        .collect();

    Json(Envelope::success(
        cmd,
        json!({
            "today": today,
            "all": all,
            "day": day_filter,
            "repo": repo,
            "smart": smart,
            "stage": stage,
            "count": rows.len(),
            "tasks": rows,
        }),
    ))
    .into_response()
}

/// `GET /api/issues` — every repo's open issues, each with its own freshness (#999).
///
/// The cross-repo aggregate the dashboard never had. Per-repo `stale` / `error` are **not
/// optional**: an empty `issues` list with no error means the backlog is genuinely empty, and one
/// with an error means we could not ask. Conflating those is the bug #999 exists to remove, so the
/// shape forces callers to keep them apart.
///
/// A repo whose slug could not be resolved is present with `slug: null` and a reason, never
/// omitted — a caller cannot act on a repo it never hears about.
pub async fn issues(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let repos = tokio::task::spawn_blocking(move || {
        let runner = bn_core::runner::RealRunner;
        bn_core::issues::all(&cfg, &runner, bn_core::util::now_epoch_secs())
    })
    .await
    .unwrap_or_default();

    let stale = repos.iter().filter(|r| r.stale).count();
    let unreachable = repos
        .iter()
        .filter(|r| r.error.is_some() && !r.stale)
        .count();
    let total: usize = repos.iter().map(|r| r.issues.len()).sum();

    Json(Envelope::success(
        "issues",
        json!({
            "repos": repos,
            "count": total,
            "stale_repos": stale,
            "unreachable_repos": unreachable,
        }),
    ))
}

/// `GET /api/pipeline/board` — every worktree's recorded stage, plus per-stage counts.
///
/// EPIC #746 built a nine-stage machine that the dashboard has never been able to see: before this,
/// a grep of `crates/bn-web/src` for `pipeline` returned nothing. The counts are what the drawing
/// (#1006) renders on each node.
///
/// The vocabulary on the wire is the durable nine-value `pipeline::stage::Stage`. The four-value
/// inference model is `Phase` (#994) and stays CLI-internal, so a `stage` field is never ambiguous.
/// Each stage node also carries the **skill** that stage dispatches by default, and each board row
/// the skill its worktree is actually running — intent and reality side by side, because an
/// explicit `--skill` makes them differ and only showing the default would state the wrong one
/// confidently.
pub async fn pipeline_board(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    // One note read per row (`board_with_skills`), so do it off the axum worker.
    let entries =
        tokio::task::spawn_blocking(move || bn_core::pipeline::stage::board_with_skills(&cfg))
            .await
            .unwrap_or_default();

    let mut counts = serde_json::Map::new();
    for stage in bn_core::pipeline::stage::Stage::ALL {
        let n = entries.iter().filter(|e| e.stage == stage).count();
        counts.insert(stage.as_str().to_string(), json!(n));
    }
    let stages: Vec<Value> = bn_core::pipeline::stage::Stage::ALL
        .iter()
        .map(|s| {
            json!({
                "id": s.as_str(),
                "glyph": s.glyph(),
                "position": s.position(),
                "count": entries.iter().filter(|e| e.stage == *s).count(),
                "skill": s.default_skill(),
            })
        })
        .collect();

    Json(Envelope::success(
        "pipeline-board",
        json!({ "board": entries, "counts": counts, "stages": stages }),
    ))
}

/// `GET /api/pipeline/stages` — the pipeline as *work*, not as counters.
///
/// `/api/pipeline/board` answers "how many worktrees sit at each stage" from the event log, and the
/// dashboard drew that number next to a task list fed by a completely different query. The two
/// could never agree, and the click-through filtered on `task.stage` — a meta field that is *read*
/// (`todo.rs`) and written nowhere, so selecting any stage produced an empty list forever.
///
/// This endpoint removes the possibility rather than fixing the symptom: **the count and the rows
/// are the same value**. `stages[].count == stages[].tasks.len()`, by construction.
///
/// The join key is `(repo, branch)` — the one identity all three stores already share:
/// `BoardEntry` (stage), the task store (`repo:`/`branch:` metas), and `LiveAgent`, whose `target`
/// is literally `repo:branch`. Nothing new is persisted and nothing is mirrored, so there is no
/// second copy of pipeline state to drift — the same reason #1004 made daily goals a projection.
///
/// A worktree at a stage with no task behind it is an **orphan**, reported per stage instead of
/// dropped: it is precisely the gap EPIC #993 closes, and silently hiding it would make the
/// pipeline read as emptier than it is.
pub async fn pipeline_stages(State(state): State<AppState>) -> Response {
    let cmd = "pipeline-stages";
    let today = bn_core::util::today();
    // `board_with_skills` reads one note per row to learn what each worktree is *actually* running
    // (#1041), so it goes off the axum worker like `/api/pipeline/board` does. Carrying it here
    // means the drill-down shows the real workflow rather than the stage's default — an explicit
    // `bn dispatch --skill debug` sits in `execute` while running `debug`, and showing the default
    // would state the wrong thing confidently.
    let cfg = state.cfg.clone();
    let entries =
        tokio::task::spawn_blocking(move || bn_core::pipeline::stage::board_with_skills(&cfg))
            .await
            .unwrap_or_default();

    let file = match bn_core::todo::load(&state.cfg.tasks_file()) {
        Ok(f) => f,
        // An unreadable store is a real failure, not an empty pipeline — the same honesty rule
        // `/api/tasks` and the issue cache follow.
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(
                    cmd,
                    "store_unreadable",
                    format!("could not read the task store: {e}"),
                )),
            )
                .into_response()
        }
    };

    let fleet = bn_core::agent::supervise(
        &state.cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    );

    // `(repo, branch)` lowercased is the join key everywhere below. Both sides are user-authored
    // (a `repo:` meta typed into a task, a branch name from the event log), so matching them
    // case-sensitively would silently split `bn/Today-Redesign` from `bn/today-redesign`.
    let key = |repo: &str, branch: &str| (repo.to_ascii_lowercase(), branch.to_ascii_lowercase());

    let agent_at: HashMap<(String, String), &bn_core::agent::LiveAgent> = fleet
        .agents
        .iter()
        .map(|a| (key(&a.repo, &a.branch), a))
        .collect();

    // Only open work is "in flight" — a finished task keeps its branch, and counting it here would
    // leave every shipped worktree permanently occupying its stage.
    let open = file.open_sorted(&today, None);

    let stages: Vec<Value> = bn_core::pipeline::stage::Stage::ALL
        .iter()
        .map(|stage| {
            let here: Vec<&bn_core::pipeline::stage::BoardEntry> =
                entries.iter().filter(|e| e.stage == *stage).collect();

            let mut tasks: Vec<Value> = Vec::new();
            let mut orphans: Vec<Value> = Vec::new();

            for entry in &here {
                let k = key(&entry.repo, &entry.branch);
                let matched: Vec<&&bn_core::todo::Task> = open
                    .iter()
                    .filter(|t| {
                        match (t.meta("repo"), t.meta("branch")) {
                            (Some(r), Some(b)) => key(r, b) == k,
                            // A task with no `branch:` is pre-pipeline work (created outside a
                            // worktree, or pulled from an issue) — it belongs to no stage.
                            _ => false,
                        }
                    })
                    .collect();

                if matched.is_empty() {
                    orphans.push(json!({
                        "repo": entry.repo,
                        "branch": entry.branch,
                        "ts": entry.ts,
                    }));
                    continue;
                }

                for t in matched {
                    let mut row = t.to_json(&today);
                    // What this worktree is really running, joined server-side rather than
                    // reassembled in the browser from a second endpoint.
                    row["skill"] = json!(entry.skill);
                    // The agent is attached to the row rather than listed separately so a task and
                    // the thing currently working on it can never be rendered out of sync.
                    if let Some(a) = agent_at.get(&k) {
                        row["agent"] = json!({
                            "session_id": a.session_id,
                            "target": a.target,
                            "harness": a.harness,
                            "health": a.health,
                            "idle_secs": a.idle_secs,
                        });
                    }
                    tasks.push(row);
                }
            }

            json!({
                "id": stage.as_str(),
                "glyph": stage.glyph(),
                "position": stage.position(),
                // Intent (what this stage dispatches by default) beside reality (what each row is
                // actually running), because an explicit `--skill` makes the two differ.
                "skill": stage.default_skill(),
                // The contract that makes the old bug unrepresentable: this *is* the row count.
                "count": tasks.len(),
                "tasks": tasks,
                "orphans": orphans,
            })
        })
        .collect();

    let in_flight: usize = stages
        .iter()
        .map(|s| s["count"].as_u64().unwrap_or(0) as usize)
        .sum();
    let orphan_count: usize = stages
        .iter()
        .map(|s| s["orphans"].as_array().map_or(0, Vec::len))
        .sum();

    Json(Envelope::success(
        cmd,
        json!({
            "today": today,
            "in_flight": in_flight,
            "orphans": orphan_count,
            "stages": stages,
        }),
    ))
    .into_response()
}

/// `GET /api/pipeline/{repo}/{branch}` — one worktree's current stage and its full history.
pub async fn pipeline_status(
    State(state): State<AppState>,
    Path((repo, branch)): Path<(String, String)>,
) -> Response {
    let cmd = "pipeline-status";
    let repo = repo.trim().to_ascii_lowercase();
    if !actions::valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }
    let branch = bn_core::plan_review::sanitize_branch(branch.trim());
    if branch.is_empty() {
        return bad_request(cmd, "invalid_branch", "branch must not be empty");
    }

    let ctx = bn_core::resolver::Resolved {
        repo: repo.clone(),
        branch: branch.clone(),
    };
    // Frontmatter is authoritative; the event log is the fallback for notes written before the
    // stage was durable. Mirrors `stage::current_for_note`.
    let note = bn_core::store::read(&state.cfg.note_file(&ctx)).ok();
    let stage = note
        .as_ref()
        .and_then(bn_core::pipeline::stage::current_for_note)
        .or_else(|| bn_core::pipeline::stage::current(&repo, &branch));
    let skill = note
        .as_ref()
        .and_then(bn_core::pipeline::stage::skill_for_note);

    let history = bn_core::pipeline::stage::history(&repo, &branch);
    Json(Envelope::success(
        cmd,
        json!({
            "repo": repo,
            "branch": branch,
            "stage": stage.map(|s| s.as_str()),
            "position": stage.map(|s| s.position()),
            "glyph": stage.map(|s| s.glyph()),
            // What this worktree is actually running, next to what its stage dispatches by default.
            "skill": skill,
            "default_skill": stage.and_then(|s| s.default_skill()),
            "history": history,
        }),
    ))
    .into_response()
}

// ---------------------------------------------------------------------------
// Action center (#522) — the unified actionable-items list
// ---------------------------------------------------------------------------

/// `GET /api/actions` — the action-centric command center's heart: every actionable signal bn
/// tracks, folded into one ranked list (`bn_core::action_center::build_actions`). Aggregates open
/// attention items, pending plan-reviews, failing crons, open note todos + blockers, stale in-flight
/// PRs, and today's open goals. Each item carries an editable **suggested prompt** (a per-type
/// template) and a **default urgency**, so the dashboard can one-click *Start agent* (dispatch or
/// queue) from any row with a real starting-point instruction pre-filled.
///
/// A pure fold over already-read viewmodels — the reads shell git/gh (`wrap` doesn't; these don't
/// either) or hit the store, all cheap in-process `bn-core` calls, so it runs on a blocking thread
/// to keep the async executor free.
pub async fn actions(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let data = tokio::task::spawn_blocking(move || {
        let today = bn_core::util::today();
        let attn = bn_core::attention::open_items(&today);
        let reviews = bn_core::plan_review::list();
        let crons = bn_core::crons::status_views(&cfg, None);
        let notes = bn_core::rollups::summary(&cfg);
        let goals = bn_core::daily::read(&cfg.daily_file(&today))
            .map(|d| d.front.goals)
            .unwrap_or_default();
        // Every orchestrate queue item across live runs — the stale-PR source.
        let queue: Vec<bn_core::orchestrate::QueueItem> = bn_core::orchestrate::run_paths()
            .into_iter()
            .filter_map(|p| bn_core::orchestrate::OrchestrateState::load(&p))
            .flat_map(|st| st.queue.into_iter())
            .collect();
        // Accepted Today's-agent suggestions promoted into the action feed (#578).
        let suggestions =
            bn_core::suggestions::list(Some(bn_core::suggestions::SuggestionStatus::Accepted), 0);

        let sources = bn_core::action_center::ActionSources {
            attn: &attn,
            reviews: &reviews,
            crons: &crons,
            notes: &notes,
            goals: &goals,
            queue: &queue,
            suggestions: &suggestions,
            now: bn_core::util::now_epoch_secs(),
        };
        let actions = bn_core::action_center::build_actions(&sources);
        let cron_activity = bn_core::action_center::cron_activity(&crons);
        bn_core::payload::actions_data(&actions, &cron_activity)
    })
    .await
    .unwrap_or_else(|_| serde_json::json!({ "count": 0, "actions": [] }));
    Json(Envelope::success("actions", data))
}

/// `GET /api/agents/catalog` — the triggerable-agent catalog (#557): every agent/job the automation
/// can fire — declared crons (`crons.toml`), backing scripts + native jobs, and the `staff-<name>`
/// agent roster (`config/agents/*.md`) — folded into one list (`crate::crons::status_views` +
/// `bn_core::action_center::build_catalog`), each with the metadata the Command Center shows and a
/// job name the `POST /api/cron/run` re-trigger fires. A read, all in-process `bn-core`; runs on a
/// blocking thread to keep the async executor free, matching the `actions`/`cron` handlers. `ctx =
/// None` gives the global-tier registry — the machine view a browser wants.
pub async fn agents_catalog(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let data = tokio::task::spawn_blocking(move || {
        let crons = bn_core::crons::status_views(&cfg, None);
        let staff = bn_core::staff::discover(&cfg, None);
        let items = bn_core::action_center::build_catalog(&crons, &staff);
        bn_core::payload::catalog_data(&items)
    })
    .await
    .unwrap_or_else(|_| serde_json::json!({ "count": 0, "agents": [] }));
    Json(Envelope::success("agents-catalog", data))
}

/// The Today tab's automation split (#575): today's scheduled crons + today's scheduled staff
/// agents, folded from the machine catalog and filtered to entries whose declared schedule *fires
/// today* ([`bn_core::action_center::build_todays_automation`]). Both `/api/today/crons` and
/// `/api/today/agents` share this fold so the two lists stay consistent; each returns the
/// `catalog_data` shape (a `CatalogItem` list) the Command Center already renders, so the Today UI
/// reuses the same row + the shared `POST /api/cron/run` trigger. Reads are all in-process
/// `bn-core`, run on a blocking thread like `agents_catalog`.
pub async fn today_crons(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let data = tokio::task::spawn_blocking(move || {
        let today = bn_core::action_center::build_todays_automation(&cfg, None);
        bn_core::payload::catalog_data(&today.crons)
    })
    .await
    .unwrap_or_else(|_| serde_json::json!({ "count": 0, "agents": [] }));
    Json(Envelope::success("today-crons", data))
}

/// `GET /api/today/agents` — the scheduled staff agents (`config/agents/*.md` roster) firing today, each
/// with its last-run health and the `staff-<name>` job the shared `POST /api/cron/run` triggers.
pub async fn today_agents(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let data = tokio::task::spawn_blocking(move || {
        let today = bn_core::action_center::build_todays_automation(&cfg, None);
        bn_core::payload::catalog_data(&today.agents)
    })
    .await
    .unwrap_or_else(|_| serde_json::json!({ "count": 0, "agents": [] }));
    Json(Envelope::success("today-agents", data))
}

/// `GET /api/today/agent` — Today's **hosted** agent: the record of what bn launched, joined with
/// the live fleet so the page can tell "bn started one" from "one is alive right now".
///
/// Singular, and deliberately distinct from `/api/today/agents` above (the scheduled staff-agent
/// roster). This is the one session the Today page streams and steers; the payload carries its
/// `target` and `control` channel, which is exactly what `SteerPanel` needs to decide whether the
/// composer is live or greyed.
///
/// Built by `bn_core::today_agent::status_view`, the same builder `bn today agent status` prints —
/// so the CLI and the dashboard cannot drift on whether the agent is up.
pub async fn today_agent(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let data = tokio::task::spawn_blocking(move || bn_core::today_agent::status_view(&cfg))
        .await
        .unwrap_or_else(|_| json!({ "running": false, "agent": Value::Null }));
    Json(Envelope::success("today-agent", data))
}

/// `GET /api/completed` — the Command Center's **Completed** tab (#561): the counterpart to the
/// actionable list. Folds every *finished* signal bn tracks — closed branch-note todos (`bn done`),
/// achieved daily goals, resolved attention items, and landed/merged orchestrate work — into one
/// most-recent-first history (`bn_core::completed::build_completed`). Each item carries its branch
/// note's **local path** + **remote URL** (via `bn_core::rollups::FsNoteLinker`, which shells git),
/// so a finished item is traceable back to its full context from anywhere.
///
/// Like `actions`, a fold over already-read viewmodels plus a git-shelling linker, so it runs on a
/// blocking thread to keep the async executor free.
pub async fn completed(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let data = tokio::task::spawn_blocking(move || {
        let today = bn_core::util::today();
        let todos = bn_core::rollups::completed_todos(&cfg);
        let goals = bn_core::daily::read(&cfg.daily_file(&today))
            .map(|d| d.front.goals)
            .unwrap_or_default();
        // Resolved attention — the completion pushes (`bn agent done`) awaiting review.
        let attn = bn_core::attention::completed_items(&today);
        // Landed/merged orchestrate work across every live run.
        let merged: Vec<bn_core::orchestrate::QueueItem> = bn_core::orchestrate::run_paths()
            .into_iter()
            .filter_map(|p| bn_core::orchestrate::OrchestrateState::load(&p))
            .flat_map(|st| st.queue.into_iter())
            .collect();

        let sources = bn_core::completed::CompletedSources {
            todos: &todos,
            goals: &goals,
            attn: &attn,
            merged: &merged,
        };
        let linker = bn_core::rollups::FsNoteLinker::new(&cfg, &bn_core::runner::RealRunner);
        let items = bn_core::completed::build_completed(&sources, &linker);
        bn_core::payload::completed_data(&items)
    })
    .await
    .unwrap_or_else(|_| serde_json::json!({ "count": 0, "completed": [] }));
    Json(Envelope::success("completed", data))
}

// ---------------------------------------------------------------------------
// Global search / command palette (#565)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    /// The palette query; absent/blank short-circuits to an empty result set.
    pub q: Option<String>,
}

/// `GET /api/search?q=` — the ⌘K command palette's backend: one ranked, typed, actionable result
/// set across everything bn aggregates. It gathers the same source viewmodels the `/api/actions`
/// handler does (so the actionable core reuses the already-ranked command-center items, and "work
/// on this task" opens the same composer), plus branch notes, learnings, the live fleet, repos,
/// crons, skills, and best-effort GitHub issues, then folds them with
/// [`bn_core::palette::search`]. A blank query does no work (and never shells `gh`). All the reads
/// are cheap in-process `bn-core` calls except the best-effort `gh issue list`, so it runs on a
/// blocking thread to keep the async executor free — matching the `actions`/`wrap` handlers.
pub async fn search(
    State(state): State<AppState>,
    Query(sq): Query<SearchQuery>,
) -> Json<Envelope> {
    let q = nonempty(sq.q).unwrap_or_default();
    if q.trim().is_empty() {
        return Json(Envelope::success(
            "search",
            bn_core::payload::palette_data("", &[]),
        ));
    }
    let cfg = state.cfg.clone();
    let data = tokio::task::spawn_blocking(move || {
        let today = bn_core::util::today();
        let now = bn_core::util::now_epoch_secs();

        // Actionable core — the exact source gather the `/api/actions` handler performs.
        let attn = bn_core::attention::open_items(&today);
        let reviews = bn_core::plan_review::list();
        let crons = bn_core::crons::status_views(&cfg, None);
        let notes_summary = bn_core::rollups::summary(&cfg);
        let goals = bn_core::daily::read(&cfg.daily_file(&today))
            .map(|d| d.front.goals)
            .unwrap_or_default();
        let queue: Vec<bn_core::orchestrate::QueueItem> = bn_core::orchestrate::run_paths()
            .into_iter()
            .filter_map(|p| bn_core::orchestrate::OrchestrateState::load(&p))
            .flat_map(|st| st.queue.into_iter())
            .collect();
        let actions =
            bn_core::action_center::build_actions(&bn_core::action_center::ActionSources {
                attn: &attn,
                reviews: &reviews,
                crons: &crons,
                notes: &notes_summary,
                goals: &goals,
                queue: &queue,
                suggestions: &[],
                now,
            });

        // Palette-only jump sources.
        let all_notes = bn_core::rollups::all_notes(&cfg);
        let note_cards: Vec<bn_core::palette::NoteCard> = all_notes
            .iter()
            .map(|bn| bn_core::palette::NoteCard {
                repo: bn.repo.clone(),
                branch: bn.branch.clone(),
                goal: bn_core::render::goal_text(&bn.note.body),
                status: bn.note.front.status.clone(),
            })
            .collect();
        let learnings =
            bn_core::learnings::collect(&cfg, &today, &bn_core::learnings::Filter::default());
        let fleet = bn_core::agent::supervise(
            &cfg,
            bn_core::agent::DEFAULT_IDLE_MINS,
            bn_core::agent::DEFAULT_STUCK_MINS,
        );
        // Distinct repos across this machine's clones + notes + the live fleet — the same set the
        // Repos page keys on, so a repo you can jump to there is reachable from the palette too.
        let mut repo_names: std::collections::BTreeSet<String> =
            bn_core::lifecycle::cloned_repos(&cfg)
                .into_iter()
                .filter(|r| cfg.is_visible_repo(r))
                .collect();
        for bn in &all_notes {
            repo_names.insert(bn.repo.to_ascii_lowercase());
        }
        for a in &fleet.agents {
            repo_names.insert(a.repo.to_ascii_lowercase());
        }
        let repo_cards: Vec<bn_core::palette::RepoCard> = repo_names
            .into_iter()
            .map(|repo| bn_core::palette::RepoCard {
                repo,
                subtitle: String::new(),
            })
            .collect();
        let skills = bn_core::skills::discover(&cfg, std::path::Path::new("."));

        // GitHub issues — best-effort, in the server's cwd repo (empty on any gh failure).
        let issues = bn_core::palette::github_issues(&bn_core::runner::RealRunner, &q, 10);

        let sources = bn_core::palette::PaletteSources {
            actions: &actions,
            notes: &note_cards,
            learnings: Some(&learnings),
            agents: &fleet.agents,
            repos: &repo_cards,
            crons: &crons,
            skills: &skills,
            issues: &issues,
        };
        let results = bn_core::palette::search(&sources, &q);
        bn_core::payload::palette_data(&q, &results)
    })
    .await
    .unwrap_or_else(|_| bn_core::payload::palette_data("", &[]));
    Json(Envelope::success("search", data))
}

// ---------------------------------------------------------------------------
// Cron registry (#483)
// ---------------------------------------------------------------------------

/// `GET /api/cron` — the machine-wide cron registry: every declared cron (`crons.toml`) joined
/// with its last-run health from the event log, plus run-only orphans. A read, all in-process
/// `bn-core` (no OS-scheduler spawn): the declared schedule + last-run status the design surfaces,
/// not the launchd/systemd unit inspection (that stays a `bn cron` CLI concern). `ctx = None` gives
/// the global-tier registry — the machine view a browser wants.
pub async fn cron(State(state): State<AppState>) -> Json<Envelope> {
    let views = bn_core::crons::status_views(&state.cfg, None);
    Json(Envelope::success(
        "cron",
        bn_core::payload::crons_view_data(&views),
    ))
}

// ---------------------------------------------------------------------------
// Skills (#483)
// ---------------------------------------------------------------------------

/// `GET /api/skills` — every skill across the personal / global / per-repo tiers (the same set
/// bn-mcp serves as prompts), each tagged with its tier and whether it's the *effective*
/// definition. Read-only; `PUT /api/skills/{name}` edits the effective file. Resolved from the
/// server's cwd, so the per-repo tiers appear when bn-web runs inside a repo.
pub async fn skills(State(state): State<AppState>) -> Json<Envelope> {
    let skills = bn_core::skills::discover(&state.cfg, std::path::Path::new("."));
    Json(Envelope::success(
        "skills",
        bn_core::payload::skills_data(&skills),
    ))
}

/// `GET /api/skills/{name}` — one skill's editable source: its effective metadata (tier, path)
/// plus the raw markdown `content` the editor loads. `404` when no tier defines `name`.
pub async fn skill_detail(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let Some(info) = bn_core::skills::effective_info(&state.cfg, std::path::Path::new("."), &name)
    else {
        return not_found("skill", &format!("no skill named '{name}'"));
    };
    let content = match std::fs::read_to_string(&info.path) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(
                    "skill",
                    "read_failed",
                    format!("could not read {}: {e}", info.path),
                )),
            )
                .into_response()
        }
    };
    Json(Envelope::success(
        "skill",
        json!({ "skill": info, "content": content }),
    ))
    .into_response()
}

// ---------------------------------------------------------------------------
// Plan-reviews (#349)
// ---------------------------------------------------------------------------

/// `GET /api/reviews` — the plan-review gate feed (#349): every plan-review, newest first, each
/// with its plan text, status, and notes. Replaces the earlier `"reviews": []` placeholder — the
/// dashboard's agent-detail panel and the pending-reviews board list read from here.
pub async fn reviews() -> Json<Envelope> {
    let reviews = bn_core::plan_review::list();
    Json(Envelope::success(
        "plan-reviews",
        bn_core::payload::plan_reviews_data(&reviews),
    ))
}

// ---------------------------------------------------------------------------
// Events (history / audit browser)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// Restrict to one event `kind`.
    pub kind: Option<String>,
    /// Only events at/after this epoch-seconds timestamp.
    pub since_ts: Option<i64>,
    /// Most-recent-N cap (default 200).
    pub limit: Option<usize>,
}

/// `GET /api/events?kind=&since_ts=&limit=` — the machine-local event log, newest-first, filtered.
pub async fn events(Query(q): Query<EventsQuery>) -> Json<Envelope> {
    let limit = q.limit.unwrap_or(200);
    let kind = nonempty(q.kind);
    let mut evs = bn_core::events::read_events_at(&bn_core::events::log_path());
    evs.retain(|e| {
        kind.as_deref().is_none_or(|k| e.kind == k) && q.since_ts.is_none_or(|ts| e.ts >= ts)
    });
    evs.reverse(); // file is oldest-first; the browser wants newest-first
    evs.truncate(limit);
    Json(Envelope::success(
        "events",
        json!({ "count": evs.len(), "events": evs }),
    ))
}

// ---------------------------------------------------------------------------
// Findings (cron findings store, #576)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct FindingsQuery {
    /// Restrict to one source (else every source is unioned).
    pub source: Option<String>,
    /// Only runs gathered on/after this local date (YYYY-MM-DD).
    pub since: Option<String>,
    /// Most-recent-N cap (default 50).
    pub limit: Option<usize>,
}

/// `GET /api/findings?source=&since=&limit=` — durable cron findings, newest-first, filtered by
/// source and date. The read side #577 (Today's agent) and the dashboard consume.
pub async fn findings(Query(q): Query<FindingsQuery>) -> Json<Envelope> {
    let limit = q.limit.unwrap_or(50);
    let source = nonempty(q.source);
    let since = nonempty(q.since);
    let runs = bn_core::findings::query(source.as_deref(), since.as_deref(), limit);
    Json(Envelope::success(
        "findings",
        json!({ "count": runs.len(), "runs": runs }),
    ))
}

// ---------------------------------------------------------------------------
// Suggestions (Today's agent, #577)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SuggestionsQuery {
    /// Restrict to one status: `suggested` | `accepted` | `dismissed` (else all).
    pub status: Option<String>,
    /// Most-recent-N cap (default 50, 0 = uncapped).
    pub limit: Option<usize>,
}

/// `GET /api/suggestions?status=&limit=` — Today's agent's suggested tasks, newest-first. Each
/// carries its source finding + rationale, so the Today page can render a reviewable proposal list.
pub async fn suggestions(Query(q): Query<SuggestionsQuery>) -> Json<Envelope> {
    let limit = q.limit.unwrap_or(50);
    let status = nonempty(q.status).and_then(|s| bn_core::suggestions::SuggestionStatus::parse(&s));
    let items = bn_core::suggestions::list(status, limit);
    Json(Envelope::success(
        "suggestions",
        json!({ "count": items.len(), "suggestions": items }),
    ))
}

// ---------------------------------------------------------------------------
// Live stream (SSE)
// ---------------------------------------------------------------------------

/// `GET /api/stream` — the SSE live stream. Subscribes this client to the shared watcher broadcast;
/// a lagged (too-slow) client silently skips dropped frames and keeps going. `EventSource`
/// auto-reconnects and re-seeds from the REST endpoints, so no history replay is needed here.
pub async fn stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let rx = state.tx.subscribe();
    let events = BroadcastStream::new(rx).filter_map(|res| match res {
        Ok(frame) => Some(Ok(frame_to_sse(&frame))),
        // Lagged: the client fell behind the channel capacity — skip the gap, don't tear down.
        Err(_) => None,
    });
    Sse::new(events).keep_alive(KeepAlive::default())
}

fn frame_to_sse(frame: &Arc<StreamFrame>) -> SseEvent {
    SseEvent::default()
        .event(frame.event)
        .data(frame.data.as_str())
}
