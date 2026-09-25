//! The pipeline reads: the nine-stage machine as a board, as work, and per worktree.
//!
//! Split out of `api.rs` under the file-size ratchet. Grouped by subject rather than by shape —
//! all three answer "where is this work in the pipeline", and they share the `Stage` vocabulary
//! that EPIC #746 made durable.

use super::bad_request;
use crate::actions;
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use bn_core::render::Envelope;
use serde_json::{json, Value};
use std::collections::HashMap;

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

    let file = match bn_core::task_store::task_store(&state.cfg).load() {
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
