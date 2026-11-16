use crate::*;

pub(super) fn route(cfg: &Config, action: CmdIntake, json: bool) {
    match action {
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
            one_shot,
            refs,
        } => {
            // Dispatch works from anywhere: cwd need not be a repo. Resolve the cwd's context
            // best-effort (for todo-id resolution / closing the source todo) — `run_dispatch`
            // requires it only when no `--repo` is given, and then emits a `--repo` nudge.
            let ctx = resolver::resolve(cfg, &std::env::current_dir().unwrap_or_default()).ok();
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
                one_shot,
                &refs,
                stage,
                // A typed `bn dispatch` always renders its seed from a workflow skill.
                None,
                json,
            );
        }
        CmdIntake::Verify(args) => emit_verify(cfg, args, json),
        CmdIntake::Gate(VerifyArgs { job, for_ }) => {
            if !wt::cmd_gate(cfg, job, json, &for_) {
                std::process::exit(1);
            }
        }
        CmdIntake::TrustFolder { path, harness } => emit_trust_folder(path, &harness, json),
        CmdIntake::Wt { action } => wt::run(cfg, action, json),
    }
}
