use crate::*;

pub(super) fn route(cfg: &Config, action: CmdFleet, json: bool) {
    match action {
        CmdFleet::Jump {
            next,
            pick_stage,
            pick_fields,
        } => {
            if let Some(s) = pick_stage {
                jump::stage(cfg, s, &pick_fields)
            } else if next {
                jump::next()
            } else {
                jump::run(cfg)
            }
        }
        CmdFleet::Tat {
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
        CmdFleet::Connect {
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
        CmdFleet::Mobile { host } => mobile::run(cfg, &host),
        CmdFleet::Notify { action } => match action {
            None | Some(NotifyCmd::Status) => cmd::notify::emit_notify_status(cfg, json),
        },
        CmdFleet::Focus { action } => match action {
            None | Some(FocusCmd::Status) => emit_focus_status(json),
            Some(FocusCmd::Start { dur }) => emit_focus_start(&dur, json),
            Some(FocusCmd::End) => emit_focus_end(cfg, json),
        },
        CmdFleet::Sync { yes } => {
            let ctx = resolve_or_exit(cfg);
            emit_sync(cfg, &ctx, yes, json);
        }
        CmdFleet::Attn {
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
            Some(AttnCmd::Badge) => emit_attn_badge(),
            Some(AttnCmd::ToNotify) => emit_attn_to_notify(json),
            Some(AttnCmd::Notified { id }) => emit_attn_notified(&id, json),
        },
        CmdFleet::Agent { action } => match action {
            Some(AgentCmd::Ingest) => emit_agent_ingest(cfg, json),
            Some(AgentCmd::Digest { repo, branch, yes }) => {
                let ctx =
                    target_ctx(cfg, repo, branch).unwrap_or_else(|| fail("Not in a git repo"));
                emit_agent_digest(cfg, &ctx, yes, json);
            }
            Some(AgentCmd::Supervise { idle, stuck }) => {
                emit_agent_supervise(cfg, idle, stuck, json)
            }
            Some(AgentCmd::Live { idle, stuck }) => agent_live::run(cfg, idle, stuck, json),
            Some(AgentCmd::Checkin { mins, note, target }) => {
                emit_agent_checkin(cfg, mins, &note, target, json)
            }
            Some(AgentCmd::Send {
                target,
                msg,
                keys,
                detach,
            }) => emit_agent_send(cfg, &target, &msg, keys, detach, json),
            Some(AgentCmd::Done {
                summary,
                pr,
                notify,
                report_only,
                failed,
                repo,
                branch,
            }) => emit_agent_done(
                cfg,
                summary,
                pr,
                notify,
                report_only,
                failed,
                repo,
                branch,
                json,
            ),
            Some(AgentCmd::Recover {
                repo,
                branch,
                all,
                base,
                yes,
            }) => emit_agent_recover(cfg, repo, branch, all, base.as_deref(), yes, json),
            Some(AgentCmd::Kill { target }) => emit_agent_kill(cfg, &target, json),
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
            None | Some(AgentCmd::Sessions { .. }) => {
                let (repo, branch) = match action {
                    Some(AgentCmd::Sessions { repo, branch }) => (repo, branch),
                    _ => (None, None),
                };
                emit_agent_sessions(cfg, repo, branch, json);
            }
            Some(AgentCmd::MemoryHook) => emit_memory_hook(cfg),
            Some(AgentCmd::Launch {
                harness,
                seed,
                seed_file,
                yolo,
                fresh,
                session,
                resume_session,
                passthrough,
            }) => cmd::agent::emit_agent_launch(
                cfg,
                harness,
                seed,
                seed_file,
                yolo,
                fresh,
                session,
                resume_session,
                passthrough,
            ),
        },
        CmdFleet::Sessions { action } => match action {
            None => emit_sessions(cfg, json),
            Some(SessionsCmd::Resume { name_or_id }) => {
                resume_registered_session(cfg, Some(&name_or_id), json, "sessions_resume")
            }
        },
        CmdFleet::Resume { name_or_id } => resume_registered_session(
            cfg,
            name_or_id.as_deref(),
            json,
            if name_or_id.is_some() {
                "sessions_resume"
            } else {
                "resume"
            },
        ),
        CmdFleet::Fleet {
            action,
            idle,
            stuck,
            fix,
            budget,
            cost,
            oneline,
            preview,
            mobile,
        } => match action {
            None => {
                if cost {
                    crate::cmd::cost::emit_live(cfg, json);
                } else if fix {
                    run_fleet_fix(cfg, idle, stuck, json);
                } else if json {
                    agent_live::emit_board_json(cfg, idle, stuck, budget);
                } else if mobile {
                    agent_live::run_board_mobile(cfg, idle, stuck, budget);
                } else {
                    agent_live::run_board(cfg, idle, stuck, budget, preview && !oneline);
                }
            }
            Some(FleetCmd::Pick {
                pick_stage: Some(stage),
                target,
                ..
            }) => agent_live::pick_stage(stage, target.as_deref()),
            Some(FleetCmd::Pick {
                pick_stage: None,
                dispatched,
                ..
            }) => agent_live::run_pick(cfg, idle, stuck, dispatched),
            Some(FleetCmd::Jump { query }) => match query {
                Some(q) => agent_live::run_direct_jump(cfg, idle, stuck, &q),
                None => agent_live::run_menu_jump(cfg, idle, stuck),
            },
            Some(FleetCmd::Steer { target }) => match target {
                Some(t) => agent_live::run_steer_target(&t),
                None => agent_live::run_steer_menu(cfg, idle, stuck),
            },
            Some(FleetCmd::Reap {
                kill,
                force,
                min_idle,
            }) => {
                reap::run_reap(
                    cfg,
                    reap::ReapOpts {
                        kill,
                        force,
                        min_idle_secs: min_idle,
                        as_json: json,
                        check_pr: true,
                        quiet: false,
                    },
                );
            }
        },
        CmdFleet::Night { action } => match action {
            None | Some(NightCmd::Status) => emit_night_status(cfg, json),
            Some(NightCmd::Plan) => emit_night_plan(cfg, json),
            Some(NightCmd::Run { max, force }) => emit_night_run(cfg, max, force, json),
            Some(NightCmd::Report { hours }) => emit_night_report(cfg, hours, json),
            Some(NightCmd::Off) => emit_night_switch(false, json),
            Some(NightCmd::On) => emit_night_switch(true, json),
        },
        CmdFleet::Dojo { action, repo, max } => match action {
            None => emit_dojo_session(cfg, dojo_selector(repo, None, None), max, json),
            Some(DojoCmd::Quiz { repo, max }) => emit_dojo_session(
                cfg,
                dojo_selector(repo, Some(bn_core::dojo::CardKind::Quiz), None),
                max,
                json,
            ),
            Some(DojoCmd::Bug { repo, max }) => emit_dojo_session(
                cfg,
                dojo_selector(repo, Some(bn_core::dojo::CardKind::BugMystery), None),
                max,
                json,
            ),
            Some(DojoCmd::Map { area, repo }) => emit_dojo_map(cfg, area, repo, json),
            Some(DojoCmd::Stats) => emit_dojo_stats(cfg, json),
            Some(DojoCmd::Config) => emit_dojo_config(cfg, json),
        },
    }
}
