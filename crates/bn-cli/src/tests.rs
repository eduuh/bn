//! Crate-root unit tests for `bn-cli` dispatch helpers, split out of `main.rs`
//! (issue #333). `mod tests;` in `main.rs` keeps `super` pointed at the crate root,
//! so `use super::*;` still resolves every re-exported handler unchanged.

use super::*;
use resolver::{JobScope, Resolved};

fn ctx(repo: &str, branch: &str) -> Resolved {
    Resolved {
        repo: repo.into(),
        branch: branch.into(),
    }
}

/// Run `f` on a roomy (8 MiB, main-thread-sized) stack, re-raising any panic so the test still
/// fails with the original message.
///
/// Building + `debug_assert`-validating clap's whole command tree is the CLI's stack-hungriest
/// operation in unoptimized (debug/test) builds. Pre-#359 the single ~100-variant `Command`
/// enum's generated `augment_subcommands` peaked at ~2.25 MiB — just over libtest's ~2 MiB
/// per-test-thread default — so any full-tree test aborted the binary with SIGABRT (hence the
/// `RUST_MIN_STACK` bump in `.cargo/config.toml`). Splitting `Command` into flattened
/// thematic command-group enums (#359) dropped the peak to ~0.87 MiB; these full-tree tests keep the roomy
/// thread as harmless headroom, while `cli_tree_fits_reduced_stack` is the tight regression guard.
fn on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    match std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(f)
        .expect("spawn big-stack test thread")
        .join()
    {
        Ok(v) => v,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[test]
fn cat_is_a_real_subcommand() {
    // Regression for bn-shortcomings #15: `bn cat` must parse as a subcommand (mirroring the
    // MCP `bn_cat` tool and the standard resume prompt), not error with "Unknown command: cat".
    // Parsing triggers clap's whole-tree `debug_assert`, so run it on a big stack (see above).
    on_big_stack(|| {
        let cli = Cli::try_parse_from(["bn", "cat"]).expect("`bn cat` should parse");
        assert!(matches!(cli.command, Some(Command::Review(CmdReview::Cat))));
    });
}

#[test]
fn bearings_is_a_real_subcommand() {
    on_big_stack(|| {
        let cli = Cli::try_parse_from(["bn", "bearings"]).expect("`bn bearings` should parse");
        assert!(matches!(
            cli.command,
            Some(Command::Misc(CmdMisc::Bearings))
        ));
    });
}

#[test]
fn connect_nested_host_flags_parse() {
    on_big_stack(|| {
        let cli = Cli::try_parse_from(["bn", "connect", "--window", "--read-only", "fedora"])
            .expect("nested `bn connect` flags should parse");
        assert!(matches!(
            cli.command,
            Some(Command::Fleet(CmdFleet::Connect {
                host: Some(ref host),
                window: true,
                read_only: true,
                pick_stage: None,
            })) if host == "fedora"
        ));
    });
}

#[test]
fn mobile_host_parses() {
    on_big_stack(|| {
        let cli = Cli::try_parse_from(["bn", "mobile", "fedora"])
            .expect("`bn mobile <host>` should parse");
        assert!(matches!(
            cli.command,
            Some(Command::Fleet(CmdFleet::Mobile { ref host })) if host == "fedora"
        ));
    });
}

#[test]
fn resume_alias_resolves_the_same_session_as_sessions_resume() {
    on_big_stack(|| {
        let canonical =
            Cli::try_parse_from(["bn", "sessions", "resume", "Session registry"]).unwrap();
        let alias = Cli::try_parse_from(["bn", "resume", "Session registry"]).unwrap();
        let canonical_query = match canonical.command {
            Some(Command::Fleet(CmdFleet::Sessions {
                action: Some(SessionsCmd::Resume { name_or_id }),
            })) => name_or_id,
            other => panic!("unexpected canonical command: {other:?}"),
        };
        let alias_query = match alias.command {
            Some(Command::Fleet(CmdFleet::Resume { name_or_id })) => name_or_id,
            other => panic!("unexpected alias command: {other:?}"),
        };
        let rows = vec![bn_core::agent::SessionRecord {
            task_name: "Session registry".into(),
            harness: "copilot".into(),
            repo: "bn".into(),
            branch: "dispatch/session-registry".into(),
            worktree: "/tmp/session-registry".into(),
            session_id: "abc-123".into(),
            controller_id: None,
            status: bn_core::agent::SessionStatus::Idle,
            pr_url: None,
            created_at: 10,
            updated_at: 20,
        }];
        assert_eq!(
            resolve_registered_session(&rows, Some(&canonical_query), "/tmp/other")
                .unwrap()
                .unwrap()
                .session_id,
            resolve_registered_session(&rows, alias_query.as_deref(), "/tmp/other")
                .unwrap()
                .unwrap()
                .session_id
        );
    });
}

#[test]
fn zero_argument_resume_selects_latest_session_for_current_worktree() {
    on_big_stack(|| {
        let cli = Cli::try_parse_from(["bn", "resume"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Fleet(CmdFleet::Resume { name_or_id: None }))
        ));
        let json_cli = Cli::try_parse_from(["bn", "--json", "resume"]).unwrap();
        assert!(json_cli.json);
        assert!(matches!(
            json_cli.command,
            Some(Command::Fleet(CmdFleet::Resume { name_or_id: None }))
        ));
        let row = |id: &str, updated_at| bn_core::agent::SessionRecord {
            task_name: id.into(),
            harness: "copilot".into(),
            repo: "bn".into(),
            branch: "dispatch/resume".into(),
            worktree: "/tmp/resume".into(),
            session_id: id.into(),
            controller_id: None,
            status: bn_core::agent::SessionStatus::Idle,
            pr_url: None,
            created_at: 10,
            updated_at,
        };
        let rows = vec![row("older", 20), row("latest", 30)];
        assert_eq!(
            resolve_registered_session(&rows, None, "/tmp/resume")
                .unwrap()
                .unwrap()
                .session_id,
            "latest"
        );
    });
}

#[test]
fn zero_argument_resume_falls_back_when_worktree_has_no_session() {
    let rows = vec![bn_core::agent::SessionRecord {
        task_name: "Other".into(),
        harness: "copilot".into(),
        repo: "bn".into(),
        branch: "dispatch/other".into(),
        worktree: "/tmp/other".into(),
        session_id: "other".into(),
        controller_id: None,
        status: bn_core::agent::SessionStatus::Idle,
        pr_url: None,
        created_at: 10,
        updated_at: 20,
    }];
    assert_eq!(
        resolve_registered_session(&rows, None, "/tmp/resume").unwrap(),
        None
    );
}

#[test]
fn task_name_flags_parse_for_dispatch_and_control() {
    on_big_stack(|| {
        let dispatch =
            Cli::try_parse_from(["bn", "dispatch", "ship it", "--name", "Readable task"]).unwrap();
        assert!(matches!(
            dispatch.command,
            Some(Command::Intake(CmdIntake::Dispatch {
                name: Some(ref name),
                ..
            })) if name == "Readable task"
        ));
        let control =
            Cli::try_parse_from(["bn", "control", "start", "--name", "Coordinate releases"])
                .unwrap();
        assert!(matches!(
            control.command,
            Some(Command::Misc(CmdMisc::Control {
                action: ControlCmd::Start {
                    name: Some(ref name),
                    ..
                },
            })) if name == "Coordinate releases"
        ));
    });
}

#[test]
fn critic_subcommands_parse() {
    // #649 Phase 3: `bn critic dispatch` / `verdict` must parse. Parsing runs clap's whole-tree
    // debug_assert, so use the big stack (see on_big_stack).
    on_big_stack(|| {
        let d = Cli::try_parse_from([
            "bn",
            "critic",
            "dispatch",
            "--branch",
            "dispatch/x",
            "--pr",
            "https://pr/1",
        ])
        .expect("`bn critic dispatch` should parse");
        assert!(matches!(
            d.command,
            Some(Command::Misc(CmdMisc::Critic {
                action: CriticCmd::Dispatch { .. }
            }))
        ));
        // Verdict defaults max-iterations to the bn-core constant.
        let v = Cli::try_parse_from(["bn", "critic", "verdict", "--fail", "--findings", "boom"])
            .expect("`bn critic verdict` should parse");
        match v.command {
            Some(Command::Misc(CmdMisc::Critic {
                action:
                    CriticCmd::Verdict {
                        max_iterations,
                        fail,
                        ..
                    },
            })) => {
                assert!(fail);
                assert_eq!(
                    max_iterations,
                    bn_core::pipeline::critic::DEFAULT_MAX_ITERATIONS
                );
            }
            _ => panic!("expected critic verdict"),
        }
    });
}

#[test]
fn pipeline_targets_and_all_parse() {
    on_big_stack(|| {
        let targeted =
            Cli::try_parse_from(["bn", "pipeline", "--repo", "bn", "--branch", "dispatch/657"])
                .expect("targeted `bn pipeline` should parse");
        assert!(matches!(
            targeted.command,
            Some(Command::Misc(CmdMisc::Pipeline {
                repo: Some(ref repo),
                branch: Some(ref branch),
                all: false,
                action: None,
            })) if repo == "bn" && branch == "dispatch/657"
        ));

        let all =
            Cli::try_parse_from(["bn", "--json", "pipeline", "--all"]).expect("all should parse");
        assert!(all.json);
        assert!(matches!(
            all.command,
            Some(Command::Misc(CmdMisc::Pipeline { all: true, .. }))
        ));
    });
}

/// `bn routine tick`/`notify`/`watch`/`service ...` all route correctly (#801 amendment) —
/// coverage for the new subcommands the deterministic-lifetime revision added alongside the
/// pre-existing now/next/status/check-in/snooze/skip/override/clear-override.
#[test]
fn routine_tick_notify_watch_and_service_subcommands_parse() {
    on_big_stack(|| {
        let tick =
            Cli::try_parse_from(["bn", "routine", "tick"]).expect("`bn routine tick` should parse");
        assert!(matches!(
            tick.command,
            Some(Command::Misc(CmdMisc::Routine {
                action: Some(RoutineCmd::Tick)
            }))
        ));

        let notify = Cli::try_parse_from(["bn", "routine", "notify"])
            .expect("`bn routine notify` should parse");
        assert!(matches!(
            notify.command,
            Some(Command::Misc(CmdMisc::Routine {
                action: Some(RoutineCmd::Notify)
            }))
        ));

        let watch = Cli::try_parse_from(["bn", "routine", "watch"])
            .expect("`bn routine watch` should parse");
        assert!(matches!(
            watch.command,
            Some(Command::Misc(CmdMisc::Routine {
                action: Some(RoutineCmd::Watch)
            }))
        ));

        let install = Cli::try_parse_from(["bn", "routine", "service", "install"])
            .expect("`bn routine service install` should parse");
        assert!(matches!(
            install.command,
            Some(Command::Misc(CmdMisc::Routine {
                action: Some(RoutineCmd::Service {
                    action: RoutineServiceCmd::Install
                })
            }))
        ));

        let status = Cli::try_parse_from(["bn", "routine", "service", "status"])
            .expect("`bn routine service status` should parse");
        assert!(matches!(
            status.command,
            Some(Command::Misc(CmdMisc::Routine {
                action: Some(RoutineCmd::Service {
                    action: RoutineServiceCmd::Status
                })
            }))
        ));
    });
}

#[test]
fn retro_subcommands_parse() {
    // The Interviewer (#261) reclaimed the `retro` name (formerly an `insights` alias). Parsing
    // runs clap's whole-tree debug_assert, so use the big stack (see on_big_stack).
    on_big_stack(|| {
        // Bare `bn retro` → the chat action (None).
        let bare = Cli::try_parse_from(["bn", "retro"]).expect("`bn retro` should parse");
        assert!(matches!(
            bare.command,
            Some(Command::Staff(CmdStaff::Retro { action: None }))
        ));
        // Subcommands: chat (+ --voice), skip, show <week>.
        assert!(matches!(
            Cli::try_parse_from(["bn", "retro", "chat", "--voice"])
                .unwrap()
                .command,
            Some(Command::Staff(CmdStaff::Retro {
                action: Some(RetroCmd::Chat { voice: true })
            }))
        ));
        assert!(matches!(
            Cli::try_parse_from(["bn", "retro", "skip"])
                .unwrap()
                .command,
            Some(Command::Staff(CmdStaff::Retro {
                action: Some(RetroCmd::Skip)
            }))
        ));
        let show = Cli::try_parse_from(["bn", "retro", "show", "2026-W28"]).unwrap();
        match show.command {
            Some(Command::Staff(CmdStaff::Retro {
                action: Some(RetroCmd::Show { week }),
            })) => assert_eq!(week.as_deref(), Some("2026-W28")),
            other => panic!("unexpected parse: {other:?}"),
        }
        // `insights` keeps its own name — the `retro` alias is gone, so this is now a distinct cmd.
        assert!(Cli::try_parse_from(["bn", "insights"]).is_ok());
    });
}

#[test]
fn orchestrate_agent_flag_parses_and_defaults() {
    // #382: `bn orchestrate --queue … --agent claude` selects the harness the daemon dispatches
    // under; omitting `--agent` leaves it `None` (the daemon then uses the config default, else
    // copilot). Parsing runs clap's whole-tree debug_assert, so use the big stack (see above).
    on_big_stack(|| {
        let with =
            Cli::try_parse_from(["bn", "orchestrate", "--queue", "338", "--agent", "claude"])
                .expect("`bn orchestrate --agent claude` should parse");
        match with.command {
            Some(Command::Review(CmdReview::Orchestrate { agent, queue, .. })) => {
                assert_eq!(agent.as_deref(), Some("claude"));
                assert_eq!(queue, vec!["338".to_string()]);
            }
            other => panic!("unexpected parse: {other:?}"),
        }
        // No `--agent` → None (default resolved downstream to copilot).
        let without = Cli::try_parse_from(["bn", "orchestrate", "--queue", "338"])
            .expect("`bn orchestrate --queue` should parse");
        match without.command {
            Some(Command::Review(CmdReview::Orchestrate { agent, .. })) => {
                assert_eq!(agent, None);
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    });
}

#[test]
fn orchestrate_doctor_parses_repo_scope() {
    on_big_stack(|| {
        let cli = Cli::try_parse_from(["bn", "orchestrate", "doctor", "--repo", "bn"])
            .expect("`bn orchestrate doctor --repo bn` should parse");
        match cli.command {
            Some(Command::Review(CmdReview::Orchestrate {
                action: Some(OrchestrateCmd::Doctor { repo }),
                ..
            })) => assert_eq!(repo.as_deref(), Some("bn")),
            other => panic!("unexpected parse: {other:?}"),
        }
    });
}

#[test]
fn orchestrate_decompose_parses_scoped_parts() {
    on_big_stack(|| {
        let cli = Cli::try_parse_from([
            "bn",
            "orchestrate",
            "decompose",
            "755",
            "--part",
            "Add core model::crates/bn-core/src/decompose.rs",
            "--part",
            "Wire CLI::crates/bn-cli/src/cmd/orchestrate.rs,crates/bn-cli/src/cli.rs",
            "--dry-run",
        ])
        .expect("`bn orchestrate decompose` should parse");
        match cli.command {
            Some(Command::Review(CmdReview::Orchestrate {
                action:
                    Some(OrchestrateCmd::Decompose {
                        issue,
                        part,
                        dry_run,
                        ..
                    }),
                ..
            })) => {
                assert_eq!(issue, "755");
                assert_eq!(part.len(), 2);
                assert!(dry_run);
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    });
}

#[test]
fn orchestrate_supervise_keep_warm_flags_parse() {
    on_big_stack(|| {
        let cli = Cli::try_parse_from([
            "bn",
            "orchestrate",
            "supervise",
            "--all-repos",
            "--keep-warm",
            "--low-water",
            "3",
            "--own",
            "bn,wira360",
            "--exclude-title",
            "audit:",
            "--exclude-label",
            "hold",
            "--brainstorm",
            "2",
        ])
        .expect("keep-warm supervise flags should parse");
        match cli.command {
            Some(Command::Review(CmdReview::Orchestrate {
                action:
                    Some(OrchestrateCmd::Supervise {
                        all_repos,
                        keep_warm,
                        low_water,
                        own,
                        exclude_title,
                        exclude_label,
                        brainstorm,
                        ..
                    }),
                ..
            })) => {
                assert!(all_repos);
                assert!(keep_warm);
                assert_eq!(low_water, Some(3));
                assert_eq!(own, vec!["bn", "wira360"]);
                assert_eq!(exclude_title, vec!["audit:"]);
                assert_eq!(exclude_label, vec!["hold"]);
                assert_eq!(brainstorm, Some(2));
            }
            other => panic!("unexpected parse: {other:?}"),
        }
        assert!(
            Cli::try_parse_from(["bn", "orchestrate", "supervise", "--low-water", "3"]).is_err()
        );
    });
}

#[test]
fn orchestrate_reset_parses_lane_and_repo_scope() {
    on_big_stack(|| {
        let cli =
            Cli::try_parse_from(["bn", "orchestrate", "reset", "dispatch/714", "--repo", "bn"])
                .expect("`bn orchestrate reset <lane> --repo bn` should parse");
        match cli.command {
            Some(Command::Review(CmdReview::Orchestrate {
                action: Some(OrchestrateCmd::Reset { lane, repo }),
                ..
            })) => {
                assert_eq!(lane, "dispatch/714");
                assert_eq!(repo.as_deref(), Some("bn"));
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    });
}

#[test]
fn orchestrate_status_why_parses() {
    on_big_stack(|| {
        let cli = Cli::try_parse_from(["bn", "orchestrate", "status", "--repo", "bn", "--why"])
            .expect("`bn orchestrate status --why` should parse");
        match cli.command {
            Some(Command::Review(CmdReview::Orchestrate {
                action:
                    Some(OrchestrateCmd::Status {
                        repo,
                        why,
                        degraded,
                    }),
                ..
            })) => {
                assert_eq!(repo.as_deref(), Some("bn"));
                assert!(why);
                assert!(!degraded);
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    });
}

#[test]
fn orchestrate_observability_flags_and_alias_parse() {
    on_big_stack(|| {
        let status =
            Cli::try_parse_from(["bn", "orchestrate", "status", "--repo", "bn", "--degraded"])
                .expect("`bn orchestrate status --degraded` should parse");
        match status.command {
            Some(Command::Review(CmdReview::Orchestrate {
                action:
                    Some(OrchestrateCmd::Status {
                        repo,
                        why,
                        degraded,
                    }),
                ..
            })) => {
                assert_eq!(repo.as_deref(), Some("bn"));
                assert!(!why);
                assert!(degraded);
            }
            other => panic!("unexpected parse: {other:?}"),
        }

        let logs = Cli::try_parse_from([
            "bn",
            "orchestrate",
            "events",
            "--repo",
            "bn",
            "--level",
            "warn",
            "--limit",
            "7",
        ])
        .expect("`bn orchestrate events` alias should parse");
        match logs.command {
            Some(Command::Review(CmdReview::Orchestrate {
                action: Some(OrchestrateCmd::Logs { repo, level, limit }),
                ..
            })) => {
                assert_eq!(repo.as_deref(), Some("bn"));
                assert!(matches!(level, Some(OrchestrateLogLevelArg::Warn)));
                assert_eq!(limit, 7);
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    });
}

#[test]
fn quarter_advise_parses_capacity_and_defaults() {
    // #458: `bn quarter advise [--capacity N]` selects the goal-aligned shortlist size; omitting
    // it defaults to 3. Parsing runs clap's whole-tree debug_assert, so use the big stack.
    on_big_stack(|| {
        let explicit = Cli::try_parse_from(["bn", "quarter", "advise", "--capacity", "5"])
            .expect("`bn quarter advise --capacity 5` should parse");
        match explicit.command {
            Some(Command::Planning(CmdPlanning::Quarter {
                action: Some(QuarterCmd::Advise { capacity }),
                ..
            })) => assert_eq!(capacity, 5),
            other => panic!("unexpected parse: {other:?}"),
        }
        let default = Cli::try_parse_from(["bn", "quarter", "advise"])
            .expect("`bn quarter advise` should parse");
        match default.command {
            Some(Command::Planning(CmdPlanning::Quarter {
                action: Some(QuarterCmd::Advise { capacity }),
                ..
            })) => assert_eq!(capacity, 3, "default capacity is 3"),
            other => panic!("unexpected parse: {other:?}"),
        }
    });
}

#[test]
fn cli_command_tree_has_no_collisions() {
    // clap validates the whole command tree (unique subcommand names/aliases, well-formed args)
    // in `debug_assert`. Regression guard for the `quarter`/`q` collision that made every CLI
    // parse abort with "command `quarter` alias `q` is duplicated". Big stack: see on_big_stack.
    use clap::CommandFactory;
    on_big_stack(|| Cli::command().debug_assert());
}

#[test]
fn cli_tree_fits_reduced_stack() {
    // Headroom canary (#359). Building + `debug_assert`-validating the whole clap tree is the
    // CLI's stack-hungriest debug-build operation. Pre-#359 the flat ~100-variant `Command` enum
    // peaked at ~2.25 MiB and overflowed libtest's ~2 MiB default thread — which is why
    // `.cargo/config.toml` bumps `RUST_MIN_STACK`. Splitting `Command` into flattened thematic command-group
    // enums dropped the peak to ~0.87 MiB. Pin the walk to a 2 MiB thread (libtest's default,
    // independent of `RUST_MIN_STACK`): if a new subcommand/arg pushes the peak back over that
    // ceiling this fails *here* — loudly and locally — instead of mysteriously in CI. To recover
    // headroom, add the new command to the last thematic command-group enum (or start another) in `cli.rs` so no
    // single enum's generated frame dominates again.
    use clap::CommandFactory;
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| Cli::command().debug_assert())
        .expect("spawn canary thread")
        .join()
        .expect(
            "clap command tree no longer fits a 2 MiB stack — the Command enum has regrown; \
             regroup it into thematic command-group enums in cli.rs (issue #359)",
        );
}

#[test]
fn capture_title_falls_back_to_url_tail() {
    // No network: gh either fails or is absent, so these exercise the fallback branch.
    // GitHub issue/PR URLs degrade to a "#<num>" label; bare URLs to their tail.
    assert_eq!(capture_title("https://example.com/things/widget"), "widget");
    // A non-github issue-shaped URL still gets the issue label from the path marker.
    assert_eq!(
        capture_title("https://gitlab.example.com/o/r/issues/7"),
        "issue #7"
    );
}

#[test]
fn switch_target_prefers_arg_then_opposite_of_current() {
    // An explicit, valid arg always wins — even if it matches the current harness.
    assert_eq!(
        switch_target_or_default(Some("claude"), Some("copilot")),
        Ok(Some("claude".to_string()))
    );
    assert_eq!(
        switch_target_or_default(Some("copilot"), None),
        Ok(Some("copilot".to_string()))
    );
    // No arg: pick the opposite of whatever is driving bn.
    assert_eq!(
        switch_target_or_default(None, Some("claude")),
        Ok(Some("copilot".to_string()))
    );
    assert_eq!(
        switch_target_or_default(None, Some("copilot")),
        Ok(Some("claude".to_string()))
    );
    // No arg and no known driver → defer to the config fallback (None).
    assert_eq!(switch_target_or_default(None, None), Ok(None));
    // An unknown explicit arg is rejected before anything is written.
    assert!(switch_target_or_default(Some("gemini"), Some("claude")).is_err());
}

#[test]
fn swap_pane_target_prefers_window_then_own_pane_only_inside_worktree() {
    let c = ctx("bn", "feat-x");
    // The worktree's window pane always wins when it exists.
    assert_eq!(
        swap_pane_target(&c, Some("%3".into()), Some("%9".into()), false),
        Ok("%3".to_string())
    );
    // No window: the calling pane is only a valid target from inside the worktree —
    // otherwise an orchestrator-driven swap would respawn the orchestrator's own pane.
    assert_eq!(
        swap_pane_target(&c, None, Some("%9".into()), true),
        Ok("%9".to_string())
    );
    assert!(swap_pane_target(&c, None, Some("%9".into()), false).is_err());
    assert!(swap_pane_target(&c, None, None, true).is_err());
}

#[test]
fn swap_respawn_args_kill_respawn_shell_backed() {
    let args = swap_respawn_args("%3", "/wt/bn/feat-x", "claude --strict-mcp-config 'go'");
    assert_eq!(
        &args[..6],
        &["respawn-pane", "-k", "-t", "%3", "-c", "/wt/bn/feat-x"]
    );
    // Shell-backed: the harness command must drop to a shell on exit, so a crash leaves a
    // live pane holding the output instead of a torn-down window.
    assert_eq!(
        args[6],
        "claude --strict-mcp-config 'go'; exec \"${SHELL:-sh}\""
    );
}

#[test]
fn default_resume_prompt_points_at_the_packet() {
    let p = default_resume_prompt(&ctx("bn", "feat/x"), Some("claude"));
    assert!(p.contains("bn/feat/x"));
    assert!(p.contains("after switching from claude"));
    assert!(p.contains("bn handoff read"));
    // Unknown driver → no "switching from" clause.
    let p = default_resume_prompt(&ctx("bn", "feat/x"), None);
    assert!(!p.contains("switching from"));
}

#[test]
fn normalize_agent_maps_aliases() {
    // copilot is the default (bare --agent) and its aliases.
    for v in ["", "copilot", "COPILOT", "gh", "github"] {
        assert_eq!(normalize_agent(v), Some("copilot"), "{v}");
    }
    // claude / claudecode / cc all mean Claude Code.
    for v in ["claude", "claudecode", "claude-code", "CC", " cc "] {
        assert_eq!(normalize_agent(v), Some("claude"), "{v}");
    }
    // Anything else is rejected (caller errors before creating a worktree).
    assert_eq!(normalize_agent("gemini"), None);
}

#[test]
fn keys_paste_steps_pastes_then_escape_delay() {
    // The paste plan stages the message into a buffer and pastes it as one block, then Escapes
    // any autocomplete popup the pasted `@`/`#` opened and settles — the submit is separate.
    let steps = keys_paste_steps("=bn:feat/x", "bn-steer-1", "steer @foo #7");
    let cmd = |args: &[&str]| TmuxStep::Cmd(args.iter().map(|s| s.to_string()).collect());
    assert_eq!(
        steps,
        vec![
            // `--` guards a message that might start with `-`; paste-buffer `-d` self-cleans.
            cmd(&["set-buffer", "-b", "bn-steer-1", "--", "steer @foo #7"]),
            cmd(&["paste-buffer", "-d", "-b", "bn-steer-1", "-t", "=bn:feat/x"]),
            cmd(&["send-keys", "-t", "=bn:feat/x", "Escape"]),
            TmuxStep::DelayMs(KEYS_SUBMIT_DELAY_MS),
        ]
    );
}

#[test]
fn keys_paste_and_submit_steps_normalize_to_an_exact_target() {
    // A raw `repo:branch` target must become `=repo:branch` on BOTH the paste and submit legs:
    // without the exact-match flag, tmux prefix/fnmatch resolution can land the steer in a
    // *different* agent's window (indices renumber after `wt cleanup`, names can share prefixes).
    let paste = keys_paste_steps("bn:feat/x", "bn-steer-1", "hi");
    let submit = keys_submit_steps("bn:feat/x");
    let targets: Vec<&String> = paste
        .iter()
        .chain(submit.iter())
        .filter_map(|s| match s {
            TmuxStep::Cmd(args) => args.iter().position(|a| a == "-t").map(|i| &args[i + 1]),
            TmuxStep::DelayMs(_) => None,
        })
        .collect();
    assert!(!targets.is_empty());
    assert!(targets.iter().all(|t| *t == "=bn:feat/x"), "{targets:?}");
}

#[test]
fn tmux_exact_target_prefixes_names_and_passes_ids_through() {
    assert_eq!(tmux_exact_target("bn:feat/x"), "=bn:feat/x");
    // Already exact, or a tmux id (pane/window/session) — untouched.
    assert_eq!(tmux_exact_target("=bn:feat/x"), "=bn:feat/x");
    assert_eq!(tmux_exact_target("%3"), "%3");
    assert_eq!(tmux_exact_target("@5"), "@5");
    assert_eq!(tmux_exact_target("$1"), "$1");
}

#[test]
fn pane_tail_trims_padding_keeps_interior_blanks_and_caps() {
    // tmux pads the bottom to pane height with blank lines and right-pads each row — both go.
    let raw = "$ bn status   \n\nworking…  \n\n\n";
    assert_eq!(pane_tail(raw, 40), vec!["$ bn status", "", "working…"]);
    // Cap keeps only the trailing N, counted after padding is trimmed.
    let raw = "a\nb\nc\nd\n\n";
    assert_eq!(pane_tail(raw, 2), vec!["c", "d"]);
    // All-blank capture collapses to nothing rather than a vec of empties.
    assert!(pane_tail("\n\n\n", 40).is_empty());
}

#[test]
fn keys_submit_steps_send_raw_carriage_return() {
    // Submit is a raw CR (`send-keys -H 0d`), not the named `Enter` the Copilot TUI swallows.
    let cmd = |args: &[&str]| TmuxStep::Cmd(args.iter().map(|s| s.to_string()).collect());
    assert_eq!(
        keys_submit_steps("=bn:feat/x"),
        vec![cmd(&["send-keys", "-t", "=bn:feat/x", "-H", "0d"])]
    );
}

// Copy-on-run: `bn verify` snapshots the shared `jobs/<name>.sh` before running it, so a
// concurrent job-script refresh that rewrites the shared file mid-run can't change what the
// gate executes. Prove it: snapshot, mutate the original, then run the snapshot.
#[cfg(unix)]
#[test]
fn verify_snapshot_runs_original_not_mutated_shared_script() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("test.sh");
    std::fs::write(&original, "#!/bin/sh\necho ORIGINAL\n").unwrap();

    let snap = snapshot_gate_script(&original, &dir.path().join("snap")).unwrap();

    // A background refresh rewrites the shared script *after* we snapshotted it.
    std::fs::write(&original, "#!/bin/sh\necho MUTATED\n").unwrap();

    // Run the snapshot exactly as emit_verify does.
    let out = std::process::Command::new("/bin/sh")
        .arg(&snap)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ORIGINAL"),
        "gate ran the mutated shared script, not the snapshot: {stdout:?}"
    );
    assert!(
        !stdout.contains("MUTATED"),
        "snapshot leaked mutated content: {stdout:?}"
    );

    // The snapshot is a distinct, executable file.
    assert_ne!(snap, original);
    let mode = std::fs::metadata(&snap).unwrap().permissions().mode();
    assert!(mode & 0o111 != 0, "snapshot is not executable: {mode:o}");

    // cleanup_snapshot removes the snapshot but never the shared script.
    cleanup_snapshot(&snap, &original);
    assert!(!snap.exists(), "snapshot not cleaned up");
    cleanup_snapshot(&original, &original);
    assert!(
        original.exists(),
        "cleanup wrongly removed the shared script"
    );
}

/// #454: a configured `[repo.<name>].verify` command is the repo's native gate — it remains the
/// sole target, without touching bn's conventional build/test/lint job resolution.
#[test]
fn resolve_verify_target_prefers_configured_command() {
    let cfg = Config {
        repos: std::collections::HashMap::from([(
            "arena".to_string(),
            resolver::RepoConfig {
                verify: Some("npm run validate".to_string()),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    let c = ctx("arena", "feature/x");
    match resolve_verify_target(&cfg, &c, None) {
        Ok(Some(gate)) => {
            assert_eq!(gate.label(), "npm run validate");
            assert_eq!(gate.targets().len(), 1);
            match &gate.targets()[0] {
                VerifyTarget::Command { label, cmd } => {
                    assert_eq!(cmd, "npm run validate");
                    assert_eq!(label, "npm run validate");
                }
                other => panic!("expected a native Command target, got {other:?}"),
            }
        }
        other => panic!("expected a configured verify gate, got {other:?}"),
    }
}

/// #454/#758: an explicit `--job` forces one bn job (never the configured command), while an
/// unconfigured repo aggregates every conventional job in build → test → lint order.
#[test]
fn resolve_verify_target_explicit_job_and_fallback_aggregate_conventional_jobs() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let jobs = tmp.path().join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    for job in ["build", "test", "lint"] {
        std::fs::write(jobs.join(format!("{job}.sh")), "#!/bin/sh\nexit 0\n").unwrap();
    }
    // Isolate the shipped global jobs dir so only our temp `<root>/jobs/test.sh` resolves.
    let empty = tmp.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let _jd = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", empty.to_str().unwrap());
    let c = ctx("arena", "feature/x");

    // A repo WITH a configured command, but an explicit `--job test` → the bn `test` job wins.
    let configured = Config {
        personal_root: tmp.path().to_path_buf(),
        work_root: tmp.path().to_path_buf(),
        repos: std::collections::HashMap::from([(
            "arena".to_string(),
            resolver::RepoConfig {
                verify: Some("npm run validate".to_string()),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    match resolve_verify_target(&configured, &c, Some("test")) {
        Ok(Some(gate)) => {
            assert_eq!(gate.label(), "test");
            assert_eq!(gate.targets().len(), 1);
            match &gate.targets()[0] {
                VerifyTarget::Script { label, path } => {
                    assert_eq!(label, "test");
                    assert!(
                        path.ends_with("jobs/test.sh"),
                        "unexpected script: {path:?}"
                    );
                }
                other => panic!("explicit --job should resolve a script, got {other:?}"),
            }
        }
        other => panic!("explicit --job should force the bn test job, got {other:?}"),
    }

    // An UNCONFIGURED repo (no `[repo]` entry, like bn) runs all conventional jobs as one gate.
    let unconfigured = Config {
        personal_root: tmp.path().to_path_buf(),
        work_root: tmp.path().to_path_buf(),
        ..Default::default()
    };
    match resolve_verify_target(&unconfigured, &c, None) {
        Ok(Some(gate)) => {
            assert_eq!(gate.label(), "build+test+lint");
            assert_eq!(
                gate.targets()
                    .iter()
                    .map(VerifyTarget::label)
                    .collect::<Vec<_>>(),
                ["build", "test", "lint"]
            );
        }
        other => panic!("unconfigured repo should aggregate conventional jobs, got {other:?}"),
    }
}

#[test]
fn run_verify_gate_is_ordered_and_fail_fast() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let jobs = tmp.path().join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    let log = tmp.path().join("order.log");
    for (job, code) in [("build", 0), ("test", 7), ("lint", 0)] {
        std::fs::write(
            jobs.join(format!("{job}.sh")),
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{job}' >> '{}'\nexit {code}\n",
                log.display()
            ),
        )
        .unwrap();
    }
    let empty = tmp.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let _jd = bn_core::test_support::EnvGuard::set("BN_JOBS_DIR", empty.to_str().unwrap());
    let cfg = Config {
        personal_root: tmp.path().to_path_buf(),
        work_root: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let gate = resolve_verify_target(&cfg, &ctx("arena", "feature/x"), None)
        .unwrap()
        .unwrap();

    assert_eq!(run_verify_gate(&gate, tmp.path(), false, None).unwrap(), 7);
    assert_eq!(std::fs::read_to_string(log).unwrap(), "build\ntest\n");
}

/// #505: `changed_files` mirrors the `test` job's rustfmt-on-changed set (committed vs
/// `origin/main`, plus working-tree edits), and `verify_ui::ui_gate_needed` fires *iff* a bn-web
/// SPA file changed — so a UI-touching branch triggers the npm gate while a pure-Rust branch skips
/// it. Builds a real bn-style bare clone with two feature branches to prove both directions.
#[cfg(unix)]
#[test]
fn changed_files_drives_ui_gate_for_bn_web_touch_only() {
    let git = |args: &[&str]| assert!(crate::git::git_ok(args), "git {args:?} failed");

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Upstream `main` with a UI file, a Rust file, and a doc — all committed as the base.
    let up = root.join("upstream");
    let up_s = up.to_string_lossy().to_string();
    std::fs::create_dir_all(up.join("crates/bn-web/ui/src")).unwrap();
    std::fs::create_dir_all(up.join("crates/bn-cli/src")).unwrap();
    git(&["-C", &up_s, "init", "-q", "-b", "main"]);
    git(&["-C", &up_s, "config", "user.email", "t@example.com"]);
    git(&["-C", &up_s, "config", "user.name", "t"]);
    std::fs::write(
        up.join("crates/bn-web/ui/src/App.tsx"),
        "export const A = 1;\n",
    )
    .unwrap();
    std::fs::write(up.join("crates/bn-cli/src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(up.join("README.md"), "hi\n").unwrap();
    git(&["-C", &up_s, "add", "-A"]);
    git(&["-C", &up_s, "commit", "-qm", "c1"]);

    // bn-style bare clone with the standard remote-tracking refspec, then fetch so `origin/main`
    // resolves inside the linked worktrees below.
    let bare = root.join("repo.git");
    let bare_s = bare.to_string_lossy().to_string();
    git(&["clone", "--bare", "-q", &up_s, &bare_s]);
    git(&[
        "--git-dir",
        &bare_s,
        "config",
        "remote.origin.fetch",
        "+refs/heads/*:refs/remotes/origin/*",
    ]);
    git(&["--git-dir", &bare_s, "fetch", "-q", "origin"]);

    // Branch A — a bn-web SPA change → the UI gate MUST fire.
    let wt_ui = root.join("wt-ui");
    let wt_ui_s = wt_ui.to_string_lossy().to_string();
    git(&[
        "--git-dir",
        &bare_s,
        "worktree",
        "add",
        "-q",
        "-b",
        "feat/ui",
        &wt_ui_s,
        "origin/main",
    ]);
    std::fs::write(
        wt_ui.join("crates/bn-web/ui/src/App.tsx"),
        "export const A = 2;\n",
    )
    .unwrap();
    git(&["-C", &wt_ui_s, "commit", "-qam", "ui change"]);
    let changed = changed_files(&wt_ui);
    assert!(
        changed.iter().any(|f| f == "crates/bn-web/ui/src/App.tsx"),
        "changed_files missed the committed UI edit: {changed:?}"
    );
    assert!(
        bn_core::verify_ui::ui_gate_needed(&changed),
        "UI gate should fire for a bn-web/ui change: {changed:?}"
    );

    // Branch B — a pure-Rust change → the UI gate MUST skip (no npm cost).
    let wt_rs = root.join("wt-rs");
    let wt_rs_s = wt_rs.to_string_lossy().to_string();
    git(&[
        "--git-dir",
        &bare_s,
        "worktree",
        "add",
        "-q",
        "-b",
        "feat/rust",
        &wt_rs_s,
        "origin/main",
    ]);
    std::fs::write(
        wt_rs.join("crates/bn-cli/src/main.rs"),
        "fn main() { let _ = 1; }\n",
    )
    .unwrap();
    git(&["-C", &wt_rs_s, "commit", "-qam", "rust change"]);
    let changed_rs = changed_files(&wt_rs);
    assert!(
        changed_rs.iter().any(|f| f == "crates/bn-cli/src/main.rs"),
        "precondition: the Rust edit should be detected: {changed_rs:?}"
    );
    assert!(
        !bn_core::verify_ui::ui_gate_needed(&changed_rs),
        "UI gate must NOT fire for a pure-Rust change: {changed_rs:?}"
    );
}

#[test]
fn agent_seed_prompt_includes_goal_and_source() {
    let p = agent_seed_prompt("Fix login", Some("https://x/issues/1"), false, false);
    assert!(p.contains("bn --cat"));
    assert!(p.contains("propose a concrete plan"));
    assert!(p.contains("Goal: Fix login."));
    assert!(p.contains("Source: https://x/issues/1."));
    // No source → no trailing Source clause.
    let p = agent_seed_prompt("Just do it", None, false, false);
    assert!(!p.contains("Source:"));
}

#[test]
fn agent_seed_prompt_yolo_ships_without_plan_wait() {
    let p = agent_seed_prompt("Fix login", Some("https://x/issues/1"), false, true);
    // --yolo auto-proceeds: no plan-first / confirmation gate.
    assert!(!p.contains("Do NOT start implementing"));
    assert!(!p.contains("propose a concrete plan"));
    // Instead it drives the full ship workflow directly.
    assert!(p.contains("implement the change directly"));
    assert!(p.contains("bn verify"));
    assert!(p.contains("open a PR"));
    assert!(p.contains("bn agent done"));
    assert!(p.contains("Goal: Fix login."));
    assert!(p.contains("Source: https://x/issues/1."));
    assert!(p.contains("do not call `ask_user`"));
    assert!(p.contains("Make reasonable implementation decisions autonomously"));

    let interactive = agent_seed_prompt("Fix login", None, false, false);
    let scout = agent_seed_prompt("Investigate login", None, true, false);
    assert!(!interactive.contains("do not call `ask_user`"));
    assert!(!scout.contains("do not call `ask_user`"));
}

#[test]
fn plan_gate_seed_registers_and_pauses_before_pr() {
    let p = plan_gate_seed("bn", "dispatch/349", "sid-9", false, 3, 2);
    // The consensus protocol: submit a plan, launch independent reviewers, poll, and don't ship
    // until low-risk quorum or a required human approval.
    assert!(p.contains("PLAN-REVIEW GATE"));
    assert!(p.contains("bn review submit --repo bn --branch dispatch/349 --session sid-9"));
    assert!(p.contains("--reviewers 3"));
    assert!(p.contains("--quorum 2"));
    assert!(p.contains("fresh independent reviewer contexts"));
    assert!(p.contains("bn review agents <id>"));
    assert!(p.contains("human-required"));
    assert!(p.contains("no-consensus"));
    assert!(p.contains("bn review status"));
    assert!(p.contains("approved"));
    assert!(p.contains("changes-requested"));
    assert!(p.contains("rejected"));
    // It overrides any earlier "open a PR" step and gates PR creation.
    assert!(p.contains("OVERRIDES") && p.contains("gh pr create"));
    // Without --verify, the approved branch doesn't mention the verify gate.
    assert!(!p.contains("bn verify"));
    // With --verify, the post-approval implement step folds in the gate.
    let pv = plan_gate_seed("bn", "dispatch/349", "sid-9", true, 3, 2);
    assert!(pv.contains("bn verify"));
}

#[test]
fn plan_gate_selection_keeps_yolo_sensitive_reviewable() {
    assert!(plan_gate_enabled(false, false, true, false));
    assert!(plan_gate_enabled(true, false, false, false));
    assert!(plan_gate_enabled(false, true, false, false));
    assert!(!plan_gate_enabled(true, true, true, true));
}

#[test]
fn review_vote_and_reviewer_scoped_show_parse() {
    on_big_stack(|| {
        let vote = Cli::try_parse_from([
            "bn",
            "review",
            "vote",
            "r7",
            "--reviewer",
            "rev2",
            "--approve",
            "--confidence",
            "95",
            "--notes",
            "done-when covered",
            "--risk",
            "migration ordering",
        ])
        .expect("agent vote command should parse");
        assert!(matches!(
            vote.command,
            Some(Command::Review(CmdReview::Review {
                action: Some(ReviewCmd::Vote {
                    ref id,
                    ref reviewer,
                    approve: true,
                    reject: false,
                    confidence: Some(95),
                    ref risks,
                    ..
                })
            })) if id == "r7" && reviewer == "rev2" && risks == &["migration ordering"]
        ));

        let show = Cli::try_parse_from(["bn", "review", "show", "r7", "--reviewer", "rev3"])
            .expect("reviewer-scoped show should parse");
        assert!(matches!(
            show.command,
            Some(Command::Review(CmdReview::Review {
                action: Some(ReviewCmd::Show {
                    ref id,
                    reviewer: Some(ref reviewer),
                })
            })) if id == "r7" && reviewer == "rev3"
        ));

        let agents = Cli::try_parse_from([
            "bn",
            "review",
            "agents",
            "r7",
            "--harness",
            "copilot",
            "--model",
            "fast",
        ])
        .expect("agent fan-out command should parse");
        assert!(matches!(
            agents.command,
            Some(Command::Review(CmdReview::Review {
                action: Some(ReviewCmd::Agents {
                    ref id,
                    ref harness,
                    model: Some(ref model),
                    add: None,
                    ref kind,
                })
            })) if id == "r7" && harness == "copilot" && model == "fast" && kind.is_empty()
        ));

        let scaled = Cli::try_parse_from([
            "bn",
            "review",
            "agents",
            "r7",
            "--add",
            "2",
            "--kind",
            "rubber-duck",
            "--kind",
            "code-review",
        ])
        .expect("scale-up command should parse");
        assert!(matches!(
            scaled.command,
            Some(Command::Review(CmdReview::Review {
                action: Some(ReviewCmd::Agents {
                    add: Some(2),
                    ref kind,
                    ..
                })
            })) if kind == &["rubber-duck".to_string(), "code-review".to_string()]
        ));
    });
}

#[test]
fn scout_seed_prompt_investigates_and_writes_to_note() {
    let p = agent_seed_prompt("Why is login flaky", None, true, false);
    assert!(p.contains("SCOUT"));
    assert!(p.contains("do NOT change code") || p.contains("Leave the code untouched"));
    // Findings must land in the branch note (bn's own store), not a new format.
    assert!(p.contains("bn add research") || p.contains("bn --path"));
    assert!(p.contains("Investigate: Why is login flaky."));
    // Ship prompt is different — no "propose a plan / implement" framing.
    assert!(!p.contains("propose a concrete plan"));
}

#[test]
fn ship_seeds_carry_concurrency_discipline_but_scouts_dont() {
    // #610: every ship-lane seed forbids destructive/global git, discourages slow/global shared
    // commands, and reaffirms one-worktree-per-lane. Read-only scouts don't touch git, so skip it.
    let assert_has_rules = |p: &str| {
        assert!(p.contains("CONCURRENCY DISCIPLINE"), "missing header: {p}");
        assert!(p.contains("git stash"));
        assert!(p.contains("git reset --hard"));
        assert!(p.contains("git checkout ."));
        assert!(p.contains("git add -A") || p.contains("git add ."));
        assert!(p.contains("specific files"));
        assert!(p.contains("full `cargo` builds"));
        assert!(p.contains("repo-wide `grep`"));
        assert!(p.contains("one worktree per lane"));
    };
    // Plain ship seed (non-yolo, plan-first) carries the rules.
    assert_has_rules(&agent_seed_prompt("Fix login", None, false, false));
    // Yolo ship seed carries them too.
    assert_has_rules(&agent_seed_prompt("Fix login", None, false, true));
    // Skill-driven ship seed carries them.
    let s = seed_test_skill("plan-work", "burn down the todos");
    assert_has_rules(&skill_seed_prompt(
        "Fix login",
        None,
        false,
        true,
        false,
        &s,
    ));

    // Scouts (read-only) do NOT get the git/concurrency rules.
    assert!(!agent_seed_prompt("Why flaky", None, true, false).contains("CONCURRENCY DISCIPLINE"));
    let inv = seed_test_skill("investigate", "Q: {{question}}");
    assert!(
        !skill_seed_prompt("Why flaky", None, true, false, false, &inv)
            .contains("CONCURRENCY DISCIPLINE")
    );
}

#[test]
fn dispatch_skill_name_defaults_by_task_type() {
    assert_eq!(dispatch_skill_name(None, false), "plan-work");
    assert_eq!(dispatch_skill_name(None, true), "investigate");
    assert_eq!(dispatch_skill_name(Some("debug"), false), "debug");
    assert_eq!(dispatch_skill_name(Some("debug"), true), "debug");
}

/// A minimal parsed skill for seed tests, with the placeholder args dispatch fills.
fn seed_test_skill(name: &str, body: &str) -> bn_core::skills::Skill {
    bn_core::skills::parse_skill(
        &format!(
            "---\nname: {name}\ndescription: d\narguments:\n  - {{ name: question, required: false }}\n  - {{ name: continuous, required: false }}\n---\n{body}"
        ),
        name,
    )
    .unwrap()
}

#[test]
fn skill_seed_prompt_embeds_rendered_workflow_and_reflect_close() {
    let s = seed_test_skill("investigate", "Q: {{question}} loop: {{continuous}}");
    let p = skill_seed_prompt(
        "Why is login flaky",
        Some("https://x/issues/1"),
        true,
        false,
        false,
        &s,
    );
    // Scout framing + the note read.
    assert!(p.contains("SCOUT"));
    assert!(p.contains("bn --cat"));
    assert!(p.contains("Source: https://x/issues/1."));
    // Rendered body (placeholders filled from the dispatch) inside workflow markers.
    assert!(p.contains("--- WORKFLOW: investigate ---"));
    assert!(p.contains("Q: Why is login flaky loop: true"));
    assert!(p.contains("--- END WORKFLOW ---"));
    // MCP → CLI mapping for harnesses without the bn MCP server.
    assert!(p.contains("`bn_cat` → `bn --cat`"));
    // Scout completion stays report-only; the reflect close feeds evolve-skills.
    assert!(p.contains("bn agent done --report-only"));
    assert!(p.contains("bn reflect \"skill:investigate — "));
    // Read-only task: no PR, no plan-approval gate.
    assert!(!p.contains("open a PR"));
    assert!(!p.contains("do NOT start implementing"));
}

#[test]
fn skill_seed_prompt_ship_gates_on_plan_approval_unless_yolo() {
    let s = seed_test_skill("plan-work", "burn down the todos");
    let p = skill_seed_prompt("Fix login", None, false, false, true, &s);
    assert!(p.contains("do NOT start implementing"));
    assert!(p.contains("burn down the todos"));
    // Fresh dispatch has no plan yet — the seed bootstraps one before plan-work runs.
    assert!(p.contains("no plan yet"));
    assert!(p.contains("bn add done-when"));
    // --verify/rebase gating, PR + ingestion + completion reporting, reflect close.
    assert!(p.contains("bn verify"));
    assert!(p.contains("after any rebase"));
    assert!(p.contains("open a PR"));
    assert!(p.contains("bn ingest"));
    assert!(p.contains("bn agent done --summary"));
    assert!(p.contains("bn reflect \"skill:plan-work — "));

    let p = skill_seed_prompt("Fix login", None, false, true, false, &s);
    // --yolo auto-proceeds: no approval gate, but the workflow + reporting stay.
    assert!(!p.contains("do NOT start implementing"));
    assert!(p.contains("proceed autonomously"));
    assert!(p.contains("do not call `ask_user`"));
    assert!(p.contains("open a PR"));

    // The plan bootstrap is plan-work-specific: another ship skill doesn't get it.
    let s = seed_test_skill("debug", "repro first");
    let p = skill_seed_prompt("Fix login", None, false, true, false, &s);
    assert!(!p.contains("no plan yet"));
    assert!(p.contains("repro first"));
    assert!(p.contains("bn reflect \"skill:debug — "));
}

#[test]
fn dispatch_helpers() {
    // todo-id recognition
    assert!(is_dispatch_todo_id("t3"));
    assert!(is_dispatch_todo_id("t12"));
    assert!(!is_dispatch_todo_id("t"));
    assert!(!is_dispatch_todo_id("tx"));
    assert!(!is_dispatch_todo_id("3"));

    // capture's "<title> — <url>" round-trips back to (title, url)
    let (title, url) = split_item_text("Fix login bug — https://github.com/o/r/issues/9");
    assert_eq!(title, "Fix login bug");
    assert_eq!(url.as_deref(), Some("https://github.com/o/r/issues/9"));
    // plain text has no url
    let (title, url) = split_item_text("just some work");
    assert_eq!(title, "just some work");
    assert_eq!(url, None);

    // slug is branch-safe and capped
    assert_eq!(slugify("Fix login bug!"), "fix-login-bug");
    assert_eq!(slugify("  Multiple   spaces  "), "multiple-spaces");
    assert_eq!(slugify("***"), "");

    // Auto-derived dispatch branches preserve the common name and suffix collisions.
    assert_eq!(
        unique_dispatch_branch("dispatch/fix-login", |_| false),
        "dispatch/fix-login"
    );
    assert_eq!(
        unique_dispatch_branch("dispatch/fix-login", |branch| {
            branch == "dispatch/fix-login"
        }),
        "dispatch/fix-login-2"
    );
    assert_eq!(
        unique_dispatch_branch("dispatch/fix-login", |branch| {
            matches!(branch, "dispatch/fix-login" | "dispatch/fix-login-2")
        }),
        "dispatch/fix-login-3"
    );
}

#[test]
fn parse_numstat_sums_and_ignores_binary() {
    let stat = "12\t3\tsrc/a.rs\n0\t5\tsrc/b.rs\n-\t-\timg.png\n";
    // 3 files; insertions 12+0+0; deletions 3+5+0 (binary '-' → 0).
    assert_eq!(parse_numstat(stat), (12, 8, 3));
    // Empty input → all zero.
    assert_eq!(parse_numstat(""), (0, 0, 0));
}

#[test]
fn filters_listening_pids_from_socket_info() {
    let sockets = vec![
        netstat2::SocketInfo {
            protocol_socket_info: netstat2::ProtocolSocketInfo::Tcp(netstat2::TcpSocketInfo {
                local_addr: "127.0.0.1".parse().unwrap(),
                local_port: 8080,
                remote_addr: "0.0.0.0".parse().unwrap(),
                remote_port: 0,
                state: netstat2::TcpState::Listen,
            }),
            associated_pids: vec![42, 7, 42],
            #[cfg(any(target_os = "linux", target_os = "android"))]
            inode: 1,
            #[cfg(any(target_os = "linux", target_os = "android"))]
            uid: 1000,
        },
        netstat2::SocketInfo {
            protocol_socket_info: netstat2::ProtocolSocketInfo::Tcp(netstat2::TcpSocketInfo {
                local_addr: "127.0.0.1".parse().unwrap(),
                local_port: 8080,
                remote_addr: "127.0.0.1".parse().unwrap(),
                remote_port: 1,
                state: netstat2::TcpState::Established,
            }),
            associated_pids: vec![99],
            #[cfg(any(target_os = "linux", target_os = "android"))]
            inode: 2,
            #[cfg(any(target_os = "linux", target_os = "android"))]
            uid: 1000,
        },
    ];
    assert_eq!(listening_pids_from_sockets(sockets, 8080), vec![7, 42]);
}

#[test]
fn unregistered_jobs_is_declared_minus_registered() {
    let declared = vec![
        "wt-cleanup".to_string(),
        "notify".to_string(),
        "standup-push".to_string(),
    ];
    let some: std::collections::HashSet<String> = ["wt-cleanup".to_string(), "notify".to_string()]
        .into_iter()
        .collect();
    assert_eq!(unregistered_jobs(&declared, &some), vec!["standup-push"]);
    // every declared job registered → no drift
    let all: std::collections::HashSet<String> = declared.iter().cloned().collect();
    assert!(unregistered_jobs(&declared, &all).is_empty());
    // nothing registered → all declared drift (the fresh-machine case)
    let none = std::collections::HashSet::new();
    assert_eq!(unregistered_jobs(&declared, &none), declared);
}

#[test]
fn claim_startup_fires_once_per_server_pid() {
    let dir = tempfile::tempdir().expect("tempdir");
    // First call for a PID claims the boot fire; every later call for the SAME PID is a no-op, so
    // re-sourcing tmux.conf within one server never double-fires the startup agents.
    assert!(
        claim_startup(dir.path(), "12345"),
        "first claim for a PID fires"
    );
    assert!(
        !claim_startup(dir.path(), "12345"),
        "same PID does not fire again"
    );
    // A fresh server (new PID after reboot / kill-server) claims again.
    assert!(
        claim_startup(dir.path(), "67890"),
        "a new server PID fires once"
    );
    assert!(!claim_startup(dir.path(), "67890"), "…and only once");
}

#[test]
fn cron_session_repo_scoped_uses_repo_name() {
    // Serialize on bn-core's shared env lock so bn-cli's env-touching tests can't race each
    // other; the guard restores BN_CRON_SESSION on drop (issue #358).
    let _g = bn_core::test_support::env_lock();
    let _cron = bn_core::test_support::EnvGuard::remove("BN_CRON_SESSION");
    let c = ctx("bn", "feat-x");
    assert_eq!(cron_session(Some(&c), JobScope::Repo), "bn");
    assert_eq!(cron_session(Some(&c), JobScope::Branch), "bn");
}

#[test]
fn cron_session_global_uses_dedicated_cron_session() {
    let _g = bn_core::test_support::env_lock();
    let _cron = bn_core::test_support::EnvGuard::remove("BN_CRON_SESSION");
    // Global crons land in the dedicated `cron` session, independent of the keepalive/planning
    // session (`BN_TMUX_SESSION`) — so a firing cron never clutters where you work.
    let tmux = bn_core::test_support::EnvGuard::remove("BN_TMUX_SESSION");
    assert_eq!(cron_session(None, JobScope::Global), "cron");
    tmux.set_value("planning");
    assert_eq!(cron_session(None, JobScope::Global), "cron");
}

#[test]
fn cron_session_pinned_overrides_scope() {
    let _g = bn_core::test_support::env_lock();
    // A global job whose work belongs to one repo pins its window there.
    let _cron = bn_core::test_support::EnvGuard::set("BN_CRON_SESSION", "BigMono");
    assert_eq!(cron_session(None, JobScope::Global), "BigMono");
    let c = ctx("bn", "feat-x");
    assert_eq!(
        cron_session(Some(&c), JobScope::Repo),
        "BigMono",
        "pin wins over repo scope too"
    );
}

#[test]
fn shq_single_quotes_and_escapes() {
    assert_eq!(shq("pr-queue"), "'pr-queue'");
    // embedded single quote → close, escaped quote, reopen
    assert_eq!(shq("a'b"), "'a'\\''b'");
}

#[test]
fn cron_window_dispatch_only_wraps_headless_outer_runs() {
    assert!(should_dispatch_cron_window(true, false, false));
    assert!(!should_dispatch_cron_window(false, false, false));
    assert!(!should_dispatch_cron_window(true, true, false));
    assert!(!should_dispatch_cron_window(true, false, true));
}

#[test]
fn cron_inner_command_carries_repo_context_only_for_scoped_jobs() {
    use bn_core::crons::CronClass;
    let c = ctx("bn", "feat-x");
    assert_eq!(
        cron_inner_command(
            "/tmp/bn",
            "notify",
            Some(&c),
            JobScope::Repo,
            CronClass::Housekeeping
        ),
        "'/tmp/bn' run 'notify' --repo 'bn' --branch 'feat-x'; exec \"${SHELL:-sh}\""
    );
    assert_eq!(
        cron_inner_command(
            "/tmp/bn",
            "notify",
            Some(&c),
            JobScope::Global,
            CronClass::Housekeeping
        ),
        "'/tmp/bn' run 'notify'; exec \"${SHELL:-sh}\""
    );
}

#[test]
fn cron_inner_command_info_class_tees_output_and_auto_hides_when_empty() {
    use bn_core::crons::CronClass;
    let cmd = cron_inner_command(
        "/tmp/bn",
        "meetings",
        None,
        JobScope::Global,
        CronClass::Info,
    );
    // Runs the job, tees to a temp file, then keeps the window open only when it printed output.
    assert!(cmd.contains("'/tmp/bn' run 'meetings' 2>&1 | tee"));
    assert!(
        cmd.contains("grep -q '[^[:space:]]'"),
        "keeps the window only when the run produced non-whitespace output: {cmd}"
    );
    assert!(
        cmd.contains("exec \"${SHELL:-sh}\""),
        "holds the window open on a non-empty gather: {cmd}"
    );
}

#[test]
fn dispatch_fork_ref_prefers_origin_when_local_not_ahead() {
    // Up-to-date local base: fork from origin, no warning.
    let (start, warn) = dispatch_fork_ref("main", true, 0, 0);
    assert_eq!(start, "origin/main");
    assert!(warn.is_none());

    // Local base behind origin (#304's bug): fork from origin AND warn.
    let (start, warn) = dispatch_fork_ref("main", true, 3, 0);
    assert_eq!(start, "origin/main");
    let w = warn.expect("behind should warn");
    assert!(w.contains("3 commit(s) behind"), "warn: {w}");
    assert!(w.contains("origin/main"), "warn names the fork source: {w}");
}

#[test]
fn dispatch_fork_ref_keeps_local_when_ahead_or_offline() {
    // origin/<base> unavailable (offline / local-only base): fall back to local, no warning.
    let (start, warn) = dispatch_fork_ref("main", false, 0, 0);
    assert_eq!(start, "main");
    assert!(warn.is_none());

    // Purely ahead: local base is a clean superset of origin — keep it (don't drop the local
    // commits), no warning.
    let (start, warn) = dispatch_fork_ref("main", true, 0, 2);
    assert_eq!(start, "main");
    assert!(warn.is_none());
}

#[test]
fn dispatch_fork_ref_forks_origin_when_diverged() {
    // #456: a diverged local base (behind AND ahead) — the rarely-updated second repo — must still
    // fork from FRESH origin/main, not the stale local ref, so the PR isn't born conflicting. The
    // advisory notes the fresh source (no "may need a rebase" stale-base fallback).
    let (start, warn) = dispatch_fork_ref("main", true, 2, 1);
    assert_eq!(start, "origin/main");
    let w = warn.expect("divergence should note the fresh fork source");
    assert!(w.contains("diverged"), "warn: {w}");
    assert!(w.contains("2 behind, 1 ahead"), "warn: {w}");
    assert!(
        w.contains("forking from origin/main"),
        "warn must name origin as the fork source, not a stale local base: {w}"
    );
    assert!(
        !w.contains("may need a rebase"),
        "the stale-base rebase warning must be gone: {w}"
    );
}

// #304 regression: dispatch must fork a new branch from the *remote* base, not a stale local
// ref. Build a file:// upstream, clone it bare bn-style, advance the remote, then dispatch and
// assert the new branch's tip is the remote tip — proving the fetch+fork-from-origin path.
#[cfg(unix)]
#[test]
fn create_worktree_forks_new_branch_from_origin_not_stale_local() {
    let git = |args: &[&str]| {
        let ok = crate::git::git_ok(args);
        assert!(ok, "git {args:?} failed");
    };
    let rev = |args: &[&str]| -> String {
        crate::git::git_capture(args, None, true).unwrap_or_else(|| panic!("git {args:?} failed"))
    };

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Upstream repo with an initial commit on `main`.
    let up = root.join("upstream");
    let up_s = up.to_string_lossy().to_string();
    std::fs::create_dir_all(&up).unwrap();
    git(&["-C", &up_s, "init", "-q", "-b", "main"]);
    git(&["-C", &up_s, "config", "user.email", "t@example.com"]);
    git(&["-C", &up_s, "config", "user.name", "t"]);
    std::fs::write(up.join("a.txt"), "1").unwrap();
    git(&["-C", &up_s, "add", "-A"]);
    git(&["-C", &up_s, "commit", "-qm", "c1"]);

    // Clone it bare, bn-style (standard remote-tracking refspec), then fetch.
    let bare = root.join("repo.git");
    let bare_s = bare.to_string_lossy().to_string();
    git(&["clone", "--bare", "-q", &up_s, &bare_s]);
    git(&[
        "--git-dir",
        &bare_s,
        "config",
        "remote.origin.fetch",
        "+refs/heads/*:refs/remotes/origin/*",
    ]);
    git(&["--git-dir", &bare_s, "fetch", "-q", "origin"]);

    // Advance the REMOTE main by one commit; the bare repo's local `refs/heads/main` stays at
    // c1 (this is exactly #304's stale-base condition).
    std::fs::write(up.join("b.txt"), "2").unwrap();
    git(&["-C", &up_s, "add", "-A"]);
    git(&["-C", &up_s, "commit", "-qm", "c2"]);
    let remote_tip = rev(&["-C", &up_s, "rev-parse", "HEAD"]);
    let local_base = rev(&["--git-dir", &bare_s, "rev-parse", "refs/heads/main"]);
    assert_ne!(remote_tip, local_base, "precondition: local base is stale");

    // Dispatch-style: create a new branch off base=main.
    let wt = root.join("wt");
    let created = create_worktree(&bare, &wt, "dispatch/x", "main").unwrap();
    assert!(created, "worktree should be created");

    // The new branch must start from the remote tip (c2), not the stale local ref (c1).
    let wt_s = wt.to_string_lossy().to_string();
    let wt_head = rev(&["-C", &wt_s, "rev-parse", "HEAD"]);
    assert_eq!(
        wt_head, remote_tip,
        "dispatch branch forked from stale local main, not origin/main (#304)"
    );
}

// #456 acceptance: a *diverged* local base (the rarely-touched second repo whose local `main` has
// both drifted behind origin AND picked up a local-only commit) must still fork a dispatch branch
// from FRESH origin/main — never the stale/diverged local ref — so the branch isn't born missing
// merged sibling work (a CONFLICTING PR). Builds the diverged condition on a bn-style bare clone,
// then dispatches and asserts the new branch's tip is origin's tip.
#[cfg(unix)]
#[test]
fn create_worktree_forks_from_origin_when_local_base_diverged() {
    let git = |args: &[&str]| {
        let ok = crate::git::git_ok(args);
        assert!(ok, "git {args:?} failed");
    };
    let rev = |args: &[&str]| -> String {
        crate::git::git_capture(args, None, true).unwrap_or_else(|| panic!("git {args:?} failed"))
    };

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Upstream repo with an initial commit on `main`.
    let up = root.join("upstream");
    let up_s = up.to_string_lossy().to_string();
    std::fs::create_dir_all(&up).unwrap();
    git(&["-C", &up_s, "init", "-q", "-b", "main"]);
    git(&["-C", &up_s, "config", "user.email", "t@example.com"]);
    git(&["-C", &up_s, "config", "user.name", "t"]);
    std::fs::write(up.join("a.txt"), "1").unwrap();
    git(&["-C", &up_s, "add", "-A"]);
    git(&["-C", &up_s, "commit", "-qm", "c1"]);

    // Clone it bare, bn-style, and fetch so refs/heads/main == origin/main == c1.
    let bare = root.join("repo.git");
    let bare_s = bare.to_string_lossy().to_string();
    git(&["clone", "--bare", "-q", &up_s, &bare_s]);
    git(&[
        "--git-dir",
        &bare_s,
        "config",
        "remote.origin.fetch",
        "+refs/heads/*:refs/remotes/origin/*",
    ]);
    git(&["--git-dir", &bare_s, "fetch", "-q", "origin"]);

    // Add a LOCAL-only commit on the bare repo's `refs/heads/main` (drift not on origin) → local
    // is "ahead". Then advance the REMOTE main separately → local is also "behind". That's diverged.
    let local_wt = root.join("local_wt");
    let local_wt_s = local_wt.to_string_lossy().to_string();
    git(&[
        "--git-dir",
        &bare_s,
        "worktree",
        "add",
        "-q",
        &local_wt_s,
        "main",
    ]);
    std::fs::write(local_wt.join("local.txt"), "local").unwrap();
    git(&["-C", &local_wt_s, "add", "-A"]);
    git(&["-C", &local_wt_s, "commit", "-qm", "local-only"]);
    // Drop the temp worktree; refs/heads/main keeps the local-only commit.
    git(&[
        "--git-dir",
        &bare_s,
        "worktree",
        "remove",
        "--force",
        &local_wt_s,
    ]);

    std::fs::write(up.join("b.txt"), "2").unwrap();
    git(&["-C", &up_s, "add", "-A"]);
    git(&["-C", &up_s, "commit", "-qm", "c2"]);
    let remote_tip = rev(&["-C", &up_s, "rev-parse", "HEAD"]);
    let local_base = rev(&["--git-dir", &bare_s, "rev-parse", "refs/heads/main"]);
    assert_ne!(
        remote_tip, local_base,
        "precondition: local base diverged from origin"
    );

    // Dispatch-style: create a new branch off base=main.
    let wt = root.join("wt");
    let created = create_worktree(&bare, &wt, "dispatch/y", "main").unwrap();
    assert!(created, "worktree should be created");

    // Even diverged, the new branch must start from the remote tip (c2), not the local ref.
    let wt_s = wt.to_string_lossy().to_string();
    let wt_head = rev(&["-C", &wt_s, "rev-parse", "HEAD"]);
    assert_eq!(
        wt_head, remote_tip,
        "diverged local base forked instead of fresh origin/main (#456)"
    );
}

#[cfg(unix)]
#[test]
fn create_worktree_checks_out_an_existing_branch_as_is() {
    let git = |args: &[&str]| assert!(crate::git::git_ok(args), "git {args:?} failed");
    let rev = |args: &[&str]| -> String {
        crate::git::git_capture(args, None, true).unwrap_or_else(|| panic!("git {args:?} failed"))
    };

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let upstream = root.join("upstream");
    let upstream_s = upstream.to_string_lossy().to_string();
    std::fs::create_dir_all(&upstream).unwrap();
    git(&["-C", &upstream_s, "init", "-q", "-b", "main"]);
    git(&["-C", &upstream_s, "config", "user.email", "t@example.com"]);
    git(&["-C", &upstream_s, "config", "user.name", "t"]);
    std::fs::write(upstream.join("base.txt"), "base\n").unwrap();
    git(&["-C", &upstream_s, "add", "base.txt"]);
    git(&["-C", &upstream_s, "commit", "-qm", "base"]);

    let bare = root.join("repo.git");
    let bare_s = bare.to_string_lossy().to_string();
    git(&["clone", "--bare", "-q", &upstream_s, &bare_s]);
    git(&["--git-dir", &bare_s, "branch", "dispatch/existing", "main"]);
    let expected = rev(&["--git-dir", &bare_s, "rev-parse", "dispatch/existing"]);

    let wt = root.join("wt");
    assert!(create_worktree(&bare, &wt, "dispatch/existing", "other-base").unwrap());
    let wt_s = wt.to_string_lossy().to_string();
    assert_eq!(
        rev(&["-C", &wt_s, "branch", "--show-current"]),
        "dispatch/existing"
    );
    assert_eq!(rev(&["-C", &wt_s, "rev-parse", "HEAD"]), expected);
}

#[cfg(unix)]
#[test]
fn dispatch_base_bootstraps_custom_default_and_fast_forwards_idempotently() {
    let git = |args: &[&str]| assert!(crate::git::git_ok(args), "git {args:?} failed");
    let rev = |args: &[&str]| {
        crate::git::git_capture(args, None, true).unwrap_or_else(|| panic!("git {args:?} failed"))
    };

    let tmp = tempfile::tempdir().unwrap();
    let upstream = tmp.path().join("upstream");
    let upstream_s = upstream.to_string_lossy().to_string();
    std::fs::create_dir_all(&upstream).unwrap();
    git(&["-C", &upstream_s, "init", "-q", "-b", "develop"]);
    git(&["-C", &upstream_s, "config", "user.email", "t@example.com"]);
    git(&["-C", &upstream_s, "config", "user.name", "t"]);
    std::fs::write(upstream.join("base.txt"), "base").unwrap();
    git(&["-C", &upstream_s, "add", "base.txt"]);
    git(&["-C", &upstream_s, "commit", "-qm", "base"]);

    let bare = tmp.path().join("bare/repo.git");
    let bare_s = bare.to_string_lossy().to_string();
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    git(&["clone", "--bare", "-q", &upstream_s, &bare_s]);
    git(&[
        "--git-dir",
        &bare_s,
        "config",
        "remote.origin.fetch",
        "+refs/heads/*:refs/remotes/origin/*",
    ]);

    let cfg = Config {
        worktree_dir: tmp.path().join("worktrees"),
        ..Default::default()
    };
    let first = ensure_dispatch_base(&cfg, "repo", &bare, None).unwrap();
    assert_eq!(first.branch, "develop");
    assert!(first.bootstrapped);
    assert_eq!(
        dispatch_base_bootstrap_message("repo", &first).as_deref(),
        Some("bn dispatch: bootstrapped develop worktree for repo from origin/develop")
    );
    let base = cfg.worktree_dir.join("repo/develop");
    let base_s = base.to_string_lossy().to_string();
    assert_eq!(rev(&["-C", &base_s, "branch", "--show-current"]), "develop");
    assert_eq!(
        rev(&["-C", &base_s, "rev-parse", "HEAD"]),
        rev(&["-C", &upstream_s, "rev-parse", "HEAD"])
    );

    std::fs::write(upstream.join("next.txt"), "next").unwrap();
    git(&["-C", &upstream_s, "add", "next.txt"]);
    git(&["-C", &upstream_s, "commit", "-qm", "next"]);
    let remote_tip = rev(&["-C", &upstream_s, "rev-parse", "HEAD"]);

    let second = ensure_dispatch_base(&cfg, "repo", &bare, None).unwrap();
    assert_eq!(second.branch, "develop");
    assert!(!second.bootstrapped);
    assert!(dispatch_base_bootstrap_message("repo", &second).is_none());
    assert_eq!(rev(&["-C", &base_s, "rev-parse", "HEAD"]), remote_tip);
}

#[test]
fn dispatch_base_reports_missing_and_invalid_bare_repositories() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = Config {
        worktree_dir: tmp.path().join("worktrees"),
        ..Default::default()
    };
    let missing = tmp.path().join("missing.git");
    let error = ensure_dispatch_base(&cfg, "repo", &missing, None).unwrap_err();
    assert!(error.contains("bare repo is missing"), "error: {error}");
    assert!(error.contains("bn wt clone"), "error: {error}");

    let invalid = tmp.path().join("invalid.git");
    std::fs::create_dir_all(&invalid).unwrap();
    let error = ensure_dispatch_base(&cfg, "repo", &invalid, None).unwrap_err();
    assert!(error.contains("bare repo validation"), "error: {error}");
    assert!(error.contains("invalid.git"), "error: {error}");
}

#[cfg(unix)]
#[test]
fn post_create_hook_receives_dispatch_environment_and_failure_is_non_fatal() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp
        .path()
        .join("worktrees")
        .join("bn")
        .join("dispatch-hook");
    let hook_dir = wt.join(".bn/hooks");
    std::fs::create_dir_all(&hook_dir).unwrap();
    let hook = hook_dir.join("post-create");
    let marker = wt.join("hook-env.txt");
    std::fs::write(
        &hook,
        "#!/bin/sh\nprintf '%s\\n%s\\n%s\\n%s\\n%s\\n' \"$PWD\" \"$BN_WORKTREE_PATH\" \
         \"$BN_BRANCH\" \"$BN_REPO\" \"$BN_MAIN_PATH\" > hook-env.txt\nexit 17\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&hook, permissions).unwrap();

    let cfg = Config {
        worktree_dir: tmp.path().join("worktrees"),
        ..Default::default()
    };
    run_post_create_hook(&cfg, &wt, "bn", "dispatch-hook", "develop");

    let lines: Vec<String> = std::fs::read_to_string(marker)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        lines,
        [
            wt.display().to_string(),
            wt.display().to_string(),
            "dispatch-hook".to_string(),
            "bn".to_string(),
            cfg.worktree_dir.join("bn/develop").display().to_string(),
        ]
    );
}
