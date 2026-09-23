//! `bn wt` — git worktree manager for the bare+worktree layout. A Rust port of the `wt`
//! zsh script (`~/.bin/wt`). Bare repos live at `<root>/bare/<repo>.git`, worktrees at
//! `<root>/worktree/<repo>/<branch>`. `<root>` is `$WT_ROOT` if set (so the `wtw` wrapper can
//! point this same logic at the Windows filesystem), else bn's configured worktree root.
//!
//! Interactive selection (no branch/worktree given) shells out to `fzf`, matching the oracle.
//! `cleanup` additionally closes the branch note and kills the worktree's tmux window(s) — the
//! latter improved over the zsh version: it runs even when invoked from outside tmux, and matches
//! windows by name (== the sanitized branch) as well as by pane cwd.
//!
//! Decomposed from the former single-file wt.rs into concern-focused submodules (issue #351):
//! `common` (shared foundation), `remote`, `add`, `review`, `window`, `list`, `merge`,
//! `cleanup` — each moved verbatim and glob-re-exported here so `wt::<symbol>` paths (and the
//! test module below) resolve unchanged. This file keeps `WtCmd`/`PoolCmd` and the dispatch.

use crate::proc::{tmux, tmux_command, tmux_ok, tmux_run};
use bn_core::resolver::{self, Config};
use bn_core::runner::CommandRunner as _;
use bn_core::util::today;
use clap::Subcommand;
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

mod add;
mod cleanup;
mod close;
mod common;
mod local_commits;
mod list;
mod merge;
mod reclaim;
mod remote;
mod review;
mod window;

pub(crate) use add::*;
pub(crate) use cleanup::*;
pub(crate) use close::*;
pub(crate) use common::*;
pub(crate) use list::*;
pub(crate) use merge::*;
pub(crate) use reclaim::*;
pub(crate) use remote::*;
pub(crate) use review::*;
pub(crate) use window::*;

#[derive(Subcommand, Debug)]
pub enum WtCmd {
    /// Clone a repo — bare+worktree by default, regular for a few configured repos.
    Clone {
        url: String,
        /// Override the repo name (default: the URL basename, sans `.git`).
        name: Option<String>,
    },
    /// Clone every repo listed in the manifest ($WT_MANIFEST or ~/.config/wt/repos).
    CloneAll {
        /// Run inline instead of spawning a tmux window.
        #[arg(long)]
        here: bool,
    },
    /// Add a worktree (interactive branch picker if none given).
    Add {
        /// Existing branch to check out (fzf picker if omitted).
        branch: Option<String>,
        /// Create a NEW branch with this name (off --base, default main).
        #[arg(short = 'b', long = "new-branch", value_name = "NAME")]
        new_branch: Option<String>,
        /// Base branch for --new-branch (default main). Also accepted as a positional after -b.
        #[arg(long, value_name = "BASE")]
        base: Option<String>,
        /// Don't auto-create the worktree's tmux window.
        #[arg(long)]
        no_tmux: bool,
    },
    /// Go to or create a worktree, then open/select its tmux window (session-per-repo). The
    /// keybinding flow (prefix+o): pick or type a branch — existing → select its window,
    /// new → create the worktree from main, then open it.
    Open {
        /// Branch/worktree to open (fzf picker over existing worktrees if omitted).
        branch: Option<String>,
        /// Always open a fresh window even if one already exists for this worktree.
        #[arg(long)]
        new: bool,
        /// Don't touch tmux (just ensure the worktree exists).
        #[arg(long)]
        no_tmux: bool,
    },
    /// List worktrees for the current repo (or all repos when outside one).
    #[command(visible_alias = "ls")]
    List,
    /// Remove a worktree (interactive picker if none given) — the `prefix D` path.
    ///
    /// The same reclaim `cleanup` runs, just with a picker in front: it hands a pool slot's lease
    /// back, closes the branch note, and applies the merged-PR / clean-tree gates. `--force` is the
    /// deliberate override, exactly as it is for `cleanup`.
    #[command(visible_alias = "rm")]
    Remove {
        worktree: Option<String>,
        /// Remove even when no PR exists or the PR is not yet merged (also forces removal of a
        /// worktree with uncommitted changes).
        #[arg(long)]
        force: bool,
    },
    /// Print a worktree's path for the shell wrapper to cd into (interactive picker if none given).
    Go { worktree: Option<String> },
    /// Fetch all remotes for the current repo's bare clone.
    Fetch,
    /// Fetch your open PRs for this repo (opened from any computer) and create local worktrees
    /// for them. With no target, lists your open PRs that lack a worktree and (on a terminal)
    /// opens an fzf multi-picker; pass a PR number/branch to create one directly, or `--all` for
    /// every candidate. Provider is detected from origin: GitHub via `gh`, Azure DevOps via `az`.
    #[command(visible_alias = "prs")]
    Remote {
        /// PR number (e.g. 42 or #42) or source branch to create directly, skipping the picker.
        target: Option<String>,
        /// Create worktrees for all your open PRs that don't already have one.
        #[arg(long)]
        all: bool,
        /// Don't auto-create the worktree's tmux window.
        #[arg(long)]
        no_tmux: bool,
    },
    /// Prune stale worktree references (current repo, or all repos when outside one).
    Prune,
    /// Close **this worktree** — the `prefix x` binding. Drops the worktree's tmux window now and
    /// reclaims it (hand a pool slot's lease back, or run `cleanup`'s gates) in the background.
    ///
    /// Never closes a tmux session: a worktree owning its session's last window leaves that window
    /// respawned as a bare shell instead. Never prints on the happy path (tmux's `run-shell` would
    /// surface stdout as a popup) — outcomes go to `display-message`.
    Close {
        /// Exit 0 iff closing needs a confirm prompt (uncommitted or unpushed work). Decides
        /// nothing and tears down nothing — the binding's `if-shell` test.
        #[arg(long)]
        check: bool,
        /// Pane to close when the path isn't a worktree we reclaim; tmux passes `#{pane_id}`.
        #[arg(long)]
        pane: Option<String>,
        /// Worktree to close; tmux passes `#{pane_current_path}`. Default: the current directory.
        #[arg(long)]
        path: Option<String>,
        /// The confirm prompt was answered yes — discard uncommitted changes in a pool slot.
        #[arg(long)]
        confirmed: bool,
        /// Do the deferred half now: reclaim the (already window-less) worktree without touching
        /// tmux. This is what the keystroke's background process runs; run it by hand to reclaim a
        /// worktree whose background reclaim did not finish.
        #[arg(long)]
        reclaim: bool,
    },
    /// Verify a worktree's PR is merged, then remove it, delete its branch, close its note,
    /// and kill its tmux window(s). Default target: the current worktree.
    Cleanup {
        /// Sweep every worktree under all bare repos and clean the merged ones.
        #[arg(long)]
        all: bool,
        /// Clean even when no PR exists or the PR is not yet merged (also forces removal of
        /// worktrees with uncommitted changes).
        #[arg(long)]
        force: bool,
        /// Worktree path or `<repo>/<branch>` slug under the worktree root.
        worktree: Option<String>,
    },
    /// Reap worktrees whose WORK IS DONE and safe to remove — the orchestration's "clean up once
    /// finished" sweep. Reaps: a **scout** worktree whose agent reported done and is clean (no
    /// commits/uncommitted — its findings live safely in the synced note), and a **ship** worktree
    /// whose PR is merged. Skips open/no-PR ships, dirty worktrees, and still-active scouts.
    /// Dry-run by default; `--yes` executes. Default target: current worktree; `--all` sweeps.
    Reap {
        /// Sweep every worktree under all bare repos.
        #[arg(long)]
        all: bool,
        /// Actually reap (default: dry-run report of what would be reaped).
        #[arg(long)]
        yes: bool,
    },
    /// Merge the current branch's GitHub PR, then clean up its worktree, branch, note, and tmux
    /// window — the local-action trigger for cleanup (no polling lag). GitHub only; ADO merges are
    /// reclaimed by the `cleanup --all` poller.
    ///
    /// A single-PR merge is **gated** by default: it rebases `origin/<branch>` onto current
    /// `origin/main` in a disposable worktree and runs the repo's verify gate on that head, so a
    /// red or stale-base PR can't land (the class that broke main). `--no-gate` bypasses it.
    ///
    /// With `--all`, merge every open PR of yours for the repo instead, with file-overlap
    /// awareness: PRs whose changed-file sets are disjoint are merged straight through, while PRs
    /// that touch the same file are serialized — one at a time, re-checking mergeability (polling
    /// past GitHub's transient `UNKNOWN` recompute window) between each. When a serialized PR
    /// flips CONFLICTING after a sibling lands, `--all` auto-rebases it onto origin/main (a mini
    /// merge-queue): pure insert/insert conflicts — e.g. two PRs each adding a variant to the same
    /// enum — are resolved by keeping both sides; anything riskier aborts and the PR is skipped
    /// for a manual rebase as before.
    Merge {
        /// Merge with a merge commit instead of the default squash.
        #[arg(long, conflicts_with = "rebase")]
        merge: bool,
        /// Rebase-merge instead of the default squash.
        #[arg(long)]
        rebase: bool,
        /// Keep the remote branch (default: delete it on merge).
        #[arg(long)]
        no_delete_branch: bool,
        /// Pass `--admin` to gh to bypass required checks.
        #[arg(long)]
        admin: bool,
        /// Skip the pre-merge gate (rebase-onto-origin/main + verify on that head). The gate is
        /// on by default for a single-PR merge and for `--all-repos` (each repo gated via ITS
        /// verify, #455) so a red or stale-base PR can't land; use this only to bypass it
        /// deliberately. (No effect with `--all`, which stays the fast, ungated batch path.)
        #[arg(long)]
        no_gate: bool,
        /// Merge every open PR of yours for the repo, sequencing file-overlapping ones.
        #[arg(long)]
        all: bool,
        /// Merge every open PR of yours across EVERY configured repo in one invocation, gating each
        /// repo via its own verify (#455). The cross-repo landing rollup.
        #[arg(long, conflicts_with = "all")]
        all_repos: bool,
        /// With `--all`/`--all-repos`: skip conflicting PRs instead of auto-rebasing them onto origin/main.
        #[arg(long)]
        no_auto_rebase: bool,
        /// Land without waiting for the PR's GitHub checks. By default a merge requires the
        /// published checks to have finished green — a PR whose CI is still running is left for a
        /// later pass, and a red one is never landed. This is what stops the poller from merging
        /// faster than CI can report (a merge cancels the in-flight run, so an ungated poller turns
        /// every check into a race it wins). Use only to land something deliberately.
        #[arg(long)]
        no_ci_gate: bool,
        /// Seconds to wait on a still-running CI run before leaving the PR for a later pass.
        #[arg(long, value_name = "SECS", default_value_t = 600)]
        ci_wait: u64,
    },
    /// Surface worktrees with uncommitted changes — the ones `cleanup` can't remove until you
    /// commit or stash. The "stuck worktree" report (local + fast; add --pr to flag merged ones).
    Status {
        /// Also list clean worktrees (default: only those with uncommitted changes).
        #[arg(long)]
        all: bool,
        /// Look up each dirty worktree's PR state to flag the merged-but-dirty case. Slower — one
        /// gh/az call per dirty worktree.
        #[arg(long)]
        pr: bool,
    },
    /// Treehouse-style pool of warm, reusable worktrees — every repo bn has a clone of, with no
    /// registration and nothing to size. Slots live at `<root>/worktree/<repo>/_pool/<n>` and keep
    /// their deps/build cache between uses, so an agent gets an isolated checkout without a cold
    /// build.
    Pool {
        #[command(subcommand)]
        action: PoolCmd,
    },
}

/// `bn wt pool` subcommands. Handlers live in the sibling `wt_pool` module; the enum stays here so
/// clap derives it in the same context as its `WtCmd` parent.
#[derive(Subcommand, Debug)]
pub enum PoolCmd {
    /// Lease an idle warm slot and switch `branch` into it (reusing cached deps), then open its
    /// tmux window — or print just the path with `--path` for agents/leases.
    Get {
        /// Branch to check out in the slot.
        branch: Option<String>,
        /// Target repo (defaults to the current repo, else an fzf pick).
        #[arg(long)]
        repo: Option<String>,
        /// Label recorded as the lease holder (defaults to $USER).
        #[arg(long)]
        holder: Option<String>,
        /// Print only the slot path and skip tmux (for agents / `--lease`-style use).
        #[arg(long)]
        path: bool,
        /// Don't open a tmux window.
        #[arg(long)]
        no_tmux: bool,
    },
    /// Return a slot to the warm pool: drop its lease and reset it (detach to main, clean
    /// untracked) while preserving ignored deps/build cache. Default target: the current slot.
    Return {
        /// Slot number, or an absolute slot path. Defaults to the slot containing the cwd.
        target: Option<String>,
        /// Reset and release even when the slot isn't leased OR has uncommitted changes (which are
        /// discarded). Without it, `return` refuses on a dirty slot so work isn't silently lost.
        #[arg(long)]
        force: bool,
    },
    /// Show each slot's state (idle / leased), branch, holder, and lease age.
    Status {
        /// Target repo (defaults to the current repo, else an fzf pick).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Pre-build every slot so they're warm and ready: create each slot's checkout (up to the
    /// configured size) and run the repo's `build.sh` (the same script `bn build` uses) in it.
    /// Scaffolds a `build.sh` template if none exists. Re-run after changing deps to re-warm.
    /// Leased slots (an agent is using them) and dirty slots (uncommitted work) are always skipped
    /// — warming would disrupt them — so only idle-clean and brand-new slots are built. `--new`
    /// narrows to brand-new slots only, leaving every existing slot untouched (ideal right after
    /// growing the pool size). Long-running, so inside tmux it opens a window in the repo's session
    /// to watch live; `--here` runs it inline (and is what that window calls).
    Warm {
        /// Target repo (defaults to the current repo, else an fzf pick).
        #[arg(long)]
        repo: Option<String>,
        /// Run inline instead of spawning a tmux window in the repo's session.
        #[arg(long)]
        here: bool,
        /// Only warm brand-new slots (no checkout yet); skip every existing slot, idle included.
        #[arg(long)]
        new: bool,
    },
    /// Pool usage + health metrics: utilization now, lifetime gets (warm-hit rate), returns,
    /// average lease duration, per-slot churn, and a health tail — slots recovered after a WSL/host
    /// restart, lifetime restart recoveries, and any lease still needing attention. Reading it also
    /// self-heals the pool (reclaims slots pinned by a prior boot).
    Metrics {
        /// Repo name or a path at/inside a repo (a pool slot, a worktree checkout, or a
        /// `<repo>.git` bare dir). A path lets you point metrics at any checkout and get that
        /// repo's pool health — or a clean "not pooled" if it has none. Defaults to `--repo`, else
        /// the current repo, else an fzf pick.
        path: Option<String>,
        /// Target repo by name instead of a path (defaults to the current repo, else an fzf pick).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Reclaim only genuinely-stale slots: over-size (slot number > configured size, e.g. after
    /// lowering it) and orphaned (bare repo gone, or git registration broken). Never touches an
    /// in-size idle warm slot — that IS the pool — nor leased/dirty/in-use slots. Dry-run by
    /// default; `--yes` executes.
    Prune {
        /// Target repo (defaults to the current repo, else an fzf pick).
        #[arg(long)]
        repo: Option<String>,
        /// Actually remove (default is a dry run).
        #[arg(long)]
        yes: bool,
        /// Sweep every pooled repo, not just the resolved one.
        #[arg(long)]
        all: bool,
        /// Also print the in-size warm slots that are kept.
        #[arg(short = 'v', long)]
        verbose: bool,
    },
    /// Remove a specific slot or the whole pool. Dry-run by default; `--yes` executes. Leased,
    /// dirty, and in-use slots are skipped unless the matching `--include-*` flag opts in; a leased
    /// slot can only be removed by exact slot number plus `--include-leased` (never under `--all`).
    Destroy {
        /// Slot number to destroy (omit and pass `--all` for the whole pool).
        slot: Option<u32>,
        /// Target repo (defaults to the current repo, else an fzf pick).
        #[arg(long)]
        repo: Option<String>,
        /// Destroy every slot in the pool.
        #[arg(long)]
        all: bool,
        /// Actually remove (default is a dry run).
        #[arg(long)]
        yes: bool,
        /// Also remove a leased slot (exact slot only — rejected with `--all`).
        #[arg(long)]
        include_leased: bool,
        /// Also remove a slot with uncommitted changes.
        #[arg(long)]
        include_dirty: bool,
        /// Also remove a slot with a live process in it.
        #[arg(long)]
        include_in_use: bool,
    },
}

/// Entry point from main.rs. Exits the process with a non-zero code on failure (CLI semantics).
pub fn run(cfg: &Config, action: WtCmd, _json: bool) {
    std::process::exit(run_code(cfg, action));
}

/// Run a `wt` action and return its exit code (0 ok, 1 failure) without exiting — so callers like
/// the native cron job registry can run e.g. `cleanup --all` in-process and still record the
/// outcome. The top-level CLI wraps this in `run`/`std::process::exit`.
pub fn run_code(cfg: &Config, action: WtCmd) -> i32 {
    let ok = match action {
        WtCmd::Clone { url, name } => cmd_clone(cfg, &url, name.as_deref()),
        WtCmd::CloneAll { here } => cmd_clone_all(cfg, here),
        WtCmd::Add {
            branch,
            new_branch,
            base,
            no_tmux,
        } => cmd_add(cfg, branch, new_branch, base, no_tmux),
        WtCmd::Open {
            branch,
            new,
            no_tmux,
        } => cmd_open(cfg, branch, new, no_tmux),
        WtCmd::List => cmd_list(cfg),
        WtCmd::Remove { worktree, force } => cmd_remove(cfg, worktree, force),
        WtCmd::Go { worktree } => cmd_go(cfg, worktree),
        WtCmd::Fetch => cmd_fetch(cfg),
        WtCmd::Remote {
            target,
            all,
            no_tmux,
        } => cmd_remote(cfg, target, all, no_tmux),
        WtCmd::Prune => cmd_prune(cfg),
        WtCmd::Close {
            check,
            pane,
            path,
            confirmed,
            reclaim,
        } => cmd_close(cfg, check, pane, path, confirmed, reclaim),
        WtCmd::Cleanup {
            all,
            force,
            worktree,
        } => cmd_cleanup(cfg, all, force, worktree),
        WtCmd::Reap { all, yes } => cmd_reap(cfg, all, yes),
        WtCmd::Merge {
            merge,
            rebase,
            no_delete_branch,
            admin,
            no_gate,
            all,
            all_repos,
            no_auto_rebase,
            no_ci_gate,
            ci_wait,
        } => {
            let flags = MergeFlags {
                merge,
                rebase,
                no_delete_branch,
                admin,
                no_auto_rebase,
                no_gate,
                ci: CiGate {
                    enabled: !no_ci_gate,
                    wait: Duration::from_secs(ci_wait),
                },
            };
            if all_repos {
                cmd_merge_all_repos(cfg, flags)
            } else if all {
                cmd_merge_all(cfg, flags)
            } else {
                cmd_merge(cfg, flags)
            }
        }
        WtCmd::Status { all, pr } => cmd_status(cfg, all, pr),
        WtCmd::Pool { action } => crate::wt_pool::run(cfg, action),
    };
    if ok {
        0
    } else {
        1
    }
}


