//! Automation and orchestration commands: everything that drives agents rather than notes.
//!
//! Split out of [`crate::cli`] so this group can gain subcommands without pushing `cli.rs` further
//! past the 1000-line ratchet (see `.file-size-baseline`).
use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum CmdAutomation {
    /// The Forge app as bn's task store (`[tasks] provider = "forge"`). `bn forge login` mints the
    /// agent member's key through your signed-in browser; `bn forge status` checks it.
    Forge {
        #[command(subcommand)]
        action: crate::cli::ForgeAppCmd,
    },
    /// What each agent harness serves — the model ids and reasoning-effort levels it accepts.
    /// Discovered from the harness itself (Copilot enumerates them in its shell completion) and
    /// cached for a day, so a pipeline stage is never configured with a model that will be silently
    /// dropped at launch. Falls back to the aliases bn validates when a harness offers no list.
    Harness {
        #[command(subcommand)]
        action: HarnessCmd,
    },
    /// Jump to whatever needs you (prefix `j`): an fzf popup stacking live tmux windows that need
    /// you (waiting / done / blocker badges) over the cross-repo `bn next` queue. ↵ switches to the
    /// window or opens the link; ctrl-r reloads. Plain list when fzf is absent.
    Jump {
        /// Fast-path (prefix Tab): jump straight to the top-ranked window, no picker.
        #[arg(long)]
        next: bool,
        /// Switch scope (prefix `` ` ``): an fzf picker over **every** window of the session you
        /// are in — quiet worktrees included — rather than only what needs you. Rows carry the tab
        /// bar's badges (blockers · todos · agent state) plus a live pane preview, ordered
        /// last-used first so a bare ↵ toggles back to the window you came from. The
        /// Session-scoped window picker (every session).
        #[arg(long)]
        session: bool,
        /// Internal: the fzf callback stage a `jump` session re-invokes (list/preview).
        #[arg(long, hide = true, value_enum)]
        pick_stage: Option<jump::JumpStage>,
        /// Internal: the focused row's hidden columns, passed to a `--pick-stage` callback.
        #[arg(hide = true, allow_hyphen_values = true)]
        pick_fields: Vec<String>,
    },
    /// Project + worktree picker (prefix `C-o`): an interactive popup over your repos (bare + flat
    /// clones) and live tmux sessions, frecency-sorted, with a git-status preview. ↵ opens the
    /// project (a bare repo opens a window/worktree sub-picker); type `repo/branch` to create that
    /// worktree, or paste a git URL to clone it; ctrl-x kills a session. `BN_PICKER=native`
    /// switches to the crossterm/nucleo frontend; otherwise fzf stays the default. Plain list when
    /// no interactive frontend is available.
    Tat {
        /// Open this project/worktree directly (`<name>`, `<repo>/<branch>`, or a git URL). Omit
        /// for the interactive picker. Also carries the hidden `ref` for `--pick-stage`.
        #[arg(allow_hyphen_values = true)]
        name: Option<String>,
        /// Scope the picker to an alternate root. `--root win` targets the Windows-filesystem repos
        /// (the `tatw` replacement); omit to span the primary root plus the auto-loaded `win` root.
        #[arg(long)]
        root: Option<String>,
        /// Internal: the fzf callback stage a `tat` session re-invokes (list/preview/kill).
        #[arg(long, hide = true, value_enum)]
        pick_stage: Option<tat::TatStage>,
        /// Internal: an extra `--pick-stage` payload (the focused row's `kind~payload`), used by the
        /// `rmworktree` action bind.
        #[arg(long, hide = true)]
        pick_arg: Option<String>,
    },
    /// Remote connect (the cross-machine `tat`): an fzf picker over your `~/.ssh/config` hosts
    /// (enriched by config.toml `[hosts]`), preview shows the remote's live `bn summary`. ↵ or
    /// `bn connect <alias>` opens `ssh -t` into `tmux new-session -A -s bn` on the target, so a
    /// dropped pipe re-attaches instead of killing work. Degrades to a plain ssh shell when the
    /// remote has no tmux, and to a numbered list when fzf is absent. `--window` wraps that
    /// connection in a local `host:<alias>` tmux session; `--read-only` observes an existing
    /// remote session without creating one.
    Connect {
        /// Host alias to connect to (as named in ~/.ssh/config). Omit for the picker.
        host: Option<String>,
        /// Open the remote in a dedicated local `host:<alias>` tmux session. Inside tmux, create
        /// or switch to that session; outside tmux, attach to it.
        #[arg(long)]
        window: bool,
        /// Attach to the remote `bn` tmux session read-only. If it does not exist, degrade to the
        /// remote login shell instead of creating it.
        #[arg(long)]
        read_only: bool,
        /// Internal: the fzf callback stage a `connect` session re-invokes (list/preview).
        #[arg(long, hide = true, value_enum)]
        pick_stage: Option<connect::ConnectStage>,
    },
    /// Print a phone-friendly mosh recipe for a known SSH host (#633 P4). The command connects
    /// through the host alias from `~/.ssh/config` and attaches to the persistent remote `bn`
    /// tmux session, so roaming, sleep, and reconnects do not kill the work.
    Mobile {
        /// Host alias to connect to (as named in ~/.ssh/config).
        host: String,
    },
    /// How the tunnel mesh looks from this machine: every host on the Dev Tunnels account, the
    /// route ssh takes to it (LAN or tunnel), whether it answers, and why not. Runs the `tunnels`
    /// job (config/jobs/tunnels.sh); reads only.
    Tunnels,
    /// commits since the note was last touched, `bn done` for todos a commit closed, and (on a
    /// MERGED PR) `bn close` + `wt cleanup`. Interactive confirm by default; nothing is written
    /// without confirm or `--yes`. Silent when already in sync.
    Sync {
        /// Apply every proposal without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// Attention items: hand-, script-, or agent-raised entries on the attention list.
    /// Bare `bn attn` lists the open set; they also fold into `bn doctor`. Resolve with `ack`
    /// (done), `remove` (cancelled), or `plan` (promote to a todo); edit with `update`.
    Attn {
        #[command(subcommand)]
        action: Option<AttnCmd>,
        /// Filter the board to these task types (repeatable or comma-separated): coding |
        /// planning | critique | review. Untyped legacy items count as `coding`.
        #[arg(long = "type", value_delimiter = ',')]
        type_filter: Vec<String>,
    },
    /// Focus mode — a declared deep-work block that suppresses + queues non-urgent surfacing.
    /// `bn focus start 90m` opens a quiet window; while active `bn next` lets only urgent
    /// signals (failing cron, a true blocker) through and queues the rest. Bare `bn focus`
    /// shows the active block (same as `status`); `bn focus end` closes it and drains the queue.
    Focus {
        #[command(subcommand)]
        action: Option<FocusCmd>,
    },
    /// Desktop-notification policy: what toasted today, what was held back, and why. A toast
    /// means work has stopped and needs your decision; everything else stays in `bn next`.
    Notify {
        #[command(subcommand)]
        action: Option<NotifyCmd>,
    },
    /// AI-agent session telemetry. Bare `bn agent` lists this branch's recent Claude/Copilot
    /// sessions; `bn agent ingest` folds new sessions into the metrics log (the poller's call).
    Agent {
        #[command(subcommand)]
        action: Option<AgentCmd>,
    },
    /// Persistent cross-repo registry of controller and worker agent sessions.
    Sessions {
        #[command(subcommand)]
        action: Option<SessionsCmd>,
    },
    /// Resume the latest recorded session for this worktree, or a named session from anywhere.
    ///
    /// `--all` switches to crash recovery: instead of one session, it reconstructs everything that
    /// was running when the machine last went down and offers it back.
    Resume {
        /// Rebuild the work that was running before this machine last went down. Shows each
        /// branch, worktree, agent session and window, and restores the ones you confirm. Reads
        /// only this machine's state — a laptop cannot restore another laptop's tmux layout.
        #[arg(long, conflicts_with = "name_or_id")]
        all: bool,
        /// Print the restore plan and change nothing.
        #[arg(long, requires = "all", conflicts_with = "yes")]
        dry_run: bool,
        /// Restore every listed row without prompting.
        #[arg(long, requires = "all")]
        yes: bool,
        /// Restore only these pool slots (repeatable). A plain `bn resume 2` stays a session query,
        /// so the slot filter needs its own flag.
        #[arg(long = "slot", value_name = "N", requires = "all")]
        slots: Vec<u32>,
        /// Exact task name, full session id, or unambiguous session-id prefix. Omit to select the
        /// latest Copilot or Claude session recorded for the current git worktree.
        name_or_id: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum HarnessCmd {
    /// List the models and effort levels each harness accepts.
    Models {
        /// Only this harness (copilot | claude). Omit for all of them.
        #[arg(long)]
        harness: Option<String>,
    },
}
