//! The sub-picker's guarded key bindings — `ctrl-d` delete, `ctrl-w` warm, `ctrl-r` release.
//!
//! Split out of `tat.rs` along the seam the bindings already draw (the 1000-line ratchet; see
//! `.file-size-baseline`): `tat.rs` paints the list and routes a selection, and this module holds
//! what a key does to the focused row *without* leaving the list. Each handler is guarded the same
//! way, and for the same reason: fzf fires a binding on whatever happens to be focused, so a stage
//! that trusts its payload will one day be handed the wrong row — or, once, the repo's `main`
//! checkout.
use super::{exact_session_target, has_session, pool_eligible, ref_repo, ref_session, Roots};
use crate::proc::{tmux, tmux_run};
use bn_core::resolver::Config;
use std::path::Path;
use std::process::Command;

/// The guard for `ctrl-d` delete: an idle worktree row is only removable when it still exists, is
/// clean, and has no unpushed commits. Returns `Err(reason)` (shown to the user) otherwise. Pure so
/// it can be unit-tested without touching git.
///
/// `name` is the worktree directory. The repo's `main` checkout is refused outright: the picker
/// never lists it as a row (see `discovery::scan_worktrees`), but this stage is reachable directly,
/// and removing that checkout also lets the `lifecycle::prune` that follows close the repo's main
/// note — both found the hard way, by handing this stage `worktree~main`.
fn delete_block(
    kind: &str,
    name: &str,
    exists: bool,
    dirty: usize,
    unpushed: u32,
) -> Result<(), String> {
    if kind != "worktree" {
        return Err(format!(
            "ctrl-d deletes an idle worktree — a {} row can't be removed here",
            if kind.is_empty() { "that" } else { kind }
        ));
    }
    if bn_core::agent::is_reserved_branch(name) {
        return Err(format!(
            "{name} is the repo's main checkout — it stays; work on main happens in the main slot"
        ));
    }
    if !exists {
        return Err("that worktree is already gone".into());
    }
    if dirty > 0 {
        return Err(format!(
            "{dirty} uncommitted change{} — commit or discard first",
            if dirty == 1 { "" } else { "s" }
        ));
    }
    if unpushed > 0 {
        return Err(format!(
            "{unpushed} unpushed commit{} — push or merge first",
            if unpushed == 1 { "" } else { "s" }
        ));
    }
    Ok(())
}

/// Count commits on the worktree's `HEAD` not yet on its remote, so `ctrl-d` won't silently drop
/// unpushed work. Tries the configured upstream, then a same-named `origin/<branch>`, then the
/// remote's default branch; returns 0 when no remote base resolves (a fresh local-only branch — the
/// dirty guard still protects uncommitted work there).
fn unpushed_commits(wt: &Path) -> u32 {
    let count = |range: &str| {
        crate::git::git_capture(&["rev-list", "--count", range], Some(wt), true)
            .and_then(|s| s.trim().parse::<u32>().ok())
    };
    if let Some(n) = count("@{u}..HEAD") {
        return n;
    }
    let branch = crate::git::git_capture(&["rev-parse", "--abbrev-ref", "HEAD"], Some(wt), true)
        .unwrap_or_default();
    if !branch.is_empty() && branch != "HEAD" {
        let ob = format!("origin/{branch}");
        if crate::git::git_capture(&["rev-parse", "--verify", "--quiet", &ob], Some(wt), true)
            .is_some()
        {
            if let Some(n) = count(&format!("{ob}..HEAD")) {
                return n;
            }
        }
    }
    if let Some(base) = crate::git::git_capture(
        &["rev-parse", "--abbrev-ref", "origin/HEAD"],
        Some(wt),
        true,
    ) {
        if !base.is_empty() {
            if let Some(n) = count(&format!("{base}..HEAD")) {
                return n;
            }
        }
    }
    0
}

/// `ctrl-d` handler: remove the focused idle worktree after the [`delete_block`] guard clears. `arg`
/// is fzf's `{1}~{2}` (`kind~payload`); only a `worktree` kind is removable. Refusals print a `✗`
/// reason and `hold()` so the message survives the fzf `reload`.
pub(super) fn rm_worktree(cfg: &Config, roots: &Roots, project: &str, arg: &str) {
    let (kind, payload) = arg.split_once('~').unwrap_or(("", arg));
    let repo = ref_repo(project);
    let wt_path = roots.worktree_dir(project).join(repo).join(payload);
    let exists = wt_path.is_dir();
    let wt_s = wt_path.to_string_lossy().to_string();
    let dirty = if exists {
        crate::wt::dirty_count(&wt_s)
    } else {
        0
    };
    let unpushed = if exists && dirty == 0 {
        unpushed_commits(&wt_path)
    } else {
        0
    };
    if let Err(reason) = delete_block(kind, payload, exists, dirty, unpushed) {
        eprintln!("✗ {reason}");
        crate::picker::hold();
        return;
    }
    let Some(bare) = crate::wt::find_bare_repo(&wt_path) else {
        eprintln!("✗ couldn't locate the bare repo for {repo}/{payload}");
        crate::picker::hold();
        return;
    };
    let ok = crate::wt::run_git(&[
        "--git-dir",
        &bare.to_string_lossy(),
        "worktree",
        "remove",
        &wt_s,
    ]);
    if ok {
        crate::wt::rmdir_if_empty_repo(&wt_path, &roots.worktree_dir(project));
        let _ = bn_core::lifecycle::prune(cfg);
        println!("✓ removed {repo}/{payload}");
    } else {
        eprintln!("✗ git refused to remove {repo}/{payload} (locked or raced dirty)");
        crate::picker::hold();
    }
}

/// `ctrl-w` handler: warm the focused repo's pool up to its configured size via `bn wt pool warm`
/// (which, inside tmux, spawns a `pool-warm` window to build in — so this returns promptly and the
/// picker can reload). Refuses only the refs pools don't apply to: tagged and `--root win` ones.
pub(super) fn warm_pool(roots: &Roots, project: &str) {
    let repo = ref_repo(project);
    if !pool_eligible(roots, project) {
        eprintln!("✗ {repo} takes the cold path — pools are a primary-root, untagged concept");
        crate::picker::hold();
        return;
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let ok = Command::new(&exe)
        .args(["wt", "pool", "warm", "--repo", repo])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("✗ pool warm failed for {repo}");
        crate::picker::hold();
    }
}

/// `ctrl-r` handler: release the focused *leased* slot back to the warm pool. `arg` is fzf's
/// `{1}~{2}` (`kind~branch`); only a `poolbranch` row is releasable. Resolves the branch to its
/// slot via [`bn_core::pool::pool_status`], then runs `bn wt pool return <slot-path>` *without*
/// `--force`, so an uncommitted slot is refused (the message + `hold()` survive the reload).
pub(super) fn release_pool(cfg: &Config, roots: &Roots, project: &str, arg: &str) {
    let (kind, branch) = arg.split_once('~').unwrap_or(("", arg));
    if kind != "poolbranch" {
        eprintln!("✗ ctrl-r releases a leased pool slot — focus a leased (◍) row");
        crate::picker::hold();
        return;
    }
    let repo = ref_repo(project);
    let status = bn_core::pool::pool_status(cfg, repo);
    let Some(slot) = status
        .slots
        .iter()
        .find(|s| s.branch.as_deref() == Some(branch))
    else {
        eprintln!("✗ no leased slot found for {branch}");
        crate::picker::hold();
        return;
    };
    let slot_path = roots
        .worktree_dir(project)
        .join(repo)
        .join("_pool")
        .join(slot.n.to_string());
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let ok = Command::new(&exe)
        .args(["wt", "pool", "return", &slot_path.to_string_lossy()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!(
            "✗ release refused for {branch} — commit/stash, or `bn wt pool return {} --force`",
            slot.n
        );
        crate::picker::hold();
    }
}

/// The session this process's pane belongs to, or `None` outside tmux.
///
/// Anchored on `$TMUX_PANE`, never on a bare `display-message`: with no `-t`, tmux answers for
/// whatever it considers current — the pane's own session when `TMUX_PANE` is in the
/// environment, but the most recently active session when it is not (a detached server, a
/// sandbox, a cron). The kill guard once compared against that answer and so refused to kill the
/// very session it had been asked to, because that session, being the newest, was "current".
fn own_session() -> Option<String> {
    let pane = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty())?;
    tmux(&["display-message", "-p", "-t", &pane, "#S"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// --- ctrl-x: kill the focused row's session ------------------------------------------------------------------

/// Kill `ref`'s session (passed via `{-1}`), unless it's the one this picker is running in.
pub(super) fn kill_from_line(r: &str) {
    if r.is_empty() {
        return;
    }
    let sess = ref_session(r);
    if own_session().is_some_and(|own| own == sess) {
        return;
    }
    if has_session(r) {
        let _ = tmux_run(&["kill-session", "-t", &exact_session_target(&sess)]);
    }
}

#[cfg(test)]
#[path = "delete_guard_tests.rs"]
mod tests;
