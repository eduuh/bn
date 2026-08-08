//! `bn wt close` — the `prefix x` action: close **this worktree**.
//!
//! One keystroke that hands a pool slot's lease back (or runs the normal `wt cleanup` gates for a
//! conventional worktree) and takes the worktree's tmux window with it. Two invariants shape
//! everything here:
//!
//! 1. **It never closes a session.** tmux destroys a session when its last window goes, so the
//!    teardown routes through [`super::window::kill_tmux_windows_into`], which respawns a last
//!    window instead of killing it.
//! 2. **It never prints on the happy path.** tmux's `run-shell` surfaces a command's stdout in a
//!    popup view window, so a chatty command would flash a pane of text on every keypress. Outcomes
//!    are reported with `display-message` instead — the `jump.rs` pattern.
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
    /// The tmux confirm prompt came back "y".
    pub confirmed: bool,
}

/// What `prefix x` does with this worktree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ClosePlan {
    /// Not ours to reclaim — behave like tmux's built-in and just close the pane. The key is never
    /// worse than the binding it replaced.
    KillPaneOnly(&'static str),
    /// Uncommitted or unpushed work: ask before anything is removed.
    Confirm {
        branch: String,
        dirty: usize,
        unpushed: u32,
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
    if (f.dirty > 0 || f.unpushed > 0) && !f.confirmed {
        return ClosePlan::Confirm {
            branch: f.branch.clone(),
            dirty: f.dirty,
            unpushed: f.unpushed,
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

/// Wait for tmux to finish reaping `wins`, bounded. This is what makes the lease release honest:
/// occupancy is derived from live tmux windows and panes, and an unregistered harness pane can only
/// be cleared by actually dying — so we release *after* the windows are gone, not before.
fn await_windows_gone(wins: &[String]) {
    for _ in 0..20 {
        let Some(live) = tmux(&["list-windows", "-a", "-F", "#{window_id}"]) else {
            return;
        };
        let live: Vec<&str> = live.lines().map(str::trim).collect();
        if !wins.iter().any(|w| live.contains(&w.as_str())) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
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
    let fallback = respawn_fallback(&worktree_dir, &repo);

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
        } => {
            flash(&format!(
                "{branch}: {dirty} uncommitted, {unpushed} unpushed — `bn wt close --confirmed` to close anyway"
            ));
            false
        }
        ClosePlan::ReleaseSlot { slot, discard } => {
            let slot_root = crate::wt_pool::pool_slot_root(&wt_path).unwrap_or(wt_path.clone());
            let wins = tmux_windows_for(&slot_root, &facts.branch);
            kill_tmux_windows_into(&wins, false, &fallback);
            await_windows_gone(&wins);
            if discard {
                // The human confirmed discarding the dirty files in a reusable slot.
                let ok = crate::wt_pool::return_slot(cfg, &slot_root, true);
                flash(&if ok {
                    format!("returned pool slot {slot} ({repo}) — discarded uncommitted changes")
                } else {
                    format!("pool slot {slot} ({repo}) could not be returned")
                });
                return ok;
            }
            let outcome = crate::wt_pool::release_dispatch_slot(cfg, &slot_root, None);
            if let Some(msg) = outcome.message() {
                flash(&msg);
            }
            matches!(
                outcome,
                crate::wt_pool::SlotRelease::Returned { .. }
                    | crate::wt_pool::SlotRelease::NotLeased { .. }
            )
        }
        ClosePlan::CleanupWorktree => {
            // `cleanup_one` kills the worktree's windows itself, under the same session guard.
            match cleanup_one(cfg, &wt_path, &worktree_dir, false) {
                Ok(()) => {
                    flash(&format!("closed {repo}/{}", facts.branch));
                    true
                }
                Err(()) => {
                    flash(&format!(
                        "{}: not reclaimable — see `bn wt cleanup` (or `bn doctor`)",
                        facts.branch
                    ));
                    false
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "close_tests.rs"]
mod close_tests;
