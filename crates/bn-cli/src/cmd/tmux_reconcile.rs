//! `bn tmux-reconcile` — restore the window → worktree link across every tmux window, and refresh
//! the per-window note badges while we are there.
//!
//! `@bn_repo`/`@bn_branch`/`@bn_worktree` are the documented authority for which worktree a window
//! represents, but they are written exactly once, when `bn wt open` creates the window. tmux-resurrect
//! saves no user options (its `window_format` carries only session/index/name/active/flags/layout),
//! so every restore wipes them. Measured on a live server: 1 of 29 windows still carried them.
//! Everything downstream then degrades to matching on the window *name* — and `agent_lease`'s
//! `observed_agent_windows` drops an unidentified window entirely.
//!
//! The badges have the same shape of problem from the other end: `sync_tmux` writes `@bn_todos` /
//! `@bn_blockers` only to the window bn was invoked in, so a note changed by an agent in another
//! window, or by cron, never reaches that window's tab. That is unbounded staleness, not lag —
//! `status-interval` already repaints every 5s, so a written value shows up promptly; the write is
//! what never happens.
//!
//! **Identity is only ever inferred where two independent signals agree.** The pane's cwd alone is
//! not enough: on the same live server, four windows sat in a *different* worktree than their name
//! claimed (a `bn-working-with-nvim` window whose pane was in `main`, and a scratch `zsh` window in
//! a notes repo). Stamping those from cwd would write a confident lie into the field every consumer
//! treats as authoritative — worse than leaving it blank. So a window is stamped only when its name
//! matches the branch its cwd resolves to, and disagreements are reported rather than guessed.

use crate::*;

/// One window as `tmux list-windows -a` reports it, with the options we may rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WinRow {
    /// `#{window_id}` (`@42`). Stable across renames, unlike the `=session:name` target the
    /// creation-time stamp uses — and resurrect renames every window as it restores.
    pub id: String,
    pub session: String,
    pub name: String,
    pub path: String,
    pub repo: String,
    pub branch: String,
    pub todos: String,
    pub blockers: String,
}

/// The identity a window should carry, once confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Identity {
    pub repo: String,
    pub branch: String,
    pub worktree: String,
}

/// What the sweep concluded about one window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Not a worktree, or a detached HEAD: leave it alone and say nothing. Plain shells, the
    /// machine session's `keep-warm` windows, and anything outside a repo land here.
    Skip,
    /// Name and cwd agree — safe to write.
    Stamp(Identity),
    /// The pane is in a different worktree than the window name claims. Never guessed; reported so
    /// it can be fixed deliberately.
    Conflict { claimed: String, actual: String },
}

/// Decide one window's identity from its name and whatever its cwd resolved to.
///
/// `hint` is the `(repo, branch)` a path resolved to, already normalized the way note keys are
/// (repo lowercased, branch `/`→`-`) — which is also the form the stamp stores, so a `perf/lazy-nvm`
/// branch in a `perf-lazy-nvm` window is agreement, not a conflict. That normalization is the whole
/// reason a plain equality test is correct here.
///
/// `dir_name` is the last component of the pane's worktree path. It is what separates a *worktree
/// window whose branch drifted* from *a shell that happens to be sitting inside a repo*: the former
/// is named after its worktree directory and is worth reporting, the latter is named `zsh` or `r`
/// and is simply not a worktree window. Without that distinction every plain shell opened inside a
/// checkout gets reported as a conflict, which is noise, and noise is how a report gets ignored.
pub(crate) fn decide(name: &str, dir_name: &str, hint: Option<(String, String, String)>) -> Decision {
    let Some((repo, branch, worktree)) = hint else {
        return Decision::Skip;
    };
    if name.is_empty() {
        return Decision::Skip;
    }
    if branch == name {
        return Decision::Stamp(Identity {
            repo,
            branch,
            worktree,
        });
    }
    if name == dir_name {
        // Named after its worktree directory, but the branch checked out there is not the one the
        // name implies — somebody switched branches under the window. Real drift; say so.
        return Decision::Conflict {
            claimed: name.to_string(),
            actual: branch,
        };
    }
    Decision::Skip
}

/// The option writes a row still needs: only those whose stored value differs, so a settled window
/// costs nothing and a second sweep is a no-op.
pub(crate) fn pending_writes(
    row: &WinRow,
    id: &Identity,
    counts: Option<(usize, usize)>,
) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let mut push = |key: &'static str, want: String, have: &str| {
        if want != have {
            out.push((key, want));
        }
    };
    push("@bn_repo", id.repo.clone(), &row.repo);
    push("@bn_branch", id.branch.clone(), &row.branch);
    if let Some((todos, blockers)) = counts {
        push(
            "@bn_todos",
            crate::cmd::setup::badge_count(todos),
            &row.todos,
        );
        push(
            "@bn_blockers",
            crate::cmd::setup::badge_count(blockers),
            &row.blockers,
        );
    }
    out
}

/// Fold every window's pending writes into ONE tmux argv, using `;` as the command separator.
///
/// tmux takes a literal `;` argv element between commands, and our command seam forwards argv
/// verbatim, so the whole sweep is a single spawn. The naive shape is one spawn per option — 29
/// windows × 4 options — which is why nothing did this before.
pub(crate) fn batch(writes: &[(String, Vec<(&'static str, String)>)]) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();
    for (id, opts) in writes {
        for (key, value) in opts {
            if !argv.is_empty() {
                argv.push(";".to_string());
            }
            argv.extend([
                "set-option".to_string(),
                "-w".to_string(),
                "-t".to_string(),
                id.clone(),
                key.to_string(),
                value.clone(),
            ]);
        }
    }
    argv
}

/// Parse the tab-separated `list-windows` output. Rows with too few fields are dropped rather than
/// guessed at — a partial row cannot be safely stamped.
pub(crate) fn parse_rows(out: &str) -> Vec<WinRow> {
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            (f.len() >= 8).then(|| WinRow {
                id: f[0].to_string(),
                session: f[1].to_string(),
                name: f[2].to_string(),
                path: f[3].to_string(),
                repo: f[4].to_string(),
                branch: f[5].to_string(),
                todos: f[6].to_string(),
                blockers: f[7].to_string(),
            })
        })
        .collect()
}

/// The `list-windows` format string, ordered to match [`parse_rows`].
const WINDOW_FORMAT: &str = "#{window_id}\t#{session_name}\t#{window_name}\t#{pane_current_path}\t#{@bn_repo}\t#{@bn_branch}\t#{@bn_todos}\t#{@bn_blockers}";

/// `bn tmux-reconcile [--dry-run]` — sweep every window, stamp the ones whose identity is
/// confirmable, refresh their note badges, and report the ones whose name and cwd disagree.
pub(crate) fn emit_tmux_reconcile(cfg: &Config, dry_run: bool) {
    reconcile(cfg, dry_run, true);
}

/// The sweep itself. `report` controls whether drift is written to stderr — the CLI says so, the
/// cron stays quiet.
fn reconcile(cfg: &Config, dry_run: bool, report: bool) {
    // Deliberately NOT gated on `$TMUX`. This sweep addresses the whole server (`list-windows -a`)
    // and targets every window by id, so it needs a reachable server — not a caller that happens to
    // be sitting inside one. A `$TMUX` guard here made [`job_tmux_reconcile`] a guaranteed no-op:
    // the `tick-5m` cron runs under systemd, outside tmux, so the backstop that exists precisely to
    // catch windows created since the last attach never once ran. It reported success every time,
    // which is why it went unnoticed. The `tmux_raw` below is the correct and sufficient check —
    // it returns `None` when there is no server to talk to.
    let Some(out) = crate::proc::tmux_raw(&["list-windows", "-a", "-F", WINDOW_FORMAT]) else {
        return;
    };
    let rows = parse_rows(&out);

    let mut writes: Vec<(String, Vec<(&'static str, String)>)> = Vec::new();
    let mut stamped = 0usize;
    let mut skipped = 0usize;
    let mut conflicts: Vec<(String, String, String)> = Vec::new();

    for row in &rows {
        // `resolve_hint` is the git-free path — it walks up to `.git`, follows the `gitdir:`
        // pointer and reads HEAD. Two subprocesses per window (`resolve`) would make a 29-window
        // sweep 58 git spawns; this makes it zero. Its doc warns it is "only ever a hint", which is
        // exactly right: the name-agreement check below is what turns it into a decision.
        let hint = bn_core::resolver::resolve_hint(cfg, std::path::Path::new(&row.path))
            .map(|r| (r.repo, r.branch, row.path.clone()));
        let dir_name = std::path::Path::new(&row.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match decide(&row.name, &dir_name, hint) {
            Decision::Skip => skipped += 1,
            Decision::Conflict { claimed, actual } => {
                conflicts.push((format!("{}:{}", row.session, row.name), claimed, actual))
            }
            Decision::Stamp(id) => {
                stamped += 1;
                let ctx = bn_core::resolver::Resolved {
                    repo: id.repo.clone(),
                    branch: id.branch.clone(),
                };
                let counts = bn_core::store::read(&cfg.note_file(&ctx)).ok().map(|note| {
                    (
                        note.front.todos.iter().filter(|t| t.is_open_todo()).count(),
                        note.front.blockers.len(),
                    )
                });
                let pending = pending_writes(row, &id, counts);
                if !pending.is_empty() {
                    writes.push((row.id.clone(), pending));
                }
            }
        }
    }

    let argv = batch(&writes);
    if dry_run {
        println!(
            "{} window(s): {stamped} identifiable · {skipped} not a worktree · {} conflicting",
            rows.len(),
            conflicts.len()
        );
        for (win, opts) in &writes {
            for (key, value) in opts {
                println!("  {win} {key} = {}", if value.is_empty() { "\"\"" } else { value });
            }
        }
        if writes.is_empty() {
            println!("  (nothing to write — already reconciled)");
        }
    } else if !argv.is_empty() {
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        crate::proc::tmux_run(&refs);
    }

    if !report {
        return;
    }
    for (win, claimed, actual) in &conflicts {
        eprintln!(
            "{win}: window is named '{claimed}' but its pane is on '{actual}' — identity left unset",
            );
    }
}

/// The `tick-5m` entry. Cheap by construction — one `list-windows`, a git-free resolve per row,
/// a note read only for windows that resolve, and a single batched tmux call when anything
/// actually changed — so it can ride the existing 5-minute cluster rather than needing a timer.
/// This is what keeps a tab badge tracking a note that some *other* window's agent edited.
///
/// Conflicts stay silent here: the cron has no one to tell, and a job that writes to stderr every
/// five minutes teaches you to stop reading cron output.
pub(crate) fn job_tmux_reconcile(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    reconcile(cfg, false, false);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, name: &str) -> WinRow {
        WinRow {
            id: id.to_string(),
            session: "bn".to_string(),
            name: name.to_string(),
            path: "/tmp".to_string(),
            repo: String::new(),
            branch: String::new(),
            todos: String::new(),
            blockers: String::new(),
        }
    }

    fn hint(repo: &str, branch: &str) -> Option<(String, String, String)> {
        Some((
            repo.to_string(),
            branch.to_string(),
            format!("/wt/{branch}"),
        ))
    }

    /// The ordinary case: the window is named after the branch its pane is on.
    #[test]
    fn a_window_whose_name_matches_its_cwd_is_stamped() {
        let d = decide(
            "using-workflows-test",
            "using-workflows-test",
            hint("bn", "using-workflows-test"),
        );
        assert!(matches!(d, Decision::Stamp(id) if id.repo == "bn"));
    }

    /// A slashed branch reaches us already normalized (`perf/lazy-nvm` → `perf-lazy-nvm`), which is
    /// also how the window is named and how the stamp stores it. A comparison against the raw
    /// branch would call this real, settled window a conflict.
    #[test]
    fn a_sanitised_slash_in_the_branch_still_counts_as_agreement() {
        let d = decide("perf-lazy-nvm", "perf-lazy-nvm", hint("dotfiles", "perf-lazy-nvm"));
        assert!(matches!(d, Decision::Stamp(_)), "{d:?}");
    }

    /// The case that makes cwd alone unsafe: this pane is in `main`, but the window represents a
    /// feature branch. Writing `main` here would be a confident lie in the field every consumer
    /// trusts, so it must be reported instead.
    #[test]
    fn a_pane_sitting_in_another_worktree_is_reported_not_guessed() {
        let d = decide(
            "bn-working-with-nvim",
            "bn-working-with-nvim",
            hint("bn", "main"),
        );
        assert_eq!(
            d,
            Decision::Conflict {
                claimed: "bn-working-with-nvim".to_string(),
                actual: "main".to_string(),
            }
        );
    }

    /// A shell window that merely happens to be sitting inside a checkout claims nothing, so it is
    /// skipped rather than reported. Reporting every `zsh` in a repo would bury the real drift.
    #[test]
    fn a_shell_window_inside_a_repo_is_not_a_conflict() {
        assert_eq!(
            decide("r", "using-workflows-test", hint("bn", "fix-jobpath-test-env-race")),
            Decision::Skip
        );
    }

    /// An unnamed window cannot be claiming a branch either.
    #[test]
    fn an_unnamed_window_is_skipped() {
        assert_eq!(decide("", "arena", hint("arena", "main")), Decision::Skip);
    }

    /// Plain shells and the machine session's `keep-warm` windows resolve to no worktree at all.
    /// They are skipped silently — they are not broken, they are simply not worktree windows.
    #[test]
    fn a_window_outside_any_worktree_is_skipped_silently() {
        assert_eq!(decide("keep-warm", "home", None), Decision::Skip);
    }

    /// The sweep must be a no-op once it has run: a window already carrying the right values emits
    /// no commands, so hanging it off an attach hook costs nothing on the second attach.
    #[test]
    fn a_window_already_carrying_the_right_values_emits_no_writes() {
        let mut r = row("@1", "main");
        r.repo = "bn".to_string();
        r.branch = "main".to_string();
        r.todos = "3".to_string();
        r.blockers = String::new();
        let id = Identity {
            repo: "bn".to_string(),
            branch: "main".to_string(),
            worktree: "/wt/main".to_string(),
        };
        assert!(pending_writes(&r, &id, Some((3, 0))).is_empty());
    }

    /// A count that drops to zero has to be written as EMPTY, not "0" — tmux's `#{?@bn_todos,…}`
    /// treats a literal "0" as truthy, so the badge would stick at zero forever.
    #[test]
    fn a_count_falling_to_zero_is_written_as_empty_not_zero() {
        let mut r = row("@1", "main");
        r.repo = "bn".to_string();
        r.branch = "main".to_string();
        r.todos = "3".to_string();
        let id = Identity {
            repo: "bn".to_string(),
            branch: "main".to_string(),
            worktree: "/wt/main".to_string(),
        };
        let w = pending_writes(&r, &id, Some((0, 0)));
        assert_eq!(w, vec![("@bn_todos", String::new())]);
    }

    /// Every window's writes go out as one tmux invocation, separated by literal `;` elements.
    #[test]
    fn writes_fold_into_a_single_semicolon_separated_argv() {
        let writes = vec![
            (
                "@1".to_string(),
                vec![("@bn_repo", "bn".to_string()), ("@bn_branch", "main".into())],
            ),
            ("@7".to_string(), vec![("@bn_todos", "2".to_string())]),
        ];
        assert_eq!(
            batch(&writes),
            vec![
                "set-option", "-w", "-t", "@1", "@bn_repo", "bn", ";", "set-option", "-w", "-t",
                "@1", "@bn_branch", "main", ";", "set-option", "-w", "-t", "@7", "@bn_todos", "2",
            ]
        );
    }

    #[test]
    fn nothing_pending_means_no_tmux_call_at_all() {
        assert!(batch(&[]).is_empty());
    }

    /// Rows are tab-separated with empty fields preserved — a window with no options set still has
    /// its trailing tabs, and splitting on whitespace would collapse them and misread the row.
    #[test]
    fn rows_parse_with_empty_option_fields_preserved() {
        let out = "@3\tbn\tusing-workflows-test\t/wt/uwt\t\t\t3\t\n@9\tmachine\tkeep-warm\t/home\t\t\t\t";
        let rows = parse_rows(out);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "using-workflows-test");
        assert_eq!(rows[0].todos, "3");
        assert!(rows[0].repo.is_empty());
        assert_eq!(rows[1].name, "keep-warm");
    }
}
