//! `bn edit diff` — pick a worktree, land in its editor on its diff.
//!
//! The gesture this replaces: noticing an agent finished somewhere, switching sessions to find it,
//! working out what it changed, and driving git by hand. Here the whole fleet is one list, the
//! preview *is* the diff, and Enter puts you in that worktree's editor with Diffview already open.
//!
//! Enter deliberately opens the diff in **that worktree's** nvim rather than the one you are
//! sitting in: the editor there is already rooted in the right repo, so LSP, gitsigns and
//! jump-to-definition all work on the code you are reading.

use super::target::{EditTarget, TargetSpec};
use bn_core::resolver::Config;
use std::process::{Command, Stdio};

/// fzf field separator. Field 1 is what you see; 2..=4 are hidden lookup columns.
const TAB: char = '\t';

/// The stages fzf re-enters bn for. Same shape as `bn jump --pick-stage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum DiffStage {
    /// Emit the row list (the `start`/`ctrl-r` reload feed).
    List,
}

/// One worktree row: `display \t repo \t branch \t worktree`.
struct Row {
    display: String,
    repo: String,
    branch: String,
    worktree: String,
}

impl Row {
    fn line(&self) -> String {
        format!(
            "{}{TAB}{}{TAB}{}{TAB}{}",
            self.display, self.repo, self.branch, self.worktree
        )
    }
}

/// Build the row list.
///
/// Every note that has a live checkout, badged with whether an editor is already up. The badge
/// comes from a **single** `list-panes -a` for the whole list — one tmux round trip per *render*,
/// never one per row, because this feed is also what `ctrl-r` re-runs.
fn rows(cfg: &Config) -> Vec<Row> {
    let editors = super::session::find_panes_all();
    let mut out = Vec::new();
    for w in bn_core::rollups::worktrees(cfg) {
        let Some(worktree) = w.worktree.clone() else {
            continue; // A note with no checkout on this machine has nothing to diff.
        };
        let badge = editors
            .iter()
            .find(|p| p.worktree == worktree)
            .map(|p| {
                if p.session == bn_core::editor::HOLD_SESSION {
                    "○" // parked — nvim is alive, just not on screen
                } else {
                    "●" // showing
                }
            })
            .unwrap_or(" ");
        let flags = match (w.open_todos, w.blockers) {
            (0, 0) => String::new(),
            (t, 0) => format!("  {t} todo"),
            (0, b) => format!("  \x1b[31m⛔{b}\x1b[0m"),
            (t, b) => format!("  {t} todo  \x1b[31m⛔{b}\x1b[0m"),
        };
        out.push(Row {
            display: format!(
                "{badge} \x1b[1m{:<14}\x1b[0m {:<34}{flags}",
                w.repo, w.branch
            ),
            repo: w.repo,
            branch: w.branch,
            worktree,
        });
    }
    out
}

/// The `--pick-stage` re-entry: emit the rows fzf reloads.
pub(crate) fn pick_stage(cfg: &Config, stage: DiffStage) {
    match stage {
        DiffStage::List => {
            for r in rows(cfg) {
                println!("{}", r.line());
            }
        }
    }
}

/// The picker itself.
pub(crate) fn run(cfg: &Config, base: Option<&str>) {
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());

    // {4} is the worktree path, so the preview is the real `bn diff` for that checkout — three-dot,
    // so it shows what this branch changed rather than counting the base's own newer commits.
    let child = Command::new("fzf")
        .arg("--reverse")
        .arg("--ansi")
        .arg("--delimiter=\t")
        .arg("--with-nth=1")
        .arg(format!("--preview={bn} diff --path {{4}} --three-dot"))
        .arg("--preview-window=right:55%:wrap")
        .arg(format!(
            "--bind=start:reload({bn} edit diff --pick-stage list)"
        ))
        .arg(format!(
            "--bind=ctrl-r:reload({bn} edit diff --pick-stage list)"
        ))
        .arg("--header=↵ open the diff in that worktree's editor · ctrl-r reload")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn();

    let child = match child {
        Ok(c) => c,
        Err(_) => {
            // No fzf: print the list and hold, matching every other bn picker's degradation.
            for r in rows(cfg) {
                println!("{}", r.display);
            }
            crate::picker::hold();
            return;
        }
    };

    let Ok(out) = child.wait_with_output() else {
        return;
    };
    let sel = String::from_utf8_lossy(&out.stdout).into_owned();
    let cols: Vec<&str> = sel.trim_end_matches('\n').split(TAB).collect();
    let (repo, branch) = (
        cols.get(1).copied().unwrap_or(""),
        cols.get(2).copied().unwrap_or(""),
    );
    if repo.is_empty() || branch.is_empty() {
        return; // cancelled
    }

    // Everything below runs *after* fzf has torn down. That ordering is load-bearing: this command
    // runs inside a `display-popup`, where `$TMUX` is set and stdout is a tty, so a `switch-client`
    // issued while fzf still owns the terminal would land in the popup instead of the real client.
    open(cfg, repo, branch, base);
}

/// Land the chosen worktree's editor and switch to it.
fn open(cfg: &Config, repo: &str, branch: &str, base: Option<&str>) {
    let spec = TargetSpec::RepoBranch(repo.to_string(), branch.to_string());
    let target = match super::target::resolve_target(cfg, &spec) {
        Ok(t) => t,
        Err(e) => return notify(&format!("bn edit diff: {e}")),
    };

    let mut sess = super::session::probe(target.clone());
    let land_args = super::LandArgs::for_diff(repo, branch, base);
    let prepared = match super::prepare_land(cfg, &sess, &land_args) {
        Ok(p) => p,
        Err(e) => return fallback(&target, &e),
    };

    match super::show_for_diff(&mut sess, &prepared) {
        // Freshly created: the spec was on its command line, so it is already landing.
        Ok(true) => {}
        Ok(false) => {
            if let Err(e) = super::land::apply(&sess, &prepared) {
                return fallback(&target, &e);
            }
        }
        Err(e) => return fallback(&target, &e),
    }

    if let Some(p) = super::session::find_pane(&target.worktree) {
        goto(&p.window, &p.pane);
    }
}

/// Switch the calling client to the window hosting the editor, then focus its pane.
fn goto(window: &str, pane: &str) {
    if let Some(sess) =
        crate::proc::tmux(&["display-message", "-p", "-t", window, "#{session_name}"])
    {
        let _ = crate::proc::tmux_ok(&["switch-client", "-t", sess.trim()]);
    }
    let _ = crate::proc::tmux_ok(&["select-window", "-t", window]);
    let _ = crate::proc::tmux_ok(&["select-pane", "-t", pane]);
}

/// When the editor cannot be reached, show the diff rather than leaving the user half-switched
/// with nothing to look at.
fn fallback(target: &EditTarget, why: &str) {
    eprintln!("bn edit diff: {why} — showing the diff here instead");
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let out = Command::new(exe)
        .args([
            "diff",
            "--three-dot",
            "--path",
            &target.worktree.display().to_string(),
        ])
        .output();
    crate::picker::page(&out.map(|o| o.stdout).unwrap_or_default());
}

fn notify(msg: &str) {
    let _ = crate::proc::tmux_ok(&["display-message", msg]);
    eprintln!("{msg}");
}
