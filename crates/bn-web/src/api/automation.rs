use super::*;
use crate::actions::server_error;

/// `GET /api/agent-defs` — the Markdown agent roster: every definition discovered across the tiers,
/// without its prompt body.
///
/// Distinct from [`agents_catalog`], which answers "what can the automation fire?" and therefore
/// drops specialists and folds in plain cron jobs. This answers "what agents are defined, and where
/// do they live?", so a specialist is listed like anything else — you can read one even though
/// nothing can trigger it.
pub async fn agent_defs(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let result = tokio::task::spawn_blocking(move || {
        bn_core::agents::discover(&cfg, None)
            .map(|catalog| {
                let agents: Vec<_> = catalog.into_iter().collect();
                bn_core::payload::agent_defs_data(&agents)
            })
            .map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(Ok(data)) => Json(Envelope::success("agent-defs", data)),
        Ok(Err(error)) => Json(Envelope::failure("agent-defs", "invalid_catalog", error)),
        Err(error) => Json(Envelope::failure(
            "agent-defs",
            "task_failed",
            error.to_string(),
        )),
    }
}

/// `GET /api/agent-defs/{name}` — one definition plus the verbatim `.md` file behind it.
pub async fn agent_def_source(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Response {
    let cmd = "agent-def";
    let Some(name) = nonempty(Some(name)) else {
        return bad_request(cmd, "invalid_name", "agent name must not be empty");
    };
    let cfg = state.cfg.clone();
    let wanted = name.clone();
    let result = tokio::task::spawn_blocking(move || {
        let catalog = bn_core::agents::discover(&cfg, None).map_err(|e| e.to_string())?;
        let Some(agent) = catalog.find(&wanted) else {
            return Ok(None);
        };
        // The file as written, frontmatter included — `AgentDef::body` has already had the
        // frontmatter stripped, and reading what is on disk is the whole point of this view.
        let source = std::fs::read_to_string(&agent.path).map_err(|e| e.to_string())?;
        Ok::<_, String>(Some(bn_core::payload::agent_def_data(agent, &source)))
    })
    .await;
    match result {
        Ok(Ok(Some(data))) => Json(Envelope::success(cmd, data)).into_response(),
        Ok(Ok(None)) => not_found(cmd, &format!("no Markdown agent named {name}")),
        Ok(Err(error)) => server_error(cmd, "read_failed", &error),
        Err(error) => server_error(cmd, "task_failed", &error.to_string()),
    }
}

/// Every runnable explicit-kind Markdown agent plus ordinary cron/native jobs.
pub async fn agents_catalog(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let result = tokio::task::spawn_blocking(move || {
        let crons = bn_core::crons::status_views(&cfg, None);
        let agents = bn_core::agents::discover(&cfg, None).map_err(|error| error.to_string())?;
        let items = bn_core::action_center::build_catalog(&crons, &agents);
        Ok::<_, String>(bn_core::payload::catalog_data(&items))
    })
    .await;
    match result {
        Ok(Ok(data)) => Json(Envelope::success("agents-catalog", data)),
        Ok(Err(error)) => Json(Envelope::failure(
            "agents-catalog",
            "invalid_catalog",
            error,
        )),
        Err(error) => Json(Envelope::failure(
            "agents-catalog",
            "task_failed",
            error.to_string(),
        )),
    }
}

/// Today's scheduled ordinary cron jobs.
pub async fn today_crons(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let result = tokio::task::spawn_blocking(move || {
        bn_core::action_center::build_todays_automation(&cfg, None)
            .map(|today| bn_core::payload::catalog_data(&today.crons))
            .map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(Ok(data)) => Json(Envelope::success("today-crons", data)),
        Ok(Err(error)) => Json(Envelope::failure("today-crons", "invalid_catalog", error)),
        Err(error) => Json(Envelope::failure(
            "today-crons",
            "task_failed",
            error.to_string(),
        )),
    }
}

/// Today's scheduled runnable Markdown agents.
pub async fn today_agents(State(state): State<AppState>) -> Json<Envelope> {
    let cfg = state.cfg.clone();
    let result = tokio::task::spawn_blocking(move || {
        bn_core::action_center::build_todays_automation(&cfg, None)
            .map(|today| bn_core::payload::catalog_data(&today.agents))
            .map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(Ok(data)) => Json(Envelope::success("today-agents", data)),
        Ok(Err(error)) => Json(Envelope::failure("today-agents", "invalid_catalog", error)),
        Err(error) => Json(Envelope::failure(
            "today-agents",
            "task_failed",
            error.to_string(),
        )),
    }
}
