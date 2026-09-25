use crate::*;

pub(super) fn route(cfg: &Config, action: CmdStaff, json: bool) {
    match action {
        CmdStaff::Afk { action } => emit_afk(cfg, action, json),
        CmdStaff::Watch {
            once,
            interval,
            stuck,
        } => emit_watch(cfg, once, interval, stuck, json),
        CmdStaff::Handoff { action } => match action {
            Some(HandoffCmd::Write {
                narrative,
                defer,
                question,
                next,
                repo,
                branch,
            }) => {
                let ctx =
                    target_ctx(cfg, repo, branch).unwrap_or_else(|| fail("Not in a git repo"));
                let narrative = (!narrative.is_empty()).then(|| narrative.join(" "));
                let input = bn_core::handoff::HandoffInput {
                    narrative,
                    deferred: defer,
                    questions: question,
                    priority_queue: next,
                };
                emit_handoff_write(cfg, &ctx, &input, json);
            }
            None | Some(HandoffCmd::Read { .. }) => {
                let (repo, branch) = match action {
                    Some(HandoffCmd::Read { repo, branch }) => (repo, branch),
                    _ => (None, None),
                };
                let ctx =
                    target_ctx(cfg, repo, branch).unwrap_or_else(|| fail("Not in a git repo"));
                emit_handoff_read(cfg, &ctx, json);
            }
        },
        CmdStaff::Switch {
            harness,
            prompt,
            in_place,
            reason,
            repo,
            branch,
        } => {
            let ctx = target_ctx(cfg, repo, branch).unwrap_or_else(|| fail("Not in a git repo"));
            // A single-target switch that didn't happen exits non-zero — the envelope already said
            // why, and `bn_runner` (and any shell caller) reads the status.
            if !emit_switch(cfg, &ctx, harness, prompt, in_place, reason, json) {
                std::process::exit(1);
            }
        }
        CmdStaff::Run {
            job,
            repo,
            branch,
            if_stale,
        } => {
            if if_stale && ran_ok_today(&job) {
                println!("bn run: skipping '{job}' — already ran successfully today (--if-stale)");
                std::process::exit(0);
            }
            let ctx = if job == "keep-warm" {
                crate::cmd::keep_warm::run_target_ctx(cfg, repo, branch)
            } else {
                target_ctx(cfg, repo, branch)
            };
            std::process::exit(run_job(cfg, ctx.as_ref(), &job));
        }
        CmdStaff::Machine { action } => match action {
            MachineCmd::List => crate::cmd::machine::emit_machine_list(cfg, json),
            MachineCmd::Merge { from, dry_run } => {
                crate::cmd::machine::emit_machine_merge(cfg, &from, dry_run, json)
            }
        },
        CmdStaff::Cron {
            action,
            repo,
            branch,
        } => {
            let ctx = target_ctx(cfg, repo, branch);
            match action {
                None | Some(CronCmd::Status) => emit_cron_dashboard(cfg, ctx.as_ref(), json),
                Some(CronCmd::Show { job }) => emit_cron_show(&job, json),
                Some(CronCmd::Failures { limit }) => emit_cron_failures(limit, json),
                Some(CronCmd::Unregister { job }) => emit_cron_unregister(&job, json),
                Some(CronCmd::Retire { job, reason }) => {
                    if !cfg.cron_enabled_here() {
                        fail_json(
                            json,
                            "cron-retire",
                            "not_enabled",
                            &format!(
                                "bn cron retire: not enabled on this machine ('{}')",
                                cfg.machine()
                            ),
                        );
                    }
                    emit_cron_retire(cfg, ctx.as_ref(), &job, &reason.join(" "), json)
                }
                Some(CronCmd::Register { dry_run }) => {
                    emit_cron_register(cfg, ctx.as_ref(), dry_run, json)
                }
                Some(CronCmd::Running) => emit_cron_running(cfg),
                Some(CronCmd::RefreshWindows) => {
                    if !cfg.cron_enabled_here() {
                        fail_json(
                            json,
                            "cron-refresh-windows",
                            "not_enabled",
                            &format!(
                                "bn cron refresh-windows: not enabled on this machine ('{}')",
                                cfg.machine()
                            ),
                        );
                    }
                    winsched::refresh();
                }
            }
        }
        CmdStaff::Startup { repo, branch } => {
            let ctx = target_ctx(cfg, repo, branch);
            emit_startup(cfg, ctx.as_ref());
        }
        CmdStaff::Skills { action, pick } => {
            let base = std::env::current_dir().unwrap_or_default();
            match action {
                None if pick => {
                    let ctx = resolver::resolve(cfg, &base).ok();
                    crate::cmd::agent_run::emit_pick(
                        cfg,
                        ctx.as_ref(),
                        bn_core::launchable::Source::Skill,
                        json,
                    )
                }
                None | Some(SkillsCmd::List) => emit_skills(cfg, &base, json),
                Some(SkillsCmd::Find { query }) => {
                    emit_skills_find(cfg, &base, &query.join(" "), json)
                }
                Some(SkillsCmd::New {
                    name,
                    description,
                    scope,
                    overwrite,
                }) => emit_skill_new(cfg, &base, &name, &description, &scope, overwrite, json),
                Some(SkillsCmd::Render { name, args }) => {
                    emit_skill_render(cfg, &base, &name, &args)
                }
                Some(SkillsCmd::Edit { name, scope }) => {
                    emit_skill_edit(cfg, &base, name.as_deref(), &scope, json)
                }
                Some(SkillsCmd::Install) => emit_skill_install(cfg, json),
            }
        }
        CmdStaff::Agents { action, pick } => {
            let base = std::env::current_dir().unwrap_or_default();
            match action {
                None if pick => {
                    let ctx = resolver::resolve(cfg, &base).ok();
                    crate::cmd::agent_run::emit_pick(
                        cfg,
                        ctx.as_ref(),
                        bn_core::launchable::Source::Agent,
                        json,
                    )
                }
                None | Some(AgentsCmd::List) => emit_agents(cfg, &base, json),
                Some(AgentsCmd::Validate) => emit_agents_validate(cfg, &base, json),
                Some(AgentsCmd::Render {
                    name,
                    args,
                    args_file,
                    stdin,
                }) => emit_agent_render(
                    cfg,
                    &base,
                    &name,
                    args.as_deref(),
                    args_file.as_deref(),
                    stdin,
                ),
                Some(AgentsCmd::Show { name, claude }) => {
                    emit_agent_show(cfg, &base, &name, claude, json)
                }
                Some(AgentsCmd::New {
                    name,
                    repo,
                    description,
                    source,
                    scope,
                    overwrite,
                }) => emit_agent_new(
                    cfg,
                    &name,
                    &repo,
                    description.as_deref(),
                    source.as_deref(),
                    &scope,
                    overwrite,
                    json,
                ),
                Some(AgentsCmd::Install { repo, harness }) => {
                    emit_agents_install(cfg, repo, harness.as_deref(), json)
                }
                Some(AgentsCmd::Uninstall { repo, harness }) => {
                    emit_agents_uninstall(cfg, repo, harness.as_deref(), json)
                }
                Some(AgentsCmd::Bootstrap { name }) => {
                    emit_agent_bootstrap(cfg, &base, &name, json)
                }
            }
        }
        CmdStaff::Mcp { action } => match action {
            McpCmd::Init {
                harness,
                with_extras,
            } => match mcp::parse_harness(&harness) {
                Some(h) => mcp::init(h, with_extras, json),
                None => fail(&format!(
                    "bn mcp init: unknown harness '{harness}' — use 'claude' or 'copilot'"
                )),
            },
            McpCmd::Serve { orchestrate } => mcp::serve(orchestrate),
        },
    }
}
