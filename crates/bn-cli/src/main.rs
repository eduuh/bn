//! bn CLI — clap dispatch → bn-core → render (text or JSON envelope).

pub(crate) use bn_core::commands::{self, AddOutcome, CmdError, DoneOutcome};
pub(crate) use bn_core::payload;
pub(crate) use bn_core::render::Envelope;
pub(crate) use bn_core::resolver::{self, Config, Resolved};
pub(crate) use bn_core::rollups;
pub(crate) use bn_core::{capture, migrate};
pub(crate) use clap::{CommandFactory, Parser, Subcommand};
pub(crate) use clap_complete::Shell;

mod agent_lease;
mod agent_live;
mod cli;
#[rustfmt::skip]
mod cmd;
mod connect;
mod cron_gen;
mod event_loop;
mod gh;
mod git;
mod hooks;
mod jump;
mod launchd;
mod mcp;
mod mobile;
mod picker;
mod proc;
mod reap;
mod retry;
mod review_pr;
mod serve;
mod staff_pick;
mod systemd;
mod tat;
mod timers;
mod toast;
mod version;
mod winsched;
#[rustfmt::skip]
mod wt;
mod wt_pool;

// The clap command surface (the `Command` enum + every `*Cmd` subcommand enum) lives in `cli`;
// glob-re-export it so `main()` and the `Cli` parser struct see the variants by bare name.
pub(crate) use cli::*;

// Handler families moved out of this file live under `cmd/` (issue #333). Glob-re-export each so
// the crate-root namespace and crate-root test modules keep seeing the handlers by their bare
// names exactly as before.
pub(crate) use cmd::agent::*;
pub(crate) use cmd::agents::*;
pub(crate) use cmd::archivist::*;
pub(crate) use cmd::attn::*;
pub(crate) use cmd::bearings::*;
pub(crate) use cmd::build::*;
pub(crate) use cmd::chief::*;
pub(crate) use cmd::control::*;
pub(crate) use cmd::core_cmds::*;
pub(crate) use cmd::cost::*;
pub(crate) use cmd::critic::*;
pub(crate) use cmd::cron::*;
pub(crate) use cmd::curriculum::*;
pub(crate) use cmd::daily::*;
pub(crate) use cmd::dispatch::*;
pub(crate) use cmd::doctor::*;
pub(crate) use cmd::dojo::*;
pub(crate) use cmd::eye::*;
pub(crate) use cmd::findings::*;
pub(crate) use cmd::focus::*;
pub(crate) use cmd::gitio::*;
pub(crate) use cmd::insights::*;
pub(crate) use cmd::issue::*;
pub(crate) use cmd::job_registry::*;
pub(crate) use cmd::jobs::*;
pub(crate) use cmd::kill_tmux::*;
pub(crate) use cmd::killport::*;
pub(crate) use cmd::learnings::*;
pub(crate) use cmd::lifecycle::*;
pub(crate) use cmd::mentor::*;
pub(crate) use cmd::night::*;
pub(crate) use cmd::orchestrate::*;
pub(crate) use cmd::plan_review::*;
pub(crate) use cmd::pr_link::*;
pub(crate) use cmd::promote::*;
pub(crate) use cmd::q_inbox::*;
pub(crate) use cmd::quarter::*;
pub(crate) use cmd::recall::*;
pub(crate) use cmd::references::*;
pub(crate) use cmd::release::*;
pub(crate) use cmd::remote::*;
pub(crate) use cmd::retro::*;
pub(crate) use cmd::review_gate::*;
pub(crate) use cmd::reviews::*;
pub(crate) use cmd::routes::{route_command, route_default};
pub(crate) use cmd::routine::*;
pub(crate) use cmd::sentinel::*;
pub(crate) use cmd::sessions::*;
pub(crate) use cmd::setup::*;
pub(crate) use cmd::skills::*;
pub(crate) use cmd::staff::*;
pub(crate) use cmd::sync::*;
pub(crate) use cmd::task::*;
pub(crate) use cmd::tmux_status::*;
pub(crate) use cmd::today::*;
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

fn main() {
    reset_sigpipe();
    let cli = Cli::parse();
    let cfg = Config::load();

    // Verbose/debug action log (opt-in): CLI `--verbose/--debug` OR env (`BN_VERBOSE`/`BN_LOG`) OR
    // `[log]` config.toml. Initialize once, before any action runs, so the runner seam and command
    // dispatch below are captured. A strict no-op when none of the three knobs is set.
    init_action_log(&cfg, cli.verbose);

    // First-bn-event catch-up: the day's first bn invocation kicks off jobs marked `catch_up` in
    // the manifest (meetings, standup) whenever you actually start work — the answer to a variable
    // start time a fixed timer can't match. Skipped for `bn run` itself (the spawned children are
    // `bn run`), so it never recurses.
    if !matches!(&cli.command, Some(Command::Staff(CmdStaff::Run { .. }))) {
        maybe_fire_catchup(&cfg);
        // The queue is on by default, so its dispatcher has to be too — a queue nothing drains
        // silently stops the machine. One stat in the common case; see `queue_install`.
        //
        // Skipped for `bn queue` itself: its fallback path can run a tick, and a tick triggered by
        // the command that was about to tick is one tick too many.
        if !matches!(&cli.command, Some(Command::Fleet(CmdFleet::Queue { .. }))) {
            cmd::queue_install::ensure(&cfg);
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
