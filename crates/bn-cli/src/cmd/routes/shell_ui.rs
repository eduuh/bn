use crate::*;

pub(super) fn route(cfg: &Config, action: CmdShellUi, json: bool) {
    match action {
        CmdShellUi::Edit { action } => crate::edit::route(cfg, *action, json),
        CmdShellUi::TmuxStatus { path } => emit_tmux_status(cfg, path),
        CmdShellUi::TmuxMcp { action } => match action {
            TmuxMcpCmd::Ensure => emit_tmux_mcp_ensure(cfg),
            TmuxMcpCmd::Badge => emit_tmux_mcp_badge(cfg),
        },
        CmdShellUi::TmuxPrune { force } => emit_tmux_prune(cfg, force, json),
        CmdShellUi::TmuxSnapshotFilter { path } => emit_snapshot_filter(cfg, &path),
        CmdShellUi::TmuxGit { path, refresh } => emit_tmux_git(path, refresh),
        CmdShellUi::TmuxGoal { path } => emit_tmux_goal(cfg, path),
        CmdShellUi::TmuxAttnCount => emit_tmux_attn_count(),
        CmdShellUi::TmuxDoneBadge {
            window,
            status,
            exited,
        } => crate::cmd::tmux_done::emit(&window, status.as_deref(), exited),
        CmdShellUi::TmuxCockpit => emit_tmux_cockpit(cfg),
        CmdShellUi::TmuxHelp => emit_tmux_help(),
        CmdShellUi::TmuxKeys { since } => crate::cmd::tmux_keys::emit_tmux_keys(since),
        CmdShellUi::TmuxNote => emit_tmux_note(cfg),
        CmdShellUi::Glyphs => emit_glyphs(),
        CmdShellUi::TmuxSync => {
            let ctx = resolve_or_exit(cfg);
            sync_tmux(cfg, &ctx);
        }
        CmdShellUi::TmuxReconcile { dry_run } => emit_tmux_reconcile(cfg, dry_run),
        CmdShellUi::TmuxAgent { state } => set_tmux_agent(&state),
        CmdShellUi::Status => {
            let ctx = resolve_or_exit(cfg);
            emit_status(cfg, &ctx, json);
        }
        CmdShellUi::Brief => {
            let ctx = resolve_or_exit(cfg);
            match bn_core::store::read(&cfg.note_file(&ctx)) {
                Ok(note) => print!("{}", bn_core::render::brief(&note, &ctx.repo, &ctx.branch)),
                Err(_) => println!("{}/{}: no note", ctx.repo, ctx.branch),
            }
        }
        CmdShellUi::Close { achievement, force } => {
            let ctx = resolve_or_exit(cfg);
            match commands::close(cfg, &ctx, &today(), achievement.as_deref()) {
                Ok(o) => {
                    if json {
                        println!(
                            "{}",
                            Envelope::success("close", payload::close_data(&o)).to_json()
                        );
                    } else {
                        println!(
                            "Closed note: {}/{} ({} todo(s) completed)",
                            o.repo, o.branch, o.todos_completed
                        );
                        if let Some(a) = &o.achievement {
                            println!("Logged achievement: {a}");
                        }
                        if !o.unmet_criteria.is_empty() {
                            eprintln!(
                                "⚠ closed with {} acceptance criterion/criteria unmet:",
                                o.unmet_criteria.len()
                            );
                            for c in &o.unmet_criteria {
                                eprintln!("  - [ ] {c}");
                            }
                        }
                        if !o.uncaptured_links.is_empty() {
                            eprintln!(
                                "⚠ not in your link store — run `bn links capture` before the worktree goes:"
                            );
                            for l in &o.uncaptured_links {
                                eprintln!("  {l}");
                            }
                        }
                        // Closing the note is the local-action trigger for reclaiming the
                        // worktree + tmux window. JSON mode stays note-only for scripted callers.
                        wt::cleanup_current(cfg, force);
                    }
                }
                Err(e) => fail(&e.message()),
            }
        }
        CmdShellUi::Reopen => {
            let ctx = resolve_or_exit(cfg);
            match commands::set_status(cfg, &ctx, "active") {
                Ok(()) => println!("Reopened: {}/{}", ctx.repo, ctx.branch),
                Err(e) => fail(&e.message()),
            }
        }
        CmdShellUi::Release {
            bump,
            dry_run,
            skip_verify,
        } => emit_release(cfg, bump, dry_run, skip_verify, json),
    }
}
