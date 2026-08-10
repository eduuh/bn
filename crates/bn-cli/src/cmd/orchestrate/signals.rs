//! Live-signal capture for `bn orchestrate` — the IO half that reads the world the pure policy in
//! [`bn_core::orchestrate`] then judges: the pane tail and presence, the tmux window listing, and
//! the git facts (WIP, commits ahead of the real base) about a lane's worktree.
//!
//! Split out of [`super`] to keep the supervisor file under the never-grow file-size ratchet, which
//! is satisfied by splitting a file rather than compressing it — and because "what is true about
//! this lane right now" is a coherent seam: everything here observes, nothing here decides.

use super::*;

/// An owned pane observation — the buffers a borrowed [`PaneObservation`] points at.
pub(super) struct Observed {
    pub(super) tail: Vec<String>,
    pub(super) has_window: bool,
    pub(super) has_process: bool,
    pub(super) plane_a: String,
    pub(super) has_wip: bool,
    /// A human is at this lane's window, now or within the decay window — the presence signal that
    /// vetoes every destructive recovery ([`bn_core::tmux::observe_presence`]).
    pub(super) attended: bool,
}

impl Observed {
    pub(super) fn as_obs(&self) -> PaneObservation<'_> {
        PaneObservation {
            tail: &self.tail,
            has_window: self.has_window,
            plane_a: &self.plane_a,
            has_wip: self.has_wip,
        }
    }
}

/// The tmux window name for an agent: `repo:sanitized-branch` (matching how `bn dispatch` opens it).
pub(super) fn tmux_target(repo: &str, branch: &str) -> String {
    format!("{repo}:{}", wt::sanitize_branch(branch))
}

/// The sanitized-branch window suffixes of `repo`'s **live agent windows** — the candidate standalone
/// dispatch lanes a supervisor adopts (#426). Thin delegate to the pure #455 policy
/// ([`bn_core::crossrepo::lane_windows_in_session`]): keeps windows in this repo's session
/// (`<repo>:<window>`), dropping the repo's own `main`/`master` checkout, warm `_pool` slots, and
/// empty names. The window name is the sanitized branch (`bn dispatch`'s naming), so this is the
/// tmux half of discovery; [`discover_lanes`] resolves each to its real branch on disk.
pub(super) fn lane_windows_for(repo: &str, windows: &[String]) -> Vec<String> {
    bn_core::crossrepo::lane_windows_in_session(repo, windows)
}

/// Discover this repo's live standalone `bn dispatch` lanes for supervisor adoption (#426): every
/// live agent window ([`lane_windows_for`]) whose worktree still exists on disk, resolved to its real
/// git branch (the form `bn dispatch --json` records and the daemon tracks). A window without a
/// worktree, or one whose branch can't be read, is skipped. Best-effort and read-only — a lane the
/// supervisor can't resolve is simply not adopted this pass (its PR still lands via `bn wt merge
/// --all`), never mis-recovered.
pub(super) fn discover_lanes(cfg: &Config, repo: &str, windows: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for win in lane_windows_for(repo, windows) {
        // The window name is the sanitized branch == the worktree dir name.
        let wt = cfg.worktree_dir.join(repo).join(&win);
        if !wt.join(".git").exists() {
            continue; // no live worktree — nothing to supervise (PRs still land unconditionally)
        }
        let branch = wt::git_out(&[
            "-C",
            &wt.to_string_lossy(),
            "rev-parse",
            "--abbrev-ref",
            "HEAD",
        ])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "HEAD")
        .unwrap_or(win);
        if !out.iter().any(|e: &String| e == &branch) {
            out.push(branch);
        }
    }
    out
}

/// Gather the live signals for one agent into an owned [`Observed`]: the pane tail, whether a live
/// window exists, its Plane-A `@bn_agent` state, and whether the worktree carries WIP.
pub(super) fn observe(
    cfg: &Config,
    repo: &str,
    branch: &str,
    windows: &[String],
    live_processes: &std::collections::BTreeSet<String>,
) -> Observed {
    let target = tmux_target(repo, branch);
    let has_window = windows.iter().any(|w| w.eq_ignore_ascii_case(&target));
    let has_process = live_processes.contains(&target.to_ascii_lowercase());
    let tail = if has_window {
        capture_pane_tail(&target, 8)
    } else {
        Vec::new()
    };
    let plane_a = crate::proc::tmux(&[
        "display-message",
        "-p",
        "-t",
        &format!("={target}"),
        "-F",
        "#{@bn_agent}",
    ])
    .unwrap_or_default();
    let wt = worktree_path(cfg, repo, branch);
    let has_wip = worktree_has_wip(&wt);
    // Only a live window can be watched; skip the tmux round-trip otherwise.
    let attended = has_window && bn_core::tmux::window_attended(&target);
    Observed {
        tail,
        has_window,
        has_process,
        plane_a,
        has_wip,
        attended,
    }
}

pub(super) fn lane_is_dead(has_window: bool, has_process: bool) -> bool {
    !has_window && !has_process
}

/// Whether a worktree carries genuine WIP the supervisor should recover — uncommitted changes, or
/// commits unique to HEAD vs **current** `origin/<default>` (#370 defect 3). The old check compared
/// against the stale *local* `main`/`master` ref (which a bare-repo worktree layout rarely updates),
/// so a clean branch that had advanced to current main read as "ahead of the old base" → false
/// `wip=true` → an infinite relaunch loop. Judging against `origin/<default>` (refreshed once per
/// tick by [`fetch_origin_default`]) means a clean tree at main's tip has zero unique commits and is
/// correctly `idle-clean`, never `idle-with-wip`.
pub(super) fn worktree_has_wip(wt: &Path) -> bool {
    if !wt.is_dir() {
        return false;
    }
    // Uncommitted work (staged, unstaged, or untracked) — `git status --porcelain` non-empty.
    if wt::dirty_count(&wt.to_string_lossy()) > 0 {
        return true;
    }
    // Committed-but-unmerged work — commits on HEAD not reachable from current origin/<default>.
    commits_ahead_of_origin_default(wt) > 0
}

/// Commits unique to the worktree's HEAD vs current `origin/<default>` (0 when HEAD sits at or
/// behind main's tip). Pure read against the already-fetched ref — no network here; the per-tick
/// refresh lives in [`fetch_origin_default`] so this stays cheap per agent.
pub(super) fn commits_ahead_of_origin_default(wt: &Path) -> u32 {
    let wts = wt.to_string_lossy();
    let base = format!("origin/{}", origin_default_branch(wt));
    wt::git_out(&["-C", &wts, "rev-list", "--count", &format!("{base}..HEAD")])
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

/// The remote's default branch name for a worktree (`origin/HEAD` → `main`), falling back to `main`
/// then `master` when `origin/HEAD` isn't set — so wip is judged against the branch the repo
/// actually merges into.
pub(super) fn origin_default_branch(wt: &Path) -> String {
    let wts = wt.to_string_lossy();
    if let Some(name) = wt::git_out(&[
        "-C",
        &wts,
        "symbolic-ref",
        "--short",
        "refs/remotes/origin/HEAD",
    ])
    .map(|s| s.trim().trim_start_matches("origin/").to_string())
    .filter(|s| !s.is_empty())
    {
        return name;
    }
    for cand in ["main", "master"] {
        if wt::git_out(&[
            "-C",
            &wts,
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("origin/{cand}"),
        ])
        .is_some()
        {
            return cand.to_string();
        }
    }
    "main".to_string()
}

/// Best-effort refresh of `origin/<default>` on the repo's shared bare, once per tick — so the wip
/// check ([`worktree_has_wip`]) is judged against the *live* main, not a ref last fetched at
/// dispatch time (#370 defect 3). Silent on failure (offline / DNS outage): the comparison then
/// falls back to the last-known origin tip, which is still current relative to the (also-stalled)
/// branch, so no false wip is introduced.
pub(super) fn fetch_origin_default(cfg: &Config, repo: &str) {
    if let Some(wt) = a_repo_worktree(cfg, repo) {
        let wts = wt.to_string_lossy().to_string();
        let default = origin_default_branch(&wt);
        let _ = wt::run_git(&["-C", &wts, "fetch", "--quiet", "origin", &default]);
    }
}

/// The last `n` non-empty lines of an agent pane's active pane, addressed by its exact window name.
pub(super) fn capture_pane_tail(target: &str, n: usize) -> Vec<String> {
    let out = crate::proc::tmux_raw(&["capture-pane", "-p", "-t", &format!("={target}")])
        .unwrap_or_default();
    let lines: Vec<String> = out
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].to_vec()
}

/// Every live tmux window as `session:window`, or `None` when tmux is unreachable (so callers can
/// distinguish "no windows" from "no tmux" and avoid a false `dead` classification). A missing
/// tmux server after reboot is an observable empty fleet, not an unreachable probe: recovery may
/// safely recreate the session through the normal fresh dispatch path.
pub(super) fn list_windows() -> Option<Vec<String>> {
    let out =
        crate::proc::tmux_command(&["list-windows", "-a", "-F", "#{session_name}:#{window_name}"])
            .output()
            .ok()?;
    classify_window_listing(
        out.status.success(),
        &String::from_utf8_lossy(&out.stdout),
        &String::from_utf8_lossy(&out.stderr),
    )
}

pub(super) fn classify_window_listing(success: bool, stdout: &str, stderr: &str) -> Option<Vec<String>> {
    if success {
        return Some(
            stdout
                .lines()
                .map(|line| line.trim().to_string())
                .filter(|line| !line.is_empty())
                .collect(),
        );
    }
    let error = stderr.to_ascii_lowercase();
    if error.contains("no server running")
        || error.contains("failed to connect to server")
        || error.contains("error connecting to")
    {
        return Some(Vec::new());
    }
    None
}

/// The worktree dir for an agent: `<worktree_dir>/<repo>/<sanitized-branch>`.
pub(super) fn worktree_path(cfg: &Config, repo: &str, branch: &str) -> PathBuf {
    cfg.worktree_dir
        .join(repo)
        .join(wt::sanitize_branch(branch))
}

/// Any existing worktree of the repo (to `cd` into for the cwd-scoped merge-queue), or `None`.
pub(super) fn a_repo_worktree(cfg: &Config, repo: &str) -> Option<PathBuf> {
    let repo_dir = cfg.worktree_dir.join(repo);
    let rd = std::fs::read_dir(&repo_dir).ok()?;
    rd.flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir() && p.join(".git").exists())
}
