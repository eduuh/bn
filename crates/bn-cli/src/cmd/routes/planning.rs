use crate::*;

pub(super) fn route(cfg: &Config, action: CmdPlanning, json: bool) {
    match action {
        CmdPlanning::Files => {
            let ctx = resolve_or_exit(cfg);
            emit_files(commands::list_files(cfg, &ctx), json);
        }
        CmdPlanning::ManagedRepos { dir } => {
            if dir {
                println!("{}", resolver::managed_repos_dir().display());
            } else if json {
                println!(
                    "{}",
                    Envelope::success(
                        "managed-repos",
                        serde_json::json!({
                            "dir": resolver::managed_repos_dir().display().to_string(),
                            "repos": cfg.managed_repos,
                        }),
                    )
                    .to_json()
                );
            } else {
                for url in &cfg.managed_repos {
                    println!("{url}");
                }
            }
        }
        CmdPlanning::Worktrees => {
            let wts = rollups::worktrees(cfg);
            if json {
                println!(
                    "{}",
                    Envelope::success("worktrees", payload::worktrees_data(&wts)).to_json()
                );
            } else if wts.is_empty() {
                println!("No active branch notes");
            } else {
                // Render natively (machine-subtree layout) rather than delegating to the
                // bash fallback, which still resolves the old machine-less paths.
                for w in &wts {
                    let loc = w.worktree.as_deref().unwrap_or(&w.path);
                    println!(
                        "{}/{}  {} todos, {} blockers  ({})  {}",
                        w.repo, w.branch, w.open_todos, w.blockers, w.status, loc
                    );
                }
            }
        }
        CmdPlanning::Todo => emit_todo(cfg, json),
        CmdPlanning::List => emit_list(cfg, json),
        CmdPlanning::Summary => emit_summary(cfg, json),
        CmdPlanning::Daily { action, date } => emit_daily(cfg, action, date, json),
        CmdPlanning::Quarter { action, quarter } => emit_quarter(cfg, action, quarter, json),
        CmdPlanning::Standup { copy, cron } => {
            // Cron gate: the standup-push job no-ops unless autopilot is enabled.
            let jc = resolver::JobConfig::load();
            if cron && !jc.standup_enabled {
                return;
            }
            let s = bn_core::reports::standup(cfg, &today());
            // One-shot PR delta, only on the cron push and only when opted in.
            let delta = if cron && jc.standup_fetch_delta {
                bn_core::reports::pr_delta(&bn_core::runner::RealRunner)
            } else {
                None
            };
            if json {
                println!(
                    "{}",
                    Envelope::success("standup", payload::standup_data(&s)).to_json()
                );
            } else if copy {
                let mut line = bn_core::reports::standup_line(&s);
                if let Some(d) = &delta {
                    line.push_str(&format!(" · {d}"));
                }
                println!("{line}");
            } else {
                print!("{}", bn_core::reports::render_standup(&s));
                if let Some(d) = &delta {
                    println!("\n{d}");
                }
            }
        }
        CmdPlanning::Task { action, all, repo } => match action {
            None | Some(TaskCmd::List) => emit_task_list(
                cfg,
                if all { TaskView::All } else { TaskView::Today },
                repo.as_deref(),
                json,
            ),
            Some(TaskCmd::Import { path }) => {
                emit_task_import(cfg, &path, repo.as_deref(), json)
            }
            Some(TaskCmd::Doc { id, open }) => emit_task_doc(cfg, &id, open, json),
            Some(TaskCmd::Pull { low_water, dry_run }) => {
                emit_task_pull(cfg, repo.as_deref(), low_water, dry_run, json)
            }
            Some(TaskCmd::Add {
                text,
                priority,
                due,
                src,
            }) => emit_task_add(cfg, &text.join(" "), priority, due.as_deref(), &src, json),
            Some(TaskCmd::Do { id }) => emit_task_mutate(cfg, TaskOp::Do, &id, json),
            Some(TaskCmd::Wip { id }) => emit_task_mutate(cfg, TaskOp::Wip, &id, json),
            Some(TaskCmd::Snooze { id, until }) => {
                emit_task_mutate(cfg, TaskOp::Snooze(&until), &id, json)
            }
            Some(TaskCmd::Cancel { id, reason }) => {
                let reason = (!reason.is_empty()).then(|| reason.join(" "));
                emit_task_mutate(cfg, TaskOp::Cancel(reason.as_deref()), &id, json);
            }
            Some(TaskCmd::Ingest {
                source,
                branch,
                replace,
            }) => emit_task_ingest(cfg, &source, branch.as_deref(), replace, json),
        },
    }
}
