//! The fzf side pane for `bn tat` — split out of `tat.rs` unchanged.
//!
//! The hottest path in the picker: fzf's `--preview` bind re-execs `bn tat --pick-stage preview`
//! once per focused row, so every subprocess here is paid on every cursor move. Keep it that way —
//! nothing in this module may block on the network.

use super::{
    codespaces, cs_account, cs_account_cfg_path, cs_auth_cmd, cs_ref_branch, cs_ref_csname,
    cs_ref_repo, cs_window_name, exact_session_target, is_bare_repo, is_cs_ref, list_worktrees,
    project_path, ref_repo, ref_session, ref_tag, split5, Roots,
};
use crate::proc::tmux_raw;
use crate::wt;

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
        let staged = numstat_count(&["-C", &path_s, "diff", "--cached", "--numstat"]);
        let unstaged = numstat_count(&["-C", &path_s, "diff", "--numstat"]);
        let untracked =
            numstat_count(&["-C", &path_s, "ls-files", "--others", "--exclude-standard"]);
        if staged > 0 {
            println!("Staged: {staged}");
        }
        if unstaged > 0 {
            println!("Modified: {unstaged}");
        }
        if untracked > 0 {
            println!("Untracked: {untracked}");
        }
        if staged == 0 && unstaged == 0 && untracked == 0 {
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

/// Number of output lines for a git command (the `… | wc -l` counters).
fn numstat_count(args: &[&str]) -> usize {
    wt::git_out(args)
        .map(|s| if s.is_empty() { 0 } else { s.lines().count() })
        .unwrap_or(0)
}
