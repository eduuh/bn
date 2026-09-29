//! `bn agent …` — the clap surface for the agent-supervision commands.
//!
//! Split out of `cli.rs` so the file-size ratchet leaves room for the supervision commands
//! (`prompt`, `answer`, `instruct`) to grow. Re-exported by `cli.rs`, so `AgentCmd` resolves
//! exactly as before.

use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum AgentCmd {
    /// Recent agent sessions for a branch (default — same as bare `bn agent`). Resolves the
    /// current worktree, or pass --repo/--branch to target another.
    Sessions {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
    },
    /// Fold not-yet-seen Claude + Copilot sessions into the metrics log: bn agent ingest.
    /// Idempotent by session id; called by the `agent-observe` job.
    Ingest {
        /// Re-read every session still on disk, replacing its recorded row, instead of only
        /// folding new ones. For when the parser changed rather than the data — rows written
        /// before cache-creation tokens were captured carry neither those nor a per-response
        /// token count. Reads supersede rather than duplicate: every read path folds the latest
        /// snapshot per session id.
        #[arg(long)]
        reingest: bool,
    },
    /// Mine this branch's recorded sessions into proposed note lines — a progress bullet, and
    /// (for code-changing sessions) a decision stub + follow-up todo — for work that never
    /// landed in the note. Propose-only, mirroring `bn sync`: `--json` emits and writes nothing;
    /// otherwise each line is confirmed (or `--yes`-applied). bn agent digest.
    Digest {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        /// Apply every proposed line without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// Live-agent fleet: every *running* Claude session with its health (active / idle / stuck),
    /// flagging any idle past `--stuck` minutes. The supervisor pass behind the FLEET cockpit
    /// panel and `--json`. bn agent supervise.
    Supervise {
        /// Minutes of quiet before an agent is *idle*.
        #[arg(long, default_value_t = bn_core::agent::DEFAULT_IDLE_MINS)]
        idle: i64,
        /// Minutes of quiet before an agent is flagged *stuck*.
        #[arg(long, default_value_t = bn_core::agent::DEFAULT_STUCK_MINS)]
        stuck: i64,
    },
    /// Joined live fleet across BOTH harnesses: every agent seen by its tmux window (@bn_agent
    /// working/waiting/done) and/or the session-log health (active/idle/stuck), one row per
    /// repo:branch. The machine-readable "what are all my agents doing" behind the fleet board;
    /// `bn agent live [--json]`.
    Live {
        /// Minutes of quiet before an agent is *idle*.
        #[arg(long, default_value_t = bn_core::agent::DEFAULT_IDLE_MINS)]
        idle: i64,
        /// Minutes of quiet before an agent is flagged *stuck*.
        #[arg(long, default_value_t = bn_core::agent::DEFAULT_STUCK_MINS)]
        stuck: i64,
    },
    /// Self-schedule a check-in nudge: in `<mins>` minutes, deliver `<note>` to this worktree's
    /// agent pane. A one-shot reminder watchdog, so it survives a reboot and is listed and
    /// stopped with `bn watchdog`. bn agent checkin <mins> "<note>".
    Checkin {
        /// Minutes from now to fire the nudge.
        mins: i64,
        /// The message to deliver at the check-in.
        note: String,
        /// Target tmux window `repo:branch` (default: the current worktree).
        #[arg(long)]
        target: Option<String>,
    },
    /// Recover an interrupted ship turn: a worktree with commits ahead of base but no PR (its agent
    /// died before pushing). Dry-run by default (reports candidates); `--yes` pushes the branch and
    /// opens a PR (`gh pr create --fill`), recording it on the note. Targets the current worktree,
    /// `--repo/--branch`, or `--all` (sweep every worktree). bn agent recover.
    Recover {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        /// Sweep every worktree, not just one target.
        #[arg(long)]
        all: bool,
        /// Base branch the work forked from (for the commits-ahead check). Default: main.
        #[arg(long)]
        base: Option<String>,
        /// Actually push + open the PR (default: dry-run report).
        #[arg(long)]
        yes: bool,
    },
    /// Signal completion back to the orchestrator (the agent runs this when it finishes) — the
    /// event-driven push. Raises a durable, keyed attention item (source `agent`, carrying repo/
    /// branch/PR link) that surfaces in `bn next` / the orchestrator's attention read, sets the
    /// window's `@bn_agent done` badge, and records it in the note. `--notify` also fires a desktop
    /// toast. Run inside the worktree, or target it with --repo/--branch. bn agent done.
    Done {
        /// One-line summary of what was accomplished.
        #[arg(long)]
        summary: Option<String>,
        /// PR URL, if one was opened (becomes the attention item's link).
        #[arg(long)]
        pr: Option<String>,
        /// Fire a desktop toast too (the immediate "wake" nudge).
        #[arg(long)]
        notify: bool,
        /// This finding is report-only — a recommendation or "nothing to fix", NOT a code change to
        /// ship. Marks the note so `bn promote` won't try to ship it (no empty PR). Scouts whose
        /// finding isn't code-shaped should pass this.
        #[arg(long)]
        report_only: bool,
        /// Report terminal worker failure instead of successful completion.
        #[arg(long)]
        failed: bool,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
    },
    /// Steer a running agent with a message and get its reply. Default: the reliable `-p --resume`
    /// channel (harness + session id from the branch note) — works for both harnesses even against a
    /// live session, and returns the agent's answer. `--keys` instead pastes into the live tmux
    /// window and submits with a raw CR, verifying the pane took it (visible there; use when the
    /// agent is idle). `<target>` is `repo:branch`. bn agent send.
    Send {
        /// `repo:branch` of the agent (its note holds the recorded session id).
        target: String,
        /// The message to send.
        msg: String,
        /// Force the visible tmux path (paste + verified raw-CR submit). Kept as an alias for
        /// `--channel keys`.
        #[arg(long)]
        keys: bool,
        /// Which channel to deliver over: `auto` (default — the live pane when the agent has a
        /// window, else a `-p --resume` turn), `keys`, or `resume`.
        ///
        /// `auto` replaced an unconditional `--resume` default that quietly forked a *second*
        /// headless turn: the session on screen never saw the message, so steering a live agent
        /// from the dashboard looked delivered and did nothing.
        #[arg(long, value_name = "CHANNEL", value_parser = ["auto", "keys", "resume"])]
        channel: Option<String>,
        /// Fire-and-forget: run the `-p --resume` turn DETACHED (own process group, survives the
        /// caller exiting) and return immediately, instead of blocking for the whole turn. For long
        /// ship tasks — the agent reports back with `bn agent done` when finished. Output is logged.
        #[arg(long)]
        detach: bool,
    },
    /// Message another LIVE agent, peer to peer — one agent asking another for something, and the
    /// reply is just another `msg` back. Your own `repo:branch` (from the cwd, or `--from`) rides in
    /// the delivered line as `[bn msg from <repo:branch>] …`, so the receiver knows who asked and how
    /// to answer. The text lands in the target's live pane as its next prompt; a target mid-turn
    /// queues it. Live-only: a target with no window is refused (never a headless `--resume` turn) —
    /// `bn agent live` lists who can be reached. bn agent msg <repo:branch> "<text>".
    Msg {
        /// `repo:branch` of the agent to message (the `target` column of `bn agent live`).
        target: String,
        /// What to ask or answer. Keep it to one message; a reply comes back the same way.
        text: String,
        /// Who the message is from, as `repo:branch`. Defaults to the current worktree's.
        #[arg(long, value_name = "REPO:BRANCH")]
        from: Option<String>,
    },
    /// Kill a running agent: its process tree, its tmux window, its agent lease and its warm pool
    /// slot (`repo:branch`, the `target` from `bn agent live`). The hard stop the web dashboard's
    /// kill button and the orchestrator's give-up path call — it genuinely frees the slot, rather
    /// than closing the window and leaving the harness reparented and running. Idempotent: a target
    /// with no window is reported as already gone. A slot holding uncommitted work is left leased
    /// and said so, never reset. bn agent kill <target>.
    Kill {
        /// `repo:branch` of the agent — its tmux window name (slashes already dashed).
        target: String,
    },
    /// Force-kill a repo's WHOLE tmux session — every agent under it at once (session is repo,
    /// window is branch). The last-resort valve for a session that is wedged as a whole; to stop one
    /// stuck agent use `bn agent kill <repo>:<branch>` instead.
    ///
    /// Each window goes down the same path `kill` uses, so leases are released and warm pool slots
    /// are handed back rather than left leased to windows that no longer exist. Worktrees are left
    /// on disk and a slot holding uncommitted changes stays leased — what a force kill discards is
    /// the agents' in-flight context, not their files. Idempotent: a repo with no live session is
    /// reported as already gone. bn agent kill-repo <repo>.
    KillRepo {
        /// The repo whose tmux session to kill — a bare repo name, not `repo:branch`.
        repo: String,
    },
    /// Restart EVERY agent in a repo, each in the worktree it was already in — the repair a wedged
    /// repo usually wants, where `kill-repo` only stops the work.
    ///
    /// Runs one `bn agent relaunch` per live agent, sequentially: respawning a repo's harnesses at
    /// once spikes RAM on a box already struggling enough to need this. The unit is a live fleet
    /// agent, so plain shell windows in the session are left alone. One agent failing does not stop
    /// the rest, and the outcome is reported per target.
    /// bn agent relaunch-repo <repo> [--hard] [--harness claude|copilot].
    RelaunchRepo {
        /// The repo whose agents to restart — a bare repo name, not `repo:branch`.
        repo: String,
        /// Start FRESH sessions seeded from each branch note's handoff instead of resuming the
        /// recorded transcripts. For a repo whose agents' context is the problem.
        #[arg(long)]
        hard: bool,
        /// Relaunch every agent under a different harness. Implies `--hard`.
        #[arg(long)]
        harness: Option<String>,
    },
    /// Kill a running agent and start it again in the SAME worktree — the repair a stuck agent
    /// actually wants, and what the dashboard's Relaunch button now calls.
    ///
    /// Its process tree is reaped (a bare `kill-window` leaves the harness reparented and running),
    /// then the harness is respawned in the checkout git says the branch is really in — never the
    /// conventional `<worktree_dir>/<repo>/<branch>`, which is wrong for every pooled slot. By
    /// default the new session RESUMES the old transcript; `--hard` starts a fresh one seeded from
    /// the branch note's handoff. The warm pool slot is kept (that is what `kill` frees), and a
    /// relaunch that could not happen exits non-zero rather than reporting success.
    /// bn agent relaunch <target> [--hard] [--harness claude|copilot].
    Relaunch {
        /// `repo:branch` of the agent — the `target` from `bn agent live`.
        target: String,
        /// Start a FRESH session seeded from the branch note's handoff instead of resuming the
        /// recorded transcript. For an agent whose context is the problem.
        #[arg(long)]
        hard: bool,
        /// Relaunch under a different harness. Implies `--hard`: a Claude transcript is not
        /// resumable by Copilot. Availability-gated — a disabled harness is an error, not a no-op.
        #[arg(long)]
        harness: Option<String>,
    },
    /// Capture the last N lines of a running agent's tmux pane (read-only). Powers the web
    /// dashboard's live-pane view; `<target>` is `repo:branch` (the `target` from `bn agent live`).
    /// bn agent pane <target> [--lines N].
    Pane {
        /// `repo:branch` of the agent — its tmux window name (slashes already dashed).
        target: String,
        /// How many trailing pane lines to return (default 40).
        #[arg(long, default_value_t = 40)]
        lines: usize,
        /// Include ANSI/SGR escape sequences (`tmux capture-pane -e`) so a terminal renderer (the
        /// web dashboard's live terminal) shows the pane's real colours/attributes instead of
        /// plain text. Off by default — the plain tail stays escape-free.
        #[arg(long)]
        ansi: bool,
    },
    /// Read the dialog a blocked agent is waiting on: the question, the subject under review, and
    /// every numbered option with its real label and what picking it *means* (approve /
    /// approve-session / reject / reject-with-feedback). The read half of supervision — what every
    /// approve button, quick-steer submenu and auto-approve rule is built on, so nothing has to
    /// guess a digit. Exits non-zero when the agent is not blocked on a dialog.
    ///
    /// The `fingerprint` it prints is the `--expect` token for `bn agent answer`: pass it back and
    /// the answer is refused if the pane has moved on. bn agent prompt <target> [--json].
    Prompt {
        /// `repo:branch` of the agent — the `target` from `bn agent live`.
        target: String,
        /// How many trailing pane lines to scan. A permission dialog is a bordered box carrying a
        /// title, the command, the question and its options, so the default is deliberately much
        /// deeper than the fleet board's own 6-line glance.
        #[arg(long, default_value_t = bn_core::agent::PROMPT_SCAN_LINES)]
        lines: usize,
    },
    /// Answer the dialog a blocked agent is waiting on — the verified alternative to firing a digit
    /// at a pane and hoping. Reads the dialog first, resolves the option you asked for against the
    /// options actually offered, sends it, then confirms the dialog is *gone*.
    ///
    /// It refuses rather than guesses: no pending dialog, an `--expect` fingerprint that no longer
    /// matches (the pane moved — approving now would answer a different question), or a semantic
    /// the harness never offered are all non-zero exits that send nothing.
    /// bn agent answer <target> (--option N | --approve | --approve-session | --reject).
    Answer {
        /// `repo:branch` of the agent — the `target` from `bn agent live`.
        target: String,
        /// Pick this exact option number. Verified against the parsed dialog before it is sent.
        #[arg(long, conflicts_with_all = ["approve", "approve_session", "reject"])]
        option: Option<u8>,
        /// Pick the one-shot approval ("Yes"). Falls back to a session-scoped allow if that is the
        /// only approval offered — both grant the action.
        #[arg(long)]
        approve: bool,
        /// Pick the session-scoped allow ("Yes, and don't ask again …").
        #[arg(long)]
        approve_session: bool,
        /// Pick the plain rejection. Never falls back to "No, and tell the agent what to do
        /// differently" — that opens a composer instead of ending the request, which leaves the
        /// lane blocked in a new way.
        #[arg(long)]
        reject: bool,
        /// The `fingerprint` from `bn agent prompt`. The answer is refused if the pane has changed
        /// since. Always pass it from an automated or remote caller.
        #[arg(long)]
        expect: Option<u64>,
    },
    /// Show the auto-approval policy in force and the recent decisions it produced — what bn is
    /// allowed to answer on your behalf, and what it actually answered while you were away.
    ///
    /// With no `approvals.toml`, bn's built-in default applies and nothing else: an orchestrated
    /// `--yolo` ship lane's command approvals are cleared with the session-scoped allow. Write a
    /// policy with `--init`; nothing in it takes effect until you set `enabled = true`.
    /// A `question-menu` is never auto-answered, at any setting.
    Approvals {
        /// Write a commented starter `approvals.toml`. Refuses to overwrite an existing one.
        #[arg(long)]
        init: bool,
        /// Run the policy over every live agent that is blocked on a dialog right now, answering
        /// the ones it allows and leaving the rest for you. This is how an agent started outside a
        /// `bn orchestrate` wave gets the policy at all — nothing else watches it.
        #[arg(long, conflicts_with = "init")]
        sweep: bool,
        /// With `--sweep`: decide and report, but send nothing. See what a policy would do before
        /// you enable it.
        #[arg(long, requires = "sweep")]
        dry_run: bool,
        /// How many recent decisions to show (default 20).
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Give a running agent a **standing** instruction: recorded under `## Directives` in its
    /// branch note *and* delivered to the live session. The note copy is the point — a directive
    /// is re-seeded into a `bn agent relaunch --hard` and into a fresh dispatch, so resetting an
    /// agent's context no longer discards what you told it.
    ///
    /// bn agent instruct <target> "<text>" [--no-send] | --list | --drop <id>.
    Instruct {
        /// `repo:branch` of the agent — the `target` from `bn agent live`.
        target: String,
        /// The instruction. Omit it with `--list` or `--drop`.
        text: Vec<String>,
        /// Record the directive without delivering it to the live session.
        #[arg(long)]
        no_send: bool,
        /// List the target's standing directives instead of adding one.
        #[arg(long)]
        list: bool,
        /// Drop a directive by its note id (`i1`).
        #[arg(long, value_name = "ID")]
        drop: Option<String>,
    },
    /// Read a session's transcript as a readable timeline: your prompts, the agent's prose, and
    /// consecutive tool calls collapsed into one summary row. Powers the web dashboard's agent
    /// activity feed — unlike `pane`, it works for a session whose tmux window has already closed,
    /// because the transcript is on disk. `<session>` is the harness session id (the `session_id`
    /// from `bn agent live`). bn agent activity <session> [--limit N].
    Activity {
        /// The harness session id — a Claude transcript or a Copilot session directory.
        session: String,
        /// How many trailing timeline entries to return (default 40).
        #[arg(long, default_value_t = 40)]
        limit: usize,
    },
    /// Claude Code SessionStart hook: point this worktree's auto-memory at a per-worktree
    /// `.claude/memory` (written idempotently to the gitignored `.claude/settings.local.json`,
    /// since the checked-in settings ignore `autoMemoryDirectory`), then print `bn handoff read`
    /// to orient the session. Replaces `scripts/wt-memory.sh`. Best-effort, quiet on no note.
    MemoryHook,
    /// Launch an AI coding agent in the current directory, resuming this directory's recorded
    /// session if one exists, else starting fresh — the single home of the resume-or-fresh rule
    /// (tmux keybinds, `bn dispatch`, and a plain `cd <wt> && bn agent launch` all behave
    /// identically). Replaces the `workflow/bin/agent` zsh launcher. Execs the harness (this
    /// process is replaced), so it never returns on success. bn agent launch [copilot|claude].
    Launch {
        /// Harness to launch: `copilot` (default) or `claude`.
        #[arg(value_name = "HARNESS")]
        harness: Option<String>,
        /// Launch on a remote host alias from `[hosts]` / `~/.ssh/config`.
        #[arg(long)]
        host: Option<String>,
        /// Seed the first turn of a FRESH session with this prompt (ignored when resuming).
        #[arg(long)]
        seed: Option<String>,
        /// Read the seed prompt from a file (keeps the launch command short). A missing/unreadable
        /// file degrades to an unseeded launch rather than erroring.
        #[arg(long = "seed-file")]
        seed_file: Option<String>,
        /// Trusted-repo autonomy: run without approval prompts even when seeded.
        #[arg(long)]
        yolo: bool,
        /// Skip resume-detection and start a FRESH session even if this worktree has a recorded one
        /// (recovery re-seed, #383).
        #[arg(long)]
        fresh: bool,
        /// Pin the harness session id (so `bn agent send` can steer this exact session via resume).
        #[arg(long)]
        session: Option<String>,
        /// Prefer this exact recorded session before falling back to a fresh launch.
        #[arg(long, hide = true)]
        resume_session: Option<String>,
        /// Let the agent reach this directory too, on top of the one it is launched in.
        /// Repeatable. An agent is otherwise confined to its launch directory (plus its own
        /// branch-note directory), so a search cannot wander into `~/Documents`.
        #[arg(long = "add-dir", value_name = "DIR")]
        add_dir: Vec<String>,
        /// Extra args passed through to the harness verbatim (everything after `--`).
        #[arg(last = true)]
        passthrough: Vec<String>,
    },
    /// Launch a catalog agent or workflow skill BY NAME, seeded with an instruction — the join
    /// between `bn agents list` (what exists) and `bn agent launch` (how one starts). Agents and
    /// skills share one namespace; an agent wins a name they both hold. Bare `bn agent run` opens
    /// an fzf picker over everything launchable. Runs in the current directory (execs the harness,
    /// so it never returns on success); `--worktree` runs it in a fresh isolated one instead.
    /// bn agent run <name> "<instruction>"
    Run {
        /// Agent or skill to launch. Omit for the picker.
        name: Option<String>,
        /// What this agent should do — appended to its rendered body as the task.
        #[arg(value_name = "INSTRUCTION")]
        instruction: Vec<String>,
        /// Harness override. Defaults to the manifest's `harness:`, else the launcher's default.
        #[arg(long)]
        harness: Option<String>,
        /// Run in a fresh worktree via `bn dispatch` rather than the current directory.
        #[arg(long)]
        worktree: bool,
        /// Target repo for `--worktree` (a bare repo under the bare root).
        #[arg(long)]
        repo: Option<String>,
        /// Base branch to fork the `--worktree` run from (default: main).
        #[arg(long)]
        base: Option<String>,
        /// Trusted-repo autonomy: launch without approval prompts.
        #[arg(long)]
        yolo: bool,
        /// Fill a declared `{{key}}` argument: repeatable, e.g. `--arg date=2026-09-05`.
        #[arg(long = "arg", value_name = "KEY=VALUE")]
        args: Vec<String>,
        /// Let the agent reach this directory too, on top of the one it is launched in.
        /// Repeatable. See `bn agent launch --add-dir`.
        #[arg(long = "add-dir", value_name = "DIR")]
        add_dir: Vec<String>,
        /// List everything launchable and exit, instead of opening the picker (for scripts).
        #[arg(long)]
        list: bool,
        /// Print the composed seed — the exact prompt the agent would start on — and exit without
        /// launching anything. The only way to see what a run will actually say, and what makes
        /// the composition testable without spawning a harness.
        #[arg(long = "print-seed")]
        print_seed: bool,
        /// Internal: the fzf picker's self-referencing callback stage.
        #[arg(long, hide = true, value_name = "STAGE")]
        pick_stage: Option<crate::cmd::agent_run::RunPickStage>,
        /// Internal: narrow the picker to one catalog (`agent` | `skill`), so a reload and a
        /// preview reproduce the row set the picker was opened with.
        #[arg(long, hide = true, value_name = "SOURCE")]
        pick_source: Option<String>,
    },
}
