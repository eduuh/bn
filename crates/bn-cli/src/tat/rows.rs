//! What the `bn tat` sub-picker's rows *are* — the `kind|payload|display` builder for one project.
//!
//! Split out of `tat.rs` rather than grown inside it (the 1000-line ratchet; see
//! `.file-size-baseline`), along the seam the module already had: `tat.rs` drives the picker,
//! `discovery.rs` answers where the data comes from, `commands.rs` holds the verbs, and this holds
//! the places — live windows, idle worktrees, open PRs, codespaces and warm pool slots.
use super::commands::{command_rows, CmdAvail};
use super::{
    agent_dot, codespaces, cs_auth_needed, cs_for_repo, cs_window_name, discovery::ordered_refs,
    discovery::remote_prs, discovery::scan_worktrees, exact_session_target, is_bare_repo,
    pool_eligible, ref_repo, ref_session, ref_tag, split7, tmux_sessions, Roots,
};
use crate::proc::tmux_raw;
use crate::wt;
use bn_core::resolver::Config;
use std::collections::HashSet;

/// Render open PRs whose branch has no local representation. The payload is only the numeric PR id;
/// branch names and titles are display data and may contain fzf's `|` delimiter.
pub(super) fn remote_pr_rows(prs: &[wt::RemotePr], represented: &HashSet<String>) -> String {
    let mut rows = String::new();
    for pr in prs {
        let branch = wt::sanitize_branch(&pr.branch);
        if represented.contains(&branch) {
            continue;
        }
        let branch_display = branch.replace('|', " ");
        let title = pr.title.replace(['\r', '\n', '|'], " ");
        let display = format!(
            " PR: {:<14} {:<8} {}",
            branch_display,
            format!("(#{} remote)", pr.number),
            title
        );
        rows.push_str(&format!("remotepr|{}|{display}\n", pr.number));
    }
    rows
}

/// Build the sub-picker's `kind|payload|display` rows for one project: live windows (each with an
/// agent-status dot), idle worktrees (fast dir-scan), remote PRs, codespace branches, and warm-pool
/// slots.
/// Returns the rows plus the live-window count (the caller's quiet-clone shortcut). Shared by the
/// initial in-process paint and the `--pick-stage sublist` reload feed so the two stay identical.
/// Returns the rows, the number of open PR rows, and the queue state the command section was built
/// from.
///
/// The states come back rather than being recomputed by the caller because deriving them costs a
/// full parse of the append-only backlog — 445 KB folding to 50 live entries on the machine this
/// was measured on, and 100 MB on one that had stalled. The picker paid it twice on every open: once
/// here, and again inside `Ticker::start` asking the same question.
pub(super) fn build_sub_rows(
    cfg: &Config,
    roots: &Roots,
    name: &str,
    refresh_worktrees: bool,
) -> (String, usize, super::queue_view::CmdStates) {
    let sess = ref_session(name);
    let repo = ref_repo(name);
    let bare = is_bare_repo(roots, name);

    let window_rows = tmux_raw(&[
        "list-windows",
        "-t",
        &exact_session_target(&sess),
        "-F",
        "#{window_index}|#{window_name}|#{pane_current_command}|#{window_active}|#{@bn_agent}|#{@bn_agent_since}|#{pane_current_path}",
    ]);
    let mut open_windows = Vec::new();
    let mut rows = String::new();
    if let Some(raw) = &window_rows {
        for line in raw.lines() {
            if line.is_empty() {
                continue;
            }
            let f = split7(line);
            open_windows.push(f[1].to_string());
            let marker = if f[3] == "1" { "*" } else { "" };
            // A live-agent status dot in front of the window row (#457): green ● working, yellow ●
            // waiting-on-you, dim ◌ idle, blank when no agent runs there.
            let dot = agent_dot(f[4], f[5]);
            let display = format!("{dot}{}{marker}: {:<14} {:<8} {}", f[0], f[1], f[2], f[6]);
            rows.push_str(&format!("window|{}|{display}\n", f[0]));
        }
    }

    let worktree_dir = roots.worktree_dir(name);
    let worktrees = if bare {
        scan_worktrees(roots, name, refresh_worktrees)
    } else {
        Vec::new()
    };
    if bare {
        for w in &worktrees {
            if open_windows.iter().any(|ow| ow == w) {
                continue; // already a window
            }
            // Same `${short/#$HOME/~}` no-op as the preview pane — the path stays full.
            let short = worktree_dir
                .join(repo)
                .join(w)
                .to_string_lossy()
                .to_string();
            let display = format!(" +: {:<14} {:<8} {}", w, "(worktree)", short);
            rows.push_str(&format!("worktree|{w}|{display}\n"));
        }
    }

    let pool_status = if bare && pool_eligible(roots, name) {
        Some(bn_core::pool::pool_status(cfg, repo))
    } else {
        None
    };
    let mut represented: HashSet<String> = open_windows.iter().cloned().collect();
    represented.extend(worktrees.iter().cloned());
    if bare {
        let bare_s = roots
            .bare_dir(name)
            .join(format!("{repo}.git"))
            .to_string_lossy()
            .to_string();
        represented.extend(
            wt::checked_out_branches(&bare_s)
                .into_keys()
                .map(|branch| wt::sanitize_branch(&branch)),
        );
    }
    if let Some(status) = &pool_status {
        represented.extend(
            status
                .slots
                .iter()
                .filter_map(|slot| slot.branch.as_deref())
                .map(wt::sanitize_branch),
        );
    }
    rows.push_str(&remote_pr_rows(&remote_prs(roots, name), &represented));

    // Codespace branches for this repo — "<branch> ☁" rows beside the worktrees, skipping any
    // already open as a window. The payload packs `csname\tbranch` (neither contains `|`).
    for cs in cs_for_repo(repo) {
        let cs_win = cs_window_name(&cs.branch);
        if open_windows.iter().any(|ow| ow == &cs_win) {
            continue;
        }
        let display = format!(
            " ☁: {:<14} {:<8} {}",
            cs.branch,
            format!("({})", cs.state),
            cs.name
        );
        rows.push_str(&format!("codespace|{}\t{}|{display}\n", cs.name, cs.branch));
    }

    // Pool slots (#457): a leased warm slot is shown by its *leasing branch* (selectable → hands
    // back that slot's window); an idle warm slot advertises a "type a branch to lease" row. Only
    // for a pooled primary-root repo, and a leased branch already open as a window is skipped so it
    // isn't listed twice.
    if let Some(status) = &pool_status {
        let now = bn_core::util::now_epoch_secs();
        for slot in &status.slots {
            // A compact "warmed <ago>" tag from the slot dir's mtime, blank when never warmed.
            let warmed = slot
                .warmed_ts
                .map(|ts| format!(" · warmed {}", crate::cmd::cron::ago((now - ts).max(0))))
                .unwrap_or_default();
            match slot.branch.as_deref() {
                Some(branch) if !branch.is_empty() => {
                    let win = wt::sanitize_branch(branch);
                    if open_windows.iter().any(|ow| ow == &win) {
                        continue; // already a live window
                    }
                    let who = slot
                        .holder
                        .as_deref()
                        .filter(|h| !h.is_empty())
                        .map(|h| format!(" · {h}"))
                        .unwrap_or_default();
                    let display = format!(
                        "  ◍: {:<14} {:<8} pool#{}{warmed}{who}",
                        branch, "(leased)", slot.n
                    );
                    rows.push_str(&format!("poolbranch|{branch}|{display}\n"));
                }
                _ if slot.warm => {
                    let display = format!(
                        "  ○: {:<14} {:<8} pool#{}{warmed} · type a branch to lease",
                        "(idle)", "(pool)", slot.n
                    );
                    rows.push_str(&format!("poolidle|{}|{display}\n", slot.n));
                }
                _ => {} // cold slot (not on disk yet) — nothing to show.
            }
        }
    }

    // The verbs, last and visibly separated. Last because a place is what you usually came for, and
    // because keeping them below the rule is half of what stops a typed branch name landing on one.
    let avail = match &pool_status {
        Some(status) => CmdAvail {
            pooled: true,
            // A cold slot is one inside the derived size that has no live checkout — exactly what
            // `pool warm` would build, so it is the honest number to put on the row.
            cold: status.total.saturating_sub(status.warm + status.leased),
            at_ceiling: status.total >= bn_core::pool::MAX_POOL_SIZE,
        },
        None => CmdAvail::default(),
    };
    let now = bn_core::util::now_epoch_secs();
    let mut states = super::queue_view::command_states(repo, now);
    let settings_note = super::commands::settings::summary(cfg);
    // The repo's own scripts. Only for an untagged ref: a `repo@win` checkout lives outside the
    // configured layout that `bn run --repo` will resolve against, so a row there would queue work
    // that runs somewhere else.
    let scripts = if bare && super::ref_tag(name).is_empty() {
        super::commands::scripts::repo_scripts(cfg, repo)
    } else {
        Vec::new()
    };
    rows.push_str(&command_rows(
        bare,
        avail,
        &scripts,
        &states,
        now_ms(),
        &settings_note,
    ));
    // Only the rows that were drawn are worth watching. `command_states` keys every job the
    // queue holds for this repo, including a cron's `keep-warm` no row shows — and the ticker
    // repaints for as long as anything in here is active.
    states.retain(|id, _| rows.contains(&format!("cmd|{id}|")));

    (rows, open_windows.len(), states)
}

/// The branch and checkout a place row points at, or `None` when it points at nothing on disk.
///
/// One resolver rather than one per caller, because **no row's payload is a branch name** and each
/// kind is wrong in its own way: a `worktree`'s payload is its *sanitized* directory (slashes
/// flattened), a `poolbranch`'s is the lease ledger's holder id — `agent:bn:worktree-commands`
/// where the checkout sits on `worktree-commands` — and a `window`'s is a tmux index. Every caller
/// that guessed instead of asking git looked up a branch note that does not exist and silently
/// rendered nothing, which is indistinguishable from "this branch has no note".
///
/// So each kind resolves to a *path* by its own rule, and the branch always comes from git there.
/// `None` on a genuinely detached checkout, which has no branch to find.
pub(super) fn row_branch(
    cfg: &Config,
    roots: &Roots,
    project: &str,
    kind: &str,
    payload: &str,
) -> Option<(String, std::path::PathBuf)> {
    let repo = ref_repo(project);
    let worktree_dir = roots.worktree_dir(project);
    let path = match kind {
        // A leased slot: find which slot the ledger holds under this name.
        "poolbranch" => {
            let n = bn_core::pool::pool_status(cfg, repo)
                .slots
                .into_iter()
                .find(|s| s.branch.as_deref() == Some(payload))?
                .n;
            worktree_dir.join(repo).join("_pool").join(n.to_string())
        }
        // A live window: ask tmux where its pane actually is. It may be a pool slot, a conventional
        // worktree, or somewhere else entirely — the pane is the only honest answer.
        "window" => {
            let target = super::exact_window_target(&super::ref_session(project), payload);
            let out = tmux_raw(&[
                "display-message",
                "-p",
                "-t",
                &target,
                "#{pane_current_path}",
            ])?;
            std::path::PathBuf::from(out.trim())
        }
        _ => worktree_dir.join(repo).join(payload),
    };
    if !path.is_dir() {
        return None;
    }
    let branch = wt::git_out(&["-C", &path.to_string_lossy(), "branch", "--show-current"])
        .filter(|b| !b.is_empty())?;
    Some((branch, path))
}

/// Wall-clock milliseconds — the only input the row spinner needs, since its frame is derived from
/// the clock rather than from any stored position.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// --- the project picker's own rows (the `--pick-stage list` feed) -----------

/// Decorated list rows: `<display>\t<ref>`. fzf shows only `<display>` (`--with-nth=1`); the
/// `<ref>` rides along as the hidden last field (`{-1}`) for preview/kill/selection.
pub(super) fn build_list(cfg: &Config, roots: &Roots, refresh: bool) {
    let refs = ordered_refs(roots, refresh);
    let known_sessions: HashSet<String> = refs.iter().map(|r| ref_session(r)).collect();
    let known_repos: HashSet<String> = refs.iter().map(|r| ref_repo(r).to_string()).collect();
    let tmux = tmux_sessions();
    for r in refs {
        let sess = ref_session(&r);
        let marker = if let Some(count) = tmux.count(&sess) {
            format!("[*{count}w] ")
        } else {
            "      ".to_string()
        };
        let tag = ref_tag(&r);
        let suffix = if tag.is_empty() {
            String::new()
        } else {
            format!(" ({tag})")
        };
        println!("{marker}{}{suffix}\t{r}", ref_repo(&r));
    }
    for sess in tmux.orphans(&known_sessions) {
        let count = tmux.count(&sess).unwrap_or(0);
        println!("[*{count}w] {sess} (session)\t{sess}");
    }
    // Codespaces whose repo is NOT cloned locally — top-level rows, like orphans (a cloned repo's
    // codespaces surface inside its worktree sub-picker instead). The hidden field is a `cs:` ref,
    // routed by `is_cs_ref` before any `ref_*` call.
    for cs in codespaces() {
        if known_repos.contains(&cs.repo) {
            continue;
        }
        println!(
            "      {}-{} (codespace)\tcs:{}/{}/{}",
            cs.repo, cs.branch, cs.repo, cs.branch, cs.name
        );
    }
    // Codespace auth lapsed (scope expired / logged out): a single actionable row, Enter runs the
    // gh remedy. Shown only when the cached list failed for auth.
    if cs_auth_needed() {
        println!("   ⚠  codespaces: gh auth expired — Enter to fix\tcs-auth");
    }
    // bn-web, the one service this picker can start and stop. Its health used to be a status-bar
    // badge (`bn tmux-mcp badge`); a badge you cannot act on is worse than a row you can (#1349).
    if let Some(row) = super::status::web_row(cfg) {
        println!("{row}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_pr_rows_hide_locally_represented_branches_and_escape_display_text() {
        let prs = vec![
            wt::RemotePr {
                number: 41,
                title: "Already local".into(),
                branch: "feat/local".into(),
                url: "https://example.test/41".into(),
            },
            wt::RemotePr {
                number: 42,
                title: "Remote | work\nfrom laptop".into(),
                branch: "feat/remote".into(),
                url: "https://example.test/42".into(),
            },
        ];
        let represented = HashSet::from([wt::sanitize_branch("feat/local")]);

        let rows = remote_pr_rows(&prs, &represented);

        assert!(!rows.contains("Already local"));
        assert!(rows.contains("remotepr|42|"));
        assert!(rows.contains("feat-remote"));
        assert!(rows.contains("Remote   work from laptop"));
        assert_eq!(rows.lines().count(), 1);
    }
}
