use crate::*;

pub(super) fn route(cfg: &Config, action: CmdAutomation, json: bool) {
    match action {
        CmdAutomation::Forge { action } => {
            std::process::exit(crate::cmd::forge_app::run(cfg, action, json))
        }
        CmdAutomation::Jump {
            next,
            session,
            pick_stage,
            pick_fields,
        } => {
            if let Some(s) = pick_stage {
                jump::stage(cfg, s, &pick_fields)
            } else if next {
                jump::next()
            } else if session {
                jump::session()
            } else {
                jump::run(cfg)
            }
        }
        CmdAutomation::Tat {
            name,
            root,
            pick_stage,
            pick_arg,
        } => {
            if let Some(s) = pick_stage {
                tat::stage(cfg, s, name.as_deref(), root.as_deref(), pick_arg.as_deref())
            } else {
                tat::run(cfg, name.as_deref(), root.as_deref())
            }
        }
        CmdAutomation::Connect {
            host,
            window,
            read_only,
            pick_stage,
        } => {
            if let Some(s) = pick_stage {
                connect::stage(cfg, s, host.as_deref())
            } else {
                connect::run(cfg, host.as_deref(), window, read_only)
            }
        }
        CmdAutomation::Harness { action } => match action {
            crate::cli::automation_cmd::HarnessCmd::Models { harness } => {
                cmd::harness_models::emit_harness_models(harness.as_deref(), json)
            }
        },
        CmdAutomation::Tunnels => std::process::exit(cmd::cron::run_job(cfg, None, "tunnels")),
        CmdAutomation::Notify { action } => match action {
            None | Some(NotifyCmd::Status) => cmd::notify::emit_notify_status(cfg, json),
        },
        CmdAutomation::Sync { yes } => {
            let ctx = resolve_or_exit(cfg);
            emit_sync(cfg, &ctx, yes, json);
        }
        CmdAutomation::Attn {
            action,
            type_filter,
        } => match action {
            None => emit_attn_board(cfg, &type_filter, json),
            Some(AttnCmd::List { type_filter }) => emit_attn_list(&type_filter, json),
            Some(AttnCmd::Status) => emit_attn_status(),
            Some(AttnCmd::Metrics { window }) => emit_attn_metrics(window, json),
            Some(AttnCmd::Add {
                text,
                severity,
                link,
                key,
                repo,
                branch,
                source,
                mode,
                prompt,
            }) => emit_attn_add(
                &source,
                &text.join(" "),
                &severity,
                repo.as_deref(),
                branch.as_deref(),
                link.as_deref(),
                key.as_deref(),
                mode.as_deref(),
                prompt.as_deref(),
                json,
            ),
            Some(AttnCmd::Gather) => {
                let ctx = target_ctx(cfg, None, None);
                std::process::exit(crate::cmd::attention_workflow::emit_attention_gather(
                    cfg,
                    ctx.as_ref(),
                ))
            }
            Some(AttnCmd::Ingest {
                source,
                severity,
                replace,
            }) => emit_attn_ingest(&source, &severity, replace, json),
            Some(AttnCmd::Ack { id }) => emit_attn_ack(&id, json),
            Some(AttnCmd::Update {
                id,
                title,
                severity,
                link,
                repo,
                branch,
                detail,
                mode,
                prompt,
            }) => emit_attn_update(
                &id,
                title.as_deref(),
                severity.as_deref(),
                link.as_deref(),
                repo.as_deref(),
                branch.as_deref(),
                detail.as_deref(),
                mode.as_deref(),
                prompt.as_deref(),
                json,
            ),
            Some(AttnCmd::Remove { id }) => emit_attn_remove(&id, json),
            Some(AttnCmd::Done { id, notes, link }) => {
                emit_attn_done(&id, notes.as_deref(), link.as_deref(), json)
            }
            Some(AttnCmd::Start { id, agent }) => emit_attn_start(cfg, &id, agent, json),
            Some(AttnCmd::Promote {
                todo_id,
                mode,
                severity,
                repo,
                branch,
            }) => emit_attn_promote(cfg, &todo_id, &mode, &severity, repo, branch, json),
            Some(AttnCmd::NextAuto) => emit_attn_next_auto(cfg, json),
            Some(AttnCmd::Plan { id, section }) => emit_attn_plan(cfg, &id, &section, json),
            Some(AttnCmd::Dispatch { id, repo }) => {
                emit_attn_dispatch(cfg, &id, repo.as_deref(), json)
            }
            Some(AttnCmd::Snooze { id, until }) => emit_attn_snooze(&id, &until, json),
            Some(AttnCmd::Badge) => crate::cmd::attn_badge::emit_attn_badge(),
            Some(AttnCmd::ToNotify) => emit_attn_to_notify(json),
            Some(AttnCmd::Notified { id }) => emit_attn_notified(&id, json),
        },
        CmdAutomation::Agent { action } => match action {
            Some(AgentCmd::Ingest { reingest }) => emit_agent_ingest(cfg, reingest, json),
            Some(AgentCmd::Digest { repo, branch, yes }) => {
                let ctx =
                    target_ctx(cfg, repo, branch).unwrap_or_else(|| fail("Not in a git repo"));
                emit_agent_digest(cfg, &ctx, yes, json);
            }
            Some(AgentCmd::Supervise { idle, stuck }) => {
                emit_agent_supervise(cfg, idle, stuck, json)
            }
            Some(AgentCmd::Live { idle, stuck, mesh }) => agent_live::run(cfg, idle, stuck, mesh, json),
            Some(AgentCmd::Checkin { mins, note, target }) => {
                emit_agent_checkin(cfg, mins, &note, target, json)
            }
            Some(AgentCmd::Send {
                target,
                msg,
                keys,
                channel,
                detach,
            }) => emit_agent_send(
                cfg,
                &target,
                &msg,
                crate::cmd::agent::SteerChannel::parse(channel.as_deref(), keys),
                detach,
                json,
            ),
            Some(AgentCmd::Msg { target, text, from }) => {
                emit_agent_msg(cfg, &target, &text, from.as_deref(), json)
            }
            Some(AgentCmd::Done {
                summary,
                pr,
                notify,
                report_only,
                failed,
                repo,
                branch,
            }) => {
                emit_agent_done(
                    cfg,
                    summary,
                    pr,
                    notify,
                    report_only,
                    failed,
                    repo,
                    branch,
                    json,
                )
            }
            Some(AgentCmd::Recover {
                repo,
                branch,
                all,
                base,
                yes,
            }) => emit_agent_recover(cfg, repo, branch, all, base.as_deref(), yes, json),
            Some(AgentCmd::Kill { target }) => emit_agent_kill(cfg, &target, json),
            Some(AgentCmd::KillRepo { repo }) => emit_repo_kill(cfg, &repo, json),
            Some(AgentCmd::RelaunchRepo {
                repo,
                hard,
                harness,
            }) => crate::cmd::agent_relaunch::emit_repo(
                cfg,
                &repo,
                hard,
                harness.as_deref(),
                json,
            ),
            Some(AgentCmd::Relaunch {
                target,
                hard,
                harness,
            }) => crate::cmd::agent_relaunch::emit(cfg, &target, hard, harness.as_deref(), json),
            Some(AgentCmd::Pane {
                target,
                lines,
                ansi,
            }) => emit_agent_pane(cfg, &target, lines, ansi, json),
            Some(AgentCmd::Activity { session, limit }) => {
                emit_agent_activity(&session, limit, json)
            }
            Some(AgentCmd::Approvals {
                init,
                sweep,
                dry_run,
                limit,
            }) => {
                if sweep {
                    crate::cmd::agent_approve::sweep(cfg, dry_run, json);
                } else {
                    crate::cmd::agent_approve::emit_approvals(cfg, init, limit, json);
                }
            }
            Some(AgentCmd::Prompt { target, lines }) => {
                crate::cmd::agent::emit_prompt(cfg, &target, lines, json)
            }
            Some(AgentCmd::Answer {
                target,
                option,
                approve,
                approve_session,
                reject,
                expect,
            }) => crate::cmd::agent::emit_answer(
                cfg,
                &target,
                option,
                approve,
                approve_session,
                reject,
                expect,
                json,
            ),
            Some(AgentCmd::Instruct {
                target,
                text,
                no_send,
                list,
                drop,
            }) => crate::cmd::agent::emit_instruct(
                cfg,
                &target,
                &text.join(" "),
                no_send,
                list,
                drop.as_deref(),
                json,
            ),
            None | Some(AgentCmd::Sessions { .. }) => {
                let (repo, branch) = match action {
                    Some(AgentCmd::Sessions { repo, branch }) => (repo, branch),
                    _ => (None, None),
                };
                emit_agent_sessions(cfg, repo, branch, json);
            }
            Some(AgentCmd::Run {
                name,
                instruction,
                harness,
                worktree,
                repo,
                base,
                yolo,
                args,
                add_dir,
                list,
                print_seed,
                pick_stage,
                pick_source,
            }) => {
                // `run` works from anywhere: the cwd need not be a repo (the picker and `--list`
                // are useful outside one, and the launcher enforces placement itself).
                let ctx = resolver::resolve(cfg, &std::env::current_dir().unwrap_or_default()).ok();
                let base_dir = std::env::current_dir().unwrap_or_default();
                let only = pick_source
                    .as_deref()
                    .and_then(bn_core::launchable::Source::parse);
                match pick_stage {
                    Some(stage) => crate::cmd::agent_run::emit_pick_stage(
                        cfg,
                        ctx.as_ref(),
                        &base_dir,
                        stage,
                        name.as_deref(),
                        only,
                    ),
                    None => crate::cmd::agent_run::emit_run(
                        cfg,
                        ctx.as_ref(),
                        name,
                        instruction,
                        harness,
                        worktree,
                        repo,
                        base,
                        yolo,
                        args,
                        add_dir,
                        list,
                        print_seed,
                        only,
                        json,
                    ),
                }
            }
            Some(AgentCmd::MemoryHook) => emit_memory_hook(cfg),
            Some(AgentCmd::Launch {
                harness,
                host,
                seed,
                seed_file,
                yolo,
                fresh,
                session,
                resume_session,
                add_dir,
                passthrough,
            }) => cmd::agent::emit_agent_launch(
                cfg,
                harness,
                host.as_deref(),
                seed,
                seed_file,
                yolo,
                fresh,
                session,
                resume_session,
                add_dir,
                passthrough,
            ),
        },
        CmdAutomation::Sessions { action } => match action {
            None => emit_sessions(cfg, json),
            Some(SessionsCmd::Resume { name_or_id }) => {
                resume_registered_session(cfg, Some(&name_or_id), json, "sessions_resume")
            }
        },
        CmdAutomation::Resume {
            all,
            dry_run,
            yes,
            slots,
            name_or_id,
        } => {
            if all {
                // Crash recovery. Bare `bn resume` and `bn resume <id>` fall through unchanged —
                // the same keystroke must not print a table one day and launch an agent the next.
                return crate::cmd::resume_fleet::emit_resume_fleet(
                    cfg,
                    &crate::cmd::resume_fleet::ResumeFleetOpts {
                        dry_run,
                        yes,
                        slots,
                    },
                    json,
                );
            }
            resume_registered_session(
                cfg,
                name_or_id.as_deref(),
                json,
                if name_or_id.is_some() {
                    "sessions_resume"
                } else {
                    "resume"
                },
            )
        }
    }
}
