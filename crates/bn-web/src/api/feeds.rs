//! The two folded feeds: what needs you, and what is already done.
//!
//! `actions` and `completed` are mirror images and always have been — the same sources (branch-note
//! todos, daily goals, attention items, orchestrate queue items) folded by the same shape of
//! builder, one selecting what is still open and the other what has closed. Keeping them in one
//! module makes that symmetry visible, and means a source added to one is an obvious question about
//! the other rather than a thing to notice later.
//!
//! Both are expensive folds over already-read viewmodels, and both are served through
//! [`crate::view_cache`] — `actions` on the short signal window, `completed` on the longest window
//! of any view, since finished work does not un-finish.

use crate::state::AppState;
use axum::{extract::State, response::Json};
use bn_core::render::Envelope;

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
///
/// Seven sources, one of them the same cron scan `/api/cron` makes, so it lands around 250ms and is
/// fetched by the rail on every screen. Cached on the shortest window of the three
/// ([`crate::view_cache::ACTIONS`]) — this is the feed whose job is to say something needs you, so
/// it buys back the duplicate work without letting a signal sit unseen.
pub async fn actions(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let data = state
        .views
        .get_or_compute(crate::view_cache::ACTIONS, move || {
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
            // Accepted Today's-agent suggestions promoted into the action feed (#578) — joined
            // against the work queue, so one whose work is already in flight reports that state
            // instead of offering to start it a second time.
            let suggestions = bn_core::suggestions::list(
                Some(bn_core::suggestions::SuggestionStatus::Accepted),
                0,
            );
            let backlog = bn_core::backlog::load(&bn_core::backlog::backlog_path());

            let sources = bn_core::action_center::ActionSources {
                attn: &attn,
                reviews: &reviews,
                crons: &crons,
                notes: &notes,
                goals: &goals,
                queue: &queue,
                suggestions: &suggestions,
                backlog: Some(&backlog),
                now: bn_core::util::now_epoch_secs(),
            };
            let actions = bn_core::action_center::build_actions(&sources);
            let cron_activity = bn_core::action_center::cron_activity(&crons);
            bn_core::payload::actions_data(&actions, &cron_activity)
        })
        .await;
    let data = data
        .map(|d| (*d).clone())
        .unwrap_or_else(|| serde_json::json!({ "count": 0, "actions": [] }));
    Json(Envelope::success("actions", data))
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
    let data = state
        .views
        .get_or_compute(crate::view_cache::COMPLETED, move || {
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
        .map(|d| (*d).clone())
        .unwrap_or_else(|| serde_json::json!({ "count": 0, "completed": [] }));
    Json(Envelope::success("completed", data))
}
