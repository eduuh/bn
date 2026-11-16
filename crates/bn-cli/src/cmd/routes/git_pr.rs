use crate::*;

pub(super) fn route(cfg: &Config, action: CmdGitPr, json: bool) {
    match action {
        CmdGitPr::Pr { url, copy, show } => {
            let ctx = resolve_or_exit(cfg);
            emit_pr(cfg, &ctx, url, copy, show, json);
        }
        CmdGitPr::Link { id } => {
            let ctx = resolve_or_exit(cfg);
            emit_link(cfg, &ctx, id, json);
        }
        CmdGitPr::LogProgress { text } => {
            let ctx = resolve_or_exit(cfg);
            let summary = text.join(" ");
            let bullet = format!("{} — {summary}", bn_core::util::now_stamp());
            match bn_core::task_doc::append_context(cfg, &ctx, "Progress", &bullet) {
                Ok(_) => {}
                Err(e) => fail(&e.to_string()),
            }
            append_inherited_worker_event(
                cfg,
                &ctx,
                bn_core::controller::ControllerWorkerEventType::Progress,
                &summary,
                None,
            )
            .unwrap_or_else(|e| fail(&format!("bn log-progress: controller event: {e}")));
        }
        CmdGitPr::Diff {
            path,
            repo,
            branch,
            base,
            three_dot,
        } => match cmd::gitio::diff_target(
            cfg,
            path.as_deref(),
            repo.as_deref(),
            branch.as_deref(),
        ) {
            Ok(target) => emit_diff(cfg, &target, base.as_deref(), three_dot, json),
            Err(e) => fail_json(json, "diff", "no_target", &format!("bn diff: {e}")),
        },
        CmdGitPr::Open { url } => {
            if !cmd::gitio::open_url(&url) {
                println!("{url}");
            }
        }

    }
}
