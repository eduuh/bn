use crate::*;

pub(super) fn route(cfg: &Config, action: CmdIntake, json: bool) {
    match action {
        CmdIntake::Add {
            section,
            text,
            prompt,
            repo,
            branch,
            if_absent,
            est,
            q,
        } => {
            let targeted = repo.is_some() || branch.is_some();
            let ctx = target_ctx_or_exit(
                cfg,
                repo,
                branch,
                "bn add",
                "bn add: not in a git repo — pass --repo and --branch to target a note",
            );
            // --prompt: read the item text from the tty (tmux quick-capture popup). An empty line
            // cancels silently (exit 0), matching the old bn-capture.sh behaviour.
            let text = if prompt {
                match prompt_capture(&section) {
                    Some(t) => t,
                    None => return,
                }
            } else if text.is_empty() {
                fail(
                    "bn add: missing item text (pass text, or --prompt to enter it interactively)",
                );
            } else {
                text.join(" ")
            };
            let est = parse_est_or_exit("bn add", est.as_deref());
            let res = commands::add_full(
                cfg,
                &ctx,
                &section,
                &text,
                if_absent.as_deref(),
                est.as_deref(),
                q.as_deref(),
                &today(),
            );
            if res.as_ref().is_ok_and(|outcome| {
                !outcome.skipped && outcome.section == bn_core::model::Section::Blocker
            }) {
                append_inherited_worker_event(
                    cfg,
                    &ctx,
                    bn_core::controller::ControllerWorkerEventType::Blocked,
                    &text,
                    None,
                )
                .unwrap_or_else(|e| fail(&format!("bn add blocker: controller event: {e}")));
            }
            emit_add(res, json);
            // Only the current branch's tmux window reflects its own counts; skip when
            // we wrote into some other note via --repo/--branch.
            if !targeted {
                sync_tmux(cfg, &ctx);
            }
        }
        CmdIntake::Capture { url, section } => {
            let ctx = resolve_or_exit(cfg);
            let title = capture_title(&url);
            let res = commands::add(
                cfg,
                &ctx,
                &section,
                &format!("{title} — {url}"),
                None,
                &today(),
            );
            emit_add(res, json);
            sync_tmux(cfg, &ctx);
        }
        CmdIntake::Q { action, text } => match action {
            Some(QCmd::Drain { auto }) => emit_q_drain(cfg, auto, json),
            None if text.is_empty() => emit_q_list(cfg, json),
            None => emit_q_capture(cfg, &text.join(" "), json),
        },
        CmdIntake::Dispatch {
            item,
            name,
            repo,
            base,
            branch,
            no_tmux,
            agent,
            scout,
            orchestrator,
            skill,
            verify,
            yolo,
            plan,
            no_plan_review,
            stage,
            fresh,
            keep_project_mcps,
            no_pool,
            with_main,
            on_pool_full,
            task,
            // Deprecated no-op: pooling is the default dispatch path now (kept hidden for
            // back-compat with callers/scripts that still pass `--pool`).
            pool: _,
            attach,
            refs,
        } => {
            // Dispatch works from anywhere: cwd need not be a repo. Resolve the cwd's context
            // best-effort (for todo-id resolution / closing the source todo) — `run_dispatch`
            // requires it only when no `--repo` is given, and then emits a `--repo` nudge.
            let ctx = resolver::resolve(cfg, &std::env::current_dir().unwrap_or_default()).ok();
            // Captured (#queue): a typed `bn dispatch` enqueues like everything else, so one
            // dispatcher decides what the machine runs and when. `--on-pool-full fail` and
            // `$BN_DISPATCH_NOW` both mean "I want this now" and keep the direct path.
            //
            // Intercepted here rather than inside `run_dispatch`, because that function's job is to
            // *launch* — teaching it to sometimes not launch would make its 23 parameters mean two
            // different things depending on config.
            if dispatch_captured(cfg, on_pool_full) {
                let ok = crate::cmd::queue_producers::capture_dispatch(
                    cfg,
                    &crate::cmd::queue_producers::DispatchRequest {
                        // Carried, not inferred: a `--stage` the caller typed must survive the
                        // queue, or the entry comes back out as whichever stage the guess allows.
                        stage,
                        repo: repo.as_deref(),
                        item: &item.join(" "),
                        scout,
                        skill: skill.as_deref(),
                        harness: agent.as_deref(),
                        branch: branch.as_deref(),
                        name: name.as_deref(),
                        yolo,
                        fresh,
                        verify,
                        plan,
                        no_plan_review,
                        with_main,
                        machine: crate::cmd::queue_producers::machine_owned(),
                        orchestrator,
                    },
                    json,
                );
                if ok {
                    return;
                }
                // Enqueue failed: fall through and launch, rather than dropping the work silently.
            }
            run_dispatch(
                cfg,
                ctx.as_ref(),
                repo.as_deref(),
                base.as_deref(),
                &item.join(" "),
                name.as_deref(),
                branch,
                no_tmux,
                agent.as_deref(),
                scout,
                orchestrator,
                skill.as_deref(),
                verify,
                yolo,
                // clap keeps these mutually exclusive, so the tri-state cannot be asked for both
                // ways at once.
                if no_plan_review {
                    crate::cmd::dispatch::PlanGate::Off
                } else if plan {
                    crate::cmd::dispatch::PlanGate::Force
                } else {
                    crate::cmd::dispatch::PlanGate::Auto
                },
                fresh,
                keep_project_mcps,
                no_pool,
                with_main,
                on_pool_full,
                task.as_deref(),
                attach,
                &refs,
                stage,
                // A typed `bn dispatch` always renders its seed from a workflow skill.
                None,
                json,
            );
        }
        CmdIntake::Promote { repo, branch } => {
            let ctx = target_ctx(cfg, repo, branch)
                .unwrap_or_else(|| fail("bn promote: not in a git repo — pass --repo/--branch"));
            emit_promote(cfg, &ctx, json);
        }
        CmdIntake::Verify { job, no_ui } => emit_verify(cfg, job, json, no_ui),
        CmdIntake::Gate { job, no_ui } => {
            if !wt::cmd_gate(cfg, job, json, no_ui) {
                std::process::exit(1);
            }
        }
        CmdIntake::TrustFolder { path, harness } => emit_trust_folder(path, &harness, json),
        CmdIntake::Wt { action } => wt::run(cfg, action, json),
        CmdIntake::Why { target, lines } => {
            let ctx = resolve_or_exit(cfg);
            emit_why(cfg, &ctx, &target, lines.as_deref(), json);
        }
        CmdIntake::Hooks { action } => match action {
            HooksCmd::Install => {
                let cwd = std::env::current_dir().unwrap_or_default();
                match hooks::install(&cwd) {
                    Ok((path, outcome)) => {
                        if json {
                            let env = Envelope::success(
                                "hooks-install",
                                serde_json::json!({
                                    "path": path.display().to_string(),
                                    "outcome": outcome,
                                }),
                            );
                            println!("{}", env.to_json());
                        } else {
                            println!("prepare-commit-msg hook {outcome}: {}", path.display());
                        }
                    }
                    Err(e) => {
                        if json {
                            println!(
                                "{}",
                                Envelope::failure("hooks-install", "install_failed", &e).to_json()
                            );
                            std::process::exit(1);
                        }
                        fail(&format!("bn hooks install: {e}"));
                    }
                }
            }
            HooksCmd::RunPrepareCommitMsg {
                msg_file, source, ..
            } => {
                let cwd = std::env::current_dir().unwrap_or_default();
                hooks::run_prepare_commit_msg(cfg, &cwd, &msg_file, source.as_deref());
            }
        },
    }
}

/// Whether a typed `bn dispatch` should enqueue rather than launch.
///
/// Two escapes, both meaning "not this one": `--on-pool-full fail`, which already says *do not
/// defer this, tell me if it cannot run now*, and `$BN_DISPATCH_NOW` for a shell alias. Reusing an
/// existing flag beats minting a `--now` that would have to be explained alongside it.
fn dispatch_captured(cfg: &Config, on_pool_full: crate::cli::OnPoolFull) -> bool {
    if matches!(on_pool_full, crate::cli::OnPoolFull::Fail) {
        return false;
    }
    if std::env::var("BN_DISPATCH_NOW").is_ok_and(|v| !v.trim().is_empty() && v.trim() != "0") {
        return false;
    }
    crate::cmd::queue_producers::active(cfg, crate::cmd::queue_producers::DISPATCH)
}
