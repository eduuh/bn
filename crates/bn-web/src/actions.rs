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

#[path = "actions/prompts.rs"]
mod prompts;
use prompts::repo_orchestrator_goal;

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
fn server_error(command: &str, code: &str, msg: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(Envelope::failure(command, code, msg)),
    )
        .into_response()
}

fn bad_request(command: &str, code: &str, msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(Envelope::failure(command, code, msg)),
    )
        .into_response()
}

/// A `409` carrying the standard failure envelope — the requested start is a no-op because the
/// resource already exists (an orchestrator is already running for this repo, #556).
fn conflict(command: &str, code: &str, msg: &str) -> Response {
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
async fn run_bn(args: Vec<String>, cwd: Option<PathBuf>) -> RunResult {
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

/// `POST /api/agents/:target/kill` → the new `bn agent kill <target>` (tmux kill-window).
pub async fn agent_kill(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(target): Path<String>,
) -> Response {
    let cmd = "agent_kill";
    if !valid_target(&target) {
        return bad_request(cmd, "invalid_target", "target must be repo:branch");
    }
    let args = vec!["agent".to_string(), "kill".to_string(), target.clone()];
    let result = run_bn(args, None).await;
    respond_process(&state, "kill", &target, addr, result)
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

/// `POST /api/repos/{repo}/pool/warm` → `bn wt pool warm --repo <repo>` (#497): pre-build every slot
/// of a repo's warm worktree pool from the dashboard. Warming is LONG-RUNNING (a checkout + the
/// repo's `build.sh` per slot), so the request must NOT block on it — the command is spawned detached
/// (nulled stdio, reaped in the background) and the handler returns a "warming started" ack
/// immediately. Open per #526 (loopback/LAN trust boundary), like the cron-run handler.
pub async fn repo_pool_warm(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(repo): Path<String>,
) -> Response {
    let cmd = "repo_pool_warm";
    let repo = repo.trim().to_string();
    if !valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }
    // Only a repo actually in the pools config can be warmed (warming is what materializes its
    // slots), so reject an unpooled/unknown repo up front — otherwise the detached spawn would
    // silently no-op (the CLI just prints "not pooled" and exits). Read via the shared bn-core pool.
    if bn_core::pool::pool_size(&repo).is_none() {
        return bad_request(
            cmd,
            "not_pooled",
            "repo is not pooled — add it to the pools config first",
        );
    }
    // `--repo=<value>` is a single unambiguous argv element (a value beginning with `-` can't be
    // mistaken for a flag), passed straight to `Command` — no shell, mirroring the runner doctrine.
    let repo_arg = format!("--repo={repo}");
    let spawned = tokio::task::spawn_blocking(move || {
        bn_runner::spawn_detached(&["wt", "pool", "warm", &repo_arg])
    })
    .await;
    let result = match spawned {
        Ok(Ok(pid)) => RunResult {
            ok: true,
            envelope: json!({
                "ok": true,
                "command": cmd,
                "data": {
                    "repo": repo,
                    "status": "started",
                    "pid": pid,
                    "message": format!("warming pool for {repo} (running in the background)"),
                },
                "error": null,
            }),
        },
        Ok(Err(e)) => RunResult {
            ok: false,
            envelope: json!({
                "ok": false, "command": cmd, "data": null,
                "error": { "code": "spawn_failed", "message": format!("failed to start warm: {e}") },
            }),
        },
        Err(e) => RunResult {
            ok: false,
            envelope: json!({
                "ok": false, "command": cmd, "data": null,
                "error": { "code": "join_error", "message": e.to_string() },
            }),
        },
    };
    respond_process(&state, "repo-pool-warm", &repo, addr, result)
}

/// `POST /api/repos/{repo}/pool` body: the slot count to pool at, or `null`/omitted to stop pooling.
#[derive(Debug, Default, Deserialize)]
pub struct PoolSetBody {
    #[serde(default)]
    pub size: Option<usize>,
}

/// `POST /api/repos/{repo}/pool {size?}` — turn pooling on for a repo at `size` slots, or off when
/// `size` is absent. The counterpart to the warm trigger: warming could only ever act on a repo
/// someone had already hand-added to `~/.config/wt/pools`, so on a machine that had never edited
/// that file every card read "pooling off" with no way out of it from here.
///
/// Writes the config in-process through `bn_core::pool` (the same edit `bn wt pool set` performs)
/// rather than spawning the CLI — it's a small atomic file rewrite, not a long job, so the response
/// can report the real outcome instead of an ack. Slots are not built here; the idle `pool-warm`
/// cron picks the repo up, or the warm button builds them now.
pub async fn repo_pool_set(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(repo): Path<String>,
    Json(body): Json<PoolSetBody>,
) -> Response {
    let cmd = "repo_pool_set";
    let repo = repo.trim().to_string();
    if !valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }
    // `default` and `*` are the config's reserved any-repo keys; writing one as a repo name would
    // silently re-point every unlisted repo's pool.
    if repo == "default" || repo == "*" {
        return bad_request(cmd, "reserved_repo", "that name is reserved by the config");
    }
    // Only a repo bn actually has a clone of — a typo here would become a dead config line that
    // nothing ever warms.
    let cloned = bn_core::lifecycle::cloned_repos(&state.cfg);
    if !cloned.contains(&repo.to_ascii_lowercase()) {
        return bad_request(cmd, "unknown_repo", "no bare clone for that repo here");
    }
    if let Some(n) = body.size {
        // Slot 1 is the controller's, so a size of 1 is a pool with nowhere to dispatch. Refuse it
        // rather than let the writer clamp it up — a caller that asked for 1 should hear that back.
        if !(bn_core::pool::MIN_POOL_SIZE..=bn_core::pool::MAX_POOL_SIZE).contains(&n) {
            return bad_request(
                cmd,
                "invalid_size",
                &format!(
                    "size must be {}..={} (slot 1 is the controller's)",
                    bn_core::pool::MIN_POOL_SIZE,
                    bn_core::pool::MAX_POOL_SIZE
                ),
            );
        }
    }
    let (repo_w, size) = (repo.clone(), body.size);
    let wrote = tokio::task::spawn_blocking(move || bn_core::pool::set_pool_size(&repo_w, size))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
    let result = match wrote {
        Ok(()) => RunResult {
            ok: true,
            envelope: json!({
                "ok": true,
                "command": cmd,
                "data": {
                    "repo": repo,
                    "size": size,
                    "message": match size {
                        Some(n) => format!("pooling {repo} at {n} slot(s)"),
                        None => format!("stopped pooling {repo}"),
                    },
                },
                "error": null,
            }),
        },
        Err(e) => RunResult {
            ok: false,
            envelope: json!({
                "ok": false, "command": cmd, "data": null,
                "error": { "code": "write_failed", "message": format!("could not write the pools config: {e}") },
            }),
        },
    };
    respond_process(&state, "repo-pool-set", &repo, addr, result)
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

#[derive(Debug, Default, Deserialize)]
pub struct RepoOrchestrateBody {
    /// The orchestrator's goal (what backlog to drain / how). Defaults to a repo-issue-draining
    /// brief when omitted, so the one-click button needs no input.
    #[serde(default)]
    pub goal: Option<String>,
    /// Launch harness — `copilot` (default) or `claude`.
    #[serde(default)]
    pub harness: Option<String>,
}

/// `POST /api/repos/{repo}/orchestrate {goal?, harness?}` (#556) — start a **main-branch**
/// orchestrator agent for one repo: the long-lived #500/#541 supervisor that spins up + supervises
/// a fleet to drain the repo's open-issue backlog. Shells out to `bn dispatch --orchestrator --repo
/// <repo> --yolo` (the orchestrator role model), which pins the agent to the shared planning
/// cockpit and attributes its run as orchestration work.
///
/// **Guarded / idempotent**: if a live orchestrator agent already targets this repo, the start is a
/// `409` no-op (never a second supervisor for the same repo). The guard reads the live fleet — the
/// same source the dashboard's Orchestrators tier renders — so what the UI shows and what this
/// rejects on are one truth.
pub async fn repo_orchestrate_start(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(repo): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "repo-orchestrate";
    let repo = repo.trim().to_string();
    if !valid_repo(&repo) {
        return bad_request(cmd, "invalid_repo", "repo has invalid characters");
    }
    let body: RepoOrchestrateBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let Some(harness) = normalize_harness(body.harness.as_deref()) else {
        return bad_request(cmd, "invalid_harness", "harness must be claude or copilot");
    };

    // Guard: never start a second orchestrator for a repo that already has a live one. Read the
    // live fleet (the same supervise pass the dashboard's Orchestrators tier renders from).
    let cfg = state.cfg.clone();
    let repo_key = repo.to_ascii_lowercase();
    let already = tokio::task::spawn_blocking(move || {
        let fleet = bn_core::agent::supervise(
            &cfg,
            bn_core::agent::DEFAULT_IDLE_MINS,
            bn_core::agent::DEFAULT_STUCK_MINS,
        );
        fleet.agents.iter().any(|a| {
            a.repo.eq_ignore_ascii_case(&repo_key)
                && a.role == bn_core::agent::AgentRole::Orchestrator
        })
    })
    .await
    .unwrap_or(false);
    if already {
        return conflict(
            cmd,
            "already_running",
            &format!("an orchestrator is already running for '{repo}'"),
        );
    }

    // An explicit brief overrides the Markdown-owned default backlog-drain goal.
    let goal = match body
        .goal
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(goal) => goal,
        None => match repo_orchestrator_goal(&state.cfg, &repo) {
            Ok(goal) => goal,
            Err(error) => return server_error(cmd, "prompt_error", &error),
        },
    };

    // Fixed template — only validated fields are interpolated, each a distinct argv element. `--`
    // ends option parsing so the free-text goal is always the positional item, never a flag.
    let args = vec![
        "dispatch".to_string(),
        "--repo".to_string(),
        repo.clone(),
        "--orchestrator".to_string(),
        "--agent".to_string(),
        harness.to_string(),
        "--yolo".to_string(),
        "--".to_string(),
        goal,
    ];
    // Declare the target as this dispatch's session (cross-repo-session guard exemption); the
    // orchestrator itself runs in the target repo, so its own re-dispatches stay in-session too.
    let result = run_bn_env(
        args,
        None,
        vec![("BN_WORKER_REPO".to_string(), repo.clone())],
    )
    .await;
    respond_process(&state, "repo-orchestrate", &repo, addr, result)
}

// ---------------------------------------------------------------------------
// Plan-reviews (#349)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ReviewDecisionBody {
    /// `approve` | `request_changes` | `reject` (hyphen spellings also accepted).
    #[serde(default)]
    pub action: String,
    /// Reviewer notes — required for `request_changes`, sent back to the agent.
    #[serde(default)]
    pub notes: Option<String>,
}

/// `POST /api/reviews/:id {action, notes?}` — the human's plan-review decision from the dashboard
/// (#349). A native in-process write over `bn_core::plan_review::decide` (locked read-modify-write,
/// like the orchestrate queue mutations), authed by the token middleware and audited as a
/// `web_action`. `approve` unblocks the paused agent, `request_changes` sends the notes back,
/// `reject` stops it. The agent's own `bn review status` poll picks up the decision.
pub async fn review_decide(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "review-decide";
    let body: ReviewDecisionBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let id = id.trim().to_string();
    // Review ids are `r<N>` — a tight allowlist keeps a bogus path segment out of the store lookup.
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return bad_request(
            cmd,
            "invalid_id",
            "review id must be alphanumeric (e.g. r1)",
        );
    }
    let Some(decision) = bn_core::plan_review::Decision::parse(&body.action) else {
        return bad_request(
            cmd,
            "invalid_action",
            "action must be approve | request_changes | reject",
        );
    };
    let notes = body
        .notes
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let now = bn_core::util::now_epoch_secs();
    let action_label = format!(
        "review-{}",
        body.action.trim().to_lowercase().replace('_', "-")
    );

    let id_for_call = id.clone();
    let result = tokio::task::spawn_blocking(move || {
        bn_core::plan_review::decide(&id_for_call, decision, notes.as_deref(), now)
    })
    .await;

    match result {
        Ok(Ok(review)) => {
            audit::record(&state, &action_label, &id, addr, true);
            Json(Envelope::success(
                cmd,
                bn_core::payload::plan_review_data(&review),
            ))
            .into_response()
        }
        Ok(Err(e)) => {
            use bn_core::plan_review::DecideError;
            audit::record(&state, &action_label, &id, addr, false);
            match e {
                DecideError::NotFound(_) => not_found_json(cmd, &e.to_string()),
                DecideError::MissingNotes => bad_request(cmd, "missing_notes", &e.to_string()),
                DecideError::EmptyPlan => bad_request(cmd, "empty_plan", &e.to_string()),
                // Already decided / awaiting the agent's revision — a conflict, not a bad request.
                DecideError::NotPending { .. } => (
                    StatusCode::CONFLICT,
                    Json(Envelope::failure(cmd, "not_pending", e.to_string())),
                )
                    .into_response(),
                DecideError::Io(_) => (
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

/// A `404` carrying the standard failure envelope — for a plan-review id that matches no record.
fn not_found_json(cmd: &str, msg: &str) -> Response {
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

    match action.as_str() {
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
    }
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
// Skill CRUD (#483) — create / edit / delete, each followed by a native harness sync
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct SkillEditBody {
    /// The full replacement markdown source (YAML frontmatter + body).
    #[serde(default)]
    pub content: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct SkillCreateBody {
    /// The skill name — also the file stem (`<name>.md`) and how it is invoked.
    #[serde(default)]
    pub name: String,
    /// The one-line `description:` frontmatter. Required — a skill without one is unservable.
    #[serde(default)]
    pub description: String,
    /// Which tier to write into ([`bn_core::skills::SkillScope::parse`] labels). Defaults to
    /// `global-personal`: private, present in every repo, and available even when bn-web runs
    /// outside a git checkout.
    #[serde(default)]
    pub scope: Option<String>,
    /// The markdown body below the frontmatter. May be empty (a stub to fill in later).
    #[serde(default)]
    pub body: String,
    /// Replace an existing skill of the same name in that tier instead of failing with a conflict.
    #[serde(default)]
    pub overwrite: bool,
}

/// The tier a `POST /api/skills` with no explicit `scope` writes into.
const DEFAULT_CREATE_SCOPE: &str = "global-personal";

/// Validate a skill name for the CRUD endpoints: the file-stem charset (letters, digits, `-`, `_`),
/// non-empty and bounded. Keeps a bogus segment out of discovery and off any path.
fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Re-materialize the native Agent Skills for Claude and Copilot after a source mutation, and
/// summarize the result for the response body.
///
/// Deliberately **non-fatal**: the bn source file is the system of record and has already been
/// written, so a sync failure (an unwritable harness root, a name collision with a skill bn does
/// not own) is reported alongside the success rather than turning a completed write into a `500`.
/// The caller sees `{ synced: true, created, updated, removed, unchanged }` or
/// `{ synced: false, error }` and can surface it.
fn sync_native_skills_summary(cfg: &bn_core::resolver::Config) -> serde_json::Value {
    use bn_core::skills::NativeInstallAction as A;
    let home = dirs::home_dir().unwrap_or_default();
    let report = bn_core::skills::sync_native_skills(cfg, std::path::Path::new("."), &home);
    let count = |want: A| {
        report
            .changes
            .iter()
            .filter(|change| change.action == want)
            .count()
    };
    // The harnesses that actually got refreshed — reported separately from the ones that failed, so
    // a partial sync (one harness clashing with a non-bn skill) is legible instead of all-or-nothing.
    let harnesses: std::collections::BTreeSet<&str> = report
        .changes
        .iter()
        .map(|change| change.harness.label())
        .collect();
    json!({
        "synced": report.complete(),
        "harnesses": harnesses,
        "created": count(A::Created),
        "updated": count(A::Updated),
        "removed": count(A::Removed),
        "unchanged": count(A::Unchanged),
        "failures": report.failures,
    })
}

/// `POST /api/skills {name, description, scope?, body?, overwrite?}` — create a skill from the
/// dashboard. Writes `<tier>/<name>.md` through [`bn_core::skills::create_skill`] (the same engine
/// as `bn skills new`), then syncs the native Claude/Copilot skills so the new workflow is
/// immediately invocable in both harnesses. `400` on a bad name/description/scope; `409` when the
/// tier already defines that name and `overwrite` is false.
pub async fn skill_create(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "skill-create";
    let body: SkillCreateBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let name = body.name.trim().to_string();
    if !valid_skill_name(&name) {
        return bad_request(
            cmd,
            "invalid_name",
            "skill name may contain letters, digits, '-', '_'",
        );
    }
    let description = body.description.trim().to_string();
    if description.is_empty() {
        return bad_request(cmd, "empty_description", "description is required");
    }
    let scope_label = body.scope.as_deref().unwrap_or(DEFAULT_CREATE_SCOPE);
    let Some(scope) = bn_core::skills::SkillScope::parse(scope_label) else {
        return bad_request(
            cmd,
            "invalid_scope",
            "scope must be global-shared | global-personal | repo-shared | repo-personal-notes | repo-personal-local",
        );
    };

    let cfg = state.cfg.clone();
    let (name_call, skill_body, overwrite) = (name.clone(), body.body.clone(), body.overwrite);
    let result = tokio::task::spawn_blocking(move || {
        let path = bn_core::skills::create_skill(
            &cfg,
            std::path::Path::new("."),
            scope,
            &name_call,
            &description,
            &skill_body,
            &[],
            overwrite,
        )?;
        let native = sync_native_skills_summary(&cfg);
        Ok::<_, std::io::Error>(json!({
            "skill": { "name": name_call, "scope": scope, "path": path.display().to_string() },
            "native": native,
        }))
    })
    .await;

    match result {
        Ok(Ok(data)) => {
            audit::record(&state, cmd, &name, addr, true);
            Json(Envelope::success(cmd, data)).into_response()
        }
        Ok(Err(e)) => {
            audit::record(&state, cmd, &name, addr, false);
            let msg = e.to_string();
            // `create_skill` refuses to clobber an existing file unless `overwrite` — that's a
            // conflict the UI resolves by re-submitting, not a malformed request.
            if msg.contains("already exists") {
                (
                    StatusCode::CONFLICT,
                    Json(Envelope::failure(cmd, "skill_exists", msg)),
                )
                    .into_response()
            } else {
                bad_request(cmd, "create_failed", &msg)
            }
        }
        Err(e) => {
            audit::record(&state, cmd, &name, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "join_error", e.to_string())),
            )
                .into_response()
        }
    }
}

/// `DELETE /api/skills/:name` — delete the *effective* skill `name` from the dashboard, then sync
/// the native harnesses so Claude and Copilot drop it too. The path comes from discovery, never
/// from the request (no arbitrary-path delete). `404` when no tier defines `name`.
///
/// Removes one definition, not the name: if a lower tier also defines `name`, that skill becomes
/// the effective one and stays installed. The response's `remaining_scope` says whether that
/// happened, so the UI can tell "gone" from "unshadowed".
pub async fn skill_delete(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(name): Path<String>,
) -> Response {
    let cmd = "skill-delete";
    let name = name.trim().to_string();
    if !valid_skill_name(&name) {
        return bad_request(
            cmd,
            "invalid_name",
            "skill name may contain letters, digits, '-', '_'",
        );
    }

    let cfg = state.cfg.clone();
    let name_call = name.clone();
    // `Err(None)` = no such skill (404); `Err(Some(msg))` = the delete itself failed (500).
    let result =
        tokio::task::spawn_blocking(move || -> Result<serde_json::Value, Option<String>> {
            let here = std::path::Path::new(".");
            let info = bn_core::skills::effective_info(&cfg, here, &name_call)
                .map_err(|error| Some(error.to_string()))?
                .ok_or(None)?;
            bn_core::skills::delete_skill(std::path::Path::new(&info.path)).map_err(Some)?;
            // A lower tier may now be the effective definition — report it rather than implying the
            // name is gone.
            let remaining = bn_core::skills::effective_info(&cfg, here, &name_call)
                .map_err(|error| Some(error.to_string()))?;
            let native = sync_native_skills_summary(&cfg);
            Ok(json!({
                "skill": info,
                "remaining_scope": remaining.map(|r| r.scope),
                "native": native,
            }))
        })
        .await;

    match result {
        Ok(Ok(data)) => {
            audit::record(&state, cmd, &name, addr, true);
            Json(Envelope::success(cmd, data)).into_response()
        }
        Ok(Err(None)) => {
            audit::record(&state, cmd, &name, addr, false);
            not_found_json(cmd, &format!("no skill named '{name}'"))
        }
        Ok(Err(Some(msg))) => {
            audit::record(&state, cmd, &name, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "delete_failed", msg)),
            )
                .into_response()
        }
        Err(e) => {
            audit::record(&state, cmd, &name, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "join_error", e.to_string())),
            )
                .into_response()
        }
    }
}

/// `PUT /api/skills/:name {content}` — edit the *effective* skill `name` from the dashboard
/// (#483). A native in-process write: it resolves the highest-precedence skill file (the one
/// bn-mcp serves), validates the new `content` parses as a skill so a bad edit can't corrupt it,
/// then overwrites the file and syncs the native Claude/Copilot skills so both harnesses pick the
/// edit up immediately. Audited as a `web_action`. `404` when no tier defines `name`; `400` when
/// the content isn't a valid skill. Only edits an existing skill — the file path comes from
/// discovery, never from the request (no arbitrary-path write).
pub async fn skill_write(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(name): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "skill-edit";
    let body: SkillEditBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let name = name.trim().to_string();
    if !valid_skill_name(&name) {
        return bad_request(
            cmd,
            "invalid_name",
            "skill name may contain letters, digits, '-', '_'",
        );
    }
    if body.content.trim().is_empty() {
        return bad_request(cmd, "empty_content", "content is required");
    }
    // Validate before touching the filesystem, so a bad edit is a clean 400 (never a corrupt file).
    if bn_core::skills::parse_skill(&body.content, &name).is_err() {
        return bad_request(
            cmd,
            "invalid_content",
            "content must be a skill: `---` YAML frontmatter with a `description`, then a body",
        );
    }

    let cfg = state.cfg.clone();
    let (name_call, content) = (name.clone(), body.content.clone());
    // `Err(None)` = no such skill (404); `Err(Some(msg))` = an IO failure on a known path (500).
    let result =
        tokio::task::spawn_blocking(move || -> Result<serde_json::Value, Option<String>> {
            let info = bn_core::skills::effective_info(&cfg, std::path::Path::new("."), &name_call)
                .map_err(|error| Some(error.to_string()))?
                .ok_or(None)?;
            bn_core::skills::save_skill(std::path::Path::new(&info.path), &content)
                .map_err(Some)?;
            let native = sync_native_skills_summary(&cfg);
            Ok(json!({ "skill": info, "native": native }))
        })
        .await;

    match result {
        Ok(Ok(data)) => {
            audit::record(&state, "skill-edit", &name, addr, true);
            Json(Envelope::success(cmd, data)).into_response()
        }
        Ok(Err(None)) => {
            audit::record(&state, "skill-edit", &name, addr, false);
            not_found_json(cmd, &format!("no skill named '{name}'"))
        }
        Ok(Err(Some(msg))) => {
            audit::record(&state, "skill-edit", &name, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "write_failed", msg)),
            )
                .into_response()
        }
        Err(e) => {
            audit::record(&state, "skill-edit", &name, addr, false);
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
// Today's agent (#577) — native in-process suggestion writes
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct TodayRunBody {
    #[serde(default)]
    pub lookback_days: Option<i64>,
    #[serde(default)]
    pub per_source: Option<usize>,
}

/// `POST /api/today/run` — the Today page's manual trigger. Runs Today's agent in-process (gather
/// the cron findings → synthesize suggested tasks → merge idempotently), then returns the run
/// report plus the refreshed open suggestions so the UI can update without a second fetch.
pub async fn today_run(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "today-run";
    // Body is optional — an empty POST uses the defaults.
    let body: TodayRunBody = if raw.is_empty() {
        TodayRunBody::default()
    } else {
        match parse_body(cmd, &raw) {
            Ok(b) => b,
            Err(resp) => return resp,
        }
    };
    let lookback = body
        .lookback_days
        .unwrap_or(bn_core::today_agent::DEFAULT_LOOKBACK_DAYS);
    let per_source = body
        .per_source
        .unwrap_or(bn_core::today_agent::DEFAULT_PER_SOURCE);
    let res = tokio::task::spawn_blocking(move || {
        bn_core::today_agent::run(&bn_core::util::today(), lookback, per_source)
    })
    .await;
    match res {
        Ok(Ok(report)) => {
            audit::record(&state, cmd, "today-agent", addr, true);
            let open = bn_core::suggestions::list(
                Some(bn_core::suggestions::SuggestionStatus::Suggested),
                0,
            );
            Json(Envelope::success(
                cmd,
                json!({ "report": report, "suggestions": open }),
            ))
            .into_response()
        }
        Ok(Err(e)) => {
            audit::record(&state, cmd, "today-agent", addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "run_failed", e.to_string())),
            )
                .into_response()
        }
        Err(e) => {
            audit::record(&state, cmd, "today-agent", addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "join_failed", e.to_string())),
            )
                .into_response()
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct TodayAgentStartBody {
    /// `claude` | `copilot`. Absent ⇒ the machine's configured default.
    #[serde(default)]
    pub harness: Option<String>,
}

/// `POST /api/today/agent/start {harness?}` → `bn today agent start`.
///
/// Shelled rather than run in-process, like every other launch this server offers (see
/// [`dispatch`]): opening a tmux window, reserving an agent lease and pinning a harness session are
/// bn-cli's job, and duplicating that ordering here is how the two copies drift. Idempotent at the
/// CLI — a live agent for today is reported, not duplicated — so a double-click cannot fork the day.
pub async fn today_agent_start(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "today-agent-start";
    let body: TodayAgentStartBody = if raw.is_empty() {
        TodayAgentStartBody::default()
    } else {
        match parse_body(cmd, &raw) {
            Ok(b) => b,
            Err(resp) => return resp,
        }
    };
    let mut args = vec![
        "today".to_string(),
        "agent".to_string(),
        "start".to_string(),
    ];
    if let Some(h) = body
        .harness
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let Some(harness) = normalize_harness(Some(h)) else {
            return bad_request(cmd, "invalid_harness", "harness must be claude or copilot");
        };
        args.push("--harness".to_string());
        args.push(harness.to_string());
    }
    let result = run_bn(args, None).await;
    respond_process(&state, "today-agent-start", "today-agent", addr, result)
}

/// `POST /api/today/agent/stop` → `bn today agent stop`. Idempotent: stopping an agent that is
/// already gone succeeds, because the state the caller asked for is the state they get.
pub async fn today_agent_stop(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
) -> Response {
    let args = vec!["today".to_string(), "agent".to_string(), "stop".to_string()];
    let result = run_bn(args, None).await;
    respond_process(&state, "today-agent-stop", "today-agent", addr, result)
}

#[derive(Debug, Default, Deserialize)]
pub struct SuggestionStatusBody {
    pub status: String,
}

/// `POST /api/suggestions/{id} {status}` — review one suggestion (accepted | dismissed |
/// suggested). Native in-process store write, so the change lands where the read API serves it.
pub async fn suggestion_status(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "suggestion-status";
    let body: SuggestionStatusBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let id = id.trim().to_string();
    // Ids are `<source>-<key>` — a filename-safe token; keep the allowlist tight.
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return bad_request(cmd, "invalid_id", "suggestion id has invalid characters");
    }
    let Some(status) = bn_core::suggestions::SuggestionStatus::parse(&body.status) else {
        return bad_request(
            cmd,
            "bad_status",
            "status must be suggested | accepted | dismissed",
        );
    };
    let id_call = id.clone();
    let res =
        tokio::task::spawn_blocking(move || bn_core::suggestions::set_status(&id_call, status))
            .await;
    match res {
        Ok(Ok(true)) => {
            audit::record(&state, cmd, &id, addr, true);
            Json(Envelope::success(
                cmd,
                json!({ "id": id, "status": status.as_str() }),
            ))
            .into_response()
        }
        Ok(Ok(false)) => {
            audit::record(&state, cmd, &id, addr, false);
            not_found_json(cmd, "no such suggestion")
        }
        Ok(Err(e)) => {
            audit::record(&state, cmd, &id, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "write_failed", e.to_string())),
            )
                .into_response()
        }
        Err(e) => {
            audit::record(&state, cmd, &id, addr, false);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Envelope::failure(cmd, "join_failed", e.to_string())),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Task writes (#1006, EPIC #993) — split by latency
// ---------------------------------------------------------------------------
//
// Two shapes, deliberately kept apart:
//
// - **Synchronous** (create, day, priority, done, snooze) — file writes measured in milliseconds,
//   served natively in a `spawn_blocking` and answered with the result.
// - **Asynchronous** (improve, triage, investigate) — LLM agents measured in *minutes*.
//   `bn_runner::run` is blocking and has no timeout, so serving one of these the same way would
//   hold an axum worker for the whole run with no progress and no way to cancel. They spawn
//   detached and return a handle immediately; progress shows up on the existing fleet surface.

/// `POST /api/tasks` — create a task.
#[derive(Debug, Default, serde::Deserialize)]
pub struct TaskCreateBody {
    pub title: String,
    pub repo: Option<String>,
    pub day: Option<String>,
    pub est: Option<String>,
    pub priority: Option<String>,
}

pub async fn task_create(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    raw: Bytes,
) -> Response {
    let cmd = "task-create";
    let body: TaskCreateBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let title = body.title.trim().to_string();
    if title.is_empty() {
        return bad_request(cmd, "empty_title", "a task needs a title");
    }
    if let Some(repo) = body.repo.as_deref().filter(|r| !r.trim().is_empty()) {
        if !valid_repo(repo.trim()) {
            return bad_request(cmd, "invalid_repo", "repo has invalid characters");
        }
    }
    for (field, value) in [("day", &body.day), ("est", &body.est)] {
        if let Some(v) = value.as_deref().filter(|v| !v.trim().is_empty()) {
            if field == "day" && bn_core::util::epoch_day(v.trim()).is_none() {
                return bad_request(cmd, "invalid_day", "day must be YYYY-MM-DD");
            }
            if field == "est" && bn_core::smart::parse_est_minutes(v.trim()).is_none() {
                return bad_request(cmd, "invalid_est", "est must be like 90m, 2h or 1h30m");
            }
        }
    }
    let priority = match body
        .priority
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        None => None,
        Some(p) if p.len() == 1 && ('A'..='C').contains(&p.chars().next().unwrap_or('z')) => {
            p.chars().next()
        }
        Some(_) => return bad_request(cmd, "invalid_priority", "priority must be A, B or C"),
    };

    let cfg = state.cfg.clone();
    let audit_title = title.clone();
    let created = tokio::task::spawn_blocking(move || {
        let path = cfg.tasks_file();
        let _lock = bn_core::todo::lock(&path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut file = bn_core::todo::load(&path).map_err(|e| e.to_string())?;
        let today = bn_core::util::today();
        let id = file.next_id();
        // The title comes off the wire, so defang it before it becomes a description — a task
        // line's words are its syntax, and #1003 made `smart:`/`repo:` security signals.
        let text = bn_core::todo::neutralize_meta(&title);
        let mut task = bn_core::todo::Task::new_todo(&id, &text, priority, "", "web", &today);
        for (key, value) in [
            ("repo", body.repo.as_deref()),
            ("day", body.day.as_deref()),
            ("est", body.est.as_deref()),
        ] {
            if let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) {
                task.set_meta_raw(key, &v.to_ascii_lowercase());
            }
        }
        file.push_task(task);
        bn_core::todo::save(&path, &file).map_err(|e| e.to_string())?;
        Ok::<String, String>(id)
    })
    .await;

    match created {
        Ok(Ok(id)) => {
            audit::record(&state, "task-create", &audit_title, addr, true);
            Json(Envelope::success(cmd, json!({ "id": id }))).into_response()
        }
        Ok(Err(e)) => {
            audit::record(&state, "task-create", &audit_title, addr, false);
            server_error(cmd, "write_failed", &e)
        }
        Err(_) => server_error(cmd, "panicked", "the task write panicked"),
    }
}

/// Has this task earned the right to be started?
///
/// Two different agents contribute, and both are required. `triage` decides what is genuinely
/// doable **today** and stamps `day:`; `improve` makes one task SMART and records the `smart:`
/// verdict. Requiring both means an agent is never handed work that is either unscheduled or
/// unscoped — and it is exactly what `task_agents::pre_score` already *ranks* by (+250 due today,
/// +200 `smart:ok`). The gate only enforces what the ranker already believed.
///
/// A `day:` in the future is deliberately not startable: it was triaged *for another day*.
fn is_triaged(task: &bn_core::todo::Task, today: &str) -> bool {
    let scheduled = task
        .meta("day")
        .is_some_and(|d| bn_core::util::epoch_day(d).is_some() && d <= today);
    scheduled && task.meta("smart") == Some("ok")
}

/// `POST /api/tasks/{id}` — a fast mutation on one task.
#[derive(Debug, Default, serde::Deserialize)]
pub struct TaskActionBody {
    pub action: String,
    pub value: Option<String>,
}

pub async fn task_action(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "task-action";
    let body: TaskActionBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let id = id.trim().to_string();
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return bad_request(cmd, "invalid_id", "task id must be alphanumeric (e.g. t42)");
    }
    let action = body.action.trim().to_ascii_lowercase();
    let value = body.value.map(|v| v.trim().to_string()).unwrap_or_default();

    match action.as_str() {
        "day" | "snooze" if bn_core::util::epoch_day(&value).is_none() => {
            return bad_request(cmd, "invalid_date", "value must be YYYY-MM-DD")
        }
        "priority"
            if !(value.len() == 1
                && ('A'..='C').contains(&value.chars().next().unwrap_or('z'))) =>
        {
            return bad_request(cmd, "invalid_priority", "value must be A, B or C")
        }
        "day" | "snooze" | "priority" | "done" | "wip" | "cancel" => {}
        _ => {
            return bad_request(
                cmd,
                "invalid_action",
                "action must be day | priority | done | wip | snooze | cancel",
            )
        }
    }

    let cfg = state.cfg.clone();
    let (act, val, task_id) = (action.clone(), value.clone(), id.clone());
    let result = tokio::task::spawn_blocking(move || {
        let path = cfg.tasks_file();
        let _lock = bn_core::todo::lock(&path);
        let mut file = bn_core::todo::load(&path).map_err(|e| e.to_string())?;
        let today = bn_core::util::today();
        // The Start gate. Enforced here rather than by hiding a button, because the dashboard is
        // not the only caller: the CLI and every agent hit this same endpoint, and a rule that
        // only the UI honours is a suggestion. `smart:` alone would not do it — it is a *ranking*
        // signal (`task_agents::pre_score` adds +200 for `ok`), and ranking never blocked anything.
        if act == "wip" {
            match file.tasks().find(|t| t.id() == Some(task_id.as_str())) {
                Some(t) if !is_triaged(t, &today) => {
                    return Err(format!("not_triaged: {task_id} has not been triaged"))
                }
                _ => {}
            }
        }
        let ok = match act.as_str() {
            "done" => file.mark_done(&task_id, &today),
            "wip" => file.mark_wip(&task_id),
            // Cancelling *is* the delete: the store never purges (done and cancelled lines stay,
            // so the file is the complete record), and `cancelled:1` keeps abandoned work from
            // ever being counted as a completion. An empty reason records no reason at all.
            "cancel" => file.cancel(
                &task_id,
                &today,
                Some(val.as_str()).filter(|r| !r.is_empty()),
            ),
            "snooze" => file.snooze(&task_id, &val),
            other => match file.task_mut(&task_id) {
                Some(task) => {
                    if other == "priority" {
                        task.priority = val.chars().next();
                    } else {
                        task.set_meta(other, &val);
                    }
                    true
                }
                None => false,
            },
        };
        if !ok {
            return Err(format!("no task {task_id}"));
        }
        bn_core::todo::save(&path, &file).map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    })
    .await;

    let label = format!("task-{action}");
    match result {
        Ok(Ok(())) => {
            audit::record(&state, &label, &id, addr, true);
            Json(Envelope::success(
                cmd,
                json!({ "id": id, "action": action }),
            ))
            .into_response()
        }
        Ok(Err(e)) if e.starts_with("no task") => {
            audit::record(&state, &label, &id, addr, false);
            not_found_json(cmd, &e)
        }
        // A refused Start is a conflict with the task's state, not a malformed request: the caller
        // sent something valid that this task has not earned yet, and triaging it makes the same
        // call succeed unchanged.
        Ok(Err(e)) if e.starts_with("not_triaged") => {
            audit::record(&state, &label, &id, addr, false);
            conflict(
                cmd,
                "not_triaged",
                "only triaged tasks can be started — run triage, or improve it past the SMART gate",
            )
        }
        Ok(Err(e)) => {
            audit::record(&state, &label, &id, addr, false);
            server_error(cmd, "write_failed", &e)
        }
        Err(_) => server_error(cmd, "panicked", "the task write panicked"),
    }
}

/// `POST /api/tasks/{id}/doc/append` — append context under a section (default `Investigation`).
///
/// Additive and unguarded: it re-reads the current doc and inserts under the heading, so an
/// agent's append and a human's edit never fight over it. Creates the doc from the template first
/// if the task has none yet.
#[derive(Debug, Default, serde::Deserialize)]
pub struct DocAppendBody {
    pub text: String,
    pub section: Option<String>,
}

pub async fn task_doc_append(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "task-doc-append";
    let body: DocAppendBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let id = id.trim().to_string();
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return bad_request(cmd, "invalid_id", "task id must be alphanumeric (e.g. t42)");
    }
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return bad_request(cmd, "empty_text", "nothing to append");
    }
    // The section becomes a `## <heading>` line, so keep it a plain heading.
    let section = body
        .section
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Investigation")
        .to_string();
    if section.len() > 40
        || !section
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == ' ')
    {
        return bad_request(cmd, "invalid_section", "section must be a plain heading");
    }

    let cfg = state.cfg.clone();
    let task_id = id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let path = cfg.tasks_file();
        let task = {
            let _lock = bn_core::todo::lock(&path);
            let file = bn_core::todo::load(&path).map_err(|e| e.to_string())?;
            let found = file
                .tasks()
                .find(|t| t.id() == Some(task_id.as_str()))
                .cloned()
                .ok_or_else(|| format!("no task {task_id}"))?;
            found
        };
        match bn_core::task_doc::append_section(&cfg, &task, &section, &text) {
            Ok(d) => {
                Ok::<_, String>(json!({ "path": d.path, "body": d.text, "version": d.version() }))
            }
            Err(bn_core::task_doc::DocError::NoPath) => Err("no_doc".to_string()),
            Err(bn_core::task_doc::DocError::Io(e)) => Err(e.to_string()),
            Err(bn_core::task_doc::DocError::Conflict(_)) => Err("conflict".to_string()),
        }
    })
    .await;

    match result {
        Ok(Ok(data)) => {
            audit::record(&state, "task-doc-append", &id, addr, true);
            Json(Envelope::success(cmd, data)).into_response()
        }
        Ok(Err(e)) if e.starts_with("no task") => {
            audit::record(&state, "task-doc-append", &id, addr, false);
            not_found_json(cmd, &e)
        }
        Ok(Err(e)) if e == "no_doc" => bad_request(cmd, "no_doc", "this task has no context doc"),
        Ok(Err(e)) => {
            audit::record(&state, "task-doc-append", &id, addr, false);
            server_error(cmd, "write_failed", &e)
        }
        Err(_) => server_error(cmd, "panicked", "the doc append panicked"),
    }
}

/// `PUT /api/tasks/{id}/doc` — replace the whole doc, guarded by `if_match` (the version the caller
/// read). A stale `if_match` is a `409 doc_conflict`, not a lost write.
#[derive(Debug, Default, serde::Deserialize)]
pub struct DocEditBody {
    pub body: String,
    pub if_match: Option<String>,
}

pub async fn task_doc_edit(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "task-doc-edit";
    let body: DocEditBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let id = id.trim().to_string();
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return bad_request(cmd, "invalid_id", "task id must be alphanumeric (e.g. t42)");
    }
    let new_body = body.body;
    if new_body.len() > 256 * 1024 {
        return bad_request(cmd, "too_large", "the doc is unexpectedly large");
    }
    let if_match = body.if_match;

    let cfg = state.cfg.clone();
    let task_id = id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let path = cfg.tasks_file();
        let task = {
            let _lock = bn_core::todo::lock(&path);
            let file = bn_core::todo::load(&path).map_err(|e| e.to_string())?;
            let found = file
                .tasks()
                .find(|t| t.id() == Some(task_id.as_str()))
                .cloned()
                .ok_or_else(|| format!("no task {task_id}"))?;
            found
        };
        match bn_core::task_doc::write(&cfg, &task, new_body, if_match.as_deref()) {
            Ok(d) => {
                Ok::<_, String>(json!({ "path": d.path, "body": d.text, "version": d.version() }))
            }
            Err(bn_core::task_doc::DocError::Conflict(_)) => Err("conflict".to_string()),
            Err(bn_core::task_doc::DocError::NoPath) => Err("no_doc".to_string()),
            Err(bn_core::task_doc::DocError::Io(e)) => Err(e.to_string()),
        }
    })
    .await;

    match result {
        Ok(Ok(data)) => {
            audit::record(&state, "task-doc-edit", &id, addr, true);
            Json(Envelope::success(cmd, data)).into_response()
        }
        Ok(Err(e)) if e.starts_with("no task") => {
            audit::record(&state, "task-doc-edit", &id, addr, false);
            not_found_json(cmd, &e)
        }
        Ok(Err(e)) if e == "no_doc" => bad_request(cmd, "no_doc", "this task has no context doc"),
        // Lost the race: the doc moved under the editor. 409 so the client keeps the operator's
        // text and reloads to show what actually landed.
        Ok(Err(e)) if e == "conflict" => {
            audit::record(&state, "task-doc-edit", &id, addr, false);
            conflict(
                cmd,
                "doc_conflict",
                "the doc changed since you opened it — your text is kept; reload to see what landed",
            )
        }
        Ok(Err(e)) => {
            audit::record(&state, "task-doc-edit", &id, addr, false);
            server_error(cmd, "write_failed", &e)
        }
        Err(_) => server_error(cmd, "panicked", "the doc write panicked"),
    }
}

/// `POST /api/tasks/{id}/agent` — launch a task agent and return immediately.
#[derive(Debug, Default, serde::Deserialize)]
pub struct TaskAgentBody {
    /// `improve` or `triage`.
    pub agent: String,
    /// The operator's "things to look out for", passed into the seed.
    pub steer: Option<String>,
}

pub async fn task_agent(
    State(state): State<AppState>,
    MaybeAddr(addr): MaybeAddr,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let cmd = "task-agent";
    let body: TaskAgentBody = match parse_body(cmd, &raw) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let id = id.trim().to_string();
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return bad_request(cmd, "invalid_id", "task id must be alphanumeric (e.g. t42)");
    }
    let agent = body.agent.trim().to_ascii_lowercase();
    if !matches!(agent.as_str(), "improve" | "triage") {
        return bad_request(cmd, "invalid_agent", "agent must be improve | triage");
    }

    // Built as argv, never a shell string — the steer text is operator input and must not be
    // interpretable as anything but one argument.
    let mut args: Vec<String> = vec!["task".into(), agent.clone()];
    if agent == "improve" {
        args.push(id.clone());
        if let Some(steer) = body
            .steer
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            args.push("--steer".into());
            args.push(steer.to_string());
        }
    }
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();

    // Detached: an LLM agent runs for minutes, and holding an axum worker for that would block a
    // pooled thread with no progress and no way to cancel. The client watches the fleet instead.
    let spawned = bn_runner::spawn_detached(&argv);
    audit::record(&state, &format!("task-{agent}"), &id, addr, spawned.is_ok());
    match spawned {
        Ok(pid) => Json(Envelope::success(
            cmd,
            json!({
                "id": id,
                "agent": agent,
                "started": true,
                "pid": pid,
                "watch": "/api/fleet",
            }),
        ))
        .into_response(),
        Err(e) => server_error(cmd, "spawn_failed", &e.to_string()),
    }
}
