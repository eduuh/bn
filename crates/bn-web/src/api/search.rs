//! `GET /api/search` — the global search / command palette (#565).
//!
//! Its own module for the reason the sibling read modules are: `api.rs` is at the file-size
//! ratchet's ceiling, and the palette is a self-contained read with its own query and row types
//! that nothing else in the surface reaches into.
use super::*;

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
    let result = tokio::task::spawn_blocking(move || {
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
        let skills = bn_core::skills::discover(&cfg, std::path::Path::new("."))
            .map_err(|error| error.to_string())?;

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
        Ok::<_, String>(bn_core::payload::palette_data(&q, &results))
    })
    .await;
    match result {
        Ok(Ok(data)) => Json(Envelope::success("search", data)),
        Ok(Err(error)) => Json(Envelope::failure("search", "invalid_catalog", error)),
        Err(error) => Json(Envelope::failure(
            "search",
            "task_failed",
            error.to_string(),
        )),
    }
}
