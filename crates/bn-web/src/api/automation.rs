use super::*;

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
