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
        CmdMisc::Build { name, repo, branch } => {
            std::process::exit(run_build_cmd(cfg, name, repo, branch))
        }
        CmdMisc::Serve {
            action,
            port,
            lan,
            daemon,
        } => serve::run_cmd(cfg, action, port, lan, daemon, json),
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
                        PipelineCmd::Sweep => {
                            crate::cmd::pipeline_sweep::emit_pipeline_sweep(cfg, json)
                        }
                        PipelineCmd::Route { task, all } => {
                            pl::emit_pipeline_route(cfg, task.as_deref(), all, json)
                        }
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
        CmdMisc::Routine { action } => emit_routine(cfg, action, json),
        CmdMisc::Control { action } => emit_control(cfg, action, json),
        CmdMisc::Version => println!("{}", version::line()),
        CmdMisc::Bearings => emit_bearings(cfg, json),
        CmdMisc::Kill { port } => std::process::exit(emit_kill(&port)),
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

/// The wrapper script emitted by `bn shell-init` (bash and zsh share this POSIX function syntax
/// verbatim).
///
/// `claude` and `copilot` **are** shadowed here: that is the point. The placement rule — main hosts
/// no agents, one agent per worktree — can only be enforced if the bare commands people actually
/// type are routed through `bn agent launch`, where the guard lives. The launcher refuses outside a
/// git worktree, so every session — including planning — has a repository branch behind it.
///
/// `agency <claude|copilot>` is kept as the explicit form of the same thing. `command` (and the
/// `execvp` the harness launcher uses) bypass shell functions, so neither wrapper recurses.
const SHELL_WRAPPERS: &str = r#"# bn shell wrappers (bn shell-init) — `claude` / `copilot` route through
# `bn agent launch`, which requires a git worktree and enforces where an agent may run: the main
# worktree hosts none (it asks what the work is and dispatches into a warm pool slot instead), and a
# worktree that already has a live agent refuses a second one.
# Escape hatch for within-repo placement rules: BN_AGENT_GUARD=0 claude …
#
# The marker below is how `bn doctor` can tell these wrappers are actually live in your shell: a
# child process cannot see its parent's functions, only its environment.
export BN_SHELL_WRAPPERS=1

_bn_launch() {
  local _bn_h="$1"; shift
  bn agent launch "$_bn_h" -- "$@"
}

claude() { _bn_launch claude "$@"; }
copilot() { _bn_launch copilot "$@"; }

# The explicit form: `agency claude "investigate X"` runs a pooled, foreground scout in its own warm
# slot — an interactive investigation that hands the slot back the moment it exits.
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
  if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo "agency: sessions require a git worktree; enter or create a repository first" >&2
    return 1
  fi
  if [ "$#" -eq 0 ]; then
    bn agent launch "$_bn_h"
    return $?
  fi
  bn dispatch --agent "$_bn_h" --yolo --scout --pool --attach -- "$@"
}
"#;

/// Emit the shell wrappers to stdout — source them with `source <(bn shell-init zsh)`.
///
/// `claude`/`copilot` inside a git work tree route through `bn agent launch`, the chokepoint that
/// enforces agent placement and refuses to create sessions outside a git worktree.
/// `agency <claude|copilot> <args>` stays the explicit pooled-scout form
/// (`bn dispatch --agent <h> --yolo --scout --pool --attach -- <args>`), a foreground investigation
/// whose warm slot returns to the pool the moment the agent exits.
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
