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
mod common;
mod list;
mod merge;
mod remote;
mod review;
mod window;

pub(crate) use add::*;
pub(crate) use cleanup::*;
pub(crate) use common::*;
pub(crate) use list::*;
pub(crate) use merge::*;
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
    /// Remove a worktree (interactive picker if none given).
    #[command(visible_alias = "rm")]
    Remove { worktree: Option<String> },
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
    /// Treehouse-style pool of warm, reusable worktrees for monorepos opted in via
    /// `~/.config/wt/pools`. Slots live at `<root>/worktree/<repo>/_pool/<n>` and keep their
    /// deps/build cache between uses, so an agent gets an isolated checkout without a cold build.
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
    /// Turn pooling on for a repo at `size` slots, or off with `--off`. Writes the repo's entry in
    /// the pools config (`~/.config/wt/pools`) — the opt-in every other pool command reads. Slots
    /// aren't built here: the idle `pool-warm` cron picks the repo up on its next pass, or run
    /// `bn wt pool warm --repo <repo>` to build them now.
    Set {
        /// Slot count to pool at (1..=16).
        size: Option<usize>,
        /// Target repo (defaults to the current repo, else an fzf pick).
        #[arg(long)]
        repo: Option<String>,
        /// Stop pooling this repo — remove its entry. Existing slots stay on disk; remove them with
        /// `bn wt pool destroy --all`.
        #[arg(long)]
        off: bool,
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
        WtCmd::Remove { worktree } => cmd_remove(cfg, worktree),
        WtCmd::Go { worktree } => cmd_go(cfg, worktree),
        WtCmd::Fetch => cmd_fetch(cfg),
        WtCmd::Remote {
            target,
            all,
            no_tmux,
        } => cmd_remote(cfg, target, all, no_tmux),
        WtCmd::Prune => cmd_prune(cfg),
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
        } => {
            if all_repos {
                cmd_merge_all_repos(
                    cfg,
                    merge,
                    rebase,
                    no_delete_branch,
                    admin,
                    no_auto_rebase,
                    no_gate,
                )
            } else if all {
                cmd_merge_all(
                    cfg,
                    merge,
                    rebase,
                    no_delete_branch,
                    admin,
                    no_auto_rebase,
                    no_gate,
                )
            } else {
                cmd_merge(cfg, merge, rebase, no_delete_branch, admin, no_gate)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_slashes() {
        assert_eq!(sanitize_branch("user/feature-x"), "user-feature-x");
        assert_eq!(sanitize_branch("main"), "main");
    }

    #[test]
    fn reconcile_stuck_acks_notice_when_worktree_absent() {
        // #534: a `wt-stuck:*` notice whose worktree was reaped out of band (here: no bare repos
        // at all, so it's "gone") must be acked by the sweep — decoupled from `cleanup_one`.
        let _g = bn_core::test_support::env_lock();
        let state = tempfile::tempdir().unwrap();
        let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", state.path());
        let root = tempfile::tempdir().unwrap();
        let _wt = bn_core::test_support::EnvGuard::set("WT_ROOT", root.path());

        bn_core::attention::add(
            "wt-cleanup",
            "merged worktree bn/gone has 1 uncommitted change(s)",
            "warn",
            Some("bn"),
            Some("gone"),
            None,
            Some("wt-stuck:bn/gone"),
            &today(),
        )
        .unwrap();
        assert_eq!(bn_core::attention::open_items(&today()).len(), 1);

        let cfg = bn_core::test_support::config(root.path());
        reconcile_stuck(&cfg);

        assert!(
            bn_core::attention::open_items(&today()).is_empty(),
            "stale wt-stuck notice must be acked when its worktree is gone"
        );
    }

    #[test]
    fn oid_lines_trims_and_drops_blanks() {
        let stdout = "\nabc123\n  def456  \n\n";
        assert_eq!(oid_lines(stdout), vec!["abc123", "def456"]);
        assert!(oid_lines("").is_empty());
        assert!(oid_lines("\n \n").is_empty());
    }

    #[test]
    fn worktree_paths_in_drops_pool_slots() {
        let bare = Path::new("/home/x/bare/arena.git");
        let listing = "worktree /home/x/worktree/arena/feature\n\
                       worktree /home/x/worktree/arena/_pool/1\n\
                       worktree /home/x/worktree/arena/_pool/2\n\
                       worktree /home/x/bare/arena.git\n";
        let got = worktree_paths_in(listing, bare);
        assert_eq!(got, vec!["/home/x/worktree/arena/feature".to_string()]);
    }

    #[test]
    fn harness_cmd_picks_the_right_cli() {
        // Interactive (yolo=false): prompts per tool — the review posture for untrusted content.
        assert_eq!(
            harness_cmd("claude", "do it", false, None),
            "claude --strict-mcp-config 'do it'"
        );
        assert_eq!(
            harness_cmd("copilot", "do it", false, None),
            "copilot -i 'do it'"
        );
        // Anything unrecognised falls back to claude (the safe default).
        assert_eq!(
            harness_cmd("", "do it", false, None),
            "claude --strict-mcp-config 'do it'"
        );
        assert_eq!(
            harness_cmd("gemini", "do it", false, None),
            "claude --strict-mcp-config 'do it'"
        );
        // Apostrophes in the prompt are escaped so they don't break the shell command.
        assert_eq!(
            harness_cmd("copilot", "someone else's PR", false, None),
            "copilot -i 'someone else'\\''s PR'"
        );
        assert_eq!(
            harness_cmd("copilot", "do it", false, Some("sid-1")),
            "copilot -i 'do it'"
        );
    }

    #[test]
    fn harness_cmd_yolo_grants_the_unattended_bypass() {
        // yolo=true → the blanket bypass, for a self-authored auto task with no human to approve.
        assert_eq!(
            harness_cmd("claude", "do it", true, None),
            "claude --strict-mcp-config --dangerously-skip-permissions 'do it'"
        );
        // Booleans first, `-i <prompt>` last: copilot's `-i` eats the next token as the prompt, so
        // the prompt must immediately follow it (else a following flag is misread as the prompt).
        assert_eq!(
            harness_cmd("copilot", "do it", true, None),
            "copilot --allow-all-tools --allow-all-paths -i 'do it'"
        );
        assert_eq!(
            harness_cmd("copilot", "do it", true, Some("abc")),
            "copilot --allow-all-tools --allow-all-paths -i 'do it'"
        );
        assert_eq!(
            harness_cmd("claude", "do it", true, Some("abc")),
            "claude --strict-mcp-config --dangerously-skip-permissions --session-id 'abc' 'do it'"
        );
    }

    #[test]
    fn harness_cmd_from_file_reads_the_prompt_with_cat() {
        // File-based prompt: the command stays short and reads the (arbitrarily long) prompt via
        // `"$(cat '<file>')"`, so a paste/typed launch can't truncate it mid-string.
        let f = Path::new("/tmp/bn-launch-prompt-bn-feat-x.md");
        assert_eq!(
            harness_cmd_from_file("claude", f, false, None),
            "claude --strict-mcp-config \"$(cat '/tmp/bn-launch-prompt-bn-feat-x.md')\""
        );
        assert_eq!(
            harness_cmd_from_file("claude", f, true, None),
            "claude --strict-mcp-config --dangerously-skip-permissions \"$(cat '/tmp/bn-launch-prompt-bn-feat-x.md')\""
        );
        // Booleans first, `-i <prompt>` last — same copilot flag-order rule as harness_cmd.
        assert_eq!(
            harness_cmd_from_file("copilot", f, false, None),
            "copilot -i \"$(cat '/tmp/bn-launch-prompt-bn-feat-x.md')\""
        );
        assert_eq!(
            harness_cmd_from_file("copilot", f, true, None),
            "copilot --allow-all-tools --allow-all-paths -i \"$(cat '/tmp/bn-launch-prompt-bn-feat-x.md')\""
        );
        // Unknown harness falls back to claude, like harness_cmd.
        assert_eq!(
            harness_cmd_from_file("gemini", f, false, None),
            "claude --strict-mcp-config \"$(cat '/tmp/bn-launch-prompt-bn-feat-x.md')\""
        );
    }

    #[test]
    fn prompt_to_file_writes_and_sanitizes_the_name() {
        let prompt = "long prompt with 'quotes' and\nnewlines";
        let path = prompt_to_file("launch", "owner/repo", "feat/x", prompt).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), prompt);
        // repo/branch separators are dashed so the filename is a single safe component.
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name, "bn-launch-prompt-owner-repo-feat-x.md");
        // Owner-only: the prompt can feed a yolo launch, so other users must not read/swap it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
        }
        // Overwritten on relaunch — same window, same file.
        let again = prompt_to_file("launch", "owner/repo", "feat/x", "v2").unwrap();
        assert_eq!(again, path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v2");
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn prompt_to_file_refuses_a_planted_symlink() {
        // A symlink pre-parked at the predictable temp path (world-writable /tmp on Linux) must
        // not be followed: creation fails, the caller falls back to the inline command, and the
        // symlink's target is never written through.
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "untouched").unwrap();
        let link = std::env::temp_dir().join("bn-symtest-prompt-owner-repo-feat-x.md");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        // remove_file unlinks the symlink itself (not the target), so create_new succeeds on a
        // fresh regular file — the attack only wins if the write goes *through* the link.
        let out = prompt_to_file("symtest", "owner/repo", "feat/x", "payload");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
        if let Some(p) = out {
            assert!(!std::fs::symlink_metadata(&p)
                .unwrap()
                .file_type()
                .is_symlink());
            let _ = std::fs::remove_file(&p);
        }
    }

    #[test]
    fn prompt_file_in_dirs_falls_back_past_an_unwritable_dir() {
        // #646: a slow/removable notes mount (WSL Windows share) can fail the primary write; the
        // launcher then can't read a seed file and would garble a long inline seed. Falling back to
        // the next candidate keeps a reliable local path available.
        let good = tempfile::tempdir().unwrap();
        let bogus = std::path::PathBuf::from("/no/such/dir/bn-646-unwritable");
        let out = prompt_file_in_dirs(&[bogus, good.path().to_path_buf()], "bn-seed.md", "payload")
            .expect("should fall back to the writable dir");
        assert!(out.starts_with(good.path()), "fell back: {}", out.display());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "payload");
    }

    #[test]
    fn prompt_file_in_dirs_returns_none_when_all_fail() {
        assert!(prompt_file_in_dirs(
            &[std::path::PathBuf::from("/no/such/a"), std::path::PathBuf::from("/no/such/b")],
            "bn-seed.md",
            "payload"
        )
        .is_none());
    }

    #[test]
    fn add_trusted_folder_adds_path_preserves_header_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(
            &cfg,
            "// User settings belong in settings.json.\n// managed automatically.\n{\n  \"trustedFolders\": [\"/a\"],\n  \"other\": 1\n}\n",
        )
        .unwrap();
        add_trusted_folder(&cfg, "/home/me/wt/new");
        let out = std::fs::read_to_string(&cfg).unwrap();
        assert!(out.starts_with("// User settings"), "header preserved");
        let body: String = out
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let folders = v["trustedFolders"].as_array().unwrap();
        assert!(folders
            .iter()
            .any(|x| x.as_str() == Some("/home/me/wt/new")));
        assert_eq!(v["other"], 1, "other keys preserved");
        // Idempotent — a second add doesn't duplicate.
        add_trusted_folder(&cfg, "/home/me/wt/new");
        let v2: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&cfg)
                .unwrap()
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        assert_eq!(v2["trustedFolders"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn add_trusted_folder_rejects_non_absolute_path() {
        // A bare token (e.g. a harness name mistaken for a folder) must never land in the trust
        // list — only absolute folder paths are written.
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(
            &cfg,
            "// managed automatically.\n{\n  \"trustedFolders\": [\"/a\"]\n}\n",
        )
        .unwrap();
        add_trusted_folder(&cfg, "claude");
        add_trusted_folder(&cfg, "relative/path");
        let body: String = std::fs::read_to_string(&cfg)
            .unwrap()
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let folders = v["trustedFolders"].as_array().unwrap();
        assert_eq!(folders.len(), 1, "no non-absolute entry was written");
        assert_eq!(folders[0].as_str(), Some("/a"));
    }

    #[test]
    fn shell_single_quote_escapes_apostrophes() {
        assert_eq!(shell_single_quote("plain"), "'plain'");
        assert_eq!(shell_single_quote("else's"), "'else'\\''s'");
    }

    #[test]
    fn worktree_paths_excludes_bare_dir_not_siblings() {
        let bare_dir = Path::new("/home/u/projects/bare");
        let listing = "\
worktree /home/u/projects/bare/repo.git
HEAD abc

worktree /home/u/projects/worktree/repo/main
branch refs/heads/main

worktree /home/u/projects/bare-staging/repo/feat
branch refs/heads/feat
";
        let got = worktree_paths_in(listing, bare_dir);
        // The bare dir's own entry is dropped; the real worktree and the look-alike sibling
        // dir (`bare-staging`, which only *string*-prefixes `bare`) are both kept.
        assert_eq!(
            got,
            vec![
                "/home/u/projects/worktree/repo/main".to_string(),
                "/home/u/projects/bare-staging/repo/feat".to_string(),
            ]
        );
    }

    #[test]
    fn regular_clone_membership() {
        let _g = bn_core::test_support::env_lock();
        let _repos = bn_core::test_support::EnvGuard::set(
            "BN_REGULAR_CLONE_REPOS",
            "my-notes, dotfiles notes",
        );
        assert!(is_regular_clone("my-notes"));
        assert!(is_regular_clone("dotfiles"));
        assert!(is_regular_clone("notes"));
        assert!(!is_regular_clone("bn"));
        assert!(!is_regular_clone(""));
    }

    #[test]
    fn shq_escapes_quotes() {
        assert_eq!(shq("a'b"), "'a'\\''b'");
        assert_eq!(shq("/x/y"), "'/x/y'");
    }

    #[test]
    fn slug_parsing() {
        assert_eq!(
            slug_from_url("git@github.com:eduuh/bn.git").as_deref(),
            Some("eduuh/bn")
        );
        assert_eq!(
            slug_from_url("https://github.com/eduuh/bn.git").as_deref(),
            Some("eduuh/bn")
        );
        assert_eq!(
            slug_from_url("https://github.com/eduuh/bn").as_deref(),
            Some("eduuh/bn")
        );
        assert_eq!(slug_from_url("git@gitlab.com:eduuh/bn.git"), None);
    }

    #[test]
    fn parse_remote_github() {
        assert_eq!(
            parse_remote("git@github.com:eduuh/bn.git"),
            Remote::GitHub {
                slug: "eduuh/bn".into()
            }
        );
        assert_eq!(
            parse_remote("https://github.com/eduuh/bn"),
            Remote::GitHub {
                slug: "eduuh/bn".into()
            }
        );
    }

    #[test]
    fn parse_remote_ado() {
        let want = Remote::Ado {
            org: "myorg".into(),
            project: "MyProject".into(),
            repo: "myrepo".into(),
        };
        assert_eq!(
            parse_remote("https://myorg@dev.azure.com/myorg/MyProject/_git/myrepo"),
            want
        );
        assert_eq!(
            parse_remote("git@ssh.dev.azure.com:v3/myorg/MyProject/myrepo"),
            want
        );
        assert_eq!(
            parse_remote("https://myorg.visualstudio.com/MyProject/_git/myrepo"),
            want
        );
        // The legacy `DefaultCollection` collection segment is skipped, so the project resolves to
        // MyProject rather than DefaultCollection.
        assert_eq!(
            parse_remote("https://myorg.visualstudio.com/DefaultCollection/MyProject/_git/myrepo"),
            want
        );
    }

    #[test]
    fn parse_remote_other() {
        assert_eq!(parse_remote("git@gitlab.com:eduuh/bn.git"), Remote::Other);
    }

    #[test]
    fn parse_gh_prs_extracts_fields() {
        let json = r#"[
            {"number":42,"title":"feat: thing","headRefName":"feature/thing","url":"https://github.com/o/r/pull/42"},
            {"number":7,"title":"fix bug","headRefName":"fix-bug","url":"https://github.com/o/r/pull/7"}
        ]"#;
        let got = parse_gh_prs(json);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].number, 42);
        assert_eq!(got[0].branch, "feature/thing");
        assert_eq!(got[0].title, "feat: thing");
        assert_eq!(got[0].url, "https://github.com/o/r/pull/42");
        // A row missing the required headRefName is skipped rather than poisoning the whole list.
        assert!(parse_gh_prs(r#"[{"number":1,"title":"t"}]"#).is_empty());
        assert!(parse_gh_prs("not json").is_empty());
    }

    #[test]
    fn parse_ado_prs_strips_ref_prefix_and_builds_url() {
        let json = r#"[
            {"pullRequestId":123,"title":"add x","sourceRefName":"refs/heads/users/me/add-x"}
        ]"#;
        let got = parse_ado_prs(
            json,
            AdoTarget {
                org: "myorg",
                project: "MyProject",
                repo: "myrepo",
            },
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].number, 123);
        assert_eq!(got[0].branch, "users/me/add-x");
        assert_eq!(
            got[0].url,
            "https://dev.azure.com/myorg/MyProject/_git/myrepo/pullrequest/123"
        );
    }

    #[test]
    fn parse_ado_prs_reads_rest_value_and_filters_by_repo() {
        // The REST `pullrequests` response wraps PRs in `{"value": [...]}` and (project-scoped)
        // includes other repos' PRs, which must be filtered out by repository.name.
        let json = r#"{"count":2,"value":[
            {"pullRequestId":10,"title":"mine","sourceRefName":"refs/heads/feature/a","repository":{"name":"myrepo"}},
            {"pullRequestId":11,"title":"other repo","sourceRefName":"refs/heads/feature/b","repository":{"name":"otherrepo"}}
        ]}"#;
        let got = parse_ado_prs(
            json,
            AdoTarget {
                org: "myorg",
                project: "MyProject",
                repo: "myrepo",
            },
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].number, 10);
        assert_eq!(got[0].branch, "feature/a");
        assert_eq!(
            got[0].url,
            "https://dev.azure.com/myorg/MyProject/_git/myrepo/pullrequest/10"
        );
    }

    #[test]
    fn remote_pr_matches_number_or_branch() {
        let pr = RemotePr {
            number: 42,
            title: "t".into(),
            branch: "feature/thing".into(),
            url: String::new(),
        };
        assert!(pr.matches("42"));
        assert!(pr.matches("#42"));
        assert!(pr.matches(" 42 "));
        assert!(pr.matches("feature/thing"));
        assert!(!pr.matches("43"));
        assert!(!pr.matches("feature"));
    }

    #[test]
    fn fzf_line_round_trips_to_number() {
        let pr = RemotePr {
            number: 99,
            title: "some title".into(),
            branch: "br".into(),
            url: String::new(),
        };
        let line = pr.fzf_line();
        assert_eq!(line, "#99  some title  [br]");
        assert_eq!(number_from_fzf_line(&line), Some(99));
        assert_eq!(number_from_fzf_line("garbage"), None);
    }

    #[test]
    fn parse_checked_out_branches_maps_branch_to_path_skips_bare_and_detached() {
        // A real `git worktree list --porcelain`: a normal worktree under a *non-canonical* dir
        // name, the bare entry, and a detached worktree (no `branch` line).
        let listing = "\
worktree /home/u/projects/worktree/bn/cron-drift
HEAD a0a94e2
branch refs/heads/feat/cron-drift-detection

worktree /home/u/projects/bare/bn.git
bare

worktree /home/u/projects/worktree/bn/detached
HEAD deadbeef
detached
";
        let map = parse_checked_out_branches(listing);
        // The full branch name maps to its (non-canonical) worktree path.
        assert_eq!(
            map.get("feat/cron-drift-detection").map(String::as_str),
            Some("/home/u/projects/worktree/bn/cron-drift")
        );
        // Bare + detached carry no branch, so they don't appear.
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn existing_worktree_for_detects_branch_under_noncanonical_dir() {
        let mut checked_out = HashMap::new();
        checked_out.insert(
            "feat/cron-drift-detection".to_string(),
            "/wt/bn/cron-drift".to_string(),
        );
        let wt_dir = Path::new("/wt");
        // Branch checked out under a custom dir name → found (not offered as a candidate).
        assert_eq!(
            existing_worktree_for(&checked_out, wt_dir, "bn", "feat/cron-drift-detection"),
            Some("/wt/bn/cron-drift".to_string())
        );
        // A branch with no worktree and no canonical dir → None (it's a real candidate).
        assert_eq!(
            existing_worktree_for(&checked_out, wt_dir, "bn", "some/other-branch"),
            None
        );
    }

    #[test]
    fn decide_window_covers_all_states() {
        // No session yet → always create it, regardless of the other flags.
        assert_eq!(decide_window(false, false, false), WinAction::CreateSession);
        assert_eq!(decide_window(false, true, true), WinAction::CreateSession);
        // Session up, no window for this branch → add one.
        assert_eq!(decide_window(true, false, false), WinAction::NewWindow);
        assert_eq!(decide_window(true, false, true), WinAction::NewWindow);
        // Window exists: select it, unless forcing a fresh one.
        assert_eq!(decide_window(true, true, false), WinAction::SelectExisting);
        assert_eq!(decide_window(true, true, true), WinAction::ForceNewWindow);
    }

    #[test]
    fn window_cmds_create_session() {
        let cmds = window_tmux_cmds(
            &WinAction::CreateSession,
            "bn",
            "feat-x",
            "/wt/bn/feat-x",
            None,
        );
        assert_eq!(
            cmds,
            vec![vec![
                "new-session",
                "-d",
                "-s",
                "bn",
                "-c",
                "/wt/bn/feat-x",
                "-n",
                "feat-x"
            ]]
        );
    }

    #[test]
    fn window_cmds_append_command_when_set() {
        // With `cmd`, the new window runs it (an agent launch) instead of a shell — appended as the
        // trailing argument tmux interprets as the window's command.
        let cmds = window_tmux_cmds(
            &WinAction::CreateSession,
            "bn",
            "feat-x",
            "/wt/bn/feat-x",
            Some("agent copilot"),
        );
        assert_eq!(cmds[0].last().map(String::as_str), Some("agent copilot"));
        let cmds = window_tmux_cmds(
            &WinAction::NewWindow,
            "bn",
            "feat-x",
            "/wt/bn/feat-x",
            Some("agent copilot"),
        );
        assert_eq!(cmds[0].last().map(String::as_str), Some("agent copilot"));
    }

    #[test]
    fn window_cmds_keep_agent_pane_on_exit() {
        // A command-backed (agent) window gets a follow-up `remain-on-exit on` so a harness crash
        // leaves an inspectable `[exited]` pane instead of silently closing the whole window.
        let want: Vec<String> = [
            "set-window-option",
            "-t",
            "=bn:feat-x",
            "remain-on-exit",
            "on",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        for action in &[
            WinAction::CreateSession,
            WinAction::NewWindow,
            WinAction::ForceNewWindow,
        ] {
            let cmds = window_tmux_cmds(
                action,
                "bn",
                "feat-x",
                "/wt/bn/feat-x",
                Some("agent copilot"),
            );
            assert_eq!(
                cmds.last(),
                Some(&want),
                "agent window ({action:?}) must persist its pane on exit"
            );
        }
        // A plain shell window (no cmd) keeps tmux's default (close on exit) — no extra command.
        let shell = window_tmux_cmds(
            &WinAction::CreateSession,
            "bn",
            "feat-x",
            "/wt/bn/feat-x",
            None,
        );
        assert_eq!(
            shell.len(),
            1,
            "shell windows emit only the creation command"
        );
        assert!(!shell[0].iter().any(|a| a == "remain-on-exit"));
    }

    #[test]
    fn window_cmds_new_and_force_window_target_repo_session() {
        let want = vec![vec![
            "new-window".to_string(),
            "-d".into(),
            "-t".into(),
            // Session target with the trailing colon — `new-window` rejects the bare `=bn`.
            "=bn:".into(),
            "-c".into(),
            "/wt/bn/feat-x".into(),
            "-n".into(),
            "feat-x".into(),
        ]];
        assert_eq!(
            window_tmux_cmds(&WinAction::NewWindow, "bn", "feat-x", "/wt/bn/feat-x", None),
            want
        );
        assert_eq!(
            window_tmux_cmds(
                &WinAction::ForceNewWindow,
                "bn",
                "feat-x",
                "/wt/bn/feat-x",
                None
            ),
            want
        );
    }

    #[test]
    fn window_cmds_select_existing_creates_nothing() {
        assert!(window_tmux_cmds(
            &WinAction::SelectExisting,
            "bn",
            "feat-x",
            "/wt/bn/feat-x",
            None
        )
        .is_empty());
    }

    /// #498: an orchestrator/planning launch targets the shared cockpit session (default `cron`) —
    /// the new-session `-s` name and the `new-window -t` target both carry it, not the repo.
    #[test]
    fn window_cmds_honor_the_orchestrator_session() {
        assert_eq!(ORCHESTRATOR_SESSION, "cron");
        let create = window_tmux_cmds(
            &WinAction::CreateSession,
            ORCHESTRATOR_SESSION,
            "orch-338",
            "/wt/bn/orch-338",
            None,
        );
        assert_eq!(
            create,
            vec![vec![
                "new-session".to_string(),
                "-d".into(),
                "-s".into(),
                "cron".into(),
                "-c".into(),
                "/wt/bn/orch-338".into(),
                "-n".into(),
                "orch-338".into(),
            ]]
        );
        let newwin = window_tmux_cmds(
            &WinAction::NewWindow,
            ORCHESTRATOR_SESSION,
            "orch-338",
            "/wt/bn/orch-338",
            None,
        );
        assert!(
            newwin[0].contains(&"=cron:".to_string()),
            "new-window must target the cockpit session, got {newwin:?}"
        );
    }

    /// Integration: the argv `window_tmux_cmds` emits must be *accepted by real tmux*. The pure
    /// unit tests above can't catch a target string tmux rejects — which is exactly how the `=bn`
    /// (missing colon) `new-window` bug passed CI while opening no window. Runs on a private socket
    /// so it can't touch a real server; skips when tmux isn't installed (tmux-less CI stays green).
    #[test]
    fn window_cmds_run_against_real_tmux() {
        let have_tmux = tmux_command(&["-V"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !have_tmux {
            eprintln!("skip window_cmds_run_against_real_tmux: tmux not on PATH");
            return;
        }
        let sock = format!("bn-wt-it-{}", std::process::id());
        let repo = "bnwtit";
        let run = |extra: &[String]| -> std::process::Output {
            let mut args = vec!["-L".to_string(), sock.clone()];
            args.extend_from_slice(extra);
            tmux_command(&args).output().expect("tmux runs")
        };

        // First worktree → CreateSession; second in the same session → NewWindow (the broken path).
        let create = window_tmux_cmds(&WinAction::CreateSession, repo, "first", "/tmp", None);
        assert!(
            run(&create[0]).status.success(),
            "new-session argv rejected by tmux"
        );
        let newwin = window_tmux_cmds(&WinAction::NewWindow, repo, "second", "/tmp", None);
        let out = run(&newwin[0]);
        let created_ok = out.status.success();
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();

        // Read the window list back, then always tear the private server down.
        let list = run(&[
            "list-windows".into(),
            "-t".into(),
            format!("={repo}:"),
            "-F".into(),
            "#{window_name}".into(),
        ]);
        let windows = String::from_utf8_lossy(&list.stdout).to_string();
        let _ = run(&["kill-server".into()]);

        assert!(created_ok, "new-window argv rejected by tmux: {stderr}");
        assert!(
            windows.lines().any(|w| w == "second"),
            "expected a 'second' window, got: [{}]",
            windows.replace('\n', ",")
        );
    }

    #[test]
    fn existing_worktree_branches_lists_recent_first_with_main_last() {
        let tmp = std::env::temp_dir().join(format!("bn_wt_open_test_{}", std::process::id()));
        let repo = tmp.join("myrepo");
        // Created oldest → newest, so `feat-b` is the most recently touched.
        for dir in ["main", "feat-a", "feat-b"] {
            std::fs::create_dir_all(repo.join(dir)).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        std::fs::create_dir_all(repo.join("_pool").join("1")).unwrap(); // the pool isn't a branch
        std::fs::write(repo.join("a-file"), b"x").unwrap(); // files are ignored
        let got = existing_worktree_branches(&tmp, "myrepo");
        let _ = std::fs::remove_dir_all(&tmp);
        // Most-recent worktree first; the main checkout is never the default landing spot.
        assert_eq!(
            got,
            vec![
                "feat-b".to_string(),
                "feat-a".to_string(),
                "main".to_string()
            ]
        );
    }

    #[test]
    fn existing_worktree_branches_empty_when_no_repo_dir() {
        let tmp = std::env::temp_dir().join("bn_wt_open_nope_dir_xyz");
        assert!(existing_worktree_branches(&tmp, "missing").is_empty());
    }

    #[test]
    fn wt_path_components_split_repo_and_branch() {
        // `<worktree_dir>/<repo>/<branch>` → branch is the leaf, repo its parent.
        assert_eq!(wt_branch_name("/p/worktree/bn/feat-x"), "feat-x");
        assert_eq!(wt_repo_name("/p/worktree/bn/feat-x"), "bn");
        // Sanitized (slashes already dashes on disk) folder names round-trip verbatim.
        assert_eq!(
            wt_branch_name("/p/worktree/bn/user-alice-fix"),
            "user-alice-fix"
        );
    }

    // Overlap→ordering is the pure core of `merge --all`: disjoint file sets batch (singleton
    // clusters), any shared file forces a serialize cluster.
    fn pr(branch: &str, files: &[&str]) -> (String, Vec<String>) {
        (
            branch.to_string(),
            files.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn merge_clusters_batches_disjoint_and_orders_overlapping() {
        // A & B both touch store.rs → one serialize cluster; C is disjoint → its own batchable one.
        let prs = [
            pr("a", &["crates/store.rs"]),
            pr("b", &["crates/store.rs", "crates/other.rs"]),
            pr("c", &["crates/main.rs"]),
        ];
        let clusters = plan_merge_clusters(&prs);
        assert_eq!(
            clusters,
            vec![
                vec!["a".to_string(), "b".to_string()], // must be ordered (share store.rs)
                vec!["c".to_string()],                  // disjoint → batchable singleton
            ]
        );
    }

    #[test]
    fn merge_clusters_all_disjoint_are_all_singletons() {
        let prs = [pr("a", &["a.rs"]), pr("b", &["b.rs"]), pr("c", &["c.rs"])];
        let clusters = plan_merge_clusters(&prs);
        assert_eq!(
            clusters,
            vec![
                vec!["a".to_string()],
                vec!["b".to_string()],
                vec!["c".to_string()],
            ],
            "no shared files → every PR batches independently"
        );
    }

    #[test]
    fn merge_clusters_overlap_is_transitive() {
        // a–b share x, b–c share y → a,b,c collapse into one ordered cluster even though a,c are
        // themselves file-disjoint.
        let prs = [
            pr("a", &["x.rs"]),
            pr("c", &["y.rs"]),
            pr("b", &["x.rs", "y.rs"]),
        ];
        let clusters = plan_merge_clusters(&prs);
        assert_eq!(
            clusters,
            vec![vec!["a".to_string(), "b".to_string(), "c".to_string()]]
        );
    }

    #[test]
    fn merge_clusters_empty_input_is_empty() {
        assert!(plan_merge_clusters(&[]).is_empty());
    }

    // union_resolve_conflicts is the safety core of the merge-queue auto-rebase: keep-both-sides is
    // only allowed for pure insert/insert hunks (empty diff3 base) — the Commands-enum signature.

    #[test]
    fn union_resolves_pure_insert_insert_keeping_both_sides() {
        let conflicted = "enum Commands {\n    A,\n<<<<<<< HEAD\n    B,\n||||||| merged common ancestors\n=======\n    C,\n>>>>>>> feat: add C\n}\n";
        assert_eq!(
            union_resolve_conflicts(conflicted).as_deref(),
            Some("enum Commands {\n    A,\n    B,\n    C,\n}\n"),
            "both added variants survive, ours (main) first"
        );
    }

    #[test]
    fn union_resolves_multiple_pure_hunks() {
        let conflicted = "<<<<<<< HEAD\na\n|||||||\n=======\nb\n>>>>>>> x\nmid\n<<<<<<< HEAD\nc\n|||||||\n=======\nd\n>>>>>>> x\n";
        assert_eq!(
            union_resolve_conflicts(conflicted).as_deref(),
            Some("a\nb\nmid\nc\nd\n")
        );
    }

    #[test]
    fn union_refuses_nonempty_base() {
        // Both sides rewrote an existing line — keeping both would duplicate/derange logic.
        let conflicted = "<<<<<<< HEAD\nours\n||||||| base\noriginal\n=======\ntheirs\n>>>>>>> x\n";
        assert_eq!(union_resolve_conflicts(conflicted), None);
    }

    #[test]
    fn union_refuses_when_any_hunk_is_dirty() {
        // One pure hunk + one both-modified hunk → the whole file is refused.
        let conflicted = "<<<<<<< HEAD\na\n|||||||\n=======\nb\n>>>>>>> x\n<<<<<<< HEAD\nc\n||||||| base\nold\n=======\nd\n>>>>>>> x\n";
        assert_eq!(union_resolve_conflicts(conflicted), None);
    }

    #[test]
    fn union_refuses_merge_style_markers() {
        // No diff3 base section — we can't verify the insert/insert shape, so refuse.
        let conflicted = "<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> x\n";
        assert_eq!(union_resolve_conflicts(conflicted), None);
    }

    #[test]
    fn union_refuses_files_without_markers() {
        // Unmerged but no textual markers (binary, delete/modify) → nothing safe to do.
        assert_eq!(union_resolve_conflicts("plain file\nno conflict\n"), None);
    }

    #[test]
    fn union_refuses_truncated_conflict() {
        let conflicted = "<<<<<<< HEAD\nours\n|||||||\n=======\ntheirs\n"; // no closing marker
        assert_eq!(union_resolve_conflicts(conflicted), None);
    }

    // The gate verdict line is the human-facing surface of `bn gate` / the pre-merge block, so its
    // rendering branches (green vs stale-base vs conflict vs red-verify) are pinned here.
    fn gate_result(pass: bool, behind: bool, code: i32, reason: &str) -> GateResult {
        GateResult {
            pass,
            job: "test".into(),
            code,
            base_short: "abc123def456".into(),
            rebased_short: "789fedcba000".into(),
            behind,
            reason: reason.into(),
        }
    }

    #[test]
    fn short_sha_takes_twelve() {
        assert_eq!(short_sha("0123456789abcdef0123"), "0123456789ab");
        assert_eq!(short_sha("abc"), "abc"); // shorter is returned whole
    }

    #[test]
    fn gate_summary_pass_notes_current_base() {
        let s = gate_summary(&gate_result(true, false, 0, ""));
        assert!(s.contains("PASS ✓"), "{s}");
        assert!(s.contains("current with origin/main@abc123def456"), "{s}");
        assert!(s.contains("verify green on 789fedcba000"), "{s}");
    }

    #[test]
    fn gate_summary_pass_flags_a_stale_base_that_was_rebased() {
        // The whole point: a branch behind main is rebased forward before verify, and the verdict
        // says so — this is the stale-base case the gate catches.
        let s = gate_summary(&gate_result(true, true, 0, ""));
        assert!(
            s.contains("rebased onto origin/main@abc123def456 (base had advanced)"),
            "{s}"
        );
    }

    #[test]
    fn gate_summary_fail_prefers_reason_over_verify_code() {
        // A setup/rebase failure carries a reason; it wins over the generic "verify red" line.
        let s = gate_summary(&gate_result(
            false,
            true,
            1,
            "rebase onto origin/main: conflict — rebase manually (escalate)",
        ));
        assert!(s.contains("FAIL ✗"), "{s}");
        assert!(s.contains("rebase manually (escalate)"), "{s}");
        assert!(!s.contains("verify red"), "{s}");
    }

    #[test]
    fn gate_summary_fail_reports_verify_exit_when_no_reason() {
        let s = gate_summary(&gate_result(false, false, 101, ""));
        assert!(s.contains("FAIL ✗"), "{s}");
        assert!(s.contains("verify red (exit 101) on 789fedcba000"), "{s}");
    }
}
