//! The fzf side pane for `bn tat` — split out of `tat.rs` unchanged.
//!
//! The hottest path in the picker: fzf's `--preview` bind re-execs `bn tat --pick-stage preview`
//! once per focused row, so every subprocess here is paid on every cursor move. Keep it that way —
//! nothing in this module may block on the network.

use super::{
    codespaces, cs_account, cs_account_cfg_path, cs_auth_cmd, cs_ref_branch, cs_ref_csname,
    cs_ref_repo, cs_window_name, exact_session_target, exact_window_target, is_bare_repo,
    is_cs_ref, list_worktrees, project_path, ref_repo, ref_session, ref_tag, split5, Roots,
};
use crate::proc::tmux_raw;
use crate::wt;
use bn_core::resolver::Config;

// --- preview (the fzf side pane) -------------------------------------------

pub(super) fn preview(roots: &Roots, project: &str) {
    if project.is_empty() {
        return;
    }
    // The codespace auth-fix row.
    if project == "cs-auth" {
        let acct = cs_account();
        println!("━━━ codespace auth ━━━\n");
        println!("`gh cs list` failed — no codespace access on the active gh account.\n");
        if !acct.is_empty() {
            println!("Enter: switch to your codespace account ({acct}); if that still");
            println!("       lacks the scope, refresh it (browser/device login).");
        } else {
            println!("Fix:  {}", cs_auth_cmd());
            println!("Enter runs it (browser/device login). Tip: if your codespaces live");
            println!(
                "under another gh account, put its username in:\n  {}",
                cs_account_cfg_path().display()
            );
        }
        return;
    }
    // Codespace rows have no local session/dir — render state from the cached gh list, before any
    // `ref_*`/`project_path` call mis-parses the `cs:` ref.
    if is_cs_ref(project) {
        let r = cs_ref_repo(project);
        let b = cs_ref_branch(project);
        let n = cs_ref_csname(project);
        let st = codespaces()
            .into_iter()
            .find(|c| c.name == n)
            .map(|c| c.state)
            .unwrap_or_default();
        println!("━━━ {r} / {b} (codespace) ━━━\n");
        println!("Repo:   {r}");
        println!("Branch: {b}");
        println!("State:  {}", if st.is_empty() { "unknown" } else { &st });
        println!("Name:   {n}\n");
        println!(
            "Enter: open \"{}\" window in session {r} (gh cs ssh + remote tmux)",
            cs_window_name(b)
        );
        return;
    }
    let tag = ref_tag(project);
    let sess = ref_session(project);
    let path = project_path(roots, project);
    let path_is_dir = path.is_dir();
    let window_rows = tmux_raw(&[
        "list-windows",
        "-t",
        &exact_session_target(&sess),
        "-F",
        "#{window_index}|#{window_name}|#{pane_current_command}|#{pane_current_path}|#{window_active}",
    ]);
    let session_up = window_rows.is_some();

    if !path_is_dir && !session_up {
        println!("Directory not found: {}", path.display());
        return;
    }

    if !tag.is_empty() {
        println!("━━━ {} ({tag}) ━━━\n", ref_repo(project));
    } else if !path_is_dir {
        println!("━━━ {project} (session) ━━━\n");
    } else {
        println!("━━━ {project} ━━━\n");
    }

    if let Some(raw) = window_rows {
        println!("Windows:");
        for line in raw.lines() {
            if line.is_empty() {
                continue;
            }
            let f = split5(line);
            let marker = if f[4] == "1" { "*" } else { "" };
            // The oracle's `${wpath/#$HOME/~}` is a silent no-op: bash tilde-expands the `~`
            // replacement back to $HOME, so it replaces $HOME with $HOME. The path stays full.
            println!("  {}{marker}: {:<14} {:<8} {}", f[0], f[1], f[2], f[3]);
        }
        println!();
    }

    let path_s = path.to_string_lossy().to_string();
    if path.join(".git").exists() {
        let branch = wt::git_out(&["-C", &path_s, "branch", "--show-current"])
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "detached".to_string());
        println!("Branch: {branch}");
        let counts = tree_counts(&path);
        for (label, n) in [
            ("Staged", counts.staged),
            ("Modified", counts.unstaged),
            ("Untracked", counts.untracked),
        ] {
            match n {
                Some(n) if n > 0 => println!("{label}: {n}"),
                None => println!("{label}: …"),
                _ => {}
            }
        }
        if counts.all_clean() {
            println!("Clean");
        }
        println!();
        println!("Recent commits:");
        if let Some(log) = wt::git_out(&["-C", &path_s, "log", "--oneline", "-5"]) {
            if !log.is_empty() {
                println!("{log}");
            }
        }
        println!();
    }

    if is_bare_repo(roots, project) {
        let wts = list_worktrees(roots, project);
        if !wts.is_empty() {
            println!("Worktrees: ({})", wts.len());
            for w in &wts {
                println!("  {w}");
            }
            println!();
        }
    }

    if path_is_dir {
        let t = if path.join("Cargo.toml").is_file() {
            "Rust"
        } else if path.join("package.json").is_file() {
            "Node.js"
        } else if path.join("go.mod").is_file() {
            "Go"
        } else if path.join("pyproject.toml").is_file() || path.join("requirements.txt").is_file() {
            "Python"
        } else {
            "Unknown"
        };
        println!("Type: {t}");
    }
}

/// How long the whole working-tree probe may take before the panel gives up and prints `…`.
///
/// The ceiling is what still reads as *instant* under a cursor held on the down arrow, not what the
/// largest repository needs — and on a large enough repository those two can never be the same
/// number. Measured on BigMono (a ~100k-file monorepo on WSL): `ls-files --others` takes ~4s warm and
/// over two minutes cold, and `diff --numstat` ~0.4s warm and ~11s on a cold index. No budget big
/// enough to finish that work is small enough to keep the picker usable, so the pane stops asking
/// and says so. Everything else in it — the branch note, divergence, recent commits — is a
/// history/object read that does not scale with the size of the checkout, and stays exact.
///
/// 150ms because `bn`'s own startup is ~10ms: on an ordinary repo all three counts land well inside
/// this and the pane is exact, while a repo that cannot answer in a sixth of a second is one you do
/// not want to wait on once per keystroke.
const TREE_BUDGET: std::time::Duration = std::time::Duration::from_millis(150);

/// The three dirty-counts for a checkout. `None` where the shared budget ran out (or git failed)
/// before that count could be answered — rendered as `…`, never silently as zero, because a panel
/// that reports a busy tree as `Clean` is worse than one that admits it did not look.
struct TreeCounts {
    staged: Option<usize>,
    unstaged: Option<usize>,
    untracked: Option<usize>,
}

impl TreeCounts {
    /// The counts that came back non-zero, as `3 staged` fragments for a one-line summary.
    fn parts(&self) -> Vec<String> {
        [
            (self.staged, "staged"),
            (self.unstaged, "modified"),
            (self.untracked, "untracked"),
        ]
        .iter()
        .filter_map(|(n, label)| n.filter(|n| *n > 0).map(|n| format!("{n} {label}")))
        .collect()
    }

    /// True only when every count was actually answered and every one was zero.
    fn all_clean(&self) -> bool {
        [self.staged, self.unstaged, self.untracked]
            .iter()
            .all(|n| *n == Some(0))
    }

    /// True when the budget expired before some count was known.
    fn incomplete(&self) -> bool {
        [self.staged, self.unstaged, self.untracked]
            .iter()
            .any(Option::is_none)
    }
}

/// Count what is dirty in `path`, under one budget shared by all three probes.
///
/// One deadline for the group rather than one each, because the ceiling that matters to the person
/// holding the down arrow is what the *pane* costs, not what any single `git` inside it costs.
fn tree_counts(path: &std::path::Path) -> TreeCounts {
    let deadline = std::time::Instant::now() + TREE_BUDGET;
    let count = |args: &[&str]| -> Option<usize> {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        crate::git::git_capture_deadline(path, args, left).map(|s| {
            if s.is_empty() {
                0
            } else {
                s.lines().count()
            }
        })
    };
    TreeCounts {
        // `--no-optional-locks` throughout: this pane reruns on every keystroke and a plain `git
        // diff` takes the index write lock to refresh it, racing the commits agents make in the
        // same worktree. Same reasoning as the tmux git segment (`cmd::tmux_status`).
        staged: count(&["--no-optional-locks", "diff", "--cached", "--numstat"]),
        unstaged: count(&["--no-optional-locks", "diff", "--numstat"]),
        // `--directory --no-empty-directory` stops git descending *into* an untracked tree, which
        // is what keeps more repos answering inside the budget: on a monorepo with `node_modules`
        // on disk it is 5s rather than 145s on a cold cache. It also makes the count agree with
        // what `git status` shows by default (`-unormal` lists `node_modules/`, not each of its
        // ninety thousand files), so the number here now means untracked *entries* — the right
        // granularity for a dirty-or-not indicator.
        untracked: count(&[
            "--no-optional-locks",
            "ls-files",
            "--others",
            "--exclude-standard",
            "--directory",
            "--no-empty-directory",
        ]),
    }
}

// --- the sub-picker's side pane --------------------------------------------
// A second preview, for the second picker. The project picker's rows are repos, so [`preview`]
// above answers "what is this project?"; the sub-picker's rows are the things *inside* one repo,
// so these answer "what is this row?" — and that is a different question per kind.
//
// The module's rule still binds: this runs once per cursor move, so nothing here may block on the
// network. PR state comes from the discovery cache, never from a fresh `gh` call.

/// Render the detail panel for one sub-picker row. `arg` is fzf's `{1}~{2}` (`kind~payload`).
pub(super) fn row(cfg: &Config, roots: &Roots, project: &str, arg: &str) {
    let (kind, payload) = arg.split_once('~').unwrap_or(("", arg));
    match kind {
        // The rule between the places and the verbs names nothing to describe.
        "cmdsep" => {}
        "cmd" => super::commands::command_preview(cfg, roots, project, payload),
        "window" => window_row(project, payload),
        "remotepr" => remote_pr_row(roots, project, payload),
        "codespace" => codespace_row(payload),
        "poolidle" => pool_idle_row(cfg, project, payload),
        // Both end in the same panel; the flag only says where to find the checkout — under
        // `_pool/<n>` for a leased slot, under its own directory for a worktree.
        "poolbranch" => branch_row(cfg, roots, project, kind, payload),
        _ => branch_row(cfg, roots, project, kind, payload),
    }
}

/// A live window: tail its active pane, the same capture `bn jump` shows. Seeing what an agent is
/// actually printing is the whole reason to look at a window row.
fn window_row(project: &str, index: &str) {
    let target = exact_window_target(&ref_session(project), index);
    let Some(pane) = active_pane(&target) else {
        println!("(no pane to capture)");
        return;
    };
    let Some(cap) = tmux_raw(&["capture-pane", "-p", "-J", "-t", &pane]) else {
        return;
    };
    let mut lines: Vec<&str> = cap.lines().collect();
    // A pane is captured to its full height, so an idle shell is one prompt under forty blank
    // lines. Printed as-is that scrolls the only interesting line out of view, so drop the trailing
    // blanks and show the tail of what is actually there.
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        println!("(pane is empty)");
        return;
    }
    for l in &lines[lines.len().saturating_sub(60)..] {
        println!("{l}");
    }
}

/// The active pane of a window target, for the capture.
fn active_pane(target: &str) -> Option<String> {
    let out = tmux_raw(&[
        "list-panes",
        "-t",
        target,
        "-F",
        "#{pane_active} #{pane_id}",
    ])?;
    out.lines().find_map(|l| {
        let (active, pane) = l.split_once(' ')?;
        (active == "1").then(|| pane.to_string())
    })
}

/// An open PR with no local checkout yet. Cache-only — see the module rule.
fn remote_pr_row(roots: &Roots, project: &str, number: &str) {
    let Ok(n) = number.parse::<i64>() else { return };
    let Some(pr) = super::remote_prs(roots, project)
        .into_iter()
        .find(|p| p.number == n)
    else {
        println!("PR #{number} — no longer in the cached list");
        return;
    };
    println!("\x1b[1;36m#{} {}\x1b[0m\n", pr.number, pr.title);
    println!("Branch: {}", pr.branch);
    println!("URL:    {}\n", pr.url);
    println!("Enter: fetch the branch and open it here");
}

fn codespace_row(payload: &str) {
    let (name, branch) = payload.split_once('\t').unwrap_or((payload, ""));
    let state = super::codespaces()
        .into_iter()
        .find(|c| c.name == name)
        .map(|c| c.state)
        .unwrap_or_default();
    println!("\x1b[1;36m{branch} (codespace)\x1b[0m\n");
    println!(
        "State: {}",
        if state.is_empty() { "unknown" } else { &state }
    );
    println!("Name:  {name}");
}

fn pool_idle_row(cfg: &Config, project: &str, slot: &str) {
    let repo = ref_repo(project);
    let status = bn_core::pool::pool_status(cfg, repo);
    println!("\x1b[1;36mpool slot {slot} — idle\x1b[0m\n");
    println!(
        "Pool:  {} slots · {} leased · {} warm",
        status.total, status.leased, status.warm
    );
    println!("\nType a branch name and press Enter to lease this slot for it.");
}

/// A worktree or a leased pool slot — the panel this whole pane was worth adding for.
///
/// Three things you otherwise had to open the worktree to learn, in the order you want them: what
/// you were doing there (the branch note's goal, open todos and blockers), whether the tree is
/// clean, and what has landed on it lately. The note is the part no other bn surface puts in front
/// of you at the moment you are choosing where to go.
fn branch_row(cfg: &Config, roots: &Roots, project: &str, kind: &str, payload: &str) {
    let repo = ref_repo(project);
    // One resolver, shared with the row menu: every kind's payload is wrong in its own way, so the
    // branch always comes from git at the checkout. See `rows::row_branch`.
    let Some((branch, path)) = super::rows::row_branch(cfg, roots, project, kind, payload) else {
        println!("{payload} — no checkout on disk, or a detached one with no branch");
        return;
    };
    let path_s = path.to_string_lossy().to_string();
    println!("\x1b[1;36m{repo} / {branch}\x1b[0m");
    println!("{}\n", path.display());

    note_panel(cfg, repo, &branch);

    let counts = tree_counts(&path);
    if counts.all_clean() {
        println!("Clean");
    } else {
        let mut parts = counts.parts();
        if counts.incomplete() {
            parts.push("…".to_string());
        }
        println!("Changes: {}", parts.join(" · "));
    }
    // How far this branch has diverged from where it forked — the number that decides whether it
    // wants a rebase before anything else.
    if let Some(ab) = wt::git_out(&[
        "-C",
        &path_s,
        "rev-list",
        "--left-right",
        "--count",
        "origin/HEAD...HEAD",
    ]) {
        if let Some((behind, ahead)) = ab.split_once(char::is_whitespace) {
            let (behind, ahead) = (behind.trim(), ahead.trim());
            if behind != "0" || ahead != "0" {
                println!("vs main: {ahead} ahead · {behind} behind");
            }
        }
    }
    println!();
    if let Some(log) = wt::git_out(&["-C", &path_s, "log", "--oneline", "-5"]) {
        if !log.is_empty() {
            println!("Recent commits:\n{log}");
        }
    }
}

/// The branch note's goal, open todos and blockers — nothing when there is no note.
///
/// Deliberately silent rather than saying "no note": a row without one is the common case for a
/// pool slot an agent just leased, and a line of absence on most rows would crowd out the rows that
/// do have something to say.
fn note_panel(cfg: &Config, repo: &str, branch: &str) {
    let ctx = bn_core::resolver::Resolved {
        repo: repo.to_lowercase(),
        branch: branch.to_string(),
    };
    let Ok(note) = bn_core::store::read(&cfg.note_file(&ctx)) else {
        return;
    };
    if let Some(goal) = bn_core::render::goal_text(&note.body) {
        if !goal.trim().is_empty() {
            println!("Goal: {}\n", goal.trim());
        }
    }
    let open: Vec<&bn_core::model::Item> = note
        .front
        .todos
        .iter()
        .filter(|t| t.done != Some(true))
        .collect();
    if !open.is_empty() {
        println!("Todo ({}):", open.len());
        for t in open.iter().take(5) {
            println!("  ☐ {}", t.text);
        }
        if open.len() > 5 {
            println!("  … {} more", open.len() - 5);
        }
        println!();
    }
    let blockers: Vec<&bn_core::model::Item> = note
        .front
        .blockers
        .iter()
        .filter(|b| b.done != Some(true))
        .collect();
    if !blockers.is_empty() {
        println!("\x1b[1;31mBlocked:\x1b[0m");
        for b in blockers.iter().take(3) {
            println!("  ⚠ {}", b.text);
        }
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the deadline exists to protect: a count the budget could not afford renders as
    /// `…`, and never as `Clean`. Reporting a busy monorepo as clean because the walk timed out is
    /// the one failure mode worse than the stall this replaced.
    #[test]
    fn a_count_the_budget_could_not_afford_is_never_reported_as_clean() {
        let timed_out = TreeCounts {
            staged: Some(0),
            unstaged: Some(0),
            untracked: None,
        };

        assert!(!timed_out.all_clean());
        assert!(timed_out.incomplete());
        assert!(timed_out.parts().is_empty());
    }

    #[test]
    fn every_count_answered_and_zero_is_clean() {
        let clean = TreeCounts {
            staged: Some(0),
            unstaged: Some(0),
            untracked: Some(0),
        };

        assert!(clean.all_clean());
        assert!(!clean.incomplete());
    }

    #[test]
    fn answered_non_zero_counts_become_summary_fragments() {
        let dirty = TreeCounts {
            staged: Some(3),
            unstaged: Some(0),
            untracked: Some(11),
        };

        assert!(!dirty.all_clean());
        assert!(!dirty.incomplete());
        assert_eq!(dirty.parts(), vec!["3 staged", "11 untracked"]);
    }

    /// A budget already spent must not spawn git at all — otherwise the "shared deadline" is only
    /// a deadline for the first probe, and the last one runs unbounded.
    #[test]
    fn a_spent_budget_refuses_to_spawn() {
        let out = crate::git::git_capture_deadline(
            std::path::Path::new("."),
            &["rev-parse", "HEAD"],
            std::time::Duration::ZERO,
        );

        assert_eq!(out, None);
    }
}
