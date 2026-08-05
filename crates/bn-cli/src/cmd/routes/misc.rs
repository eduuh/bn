use crate::*;

pub(super) fn route(cfg: &Config, action: CmdMisc, json: bool) {
    match action {
        CmdMisc::Reset { force } => {
            if !wt::reset(cfg, force) {
                std::process::exit(1);
            }
        }
        CmdMisc::Rename { new_branch } => {
            if !wt::rename(cfg, &new_branch) {
                std::process::exit(1);
            }
        }
        CmdMisc::Build { name } => std::process::exit(run_build_cmd(cfg, name)),
        CmdMisc::Serve {
            action,
            port,
            lan,
            daemon,
        } => serve::run_cmd(action, port, lan, daemon, json),
        CmdMisc::Cost { by, window, live } => emit_cost(cfg, by.as_deref(), window, live, json),
        CmdMisc::Worker { action } => std::process::exit(emit_worker(cfg, action, json)),
        CmdMisc::Tools { action } => std::process::exit(emit_tools(action, json)),
        CmdMisc::Pipeline {
            repo,
            branch,
            all,
            action,
        } => {
            use crate::cmd::pipeline as pl;
            match action {
                None => pl::emit_pipeline(cfg, repo, branch, all, json),
                Some(action) => {
                    let ctx =
                        resolver::resolve(cfg, &std::env::current_dir().unwrap_or_default()).ok();
                    match action {
                        PipelineCmd::Status { repo, branch } => pl::emit_pipeline_status(
                            cfg,
                            ctx.as_ref(),
                            repo.as_deref(),
                            branch.as_deref(),
                            json,
                        ),
                        PipelineCmd::Set {
                            stage,
                            repo,
                            branch,
                        } => pl::emit_pipeline_set(
                            cfg,
                            ctx.as_ref(),
                            repo.as_deref(),
                            branch.as_deref(),
                            &stage,
                            json,
                        ),
                        PipelineCmd::Advance { repo, branch } => pl::emit_pipeline_advance(
                            cfg,
                            ctx.as_ref(),
                            repo.as_deref(),
                            branch.as_deref(),
                            json,
                        ),
                        PipelineCmd::Investigate { action } => match action {
                            PipelineInvestigateCmd::Finish {
                                repo,
                                branch,
                                unknown,
                                blocked,
                                no_tmux,
                            } => pl::emit_pipeline_investigate_finish(
                                cfg,
                                ctx.as_ref(),
                                repo.as_deref(),
                                branch.as_deref(),
                                &unknown,
                                blocked.as_deref(),
                                no_tmux,
                                json,
                            ),
                            PipelineInvestigateCmd::Contribute {
                                repo,
                                branch,
                                token,
                                finding,
                                blocked,
                            } => pl::emit_pipeline_investigate_contribute(
                                cfg,
                                &repo,
                                &branch,
                                &token,
                                &finding,
                                blocked.as_deref(),
                                json,
                            ),
                        },
                        PipelineCmd::Board => pl::emit_pipeline_board(cfg, json),
                        PipelineCmd::Badge { path } => pl::emit_pipeline_badge(cfg, path),
                    }
                }
            }
        }
        CmdMisc::Eye { action } => emit_eye(cfg, action, json),
        CmdMisc::Routine { action } => emit_routine(cfg, action, json),
        CmdMisc::Control { action } => emit_control(cfg, action, json),
        CmdMisc::Version => println!("{}", version::line()),
        CmdMisc::Bearings => emit_bearings(cfg, json),
        CmdMisc::External(args) => std::process::exit(delegate(&args)),
        CmdMisc::Kill { port } => std::process::exit(emit_kill(&port)),
        CmdMisc::KillTmux { no_backup, yes } => std::process::exit(emit_kill_tmux(no_backup, yes)),
        CmdMisc::Issue { action } => match action {
            IssueCmd::File {
                title,
                body,
                kind,
                labels,
                repo,
                allow_duplicate,
            } => emit_issue_file(title, body, kind, labels, repo, allow_duplicate, json),
        },
        CmdMisc::Ref { action, name } => emit_ref(action, name, json),
        CmdMisc::Critic { action } => {
            let ctx = resolver::resolve(cfg, &std::env::current_dir().unwrap_or_default()).ok();
            match action {
                CriticCmd::Dispatch {
                    branch,
                    repo,
                    pr,
                    base,
                    agent,
                    no_tmux,
                } => run_critic_dispatch(
                    cfg,
                    ctx.as_ref(),
                    repo.as_deref(),
                    &branch,
                    pr.as_deref(),
                    &base,
                    agent.as_deref(),
                    no_tmux,
                    json,
                ),
                CriticCmd::Verdict {
                    repo,
                    branch,
                    pass,
                    fail,
                    findings,
                    findings_file,
                    max_iterations,
                } => {
                    if pass == fail {
                        fail_json(
                            json,
                            "critic",
                            "bad-verdict",
                            "bn critic verdict: pass exactly one of --pass / --fail",
                        );
                    }
                    let findings_text = findings_file
                        .as_deref()
                        .and_then(|p| std::fs::read_to_string(p).ok())
                        .or(findings);
                    run_critic_verdict(
                        cfg,
                        ctx.as_ref(),
                        repo.as_deref(),
                        branch.as_deref(),
                        pass,
                        findings_text.as_deref(),
                        max_iterations,
                        json,
                    );
                }
            }
        }
        CmdMisc::Clean { all } => {
            if all {
                emit_clean_all(cfg, json);
            } else {
                let ctx = resolve_or_exit(cfg);
                emit_clean(cfg, &ctx, json);
            }
        }
        CmdMisc::Prune => emit_prune(cfg, json),
        CmdMisc::Stale { days } => emit_stale(cfg, days, json),
        CmdMisc::Archive { days, dry_run } => emit_archive(cfg, days, dry_run, json),
        CmdMisc::Completion { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
        }
        CmdMisc::ShellInit { shell } => emit_shell_init(shell),
        // Bare `bn`: ensure the note exists; print its dir, or the full note JSON with --json.
    }
}

/// The `agency` wrapper script emitted by `bn shell-init` (bash and zsh share this POSIX function
/// syntax verbatim). It is deliberately namespaced under `agency <claude|copilot>` rather than
/// shadowing the bare `claude`/`copilot` commands, so the real tools stay untouched on a machine
/// that also uses them directly — you opt into the dispatched flow per-invocation with the
/// `agency` prefix. `command` (and the `execvp` the harness launcher uses) bypass shell functions,
/// so the wrapper never recurses into itself.
const SHELL_WRAPPERS: &str = r#"# bn shell wrapper (bn shell-init) — `agency <claude|copilot> ...` dispatches a pooled, foreground
# investigation agent when run inside a git repo, and falls through to the real binary otherwise.
# It does NOT shadow the bare `claude`/`copilot` commands, so the real tools stay untouched — you
# opt into the dispatched flow per-invocation with the `agency` prefix.
agency() {
  if [ "$#" -eq 0 ]; then
    echo "usage: agency <claude|copilot> [args...]" >&2
    return 2
  fi
  local _bn_h="$1"; shift
  case "$_bn_h" in
    claude|copilot) ;;
    *) echo "agency: unknown agent '$_bn_h' (use: claude | copilot)" >&2; return 2 ;;
  esac
  if [ "$#" -eq 0 ] || ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    command "$_bn_h" "$@"
    return $?
  fi
  bn dispatch --agent "$_bn_h" --yolo --scout --pool --attach -- "$@"
}
"#;

/// Emit the `agency` shell wrapper to stdout — source it with `source <(bn shell-init zsh)`.
/// `agency <claude|copilot> <args>` inside a git work tree runs
/// `bn dispatch --agent <h> --yolo --scout --pool --attach -- <args>`: a pooled, foreground
/// investigation whose warm slot returns to the pool the moment the agent exits. With no extra
/// args, or outside a repo, it falls through to the real binary (`command <h>`), so the bare
/// `claude`/`copilot` commands are never shadowed and normal use is unaffected.
fn emit_shell_init(shell: Shell) {
    match shell {
        Shell::Bash | Shell::Zsh => {}
        other => {
            eprintln!("bn shell-init: only bash and zsh are supported (got {other})");
            std::process::exit(2);
        }
    }
    print!("{SHELL_WRAPPERS}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_wrappers_define_pooled_foreground_scout() {
        // A single `agency` dispatcher — NOT bare `claude`/`copilot` overrides (no global shadow).
        assert!(SHELL_WRAPPERS.contains("agency() {"));
        assert!(!SHELL_WRAPPERS.contains("claude() {"));
        assert!(!SHELL_WRAPPERS.contains("copilot() {"));
        // Only the two known harnesses are accepted.
        assert!(SHELL_WRAPPERS.contains("claude|copilot) ;;"));
        // Inside a repo with args: a pooled, foreground investigation (the exact opt-in defaults).
        assert!(SHELL_WRAPPERS
            .contains("bn dispatch --agent \"$_bn_h\" --yolo --scout --pool --attach -- \"$@\""));
        // Fall-through to the real binary (no recursion) when given no args or not in a work tree.
        assert!(SHELL_WRAPPERS.contains("[ \"$#\" -eq 0 ]"));
        assert!(SHELL_WRAPPERS.contains("git rev-parse --is-inside-work-tree"));
        assert!(SHELL_WRAPPERS.contains("command \"$_bn_h\" \"$@\""));
    }
}
