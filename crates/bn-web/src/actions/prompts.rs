use bn_core::resolver::Config;

pub(super) fn repo_orchestrator_goal(cfg: &Config, repo: &str) -> Result<String, String> {
    let mut args = serde_json::Map::new();
    args.insert("repo".into(), serde_json::Value::String(repo.to_string()));
    bn_core::skills::render_required(
        cfg,
        std::path::Path::new("."),
        "repo-orchestrator-goal",
        Some(&args),
    )
    .map_err(|error| error.to_string())
}
