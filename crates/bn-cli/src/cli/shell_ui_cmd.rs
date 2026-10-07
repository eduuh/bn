//! `bn`'s shell and tmux surface — the commands the tmux config, the status bar and the shell
//! wrappers call, plus the note-editing verbs a person types by hand.
//!
//! Split out of `cli.rs` under the 1000-line ratchet, following the one-subcommand-enum-per-file
//! convention its siblings already use.
use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum CmdShellUi {
    /// The per-worktree nvim: show it as a pane beside your work, park it without killing it, or
    /// report where it is. Replaces the disposable `prefix e` popup, whose only exit was quitting
    /// nvim — here the pane hides into a detached hold session, so buffers, undo and LSP survive.
    Edit {
        // Boxed: this payload is an order of magnitude larger than any other variant here, and an
        // unboxed one would make every `CmdShellUi` value carry its footprint.
        #[command(subcommand)]
        action: Box<crate::edit::EditCmd>,
    },
    /// One-line status badge for the tmux status bar (replaces branch-note-status.sh).
    /// Resolves from the given path (tmux passes #{pane_current_path}) or cwd; prints
    /// nothing when there's no note or it's closed.
    TmuxStatus { path: Option<String> },
    /// Report tmux sessions that name no repository on this machine, and with `--force` kill them.
    /// bn opens a session per repo and a window per worktree; anything else is left over from an
    /// older layout or made by hand. Dry run by default. Never automated — killing a session is
    /// reclamation, which stays in your hands (see the note in config/crons.toml).
    ///
    /// Spared whatever they are called: `_bn_edit` and other `_`-prefixed storage sessions,
    /// `host:<alias>` remote attachments, anything with a client attached, a pane running something
    /// other than a shell, or a live bn agent.
    TmuxPrune {
        /// Actually kill them (default: report only).
        #[arg(long)]
        force: bool,
    },
    /// Drop non-repository sessions from a tmux-resurrect snapshot, in place. Wired to resurrect's
    /// `post-save-layout` hook, so an orphan session stops being restored after a reboot without
    /// anything having to kill it. Fails open: an unreadable file or an unreadable project root
    /// leaves the snapshot untouched.
    TmuxSnapshotFilter {
        /// The state file resurrect just wrote (the hook passes it).
        path: String,
    },
    /// Themed git segment for the tmux status bar (replaces workflow/bin/bn-git-status.sh):
    /// `⎇ <branch> ↑ahead ↓behind ●dirty` from one `git status --porcelain=v2 --branch`.
    /// Resolves from the given path (tmux passes #{pane_current_path}) or cwd; silent outside
    /// a git repo so the segment collapses. Served from a short-lived cache; a stale entry is
    /// printed at once and refreshed by a detached `--refresh` run.
    TmuxGit {
        path: Option<String>,
        /// Internal: recompute the cached segment for `path` (the detached background refresh).
        #[arg(long, hide = true)]
        refresh: bool,
    },
    /// Focus segment for the tmux status bar (replaces workflow/bin/bn-goal-status.sh): the
    /// linked PR's title (preferred, cached 5 min via gh) or the branch goal, truncated. Prints
    /// a dim "set a goal" nudge when there's a note but no goal/PR; silent outside a repo.
    TmuxGoal { path: Option<String> },
    /// Global attention tally for the tmux status bar (replaces
    /// workflow/bin/bn-attention-count.sh): counts windows `waiting` (an agent needs you) and
    /// `ready`/done across ALL sessions, amber→red once something has waited 15+ min. Silent
    /// when all quiet so the bar stays clean.
    TmuxAttnCount,
    /// Mark a finished window: from the global alert-silence hook when a watched window goes
    /// quiet, or from the pane-died / pane-exited hooks when its pane exits. Sets @bn_done (✓, or
    /// ✗ for a non-zero exit), clears monitor-silence, and fires a best-effort desktop toast.
    TmuxDoneBadge {
        /// The tmux window id (`#{hook_window}`) to badge.
        window: String,
        /// The pane's exit status, as `pane-died` passes it (`#{pane_dead_status}`). Omit it for
        /// the silence watch.
        #[arg(long)]
        status: Option<String>,
        /// Called from `pane-exited`: the pane is already gone, so its status is unknown and only
        /// a watched window is badged.
        #[arg(long, conflicts_with = "status")]
        exited: bool,
    },
    /// Orchestration roll-up popup (bound to prefix S; replaces workflow/bin/tmux-cockpit.sh):
    /// in-flight windows across every session (agent state / silence-watch), fleet health
    /// (`agent supervise`), and the bn cross-branch summary. Waits for Enter before closing.
    TmuxCockpit,
    /// Curated keybinding cheat-sheet popup (bound to prefix ?; replaces
    /// workflow/bin/tmux-help.sh). The workflow bindings worth remembering, grouped. Waits for
    /// Enter before closing.
    TmuxHelp,
    /// How often each curated tmux binding was pressed, read from the key log the `tmux-keylog`
    /// job drains out of tmux's message log every 5 minutes. Bindings never pressed show `never`.
    TmuxKeys {
        /// How far back to count, e.g. `7d`, `36h` (default 7d).
        #[arg(long)]
        since: Option<String>,
    },
    /// Open the current dir's branch note in $EDITOR (bound to prefix n; replaces
    /// workflow/bin/tmux-note.sh). Execs the editor so closing it closes the popup.
    TmuxNote,
    /// Icon-vocabulary cheatsheet: every Nerd Font glyph bn uses (name, codepoint, meaning)
    /// plus a mock of the two tmux status rows. If a row shows tofu/blanks, your font is
    /// missing the glyph. The mirrors listed in bn-core/src/glyph.rs must match this set.
    Glyphs,
    /// Push the current note's counts into per-window tmux options (@bn_todos, @bn_blockers)
    /// so the window list can decorate cheaply. Runs automatically after `bn add` / `bn done`
    /// inside tmux. Decorates the window bn ran in; `bn tmux-reconcile` sweeps them all.
    TmuxSync,
    /// Restore the window -> worktree link (@bn_repo/@bn_branch) across every window and refresh
    /// their note badges: window options do not survive a tmux-resurrect restore, and badges are
    /// otherwise written only to the window bn ran in. Stamps only where a window's name agrees
    /// with the branch its pane is on; drift is reported, never guessed. Idempotent.
    TmuxReconcile {
        /// Print the decisions and pending writes without touching tmux.
        #[arg(long)]
        dry_run: bool,
    },
    /// Set the live agent-activity decoration on the calling pane's window via the
    /// @bn_agent option. Driven by Claude Code hooks: UserPromptSubmit -> working,
    /// Notification -> waiting, Stop -> done. `clear` blanks it. No-op outside tmux,
    /// I/O-free (no note read) so hooks stay fast. Takes precedence over @bn_done (the
    /// silence-watch fallback) in the status line and cockpit — see workflow/tmux.conf.
    TmuxAgent {
        /// working | waiting | done | clear
        state: String,
    },
    /// Status for the current branch.
    Status,
    /// Lean context snapshot (goal, blockers, open todos, recent progress/decisions).
    Brief,
    /// Complete and close the branch note: mark open todos done, log an achievement (from the
    /// note's Goal + PR, or `--achievement`), and set status closed. Run when work ships; then,
    /// when run interactively inside a feature worktree, it reclaims that worktree the same way
    /// `wt cleanup` does — removes it, deletes the branch, and kills its tmux window — provided
    /// the PR is merged and the tree is clean (use `--force` to reclaim regardless).
    Close {
        /// Achievement line to log instead of the one derived from the Goal + PR.
        #[arg(long)]
        achievement: Option<String>,
        /// Reclaim the worktree even if the PR isn't merged or the tree is dirty.
        #[arg(long)]
        force: bool,
    },
    /// Reopen a closed branch note.
    Reopen,
    /// Cut a GitHub release end-to-end (#627): bump the semver from the last tag (default patch,
    /// `--bump major|minor|patch`), create + push the tag, and `gh release create` with notes
    /// rolled up from the PRs merged since the last tag. Gated + idempotent — refuses on a dirty or
    /// behind-`origin/main` tree, and never releases the same commit twice. Runs the local `bn
    /// verify` gate first to confirm `main` is green (`--skip-verify` trusts the caller); `--dry-run`
    /// prints the tag + notes without touching git/GitHub.
    Release {
        /// Which semver component to bump: major | minor | patch (default: patch).
        #[arg(long)]
        bump: Option<String>,
        /// Print the tag + rolled-up notes without creating/pushing the tag or the release.
        #[arg(long)]
        dry_run: bool,
        /// Skip the local `bn verify` gate (trust the caller that `main` is green).
        #[arg(long)]
        skip_verify: bool,
    },
}
