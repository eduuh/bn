use crate::*;

pub(crate) fn emit_plan(cfg: &Config, action: Option<PlanCmd>, as_json: bool) {
    let ctx = resolve_or_exit(cfg);
    let result = match action.unwrap_or(PlanCmd::Status) {
        PlanCmd::Init => bn_core::plan::init_active(cfg, &ctx).map(|path| {
            serde_json::json!({
                "path": path.display().to_string(),
                "relative": bn_core::plan::ACTIVE_PLAN_REL,
            })
        }),
        PlanCmd::Path { active: _, agent } => {
            let path = if let Some(session_id) = agent {
                bn_core::plan::init_agent(cfg, &ctx, &session_id)
            } else {
                bn_core::plan::init_active(cfg, &ctx)
            };
            path.map(|path| {
                serde_json::json!({
                    "path": path.display().to_string(),
                    "relative": cfg.note_relative_path(&ctx, &path),
                })
            })
        }
        PlanCmd::Open => bn_core::plan::init_active(cfg, &ctx).and_then(|path| {
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".to_string());
            let status = std::process::Command::new(&editor).arg(&path).status()?;
            if !status.success() {
                return Err(bn_core::plan::PlanError::Io(std::io::Error::other(format!(
                    "{editor} exited with {status}"
                ))));
            }
            Ok(serde_json::json!({ "path": path.display().to_string(), "opened": true }))
        }),
        PlanCmd::Status => bn_core::plan::status(cfg, &ctx).map(|status| serde_json::json!(status)),
        PlanCmd::Promote { draft } => {
            let draft = if draft.contains('/') || draft.ends_with(".md") {
                draft
            } else {
                let path = cfg.agent_plan_file(&ctx, &draft);
                cfg.note_relative_path(&ctx, &path)
                    .unwrap_or_else(|| format!("plans/agents/{draft}.md"))
            };
            bn_core::plan::promote(cfg, &ctx, &draft).map(|path| {
                serde_json::json!({
                    "path": path.display().to_string(),
                    "source": draft,
                    "relative": bn_core::plan::ACTIVE_PLAN_REL,
                })
            })
        }
        PlanCmd::Migrate => bn_core::plan::migrate(cfg, &ctx).map(|path| {
            serde_json::json!({
                "path": path.display().to_string(),
                "relative": bn_core::plan::ACTIVE_PLAN_REL,
            })
        }),
    };

    match result {
        Ok(data) if as_json => println!("{}", Envelope::success("plan", data).to_json()),
        Ok(data) => {
            if let Some(path) = data.get("path").and_then(|v| v.as_str()) {
                println!("{path}");
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&data).unwrap_or_else(|_| data.to_string())
                );
            }
        }
        Err(error) if as_json => println!(
            "{}",
            Envelope::failure("plan", "plan-error", error.to_string()).to_json()
        ),
        Err(error) => fail(&format!("bn plan: {error}")),
    }
}
