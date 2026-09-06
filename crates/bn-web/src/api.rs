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
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

#[path = "api/automation.rs"]
mod automation;
#[path = "api/repo_read.rs"]
mod repo_read;
#[path = "api/repo_scripts.rs"]
mod repo_scripts;
pub use automation::{agent_def_source, agent_defs, agents_catalog, today_agents, today_crons};
#[path = "api/metrics.rs"]
mod metrics_api;
pub use metrics_api::*;
#[path = "api/pipeline.rs"]
mod pipeline_api;
pub use pipeline_api::*;
#[path = "api/feeds.rs"]
mod feeds;
#[path = "api/note_files.rs"]
mod note_files;
#[path = "api/search.rs"]
mod search_api;
#[path = "api/shell_read.rs"]
mod shell_read;
pub use feeds::{actions, completed};
pub use note_files::*;
pub use repo_read::{repo_issues, repos};
pub use repo_scripts::repo_script;
pub use search_api::search;
pub use shell_read::{attn, cron, fleet, machine, nodes};

pub(crate) fn nonempty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty())
}

/// A 404 carrying the standard failure envelope (JSON), for "no such live agent / note" reads.
pub(crate) fn not_found(command: &str, message: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(Envelope::failure(command, "not_found", message)),
    )
        .into_response()
}

/// A 400 carrying the standard failure envelope (JSON), for a read whose query params are missing
/// or malformed before any store is touched.
pub(crate) fn bad_request(command: &str, code: &str, message: &str) -> Response {
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
// Notes
// ---------------------------------------------------------------------------

/// `GET /api/notes` — the branch-note rollup (repo/branch with open todos + blockers).
///
/// The sweep reads every branch note on the machine — hundreds of files — so it runs on the
/// blocking pool rather than on an axum worker. Called inline it pins a runtime thread for the
/// whole walk, and with enough of them the server stops answering *anything*, including
/// `/api/health`: a slow endpoint becomes a down dashboard.
pub async fn notes(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let entries = match tokio::task::spawn_blocking(move || {
        bn_core::payload::summary_data(&bn_core::rollups::summary(&cfg))
    })
    .await
    {
        Ok(data) => data,
        Err(e) => return Json(Envelope::failure("notes", "join_failed", e.to_string())),
    };
    Json(Envelope::success("notes", entries))
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
// Goals (daily)
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

    Json(Envelope::success("goals", json!({ "daily": daily })))
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

// ---------------------------------------------------------------------------
// Tasks, issues, pipeline (#1005, EPIC #993) — the read side of the task-first model
// ---------------------------------------------------------------------------

pub use crate::task_api::tasks;

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

/// `GET /api/issues` — every repo's open issues, each with its own freshness (#999).
///
/// The cross-repo aggregate the dashboard never had. Per-repo `stale` / `error` are **not
/// optional**: an empty `issues` list with no error means the backlog is genuinely empty, and one
/// with an error means we could not ask. Conflating those is the bug #999 exists to remove, so the
/// shape forces callers to keep them apart.
///
/// A repo whose slug could not be resolved is present with `slug: null` and a reason, never
/// omitted — a caller cannot act on a repo it never hears about.
/// The sweep is fanned out across repos and served from a TTL window — see
/// [`crate::view_cache::ISSUES`] for why an endpoint that already has a per-repo cache still needs
/// one, and `bn_core::issues::all_parallel` for the fan-out.
pub async fn issues(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let data = state
        .views
        .get_or_compute(crate::view_cache::ISSUES, move || {
            let repos = bn_core::issues::all_parallel(&cfg, bn_core::util::now_epoch_secs());
            let stale = repos.iter().filter(|r| r.stale).count();
            let unreachable = repos
                .iter()
                .filter(|r| r.error.is_some() && !r.stale)
                .count();
            let total: usize = repos.iter().map(|r| r.issues.len()).sum();
            json!({
                "repos": repos,
                "count": total,
                "stale_repos": stale,
                "unreachable_repos": unreachable,
            })
        })
        .await;

    match data {
        Some(data) => Json(Envelope::success("issues", (*data).clone())),
        None => Json(Envelope::failure(
            "issues",
            "view_panicked",
            "sweeping the repos' issues failed",
        )),
    }
}

// ---------------------------------------------------------------------------
// Skills (#483)
// ---------------------------------------------------------------------------

/// `GET /api/skills` — every skill across the personal / global / per-repo tiers (the same set
/// bn-mcp serves as prompts), each tagged with its tier and whether it's the *effective*
/// definition. Read-only; `PUT /api/skills/{name}` edits the effective file. Resolved from the
/// server's cwd, so the per-repo tiers appear when bn-web runs inside a repo.
pub async fn skills(State(state): State<AppState>) -> Json<Envelope> {
    match bn_core::skills::discover(&state.cfg, std::path::Path::new(".")) {
        Ok(skills) => Json(Envelope::success(
            "skills",
            bn_core::payload::skills_data(&skills),
        )),
        Err(error) => Json(Envelope::failure(
            "skills",
            "invalid_catalog",
            error.to_string(),
        )),
    }
}

/// `GET /api/skills/{name}` — one skill's editable source: its effective metadata (tier, path)
/// plus the raw markdown `content` the editor loads. `404` when no tier defines `name`.
pub async fn skill_detail(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let info = match bn_core::skills::effective_info(&state.cfg, std::path::Path::new("."), &name) {
        Ok(Some(info)) => info,
        Ok(None) => return not_found("skill", &format!("no skill named '{name}'")),
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(
                    "skill",
                    "invalid_catalog",
                    error.to_string(),
                )),
            )
                .into_response()
        }
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
