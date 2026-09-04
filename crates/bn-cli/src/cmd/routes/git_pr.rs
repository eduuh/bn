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
        CmdGitPr::Notes { text } => {
            let ctx = resolve_or_exit(cfg);
            emit_body_section(cfg, &ctx, "notes", &text, json);
        }
        CmdGitPr::Tc { text } => {
            let ctx = resolve_or_exit(cfg);
            emit_body_section(cfg, &ctx, "tc", &text, json);
        }
        CmdGitPr::LogProgress { text } => {
            let ctx = resolve_or_exit(cfg);
            let summary = text.join(" ");
            let bullet = format!("{} — {summary}", bn_core::util::now_stamp());
            match commands::append_body(cfg, &ctx, "progress", &bullet, &today()) {
                Ok(_) => {}
                Err(e) => fail(&e.message()),
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
        CmdGitPr::Shot { gif, fresh } => {
            let ctx = resolve_or_exit(cfg);
            emit_shot(cfg, &ctx, gif, fresh, json);
        }
        CmdGitPr::Autopush { message } => std::process::exit(emit_autopush(&message.join(" "))),
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
        CmdGitPr::Migrate => {
            let report = migrate::migrate_all(cfg);
            for f in &report.failed {
                eprintln!("migrate failed: {f}");
            }
            println!(
                "Migrated {} note(s), {} already single-file, {} failed",
                report.migrated.len(),
                report.skipped,
                report.failed.len()
            );
        }
        CmdGitPr::MigrateLayout { dry_run, force } => {
            let report = migrate::migrate_layout(cfg, dry_run, force);
            for line in &report.moved {
                println!("{}: {line}", if dry_run { "would move" } else { "moved" });
            }
            for s in &report.skipped {
                eprintln!("skip: {s}");
            }
            for f in &report.failed {
                eprintln!("migrate-layout failed: {f}");
            }
            println!(
                "migrate-layout: {} {} note dir(s), {} skipped, {} failed",
                if dry_run { "would move" } else { "moved" },
                report.moved.len(),
                report.skipped.len(),
                report.failed.len()
            );
        }
    }
}
