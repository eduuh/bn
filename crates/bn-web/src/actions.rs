//! The authed, audited write layer (design §2.3).
//!
//! Two deliberate classes of write:
//!
//! * **Native state writes** mutate the orchestrator's own `orchestrate-<repo>.json` in-process
//!   through [`bn_core::orchestrate::mutate`] — a locked read-modify-write the running daemon shares,
//!   so a queue edit lands within one tick with no restart and is never clobbered by a concurrent
//!   tick (#392). Endpoints: queue add / remove / reorder, scale, stop.
//! * **Process writes** shell out to `bn --json <fixed args>` via [`crate::bn_runner`] and return the
//!   command's [`Envelope`] — the current way to reach tmux/git/gh/launchd (dispatch,
//!   send, relaunch, kill, land, start). Since #428 this is a boundary choice rather than a
//!   doctrine requirement (the shared tmux wrappers now live in [`bn_core::tmux`]).
//!
//! Every accepted write (and every rejected one) is recorded via [`crate::audit`], which the SSE
//! event tail echoes live. Inputs are validated and interpolated only as distinct argv elements —
//! there is no shell and no arbitrary-subcommand path (design §5 blast-radius).

use crate::audit;
use crate::bn_runner::{self, RunResult};
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{ConnectInfo, FromRequestParts, Path, Query, State},
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Json, Response},
};
use bn_core::orchestrate::{self, OrchestrateState};
use bn_core::render::Envelope;
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use std::path::PathBuf;

#[path = "actions/kill.rs"]
mod kill;
#[path = "actions/plan.rs"]
mod plan;
#[path = "actions/prompts.rs"]
mod prompts;
#[path = "actions/repo_pool.rs"]
mod repo_pool;
#[path = "actions/repo_setup.rs"]
mod repo_setup;
pub use repo_pool::{repo_pool_schedule_set, repo_pool_warm};
#[path = "actions/reviews.rs"]
mod reviews;
// The two Markdown-definition CRUD surfaces — one skill file, one agent file — each a validated
// write followed by a native harness materialization. Siblings by shape, so siblings by file.
#[path = "actions/agent_defs.rs"]
mod agent_defs;
#[path = "actions/skills.rs"]
mod skills;
#[path = "actions/today.rs"]
mod today;
pub use agent_defs::{agent_def_create, agent_def_delete, agent_def_write};
pub use kill::{agent_kill, repo_drain, repo_session_relaunch};
pub use plan::plan_promote;
pub use repo_setup::{repo_orchestrate_start, repo_script_write, repo_setup_agent_start};
pub use reviews::review_decide;
pub use skills::{skill_create, skill_delete, skill_write};
pub use today::{suggestion_status, today_agent_start, today_agent_stop, today_run};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// The caller's socket address, or `None`. An infallible extractor: it reads the `ConnectInfo`
/// (present in production via `into_make_service_with_connect_info`, absent under a bare test
/// `oneshot`) from the request extensions and never rejects — so a handler can audit `remote_addr`
/// without every test having to install a `MockConnectInfo` layer.
pub struct MaybeAddr(pub Option<SocketAddr>);

impl<S: Send + Sync> FromRequestParts<S> for MaybeAddr {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(MaybeAddr(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0),
        ))
    }
}

/// Parse a JSON request body tolerantly: an empty body (a `curl -X POST` with no `-d`) yields the
/// type's default, so an all-optional body (`{repo?}`) works without a payload; a present-but-broken
/// body is a `400`. Required fields default to empty and are caught by each handler's validation.
// Err is an axum `Response` by design throughout this handler module (the rejection IS the HTTP
// reply), so the large-Err lint doesn't apply here.
#[allow(clippy::result_large_err)]
pub(crate) fn parse_body<T: Default + serde::de::DeserializeOwned>(
    cmd: &str,
    bytes: &Bytes,
) -> Result<T, Response> {
    if bytes.is_empty() {
        return Ok(T::default());
    }
    serde_json::from_slice(bytes)
        .map_err(|e| bad_request(cmd, "invalid_json", &format!("invalid JSON body: {e}")))
}

/// A `400` carrying the standard failure envelope — for input the handler rejects before touching
/// any state or subprocess.
/// A 500 carrying the standard failure envelope — a write that reached the store and failed there.
pub(crate) fn server_error(command: &str, code: &str, msg: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(Envelope::failure(command, code, msg)),
    )
        .into_response()
}

pub(crate) fn bad_request(command: &str, code: &str, msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(Envelope::failure(command, code, msg)),
    )
        .into_response()
}

/// A `409` carrying the standard failure envelope — the requested start is a no-op because the
/// resource already exists (an orchestrator is already running for this repo, #556).
pub(crate) fn conflict(command: &str, code: &str, msg: &str) -> Response {
    (
        StatusCode::CONFLICT,
        Json(Envelope::failure(command, code, msg)),
    )
        .into_response()
}

/// A `503` when durable orchestrator state is temporarily unavailable for mutation.
fn unavailable(command: &str, msg: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(Envelope::failure(command, "state_unavailable", msg)),
    )
        .into_response()
}

/// A `404` for "no orchestration run exists to mutate" — a native queue edit needs a live run file.
fn no_run(command: &str, repo: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(Envelope::failure(
            command,
            "no_run",
            format!("no orchestration run for repo '{repo}' (start one with POST /api/orchestrate/start)"),
        )),
    )
        .into_response()
}

/// Validate a repo name — the sanitize-run-key charset. Keeps a bogus value from ever reaching a
/// state path or an argv slot.
pub(crate) fn valid_repo(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Validate an agent target (`repo:branch` tmux window name). Must contain a `:` and only safe path
/// characters — never whitespace or shell metacharacters (belt-and-suspenders; it's passed as argv).
pub(crate) fn valid_target(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.contains(':')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/'))
}

/// Normalize a requested harness to the CLI's accepted set (allowlist). `None`/empty → copilot.
fn normalize_harness(h: Option<&str>) -> Option<&'static str> {
    match h.map(|s| s.trim().to_lowercase()).as_deref() {
        None | Some("") | Some("copilot") => Some("copilot"),
        Some("claude") | Some("claudecode") | Some("claude-code") => Some("claude"),
        _ => None,
    }
}

/// Resolve which run a native queue edit targets: the explicit repo if given (and valid), else the
/// single live run on the machine. Errors (as a `Response`) when the repo is invalid, or when it's
/// omitted and there isn't exactly one run to disambiguate.
#[allow(clippy::result_large_err)]
fn resolve_repo(command: &str, explicit: Option<String>) -> Result<String, Response> {
    if let Some(r) = explicit
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        if !valid_repo(&r) {
            return Err(bad_request(
                command,
                "invalid_repo",
                "repo has invalid characters",
            ));
        }
        return Ok(r);
    }
    let repos: Vec<String> = orchestrate::run_paths()
        .iter()
        .filter_map(|p| OrchestrateState::load(p))
        .map(|st| st.repo)
        .collect();
    match repos.len() {
        1 => Ok(repos.into_iter().next().unwrap()),
        0 => Err(bad_request(
            command,
            "no_run",
            "no orchestration run exists — pass \"repo\" or start one first",
        )),
        _ => Err(bad_request(
            command,
            "ambiguous_repo",
            &format!("several runs live ({}) — pass \"repo\"", repos.join(", ")),
        )),
    }
}

/// Run a native locked mutation off the async runtime (the [`orchestrate::mutate`] lock + file IO is
/// blocking). Returns the closure's value, or `None` when no run exists at `path`.
async fn mutate_blocking<R, F>(path: PathBuf, f: F) -> std::io::Result<Option<R>>
where
    R: Send + 'static,
    F: FnOnce(&mut OrchestrateState) -> R + Send + 'static,
{
    tokio::task::spawn_blocking(move || orchestrate::mutate(&path, f))
        .await
        .map_err(std::io::Error::other)?
}

/// Run a `bn --json` subprocess off the async runtime and return its envelope.
pub(crate) async fn run_bn(args: Vec<String>, cwd: Option<PathBuf>) -> RunResult {
    run_bn_env(args, cwd, Vec::new()).await
}

/// Like [`run_bn`], but sets extra env on the child. The cross-repo dispatch actions use it to
/// declare `BN_WORKER_REPO=<target>` so the CLI's cross-repo-session guard treats the spawn as the
/// target repo's own session (the server runs from its own worktree, so it would otherwise look
/// like a stray cross-repo hop and be blocked).
pub(crate) async fn run_bn_env(
    args: Vec<String>,
    cwd: Option<PathBuf>,
    envs: Vec<(String, String)>,
) -> RunResult {
    tokio::task::spawn_blocking(move || {
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let env_refs: Vec<(&str, &str)> =
            envs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        bn_runner::run_env(&refs, cwd.as_deref(), &env_refs)
    })
    .await
    .unwrap_or_else(|e| RunResult {
        ok: false,
        envelope: json!({
            "ok": false, "command": "bn", "data": null,
            "error": { "code": "join_error", "message": e.to_string() }
        }),
    })
}

/// Audit a process write and return its envelope as `200` (the envelope's own `ok` conveys the
/// command's success/failure, per the "return the underlying Envelope" contract).
pub(crate) fn respond_process(
    state: &AppState,
    action: &str,
    target: &str,
    addr: Option<SocketAddr>,
    result: RunResult,
) -> Response {
    audit::record(state, action, target, addr, result.ok);
    (StatusCode::OK, Json(result.envelope)).into_response()
}

// ---------------------------------------------------------------------------
// Native state writes
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct QueueAddBody {
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub items: Vec<String>,
}

/// `POST /api/orchestrate/queue {repo?, items:[…]}` — enqueue work (deduped by derived id).
///
/// Goes through `bn queue add` — the durable backlog — instead of injecting into a live run's
/// ledger, which bypassed the ranking, the conflict hold and the names-a-task invariant, and
/// needed a run to already be up. Shelling the CLI (as `POST /api/dispatch` does) keeps the
/// browser and the terminal on one implementation.
pub async fn queue_add(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "orchestrate-queue-add";
    let body: QueueAddBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let items: Vec<String> = body
        .items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if items.is_empty() {
        return bad_request(cmd, "empty", "no items to queue");
    }
    let repo = match resolve_repo(cmd, body.repo) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let (mut added, mut failed) = (Vec::new(), Vec::new());
    for item in &items {
        // Fixed template; `--` ends option parsing so an item starting with `-` is never a flag.
        let args = ["queue", "add", "--repo", &repo, "--", item].map(str::to_string);
        let env = vec![("BN_WORKER_REPO".to_string(), repo.clone())];
        let result = run_bn_env(args.to_vec(), None, env).await;
        match result.ok {
            true => added.push(item.clone()),
            false => failed.push(json!({ "item": item, "envelope": result.envelope })),
        }
    }
    let ok = failed.is_empty();
    audit::record(&state, "queue-add", &repo, addr, ok);
    if !ok && added.is_empty() {
        return unavailable(cmd, "could not enqueue any item");
    }
    Json(Envelope::success(
        cmd,
        json!({ "repo": repo, "added": added, "failed": failed }),
    ))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct RepoQuery {
    #[serde(default)]
    pub repo: Option<String>,
}

/// `DELETE /api/orchestrate/queue/:id?repo=` — drop a still-queued item. In-flight items can't be
/// removed (kill the agent instead), so this is a no-op for anything past `queued`.
pub async fn queue_remove(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(id): Path<String>,
    Query(q): Query<RepoQuery>,
) -> Response {
    let cmd = "orchestrate-queue-remove";
    let id = id.trim().to_string();
    if id.is_empty() {
        return bad_request(cmd, "empty", "no item id");
    }
    let repo = match resolve_repo(cmd, q.repo) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let path = orchestrate::state_path_for(&repo);
    let now = bn_core::util::now_epoch_secs();
    let id_c = id.clone();
    let result = mutate_blocking(path, move |st| {
        let removed = st.remove_queued(&id_c, now);
        (removed, st.counts())
    })
    .await;

    match result {
        Ok(Some((removed, counts))) => {
            audit::record(
                &state,
                "queue-remove",
                &format!("{repo}/{id}"),
                addr,
                removed,
            );
            Json(Envelope::success(
                cmd,
                json!({ "repo": repo, "id": id, "removed": removed, "counts": counts }),
            ))
            .into_response()
        }
        Ok(None) => {
            audit::record(&state, "queue-remove", &format!("{repo}/{id}"), addr, false);
            no_run(cmd, &repo)
        }
        Err(e) => {
            audit::record(&state, "queue-remove", &format!("{repo}/{id}"), addr, false);
            unavailable(cmd, &e.to_string())
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ReorderBody {
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub ids: Vec<String>,
}

/// `POST /api/orchestrate/queue/reorder {repo?, ids:[…]}` — permute the queued subset to follow
/// `ids` (unlisted queued items keep their order after the listed ones). Dispatched/merged/failed
/// items never move.
pub async fn queue_reorder(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "orchestrate-queue-reorder";
    let body: ReorderBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let ids: Vec<String> = body
        .ids
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if ids.is_empty() {
        return bad_request(cmd, "empty", "no ids given");
    }
    let repo = match resolve_repo(cmd, body.repo) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let path = orchestrate::state_path_for(&repo);
    let now = bn_core::util::now_epoch_secs();
    let result = mutate_blocking(path, move |st| {
        let changed = st.reorder_queued(&ids, now);
        (changed, st.counts())
    })
    .await;

    match result {
        Ok(Some((changed, counts))) => {
            audit::record(&state, "queue-reorder", &repo, addr, true);
            Json(Envelope::success(
                cmd,
                json!({ "repo": repo, "changed": changed, "counts": counts }),
            ))
            .into_response()
        }
        Ok(None) => {
            audit::record(&state, "queue-reorder", &repo, addr, false);
            no_run(cmd, &repo)
        }
        Err(e) => {
            audit::record(&state, "queue-reorder", &repo, addr, false);
            unavailable(cmd, &e.to_string())
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ScaleBody {
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub concurrency: usize,
}

/// `POST /api/orchestrate/scale {repo?, concurrency}` — set the run's concurrency cap live; the
/// daemon honors it on the next tick (the #385 adjust-mid-run behavior).
pub async fn scale(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "orchestrate-scale";
    let body: ScaleBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    if body.concurrency < 1 || body.concurrency > 32 {
        return bad_request(cmd, "range", "concurrency must be 1..=32");
    }
    let repo = match resolve_repo(cmd, body.repo) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let path = orchestrate::state_path_for(&repo);
    let concurrency = body.concurrency;
    let result = mutate_blocking(path, move |st| {
        st.concurrency = concurrency;
        st.effective_concurrency()
    })
    .await;

    match result {
        Ok(Some(effective)) => {
            audit::record(&state, "scale", &repo, addr, true);
            Json(Envelope::success(
                cmd,
                json!({ "repo": repo, "concurrency": concurrency, "effective": effective }),
            ))
            .into_response()
        }
        Ok(None) => {
            audit::record(&state, "scale", &repo, addr, false);
            no_run(cmd, &repo)
        }
        Err(e) => {
            audit::record(&state, "scale", &repo, addr, false);
            unavailable(cmd, &e.to_string())
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct RepoBody {
    #[serde(default)]
    pub repo: Option<String>,
}

/// `POST /api/orchestrate/stop {repo?}` — set `stopped` in-process (the daemon's next tick exits)
/// AND run `bn orchestrate stop` for the launchd unload the CLI owns.
pub async fn stop(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "orchestrate-stop";
    let body: RepoBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let repo = match resolve_repo(cmd, body.repo) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    // Native: best-effort flag flip for an immediate stop. The CLI unload below is authoritative
    // and must still run if the state file vanished or its lock is temporarily unavailable.
    let path = orchestrate::state_path_for(&repo);
    let _native_mutation = mutate_blocking(path, move |st| st.stopped = true).await;
    // Process: the launchd unload lives behind the CLI.
    let result = run_bn(
        vec![
            "orchestrate".into(),
            "stop".into(),
            "--repo".into(),
            repo.clone(),
        ],
        None,
    )
    .await;
    respond_process(&state, "stop", &repo, addr, result)
}

// ---------------------------------------------------------------------------
// Process writes
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct DispatchBody {
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub item: String,
    #[serde(default)]
    pub harness: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

/// `POST /api/dispatch {repo, item, harness, kind}` → `bn dispatch --repo … --agent … [--scout] …`.
pub async fn dispatch(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "dispatch";
    let body: DispatchBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let repo = body.repo.trim().to_string();
    if !valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }
    let item = body.item.trim().to_string();
    if item.is_empty() {
        return bad_request(cmd, "empty_item", "item is required");
    }
    let Some(harness) = normalize_harness(body.harness.as_deref()) else {
        return bad_request(cmd, "invalid_harness", "harness must be claude or copilot");
    };
    let scout = matches!(body.kind.as_deref().map(str::trim), Some("scout"));

    // Fixed template — only validated fields are interpolated, each as a distinct argv element.
    let mut args = vec![
        "dispatch".to_string(),
        "--repo".to_string(),
        repo.clone(),
        "--agent".to_string(),
        harness.to_string(),
    ];
    if scout {
        args.push("--scout".to_string());
    }
    // `--` ends option parsing, so a free-text item beginning with `-` is always taken as the
    // positional item and never a flag (defense-in-depth; the item is already one argv element).
    args.push("--".to_string());
    args.push(item.clone());

    // Declare the target as this dispatch's session so the CLI's cross-repo-session guard treats a
    // dashboard dispatch as the target repo's own session (this server runs from its own worktree).
    let result = run_bn_env(
        args,
        None,
        vec![("BN_WORKER_REPO".to_string(), repo.clone())],
    )
    .await;
    respond_process(&state, "dispatch", &repo, addr, result)
}

#[derive(Debug, Default, Deserialize)]
pub struct SendBody {
    #[serde(default)]
    pub message: String,
}

/// `POST /api/agents/:target/send {message}` → `bn agent send <target> <message>`.
pub async fn agent_send(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(target): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "agent_send";
    let body: SendBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    if !valid_target(&target) {
        return bad_request(cmd, "invalid_target", "target must be repo:branch");
    }
    let message = body.message.trim().to_string();
    if message.is_empty() {
        return bad_request(cmd, "empty_message", "message is required");
    }
    // `--` guards a message that begins with `-` from being parsed as a flag; it's still a single
    // argv element, so there is no shell interpretation of its contents.
    let args = vec![
        "agent".to_string(),
        "send".to_string(),
        target.clone(),
        "--".to_string(),
        message,
    ];
    let result = run_bn(args, None).await;
    respond_process(&state, "send", &target, addr, result)
}

#[derive(Debug, Default, Deserialize)]
pub struct RelaunchBody {
    #[serde(default)]
    pub hard: bool,
    /// #501: relaunch the agent under a different harness *in place* — same worktree/branch, note
    /// preserved — instead of the recover path. Canonicalized + availability-gated by
    /// [`bn_core::harness::resolve_relaunch`]; absent/blank ⇒ the plain recover relaunch below.
    #[serde(default)]
    pub harness: Option<String>,
}

/// The `bn agent relaunch` argv a relaunch issues. Pure → unit-testable; `harness`, when present,
/// is already the canonical name from [`bn_core::harness::resolve_relaunch`].
fn relaunch_args(target: &str, hard: bool, harness: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "agent".to_string(),
        "relaunch".to_string(),
        target.to_string(),
    ];
    if hard {
        args.push("--hard".to_string());
    }
    if let Some(h) = harness {
        args.push("--harness".to_string());
        args.push(h.to_string());
    }
    args
}

/// `POST /api/agents/:target/relaunch {hard?, harness?}` → `bn agent relaunch <repo:branch>`.
///
/// Kill the agent's process tree and start it again in the *same* worktree — resuming its own
/// transcript by default, or, with `hard` (or a `harness` change, which implies it), a fresh session
/// seeded from the branch note's handoff. The warm pool slot is deliberately kept; freeing it is
/// `kill`'s job.
///
/// This used to shell out to `bn agent recover --yes`, which is not a relaunch at all — it pushes a
/// branch and opens a PR — and which located the worktree by the conventional
/// `<worktree_dir>/<repo>/<branch>` join, wrong for every pooled agent. Both are fixed in the CLI,
/// which now also *fails* when a relaunch didn't happen, so the dashboard needs no special-casing
/// here: the envelope's `ok` is the truth.
pub async fn agent_relaunch(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(target): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "agent_relaunch";
    let body: RelaunchBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    if !valid_target(&target) {
        return bad_request(cmd, "invalid_target", "target must be repo:branch");
    }

    // Availability-gate an explicit harness here so an org-disabled harness is a `400` the client
    // can show, rather than a subprocess round-trip. The CLI gates it again — this is the fast path,
    // not the enforcement.
    let requested = body
        .harness
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let canonical = match requested {
        Some(h) => match bn_core::harness::resolve_relaunch(&state.cfg, h) {
            Ok(c) => Some(c),
            Err(reason) => return bad_request(cmd, "harness_unavailable", &reason),
        },
        None => None,
    };

    let action = match (canonical, body.hard) {
        (Some(_), _) => "relaunch-harness",
        (None, true) => "relaunch-hard",
        (None, false) => "relaunch",
    };
    let result = run_bn(relaunch_args(&target, body.hard, canonical), None).await;
    respond_process(&state, action, &target, addr, result)
}

/// `POST /api/land {repo?}` → `bn wt merge --all` from a worktree of the repo (the daemon's
/// `land_ready` path). Human-only command → [`crate::bn_runner`] synthesizes the envelope.
pub async fn land(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "land";
    let body: RepoBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let repo = match resolve_repo(cmd, body.repo) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    // The merge-queue keys off the cwd's bare repo, so run it from one of the repo's worktrees.
    let cwd = a_repo_worktree(&state, &repo);
    if cwd.is_none() {
        audit::record(&state, "land", &repo, addr, false);
        return bad_request(
            cmd,
            "no_worktree",
            &format!("no worktree found for repo '{repo}'"),
        );
    }
    let args = vec!["wt".to_string(), "merge".to_string(), "--all".to_string()];
    let result = run_bn(args, cwd).await;
    respond_process(&state, "land", &repo, addr, result)
}

/// Validate a cron job name before it reaches an argv slot: the sanitize-run-key charset (ASCII
/// alphanumerics + `.` `_` `-`), which covers every real job name (`wt-cleanup`, `staff-chief-am`,
/// …). Belt-and-suspenders — the name is already passed as one argv element, never through a shell.
fn valid_job(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[derive(Debug, Default, Deserialize)]
pub struct CronRunBody {
    #[serde(default)]
    pub job: String,
}

/// `POST /api/cron/run {job}` → `bn run <job>`: fire a cron job on demand — the *same* code path a
/// scheduled timer runs, recording the outcome to the event log — and return the runner's envelope
/// (exit + captured output tail). Open per #526 (loopback/LAN trust boundary); a human-only command,
/// so [`crate::bn_runner`] synthesizes the envelope from the process's exit + output.
pub async fn cron_run(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "cron_run";
    let body: CronRunBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let job = body.job.trim().to_string();
    if !valid_job(&job) {
        return bad_request(cmd, "invalid_job", "job has invalid characters");
    }
    // `--` ends option parsing so a job name beginning with `-` is always taken as the positional;
    // it's a single argv element passed straight to `Command`, so there's no shell interpretation.
    let args = vec!["run".to_string(), "--".to_string(), job.clone()];
    let result = run_bn(args, None).await;
    respond_process(&state, "cron-run", &job, addr, result)
}

#[derive(Debug, Default, Deserialize)]
pub struct StartBody {
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub items: Vec<String>,
    #[serde(default)]
    pub concurrency: Option<usize>,
}

/// `POST /api/orchestrate/start {repo?, items, concurrency?}` → `bn orchestrate --queue … --repo …
/// --concurrency …` (starts/extends the launchd-daemonized run).
pub async fn orchestrate_start(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "orchestrate";
    let body: StartBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let items: Vec<String> = body
        .items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if items.is_empty() {
        return bad_request(cmd, "empty", "no items to start");
    }
    // `--queue` takes these as multi-values, so a `-`-leading entry would be parsed as a flag on
    // `bn orchestrate` — reject it (issue #s / URLs / free text never start with '-').
    if items.iter().any(|s| s.starts_with('-')) {
        return bad_request(cmd, "invalid_item", "queue items must not start with '-'");
    }
    let concurrency = body.concurrency.unwrap_or(2);
    if !(1..=32).contains(&concurrency) {
        return bad_request(cmd, "range", "concurrency must be 1..=32");
    }
    let repo = body
        .repo
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(r) = &repo {
        if !valid_repo(r) {
            return bad_request(cmd, "invalid_repo", "repo has invalid characters");
        }
    }

    let mut args = vec!["orchestrate".to_string(), "--queue".to_string()];
    args.extend(items);
    args.push("--concurrency".to_string());
    args.push(concurrency.to_string());
    if let Some(r) = &repo {
        args.push("--repo".to_string());
        args.push(r.clone());
    }

    let target = repo.clone().unwrap_or_else(|| "-".to_string());
    let result = run_bn(args, None).await;
    respond_process(&state, "start", &target, addr, result)
}

// ---------------------------------------------------------------------------
// Plan-reviews (#349)
// ---------------------------------------------------------------------------

/// A `404` carrying the standard failure envelope — for a plan-review id that matches no record.
pub(crate) fn not_found_json(cmd: &str, msg: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(Envelope::failure(cmd, "not_found", msg)),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Attention actions (#483)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct AttnActionBody {
    /// `ack` | `remove` | `plan` | `snooze`.
    #[serde(default)]
    pub action: String,
    /// The branch-note section a `plan` promotion lands in (default `todo`).
    #[serde(default)]
    pub section: Option<String>,
    /// The `YYYY-MM-DD` date a `snooze` hides the item until (required for `snooze`).
    #[serde(default)]
    pub until: Option<String>,
}

/// `POST /api/attention/:id {action, section?, until?}` — resolve an attention item from the
/// dashboard (#483). A native in-process write over `bn_core::attention` (append-only event, like
/// the plan-review decision), authed by the token middleware and audited as a `web_action`:
/// * `ack` — mark it resolved/done.
/// * `remove` — dismiss it as raised in error (a distinct event, so metrics tell it from an ack).
/// * `snooze` — hide it until `until` (`YYYY-MM-DD`).
/// * `plan` — promote it to a branch-note todo (on its own repo/branch when it carries them),
///   resolving it off the list. Runs off the async runtime (event-log + note file IO).
pub async fn attn_action(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "attn-action";
    let body: AttnActionBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let id = id.trim().to_string();
    // Attention ids are `a<N>` — the same tight alphanumeric allowlist as review ids.
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return bad_request(
            cmd,
            "invalid_id",
            "attention id must be alphanumeric (e.g. a1)",
        );
    }
    let action = body.action.trim().to_lowercase();
    let action_label = format!("attn-{action}");

    let response = match action.as_str() {
        "ack" | "remove" => {
            let id_call = id.clone();
            let act = action.clone();
            let res = tokio::task::spawn_blocking(move || {
                if act == "ack" {
                    bn_core::attention::ack(&id_call)
                } else {
                    bn_core::attention::remove(&id_call)
                }
            })
            .await;
            attn_bool_response(&state, cmd, &action_label, &id, addr, res)
        }
        "snooze" => {
            let Some(until) = body
                .until
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                return bad_request(
                    cmd,
                    "missing_until",
                    "snooze needs a YYYY-MM-DD `until` date",
                );
            };
            let (id_call, until_s) = (id.clone(), until.to_string());
            let res =
                tokio::task::spawn_blocking(move || bn_core::attention::snooze(&id_call, &until_s))
                    .await;
            // A bad date surfaces as the action's `Err` (→ 400); an unknown id as `Ok(false)`.
            attn_bool_response(&state, cmd, &action_label, &id, addr, res)
        }
        "plan" => {
            let section = body
                .section
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("todo")
                .to_string();
            let cfg = state.cfg.clone();
            let id_call = id.clone();
            let res = tokio::task::spawn_blocking(move || {
                bn_core::attention::plan_item(
                    &cfg,
                    std::path::Path::new("."),
                    &id_call,
                    &section,
                    &bn_core::util::today(),
                )
            })
            .await;
            match res {
                Ok(Ok(out)) => {
                    audit::record(&state, &action_label, &id, addr, true);
                    Json(Envelope::success(
                        cmd,
                        bn_core::payload::attn_plan_data(
                            &id,
                            bn_core::payload::PlannedTodo {
                                id: &out.todo.id,
                                section: out.todo.section.label(),
                                rb: out.ctx.rb(),
                            },
                        ),
                    ))
                    .into_response()
                }
                Ok(Err(e)) => {
                    use bn_core::attention::PlanError;
                    audit::record(&state, &action_label, &id, addr, false);
                    match e {
                        PlanError::NoItem => {
                            not_found_json(cmd, &format!("no attention item {id}"))
                        }
                        PlanError::NoTarget(_) => bad_request(
                            cmd,
                            "no_target",
                            "item carries no repo/branch to plan onto",
                        ),
                        PlanError::Add(e) => bad_request(cmd, e.code(), &e.message()),
                        PlanError::Resolve(e) => (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(Envelope::failure(cmd, "io", e.to_string())),
                        )
                            .into_response(),
                    }
                }
                Err(e) => {
                    audit::record(&state, &action_label, &id, addr, false);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(Envelope::failure(cmd, "join_error", e.to_string())),
                    )
                        .into_response()
                }
            }
        }
        _ => bad_request(
            cmd,
            "invalid_action",
            "action must be ack | remove | snooze | plan",
        ),
    };

    // Acking, snoozing or planning an item changes both signal views. Without this the row the user
    // just cleared would keep rendering — and the rail's badge keep counting it — until the TTL
    // expired, which reads as the click having done nothing.
    state.views.invalidate(crate::view_cache::ATTN).await;
    state.views.invalidate(crate::view_cache::ACTIONS).await;
    response
}

/// Shape the response for the boolean-returning attention actions (ack/remove/snooze): `Ok(true)`
/// → `200` with `{id, ok:true}`; `Ok(false)` (unknown id) → `404`; an `Err`/join failure → the
/// error envelope. Every outcome is audited with `ok` reflecting whether the item was found.
fn attn_bool_response(
    state: &AppState,
    cmd: &str,
    action_label: &str,
    id: &str,
    addr: Option<SocketAddr>,
    res: Result<std::io::Result<bool>, tokio::task::JoinError>,
) -> Response {
    match res {
        Ok(Ok(true)) => {
            audit::record(state, action_label, id, addr, true);
            Json(Envelope::success(
                cmd,
                bn_core::payload::attn_resolve_data(id, true),
            ))
            .into_response()
        }
        Ok(Ok(false)) => {
            audit::record(state, action_label, id, addr, false);
            not_found_json(cmd, &format!("no attention item {id}"))
        }
        Ok(Err(e)) => {
            audit::record(state, action_label, id, addr, false);
            bad_request(cmd, "action_failed", &e.to_string())
        }
        Err(e) => {
            audit::record(state, action_label, id, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "join_error", e.to_string())),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Handoff write (#509) — native in-process resume-packet write, open per #526
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct HandoffWriteBody {
    /// The target repo (normalized to lowercase by the resolver).
    #[serde(default)]
    pub repo: Option<String>,
    /// The target branch (slashes normalized to dashes by the resolver).
    #[serde(default)]
    pub branch: Option<String>,
    /// Layer 2: the narrative — why it looks this way. Blank/absent leaves it empty.
    #[serde(default)]
    pub narrative: Option<String>,
    /// Layer 3: decisions deferred to the next session (the half the note can't supply).
    #[serde(default)]
    pub deferred: Vec<String>,
    /// Layer 4: extra open questions beyond the note's asks.
    #[serde(default)]
    pub questions: Vec<String>,
    /// Layer 5: an explicit priority queue; empty defaults to the note's open todos.
    #[serde(default)]
    pub priority_queue: Vec<String>,
}

/// `POST /api/handoff {repo, branch, narrative?, deferred?, questions?, priority_queue?}` — compose
/// and persist the branch's resume packet from the dashboard (#509), the write counterpart to the
/// `GET /api/handoff` read. A native in-process write via [`bn_core::handoff::write`]: it reads the
/// branch note, composes the five layers with the author-supplied ones, and writes `handoff.json`
/// (+ the `.md` mirror) beside the note. Open per #526 (loopback/LAN trust boundary) and audited as
/// a `web_action`. `400` on a missing/invalid target; `500` (`no_note`) when the branch has no note
/// to compose from.
pub async fn handoff_write(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "handoff-write";
    let body: HandoffWriteBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };

    let repo = body.repo.map(|s| s.trim().to_string()).unwrap_or_default();
    let branch = body
        .branch
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if repo.is_empty() || branch.is_empty() {
        return bad_request(cmd, "missing_target", "repo and branch are required");
    }
    if !valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }

    let Ok(ctx) = bn_core::resolver::resolve_target(
        &state.cfg,
        std::path::Path::new("."),
        Some(repo.clone()),
        Some(branch.clone()),
    ) else {
        return bad_request(cmd, "invalid_target", "could not resolve repo/branch");
    };

    let input = bn_core::handoff::HandoffInput {
        narrative: body.narrative.filter(|s| !s.trim().is_empty()),
        deferred: body.deferred,
        questions: body.questions,
        priority_queue: body.priority_queue,
    };

    let cfg = state.cfg.clone();
    let result = tokio::task::spawn_blocking(move || {
        bn_core::handoff::write(&cfg, &ctx, &bn_core::util::today(), &input)
    })
    .await;

    let target = format!("{repo}/{branch}");
    match result {
        Ok(Ok(h)) => {
            audit::record(&state, "handoff-write", &target, addr, true);
            let data = json!({
                "handoff": h,
                "markdown": bn_core::handoff::render_markdown(&h),
                "rendered": bn_core::handoff::render_orientation(&h),
            });
            Json(Envelope::success(cmd, data)).into_response()
        }
        Ok(Err(e)) => {
            audit::record(&state, "handoff-write", &target, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(
                    cmd,
                    "no_note",
                    format!("handoff write failed: {e}"),
                )),
            )
                .into_response()
        }
        Err(e) => {
            audit::record(&state, "handoff-write", &target, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "join_error", e.to_string())),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Goal-note connect/disconnect (#564) — native in-process daily-goal writes, open per #526
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct GoalNoteConnectBody {
    /// A linked branch note's repo (paired with `branch`); a link needs both.
    #[serde(default)]
    pub repo: Option<String>,
    /// The linked branch note's branch (note-key form).
    #[serde(default)]
    pub branch: Option<String>,
    /// Attached free-text / uploaded note content (used when there's no complete link).
    #[serde(default)]
    pub text: Option<String>,
    /// Optional human label (an upload's filename, or a title for the text).
    #[serde(default)]
    pub title: Option<String>,
}

/// A daily-goal id is `g<digits>` (matching [`bn_core::daily`]'s allocation).
fn valid_goal_id(s: &str) -> bool {
    s.strip_prefix('g')
        .is_some_and(|r| !r.is_empty() && r.len() <= 12 && r.bytes().all(|b| b.is_ascii_digit()))
}

/// A goal-note id is `n<digits>` (per-goal, allocated by [`bn_core::daily::connect_note`]).
fn valid_note_id(s: &str) -> bool {
    s.strip_prefix('n')
        .is_some_and(|r| !r.is_empty() && r.len() <= 12 && r.bytes().all(|b| b.is_ascii_digit()))
}

/// Map a [`bn_core::commands::CmdError`] to the standard failure envelope + HTTP status: a missing
/// goal/note is a `404`, an IO failure a `500`, everything else (empty input, ambiguous query) a
/// `400`. The envelope's `code`/`message` come straight from the error so the CLI, MCP, and web
/// speak one shape.
fn cmd_error_response(cmd: &str, e: &bn_core::commands::CmdError) -> Response {
    use bn_core::commands::CmdError;
    let status = match e {
        CmdError::NotFound(_) | CmdError::NoNote => StatusCode::NOT_FOUND,
        CmdError::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::BAD_REQUEST,
    };
    (status, Json(Envelope::failure(cmd, e.code(), e.message()))).into_response()
}

/// Shared reply for the two goal-note writes: audit, then render the outcome (`goal` + `note`) on
/// success, map the `CmdError` on a domain failure, or `500` a spawn-join failure.
fn goal_note_response(
    state: &AppState,
    cmd: &str,
    goal_id: &str,
    addr: Option<SocketAddr>,
    res: Result<
        Result<bn_core::daily::NoteOutcome, bn_core::commands::CmdError>,
        tokio::task::JoinError,
    >,
) -> Response {
    match res {
        Ok(Ok(out)) => {
            audit::record(state, cmd, goal_id, addr, true);
            Json(Envelope::success(
                cmd,
                bn_core::payload::goal_note_data(&out),
            ))
            .into_response()
        }
        Ok(Err(e)) => {
            audit::record(state, cmd, goal_id, addr, false);
            cmd_error_response(cmd, &e)
        }
        Err(e) => {
            audit::record(state, cmd, goal_id, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "join_error", e.to_string())),
            )
                .into_response()
        }
    }
}

/// `POST /api/goals/:id/notes {repo?, branch?, text?, title?}` — connect a note to today's daily
/// goal `id` (#564): a linked branch note (`repo`+`branch`) and/or attached free-text / uploaded
/// content. A native in-process write via [`bn_core::daily::connect_note`] (stored inline on the
/// goal so it survives carry-forward). Open per #526 and audited. `400` on a bad id / empty input /
/// oversize text; `404` when no goal `id` exists on today's list.
pub async fn goal_note_connect(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "goal-note-connect";
    let body: GoalNoteConnectBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let id = id.trim().to_string();
    if !valid_goal_id(&id) {
        return bad_request(cmd, "invalid_id", "goal id must be like g1");
    }
    // If a repo is supplied it becomes a note-jump ref; keep it to the safe path/key charset.
    if let Some(r) = body
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !valid_repo(r) {
            return bad_request(cmd, "invalid_repo", "repo has invalid characters");
        }
    }
    // Bound the attached text so a paste/upload can't write an unbounded file.
    if body.text.as_deref().is_some_and(|t| t.len() > 100_000) {
        return bad_request(cmd, "text_too_large", "attached text exceeds 100 KB");
    }

    let cfg = state.cfg.clone();
    let id_call = id.clone();
    let input = bn_core::daily::NoteInput {
        repo: body.repo,
        branch: body.branch,
        text: body.text,
        title: body.title,
    };
    let res = tokio::task::spawn_blocking(move || {
        bn_core::daily::connect_note(&cfg, &bn_core::util::today(), &id_call, input)
    })
    .await;
    goal_note_response(&state, cmd, &id, addr, res)
}

/// `DELETE /api/goals/:id/notes/:note_id` — disconnect note `note_id` from today's daily goal `id`
/// (#564), the inverse of [`goal_note_connect`]. `400` on a bad id; `404` when the goal or note is
/// gone.
pub async fn goal_note_disconnect(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path((id, note_id)): Path<(String, String)>,
) -> Response {
    let cmd = "goal-note-disconnect";
    let id = id.trim().to_string();
    let note_id = note_id.trim().to_string();
    if !valid_goal_id(&id) {
        return bad_request(cmd, "invalid_id", "goal id must be like g1");
    }
    if !valid_note_id(&note_id) {
        return bad_request(cmd, "invalid_note_id", "note id must be like n1");
    }
    let cfg = state.cfg.clone();
    let (id_call, nid) = (id.clone(), note_id.clone());
    let res = tokio::task::spawn_blocking(move || {
        bn_core::daily::disconnect_note(&cfg, &bn_core::util::today(), &id_call, &nid)
    })
    .await;
    goal_note_response(&state, cmd, &id, addr, res)
}

/// Find a worktree of `repo` to run a cwd-scoped command (`wt merge --all`) from. Mirrors the
/// daemon's `a_repo_worktree`: the first existing `<worktree_dir>/<repo>/<*>` directory.
fn a_repo_worktree(state: &AppState, repo: &str) -> Option<PathBuf> {
    // `valid_repo` permits `.`, so guard against a traversal key (`..`, path separators) escaping
    // worktree_dir before we join it — this is the only place a repo becomes a filesystem path.
    if repo.is_empty() || repo.contains("..") || repo.contains('/') || repo.contains('\\') {
        return None;
    }
    let repo_dir = state.cfg.worktree_dir.join(repo);
    let rd = std::fs::read_dir(&repo_dir).ok()?;
    rd.flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir() && p.join(".git").exists())
}

// ---------------------------------------------------------------------------
// Task writes (#1006, EPIC #993)
// ---------------------------------------------------------------------------
//
// Split into `task_writes.rs` (store/doc writes) and `task_attach_api.rs` (the only handlers that
// move bytes). Re-exported flat, so every `actions::task_*` route stays spelled the way it was.
pub use crate::task_attach_api::{
    task_attach_add, task_attach_delete, task_attach_get, task_attach_list,
};
pub use crate::task_writes::{
    task_action, task_agent, task_create, task_doc_append, task_doc_edit,
};
