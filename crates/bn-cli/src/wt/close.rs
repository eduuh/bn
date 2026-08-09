//! `bn wt close` — the `prefix x` action: close **this worktree**.
//!
//! The keystroke is split in two, and the split is the design:
//!
//! * **Now** — drop the worktree's tmux window. This is all a human is waiting for, and it is a
//!   handful of tmux calls.
//! * **Later** — reclaim the worktree: hand a pool slot's lease back, or run the normal `wt cleanup`
//!   gates for a conventional one. That half is *not* keystroke-sized — a warm reset re-checks-out
//!   the tree, and the cleanup gates make a `gh pr view` round-trip — so it is filed on the machine
//!   queue as a [`BacklogKind::Cleanup`] entry and runs when the machine has a moment. `bn wt close
//!   --reclaim` is the deferred half, and the queued command is exactly that.
//!
//! Deferring also dissolves an ordering constraint the inline version had to fight. Slot occupancy
//! is derived from live tmux windows, so releasing a lease before the window is gone is refused as
//! "slot in use" — the inline version therefore polled tmux for the window to be reaped, on the
//! keystroke path. Queued, the window has been gone for as long as the queue took to get here.
//!
//! Three invariants shape everything below:
//!
//! 1. **It never closes a session.** tmux destroys a session when its last window goes, so the
//!    teardown routes through [`super::window::kill_tmux_windows_into`], which respawns a last
//!    window instead of killing it.
//! 2. **It never prints on the happy path.** tmux's `run-shell` surfaces a command's stdout in a
//!    popup view window, so a chatty command would flash a pane of text on every keypress. Outcomes
//!    are reported with `display-message` instead — the `jump.rs` pattern. `--reclaim` is the
//!    exception: it runs headless under the queue, whose log wants stdout.
//! 3. **It never blocks on work a human is not waiting for.** See the split above.
//!
//! The decision is a pure function ([`close_plan`]) over facts gathered up front, because the
//! pane's cwd is the only thing that identifies the worktree and it must be read *before* anything
//! is torn down.

use super::*;

/// Everything the close decision depends on, gathered before any teardown.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CloseFacts {
    /// The path resolves under a bn-managed bare repo.
    pub managed: bool,
    /// `main`/`master` — the checkout that hosts no agents and is never reclaimed.
    pub reserved: bool,
    /// Pool slot number, for a path under `<repo>/_pool/<n>`.
    pub slot: Option<u32>,
    pub branch: String,
    /// Uncommitted entries (modified + untracked).
    pub dirty: usize,
    /// Commits not yet on the upstream (or, with no upstream, ahead of the default branch).
    pub unpushed: u32,
    /// A live agent working in the checkout (its one-line description), if any. A clean tree is
    /// not the same thing as an idle worktree: an agent mid-turn — or a human typing a prompt
    /// into one — has nothing uncommitted yet, and closing on that basis kills it silently.
    pub occupant: Option<String>,
    /// The tmux confirm prompt came back "y".
    pub confirmed: bool,
}

/// What `prefix x` does with this worktree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ClosePlan {
    /// Not ours to reclaim — behave like tmux's built-in and just close the pane. The key is never
    /// worse than the binding it replaced.
    KillPaneOnly(&'static str),
    /// Uncommitted or unpushed work, or a live agent in the checkout: ask before anything is
    /// removed.
    Confirm {
        branch: String,
        dirty: usize,
        unpushed: u32,
        /// The live agent the close would kill, if that is what triggered the prompt.
        occupant: Option<String>,
    },
    /// A pool slot: hand the lease back and warm-reset the checkout. `discard` is set only when the
    /// human confirmed throwing away uncommitted work in a *reusable* slot — no branch is deleted.
    ReleaseSlot { slot: u32, discard: bool },
    /// A conventional worktree: run `wt cleanup`'s normal gates (merged PR, clean tree, no open
    /// todos). Never forced — see [`close_plan`].
    CleanupWorktree,
}

/// The decision. Pure over its inputs so every branch is testable without git or tmux.
///
/// Note the asymmetry in what a confirmation authorises. In a **pool slot** it means "discard the
/// dirty files" — `warm_reset` restores the checkout and the branch survives, so the blast radius
/// is exactly those files. In a **conventional worktree** it authorises nothing extra: forcing
/// there would `git branch -D` and destroy unpushed commits, so cleanup runs under its normal
/// gates, refuses, and raises the existing `wt-stuck:<repo>/<branch>` attention item.
/// `bn wt cleanup --force` stays the deliberate destroy.
pub(crate) fn close_plan(f: &CloseFacts) -> ClosePlan {
    if !f.managed {
        return ClosePlan::KillPaneOnly("not a managed worktree");
    }
    if f.reserved {
        return ClosePlan::KillPaneOnly("main worktree");
    }
    if f.slot == Some(bn_core::agent::CONTROLLER_POOL_SLOT) {
        // The one gate `evict_occupant` refuses to let `--force` past. A keybinding must not become
        // a quieter way around it.
        return ClosePlan::KillPaneOnly("controller slot");
    }
    // A live agent counts as work in flight exactly like dirty files do. This is the gate that was
    // missing when `prefix x` killed an agent mid-prompt: its tree was clean, so the close ran on
    // the keystroke, took every window in the slot with it, and the queued reclaim reset the
    // checkout seconds later.
    if (f.dirty > 0 || f.unpushed > 0 || f.occupant.is_some()) && !f.confirmed {
        return ClosePlan::Confirm {
            branch: f.branch.clone(),
            dirty: f.dirty,
            unpushed: f.unpushed,
            occupant: f.occupant.clone(),
        };
    }
    match f.slot {
        Some(slot) => ClosePlan::ReleaseSlot {
            slot,
            discard: f.confirmed,
        },
        None => ClosePlan::CleanupWorktree,
    }
}

/// Whether this plan needs the tmux confirm prompt. The `--check` exit status *is* this predicate,
/// so the tmux.conf branch and the Rust branch cannot drift apart.
pub(crate) fn needs_confirm(plan: &ClosePlan) -> bool {
    matches!(plan, ClosePlan::Confirm { .. })
}

/// Unpushed commits: the tracking branch if there is one, else ahead-of-default. Same rule as
/// [`crate::reap`]'s work-in-progress check, so the two can't disagree about what "unpushed" means.
fn unpushed_count(wt: &Path) -> u32 {
    let wt_s = wt.to_string_lossy();
    if let Some(n) = git_out(&["-C", &wt_s, "rev-list", "--count", "@{upstream}..HEAD"])
        .and_then(|s| s.trim().parse::<u32>().ok())
    {
        return n;
    }
    commits_ahead(wt)
}

/// Gather [`CloseFacts`] for a path. All local git — no `gh` call in the keystroke path; the PR
/// gate only runs later, inside `cleanup_one`, where it already lives.
fn probe(path: &Path, confirmed: bool) -> CloseFacts {
    let Some(_bare) = find_bare_repo(path) else {
        return CloseFacts::default();
    };
    let slot_root = crate::wt_pool::pool_slot_root(path);
    let wt_root = slot_root.clone().unwrap_or_else(|| path.to_path_buf());
    let slot = slot_root.as_deref().and_then(slot_number);
    let branch = git_out(&["-C", &wt_root.to_string_lossy(), "branch", "--show-current"])
        .unwrap_or_default();
    CloseFacts {
        managed: true,
        // A detached slot (branch deleted underneath) is not "main" — only a real reserved branch
        // name is, and only when we could read one.
        reserved: !branch.is_empty() && bn_core::agent::is_reserved_branch(&branch),
        slot,
        branch,
        dirty: dirty_count(&wt_root.to_string_lossy()),
        unpushed: unpushed_count(&wt_root),
        // The same occupancy oracle the pool's lease paths read, asked *before* any window is
        // torn down — afterwards there is nothing left to observe. Also re-asked when the queued
        // reclaim runs, where it guards against the slot having been re-leased to a new live
        // agent between the keystroke and the queue getting here.
        occupant: crate::agent_lease::live_agents_in(&wt_root)
            .into_iter()
            .next()
            .map(|o| o.describe()),
        confirmed,
    }
}

/// The `<n>` of a `.../_pool/<n>` slot root.
fn slot_number(slot: &Path) -> Option<u32> {
    slot.file_name()?.to_string_lossy().parse().ok()
}

/// One-line feedback in the tmux status line. The only user-facing channel this command has.
fn flash(msg: &str) {
    let _ = tmux_run(&["display-message", msg]);
}

/// Close a pane, without ever taking a session down with it. A pane that is the last one in its
/// window is closed as a *window* instead, so the session guard in [`super::window`] applies.
fn close_pane(pane: &str, fallback: &Path) {
    let window = tmux(&["display-message", "-p", "-t", pane, "#{window_id}"]);
    let siblings = window
        .as_deref()
        .and_then(|w| tmux(&["list-panes", "-t", w, "-F", "#{pane_id}"]))
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    match (window, siblings) {
        // Last pane in its window: closing it closes the window, which may close the session.
        (Some(w), 0 | 1) => kill_tmux_windows_into(&[w], false, fallback),
        _ => {
            let _ = tmux_run(&["kill-pane", "-t", pane]);
        }
    }
}

/// `bn wt close` — see the module docs. Returns the CLI's exit semantics; with `--check` the return
/// value *is* the answer ("does this need a prompt?"), and nothing is torn down.
pub(crate) fn cmd_close(
    cfg: &Config,
    check: bool,
    pane: Option<String>,
    path: Option<String>,
    confirmed: bool,
    reclaim: bool,
) -> bool {
    let wt_path = path.map(PathBuf::from).unwrap_or_else(cwd);
    let facts = probe(&wt_path, confirmed);
    let plan = close_plan(&facts);
    if check {
        return needs_confirm(&plan);
    }

    let (_, worktree_dir) = layout(cfg);
    let repo = find_bare_repo(&wt_path)
        .map(|b| repo_name(&b))
        .unwrap_or_default();
    if reclaim {
        return cmd_reclaim(cfg, &wt_path, &worktree_dir, &repo, &facts, plan);
    }
    let fallback = respawn_fallback(&worktree_dir, &repo);
    let label = label_for(&wt_path, &facts.branch);

    match plan {
        ClosePlan::KillPaneOnly(_) => {
            if let Some(pane) = pane {
                close_pane(&pane, &fallback);
            }
            true
        }
        // Only reachable on a hand-run without `--check` — the binding asks first. Say why and
        // touch nothing.
        ClosePlan::Confirm {
            branch,
            dirty,
            unpushed,
            occupant,
        } => {
            flash(&match occupant {
                Some(who) => format!(
                    "{branch}: a live {who} is working here — `bn wt close --confirmed` to close anyway"
                ),
                None => format!(
                    "{branch}: {dirty} uncommitted, {unpushed} unpushed — `bn wt close --confirmed` to close anyway"
                ),
            });
            false
        }
        // Both reclaimable plans do the same two things here, in this order: take the window now,
        // file the expensive half for later. Which reclaim it turns out to be is re-decided when
        // the queue runs it, by the same `close_plan`.
        ClosePlan::ReleaseSlot { .. } | ClosePlan::CleanupWorktree => {
            let root = target_root(&wt_path);
            let wins = tmux_windows_for(&root, &facts.branch);
            kill_tmux_windows_into(&wins, false, &fallback);
            flash(&reclaim::defer(cfg, &repo, &label, &root, confirmed));
            true
        }
    }
}

/// `bn wt close --reclaim` — the deferred half, and what the queued `cleanup` entry runs.
///
/// Touches no tmux (the window went with the keystroke) and queues nothing (it *is* the queued
/// work). Unlike the keystroke path it prints: it runs headless under the dispatcher, whose log is
/// the only place its outcome can be read.
fn cmd_reclaim(
    cfg: &Config,
    wt_path: &Path,
    worktree_dir: &Path,
    repo: &str,
    facts: &CloseFacts,
    plan: ClosePlan,
) -> bool {
    // What the human's "y" actually authorised. Only a confirmed *pool slot* discards anything —
    // outside one a confirmation buys no extra power, so cleanup runs under its own gates. This is
    // [`close_plan`]'s asymmetry, read off the plan rather than re-derived.
    let force = matches!(plan, ClosePlan::ReleaseSlot { discard: true, .. });
    match plan {
        ClosePlan::KillPaneOnly(why) => {
            println!("nothing to reclaim at {} — {why}", wt_path.display());
            true
        }
        // Work — or a live agent — appeared after the window closed. Never discard either on a
        // decision the human made about a different tree; leave the worktree alone and say so.
        // The occupant case is the enqueue-to-run race: the slot was free when the keystroke
        // filed this entry, and has since been re-leased to a new agent.
        ClosePlan::Confirm {
            branch,
            dirty,
            unpushed,
            occupant,
        } => {
            match occupant {
                Some(who) => println!(
                    "{branch}: a live {who} is working here now — left alone; \
                     `bn wt close --reclaim --confirmed --path {}` to reclaim it anyway",
                    wt_path.display()
                ),
                None => println!(
                    "{branch}: {dirty} uncommitted, {unpushed} unpushed since it was closed — \
                     left alone; `bn wt close --reclaim --confirmed --path {}` to reclaim it anyway",
                    wt_path.display()
                ),
            }
            false
        }
        // Both reclaimable plans are the same call. `cleanup_one` is the one reclaim path — it
        // routes a pool slot to its lease hand-back and a conventional worktree to the gates — so
        // `prefix x` does not get a private copy of either. The only thing this branch still decides
        // is what the human's confirmation bought, and `close_plan` has already worked that out:
        // discarding a reusable slot's files, and nothing whatsoever outside one.
        ClosePlan::ReleaseSlot { .. } | ClosePlan::CleanupWorktree => {
            let label = label_for(wt_path, &facts.branch);
            match cleanup_one(cfg, &target_root(wt_path), worktree_dir, force) {
                Ok(()) => {
                    println!("reclaimed {repo}/{label}");
                    true
                }
                Err(()) => {
                    println!("{repo}/{label}: not reclaimable — see `bn wt cleanup` (or `bn doctor`)");
                    false
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "close_tests.rs"]
mod close_tests;
