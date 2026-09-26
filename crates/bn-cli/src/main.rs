//! bn CLI — clap dispatch → bn-core → render (text or JSON envelope).

pub(crate) use bn_core::commands::{self, AddOutcome, CmdError, DoneOutcome};
pub(crate) use bn_core::payload;
pub(crate) use bn_core::render::Envelope;
pub(crate) use bn_core::resolver::{self, Config, Resolved};
pub(crate) use bn_core::rollups;
pub(crate) use bn_core::style;
pub(crate) use bn_core::{capture, migrate};
pub(crate) use clap::{CommandFactory, Parser, Subcommand};
pub(crate) use clap_complete::Shell;

/// `println!` for a line built with the `wt` palette idiom (`{RED}…{NC}`), flattened when stdout
/// is not painting.
///
/// The `wt` commands wrap a whole message in one colour run rather than calling a helper per
/// fragment, and rewriting 180-odd of those into `style::danger(&format!(…))` would both churn the
/// diff and push `wt/merge.rs` past the file-size ratchet. Gating at the sink keeps the readable
/// shape and still honours NO_COLOR: format with escapes, then strip them when nobody can see them.
macro_rules! cprintln {
    ($($arg:tt)*) => {
        println!("{}", $crate::style::strip_unless_enabled(&format!($($arg)*)))
    };
}

/// [`cprintln`] for stderr, which gets its own TTY answer — see [`bn_core::style::enabled_stderr`].
macro_rules! ceprintln {
    ($($arg:tt)*) => {
        eprintln!("{}", $crate::style::strip_unless_enabled_err(&format!($($arg)*)))
    };
}

mod agent_lease;
mod agent_live;
mod cli;
#[rustfmt::skip]
mod cmd;
mod connect;
mod cron_gen;
mod edit;
mod event_loop;
mod gh;
mod git;
mod harness_dispatch;
mod hooks;
mod jump;
mod launchd;
mod main_slot;
mod mcp;
mod mobile;
mod origin_fetch;
mod picker;
mod proc;
mod reap;
mod retry;
mod review_pr;
mod serve;
mod systemd;
mod tat;
mod timers;
mod toast;
mod tui;
mod version;
mod winsched;
#[rustfmt::skip]
mod wt;
mod wt_pool;
mod wt_pool_refresh;

// The clap command surface (the `Command` enum + every `*Cmd` subcommand enum) lives in `cli`;
// glob-re-export it so `main()` and the `Cli` parser struct see the variants by bare name.
pub(crate) use cli::*;

// Handler families moved out of this file live under `cmd/` (issue #333). Glob-re-export each so
// the crate-root namespace and crate-root test modules keep seeing the handlers by their bare
// names exactly as before.
pub(crate) use cmd::agent::*;
pub(crate) use cmd::agent_resume::*;
pub(crate) use cmd::agents::*;
pub(crate) use cmd::attn::*;
pub(crate) use cmd::build::*;
pub(crate) use cmd::control::*;
pub(crate) use cmd::core_cmds::*;
pub(crate) use cmd::cost::*;
pub(crate) use cmd::critic::*;
pub(crate) use cmd::cron::*;
pub(crate) use cmd::daily::*;
pub(crate) use cmd::dispatch::*;
pub(crate) use cmd::doctor::*;
pub(crate) use cmd::ensure_tmux::*;
pub(crate) use cmd::findings::*;
pub(crate) use cmd::focus::*;
pub(crate) use cmd::gitio::*;
pub(crate) use cmd::insights::*;
pub(crate) use cmd::issue::*;
pub(crate) use cmd::job_registry::*;
pub(crate) use cmd::jobs::*;
pub(crate) use cmd::killport::*;
pub(crate) use cmd::learnings::*;
pub(crate) use cmd::lifecycle::*;
pub(crate) use cmd::links::*;
pub(crate) use cmd::plan_review::*;
pub(crate) use cmd::pr_link::*;
pub(crate) use cmd::promote::*;
pub(crate) use cmd::q_inbox::*;
pub(crate) use cmd::recall::*;
pub(crate) use cmd::references::*;
pub(crate) use cmd::release::*;
pub(crate) use cmd::review_gate::*;
pub(crate) use cmd::reviews::*;
pub(crate) use cmd::routes::{route_command, route_default};
pub(crate) use cmd::sessions::*;
pub(crate) use cmd::setup::*;
pub(crate) use cmd::skills::*;
pub(crate) use cmd::sync::*;
pub(crate) use cmd::task::*;
pub(crate) use cmd::task_view::*;
pub(crate) use cmd::tmux_reconcile::emit_tmux_reconcile;
pub(crate) use cmd::tmux_sessions::*;
pub(crate) use cmd::tmux_status::*;
pub(crate) use cmd::tools::*;
pub(crate) use cmd::track::*;
pub(crate) use cmd::verify::*;
pub(crate) use cmd::watch::*;
pub(crate) use cmd::worker::*;

/// Today's date (`YYYY-MM-DD`) via the core [`Clock`] seam. The CLI is the entry-point boundary
/// that produces "now" and threads it into core, so it sources it through a [`RealClock`] — the
/// same value as before, now behind the injectable seam.
fn today() -> String {
    use bn_core::clock::Clock;
    bn_core::clock::RealClock.today()
}

/// Stamp provenance onto a `bn learned`/`bn reflect` entry: the repo/branch of the cwd worktree
/// (if any), the branch note's issue reference, and the harness driving bn. Recorded at write
/// time so `bn learnings` can attribute a fleet's learnings back to the dispatch that produced
/// them. Outside a worktree only the harness (if any) is stamped — the daily/global axis.
fn log_provenance(cfg: &Config) -> bn_core::daily::LogProvenance {
    let harness = bn_core::util::detect_harness();
    let cwd = std::env::current_dir().unwrap_or_default();
    match resolver::resolve_hint(cfg, &cwd) {
        Some(ctx) => {
            let issue = bn_core::store::read(&cfg.note_file(&ctx))
                .ok()
                .and_then(|n| bn_core::learnings::note_issue(&n));
            bn_core::daily::LogProvenance {
                repo: Some(ctx.repo),
                branch: Some(ctx.branch),
                issue,
                harness,
            }
        }
        None => bn_core::daily::LogProvenance {
            harness,
            ..Default::default()
        },
    }
}

/// Branch notes — Rust rewrite (Phase 1 notes core).
#[derive(Parser, Debug)]
#[command(
    name = "bn",
    version = version::VERSION,
    disable_help_subcommand = true
)]
struct Cli {
    /// Print the note directory path for the current branch.
    #[arg(long = "path")]
    path: bool,

    /// Print the resolved state directory (event log + caches).
    #[arg(long = "state-dir")]
    state_dir: bool,

    /// Print the current branch note (action view).
    #[arg(long = "cat")]
    cat: bool,

    /// Open the note in $EDITOR (creating it if missing).
    #[arg(long = "edit")]
    edit: bool,

    /// Emit a machine-readable JSON envelope instead of human text.
    #[arg(long = "json", global = true)]
    json: bool,

    /// Write a structured action log (verbose/debug mode) of every high-value action bn performs
    /// to a rotating file under the state dir. Also enabled by `BN_VERBOSE`/`BN_LOG` or `[log]`
    /// in config.toml. `--debug` is an alias.
    #[arg(long = "verbose", visible_alias = "debug", global = true)]
    verbose: bool,

    /// Enable @eduuh's personal experimental commands (blog/learn-blog/tool) in the bash layer.
    #[arg(long = "eduuhexperiment", global = true, hide = true)]
    eduuhexperiment: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

// Bare `bn cron` (no subcommand) emits the unified dashboard: see `emit_cron_dashboard`.

/// The one line a migrated machine sees: where its settings went, and where the full account is.
fn migration_notice(report: &bn_core::config_migrate::Report) -> String {
    let mut into = Vec::new();
    if let Some(shared) = &report.shared {
        into.push(format!("{} (every machine)", shared.display()));
    }
    if let Some(machine) = &report.machine {
        into.push(format!("{} (this machine)", machine.display()));
    }
    let into = if into.is_empty() {
        "nowhere, because nothing in it is still read".to_string()
    } else {
        into.join(" and ")
    };
    format!(
        "bn: moved your config into {into}. The old file is kept at {}; {} lists what moved.",
        report.backup.display(),
        bn_core::resolver::config_dir()
            .join("config-migration.txt")
            .display()
    )
}

fn main() {
    reset_sigpipe();
    let cli = Cli::parse();
    // The first bn after the upgrade moves an old `config.toml` into the notes tree, before
    // anything reads config. Every later run costs one failed stat.
    if let Some(report) = bn_core::config_migrate::run() {
        eprintln!("{}", migration_notice(&report));
    }
    let cfg = Config::load();

    // fzf re-execs `bn` for every picker callback — one process per cursor move for `--preview`,
    // one per `reload()` bind. Machine housekeeping (reconciling managed services, firing cron
    // catch-up) is never the right thing to do on a keystroke, and
    // any one of those growing a subprocess again would land straight back on the interactive
    // path. Gate the whole block on the callback so it cannot.
    let internal = is_internal_callback(&cli.command);

    if !internal {
        serve::reconcile(&cfg);
    }

    // Verbose/debug action log (opt-in): CLI `--verbose/--debug` OR env (`BN_VERBOSE`/`BN_LOG`) OR
    // `[log]` config.toml. Initialize once, before any action runs, so the runner seam and command
    // dispatch below are captured. A strict no-op when none of the three knobs is set.
    init_action_log(&cfg, cli.verbose);

    // First-bn-event catch-up: the day's first bn invocation kicks off jobs marked `catch_up` in
    // the manifest (meetings, standup) whenever you actually start work — the answer to a variable
    // start time a fixed timer can't match. Skipped for `bn run` itself (the spawned children are
    // `bn run`), so it never recurses.
    if !internal && !matches!(&cli.command, Some(Command::Staff(CmdStaff::Run { .. }))) {
        maybe_fire_catchup(&cfg);
        // Timers follow the declared crons without anyone running `bn cron register`: a cheap,
        // rate-limited check that registers in the background when the manifest moved. Never from
        // `bn cron register` itself, which records what it registered.
        if !matches!(
            &cli.command,
            Some(Command::Staff(CmdStaff::Cron {
                action: Some(CronCmd::Register { .. }),
                ..
            }))
        ) {
            maybe_reconcile_timers(&cfg);
        }
    }

    if cli.eduuhexperiment {
        // Propagate to the delegated bash layer, which gates blog/learn-blog/tool on it.
        #[allow(clippy::disallowed_methods)]
        // production propagation to a child process, not test env
        std::env::set_var("BN_EDUUHEXPERIMENT", "1");
    }
    if cli.path {
        let ctx = resolve_or_exit(&cfg);
        println!("{}", cfg.note_dir(&ctx).display());
        return;
    }
    if cli.state_dir {
        println!("{}", resolver::state_dir().display());
        return;
    }
    if cli.cat {
        let ctx = resolve_or_exit(&cfg);
        emit_cat(&cfg, &ctx);
        return;
    }
    if cli.edit {
        let ctx = resolve_or_exit(&cfg);
        let dir =
            commands::ensure_note(&cfg, &ctx, &today()).unwrap_or_else(|e| fail(&e.message()));
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
        let status = std::process::Command::new(editor)
            .arg(dir.join("note.md"))
            .status();
        std::process::exit(status.ok().and_then(|s| s.code()).unwrap_or(1));
    }

    match cli.command {
        Some(command) => route_command(&cfg, command, cli.json),
        None => route_default(&cfg, cli.json),
    }
}

/// Is this invocation a machine-driven render rather than something the user typed?
///
/// Two families, same reason. The pickers (`tat`, `jump`, `connect`) are self-referencing: fzf's
/// `--preview`, `reload()` and action binds all re-invoke `bn … --pick-stage <stage>`. And the
/// tmux status line re-invokes some of these commands from `#(...)` on every `status-interval`
/// repaint — at 5s that was roughly 121k processes a day across seven segments, none of them a
/// user action.
///
/// Two of those seven are left on the bar: `tmux-git` and `tmux-status`. `tmux-goal` went when the
/// bar folded to one row. The other three — `attn badge`, `tmux-mcp badge`, `cron running` — moved into `bn tat`'s header (#1349), which calls them in-process. They stay
/// listed below because they are still commands a script or a `#()` can invoke, and the reasoning
/// has not changed for them.
///
/// Those processes exist only to print a fragment and exit, so they skip the once-per-invocation
/// machine housekeeping in [`main`]: reconciling managed services and firing cron catch-up. Doing any of that on a status repaint is never right, and it is the path
/// where an added subprocess would be least visible and most expensive.
fn is_internal_callback(command: &Option<Command>) -> bool {
    matches!(
        command,
        Some(Command::Automation(
            CmdAutomation::Tat {
                pick_stage: Some(_),
                ..
            } | CmdAutomation::Jump {
                pick_stage: Some(_),
                ..
            } | CmdAutomation::Connect {
                pick_stage: Some(_),
                ..
            } | CmdAutomation::Attn {
                action: Some(AttnCmd::Badge),
                ..
            }
        )) | Some(Command::Staff(CmdStaff::Cron {
            action: Some(CronCmd::Running),
            ..
        })) | Some(Command::ShellUi(
            CmdShellUi::TmuxGoal { .. }
                | CmdShellUi::TmuxGit { .. }
                | CmdShellUi::TmuxStatus { .. }
                | CmdShellUi::TmuxMcp {
                    action: TmuxMcpCmd::Badge,
                    ..
                }
        ))
    )
}

/// Initialize the opt-in verbose/debug action log. Enabled when the CLI `--verbose/--debug` flag
/// is set OR the resolved `[log]`/env config turns it on. Records the (redacted) invocation as the
/// session's first action so the log opens with what the operator actually ran. No-op otherwise.
fn init_action_log(cfg: &Config, cli_verbose: bool) {
    let enabled = cli_verbose || cfg.log.enabled;
    let level = bn_core::log::LogLevel::parse(&cfg.log.level).unwrap_or_default();
    let dir = cfg.log.dir.clone().unwrap_or_else(resolver::log_dir);
    bn_core::log::init(enabled, level, dir, cfg.log.retain_days);
    if enabled {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        bn_core::log::record(
            bn_core::log::LogLevel::Info,
            "dispatch",
            "invoke",
            serde_json::json!({ "argv": bn_core::log::redact_args(&argv) }),
            serde_json::Value::Null,
            None,
        );
    }
}

/// Print the success envelope for `tag`/`data` and return `true`, so list/report emitters can
/// guard their JSON short-circuit as `if as_json && json_ok(tag, data) { return; }`.
pub(crate) fn json_ok(tag: &str, data: serde_json::Value) -> bool {
    println!("{}", Envelope::success(tag, data).to_json());
    true
}

// --- bn task: the strong per-repo TODO.txt --------------------------------------------------

// ---- bn q — global capture inbox ----

// --- native job registry ----------------------------------------------------
// Jobs ported off their `jobs/<job>.sh` shims into the binary. Each thin shim was just a `bn`
// subcommand; running it in-process deletes the script, works cross-platform, and avoids a
// re-exec. `run_job` checks `native_job` first and falls back to `<job>.sh` for un-ported jobs,
// so migration is incremental and zero-risk.

// --- agent staff framework (`bn staff` / `bn feed`, issue #256) ---------------------------------

// --- bn night — the nightly fleet planner ---------------------------------------------------

// ---- the Chief of Staff (#257) -------------------------------------------------------------

/// Print an error to stderr and exit 1. Returns `!` so it composes in match arms.
pub(crate) fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}

/// Exit status for a refusal that retrying cannot change.
///
/// `bn dispatch` says no for two different kinds of reason. "No slot free" and "worktree occupied
/// by a live agent" are about the moment, and a later attempt may get a different answer. "This
/// item's branch is already active" is about the request: a merged branch's leftover ref satisfies
/// that test with no agent alive and nothing left to finish, so every retry gets the same refusal.
/// A distinct exit status lets a caller stop retrying at once.
pub(crate) const EXIT_REFUSED: i32 = 3;

/// [`fail_json`], exiting [`EXIT_REFUSED`]: the same envelope, a different verdict on retrying.
pub(crate) fn refuse_json(json: bool, command: &str, code: &str, msg: &str) -> ! {
    if json {
        println!(
            "{}",
            bn_core::render::Envelope::failure(command, code, msg).to_json()
        );
    } else {
        eprintln!("{msg}");
    }
    std::process::exit(EXIT_REFUSED);
}

/// Like [`fail`], but honors `--json`: on `json` it emits the stable `{ok:false,error}` envelope to
/// **stdout** (so a `--json` consumer never gets empty output — #452) before exiting 1; otherwise
/// it prints `msg` to stderr. `command` is the envelope's command tag, `code` a stable error code.
pub(crate) fn fail_json(json: bool, command: &str, code: &str, msg: &str) -> ! {
    if json {
        println!(
            "{}",
            bn_core::render::Envelope::failure(command, code, msg).to_json()
        );
    } else {
        eprintln!("{msg}");
    }
    std::process::exit(1);
}

#[cfg(test)]
mod prompt_command_tests {
    use super::*;
    use clap::CommandFactory;

    /// Every backtick-quoted `bn <sub>` command in the shipped prompts and agent definitions must
    /// name a real first-level clap subcommand. This is the guardrail that catches phantom or
    /// removed commands frozen into instruction text (the class of bug behind `bn dispatch
    /// --host`, `sprint-setup`, and the `bn_next`-style tool names).
    #[test]
    fn shipped_prompts_reference_only_real_subcommands() {
        let cmd = Cli::command();
        let mut names: Vec<String> = Vec::new();
        for sub in cmd.get_subcommands() {
            names.push(sub.get_name().to_string());
            names.extend(sub.get_all_aliases().map(str::to_string));
        }
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        for dir in [root.join("config/prompts"), root.join("config/agents")] {
            for entry in std::fs::read_dir(&dir).expect("config dir listing") {
                let path = entry.expect("dir entry").path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("prompt readable");
                for span in text.split('`').skip(1).step_by(2) {
                    let Some(rest) = span.strip_prefix("bn ") else {
                        continue;
                    };
                    let Some(first) = rest.split_whitespace().next() else {
                        continue;
                    };
                    // Flags (`bn --cat`) and placeholders (`bn <command>`) aren't subcommands.
                    if first.starts_with('-') || first.starts_with('<') || first.starts_with('{') {
                        continue;
                    }
                    assert!(
                        names.iter().any(|n| n == first),
                        "{}: `bn {first} …` names an unknown subcommand",
                        path.display()
                    );
                }
            }
        }
    }

    /// Every `run:` and `gather:` line in an agent manifest must parse as a real bn invocation.
    ///
    /// These are the lines that actually *execute*: `run:` is what a scheduled agent does, and
    /// `gather:` is what fills its packet. The backtick check above never sees them — they live in
    /// frontmatter, not prose — so before this a manifest could name a command that had been
    /// renamed away and nothing would notice until the cron failed at 05:30. That is exactly the
    /// bug class the backtick check exists to prevent, on the half of the file that matters more.
    ///
    /// Parsing rather than running: a gather touches the network and the stores, and this must
    /// stay a unit test. `try_get_matches_from` proves the subcommand and every flag exist, which
    /// is what actually rots.
    #[test]
    fn agent_run_and_gather_lines_parse_as_real_commands() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/agents");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).expect("agents dir listing") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("agent readable");
            for line in text.lines() {
                let Some(rest) = line
                    .strip_prefix("run:")
                    .or_else(|| line.strip_prefix("gather:"))
                else {
                    continue;
                };
                let command = rest.trim().trim_matches(['"', '\''].as_slice());
                let Some(args) = command.strip_prefix("bn ") else {
                    // A manifest may shell out to something that is not bn (a user script); this
                    // test only speaks for bn's own surface.
                    continue;
                };
                let argv = std::iter::once("bn").chain(args.split_whitespace());
                let parsed = Cli::command().try_get_matches_from(argv);
                assert!(
                    parsed.is_ok(),
                    "{}: `{command}` does not parse — {}",
                    path.display(),
                    parsed.unwrap_err().kind()
                );
                checked += 1;
            }
        }
        // A refactor that renamed the frontmatter keys would otherwise leave this test passing
        // vacuously, which is worse than not having it. The floor was 10 while the staff roster
        // shipped a dozen agents; retiring that roster left 4, so it tracks the smaller catalog —
        // the guard is "did we check anything at all", not a target headcount.
        assert!(
            checked >= 3,
            "expected the shipped roster to declare run:/gather: lines, found {checked}"
        );
    }
}
