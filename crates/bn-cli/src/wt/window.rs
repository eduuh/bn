//! Tmux window orchestration for `wt`: `open`, the window create/select decision,
//! per-worktree window commands, and window discovery/teardown. Moved verbatim from
//! the former wt.rs god file (issue #351).

use super::*;

/// `bn wt open` — the prefix+o flow: resolve the current repo, pick or type a branch, ensure its
/// worktree exists (creating from main if new), then open/select its tmux window. Session-per-repo.
pub(crate) fn cmd_open(
    cfg: &Config,
    branch: Option<String>,
    force_new: bool,
    no_tmux: bool,
) -> bool {
    let (bare_dir, worktree_dir) = layout(cfg);
    let bare = match resolve_bare_or_pick(&bare_dir) {
        Some(b) => b,
        None => return false,
    };
    let name = repo_name(&bare);
    let bare_s = bare.to_string_lossy().to_string();

    let branch = match branch {
        Some(b) => b,
        None => {
            let items = existing_worktree_branches(&worktree_dir, &name);
            match fzf_pick_or_create(&items, "Open (Enter) or type a new branch") {
                Some(b) => b,
                None => return true,
            }
        }
    };

    let sanitized = sanitize_branch(&branch);
    let wt_path = worktree_dir.join(&name).join(&sanitized);
    if !wt_path.is_dir() {
        let _ = std::fs::create_dir_all(worktree_dir.join(&name));
        println!("{BLUE}Creating worktree: {}{NC}", wt_path.display());
        if !create_worktree(&bare_s, &branch, &wt_path.to_string_lossy(), true) {
            return false;
        }
        println!("{GREEN}Created worktree: {}{NC}", wt_path.display());
    }
    if !no_tmux {
        open_worktree_window(&name, &sanitized, &wt_path, force_new);
    }
    println!("cd {}", wt_path.display());
    true
}

/// Ensure the worktree for `repo`/`branch` exists (creating it from the default branch if new) and
/// open/select its tmux window — the mechanical half of `bn attn start`. Like `wt open`, but
/// targets an explicit repo (not the cwd's), so an attention item can point anywhere. Session-per-
/// repo, named via [`repo_name`]/[`sanitize_branch`] so the window joins the item on the board.
/// Returns the worktree path, or `None` if the bare repo can't be resolved or creation fails.
pub(crate) fn open_item_worktree(cfg: &Config, repo: &str, branch: &str) -> Option<PathBuf> {
    let bare = bare_for(cfg, repo)?;
    let bare_s = bare.to_string_lossy().to_string();
    let name = repo_name(&bare);
    let sanitized = sanitize_branch(branch);
    let (_, worktree_dir) = layout(cfg);
    let wt_path = worktree_dir.join(&name).join(&sanitized);
    if !wt_path.is_dir() {
        let _ = std::fs::create_dir_all(worktree_dir.join(&name));
        if !create_worktree(&bare_s, branch, &wt_path.to_string_lossy(), true) {
            return None;
        }
    }
    open_worktree_window(&name, &sanitized, &wt_path, false);
    Some(wt_path)
}

/// Read-only status of a branch's existing worktree, for the night planner — never creates one.
/// Returns `(worktree_path, dirty, changed_files)`: the worktree dir if it exists on disk, whether
/// it has uncommitted changes, and the files it has changed vs `base` (`git diff --name-only
/// base...branch`, three-dot — changes on the branch since it diverged). With no worktree yet the
/// branch is clean, but committed changes are still read from the bare repo so file-overlap
/// detection stays honest. All checks are local and best-effort (empty on any git failure).
pub(crate) fn branch_worktree_status(
    cfg: &Config,
    repo: &str,
    branch: &str,
    base: &str,
) -> (Option<PathBuf>, bool, Vec<String>) {
    let Some(bare) = bare_for(cfg, repo) else {
        return (None, false, Vec::new());
    };
    let name = repo_name(&bare);
    let sanitized = sanitize_branch(branch);
    let (_, worktree_dir) = layout(cfg);
    let wt_path = worktree_dir.join(&name).join(&sanitized);
    let range = format!("{base}...{branch}");
    let changed = |dir: &str| -> Vec<String> {
        git_out(&["-C", dir, "diff", "--name-only", &range])
            .map(|s| {
                s.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    if wt_path.is_dir() {
        let wt = wt_path.to_string_lossy().to_string();
        let dirty = dirty_count(&wt) > 0;
        (Some(wt_path), dirty, changed(&wt))
    } else {
        // No worktree yet — clean, but still read committed changes vs base from the bare repo so
        // overlap detection doesn't treat an un-checked-out feature branch as touching nothing.
        (None, false, changed(&bare.to_string_lossy()))
    }
}

/// Start attention item `id`: open its worktree tmux window and mark it in-flight. Returns the
/// worktree path, or `None` if the item is unknown, carries no repo/branch, or the worktree/window
/// couldn't be opened. The terse shared core behind the `bn jump` / `bn doctor --pick` start
/// bindings (`bn attn start` keeps its own granular error messages).
pub(crate) fn start_attn_item(cfg: &Config, id: &str) -> Option<PathBuf> {
    let item = bn_core::attention::get(id)?;
    let (repo, branch) = (item.repo?, item.branch?);
    let wt_path = open_item_worktree(cfg, &repo, &branch)?;
    let _ = bn_core::attention::mark_started(id);
    Some(wt_path)
}

/// Sanitized branch names that already have a worktree directory for `repo` (the `open` picker's
/// candidate list). Empty when the repo has no worktrees yet.
///
/// Ordered **most-recently-active first**, with `main`/`master` forced last: fzf highlights the top
/// row, so this makes the default landing spot the worktree you were actually working in rather
/// than the main checkout — which hosts no agents and is meant to stay clean. Activity is the newer
/// of the worktree dir's own mtime and its `.git` pointer's (checkouts, index writes and branch
/// switches all touch one of the two); it is a heuristic for ordering a picker, nothing more.
///
/// The `_pool` container is filtered out — its slots are leased by `bn dispatch`/`bn control`, never
/// picked by hand, and it is not a branch.
pub(crate) fn existing_worktree_branches(worktree_dir: &Path, repo: &str) -> Vec<String> {
    let recency = |dir: &Path| {
        let stamp = |p: &Path| {
            std::fs::symlink_metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0)
        };
        stamp(dir).max(stamp(&dir.join(".git")))
    };
    let mut entries: Vec<(bool, u64, String)> = std::fs::read_dir(worktree_dir.join(repo))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| (e.path(), e.file_name().to_string_lossy().to_string()))
        .filter(|(_, name)| name != "_pool")
        .map(|(path, name)| {
            (
                bn_core::agent::is_reserved_branch(&name),
                recency(&path),
                name,
            )
        })
        .collect();
    // (is_main asc → main last) then (recency desc) then (name asc) for a stable tie-break.
    entries.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    entries.into_iter().map(|(_, _, name)| name).collect()
}

/// Like `fzf_select` but `--print-query`: returns the highlighted item, or — when the typed query
/// matches nothing — the query itself (so the caller can create it). None on cancel / fzf missing.
pub(crate) fn fzf_pick_or_create(items: &[String], header: &str) -> Option<String> {
    let mut child = Command::new("fzf")
        .args(["--reverse", "--print-query", "--header", header])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let input = items.join("\n");
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let out = child.wait_with_output().ok()?;
    // fzf: exit 0 = selection, 1 = no match (query only), 130 = cancelled.
    match out.status.code() {
        Some(0) | Some(1) => {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut lines = text.lines();
            let query = lines.next().unwrap_or("").trim().to_string();
            let selection = lines.next().unwrap_or("").trim().to_string();
            let chosen = if !selection.is_empty() {
                selection
            } else {
                query
            };
            if chosen.is_empty() {
                None
            } else {
                Some(chosen)
            }
        }
        _ => None,
    }
}

/// The tmux step needed to reach a worktree's window under the session-per-repo layout. Pure data
/// so the decision is unit-testable without a tmux server.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WinAction {
    /// No session for this repo yet — create it detached with this first window.
    CreateSession,
    /// Session exists but has no window for this branch — add one (detached).
    NewWindow,
    /// A window for this branch already exists and the caller isn't forcing — just select it.
    SelectExisting,
    /// A window exists but the caller forced a fresh one.
    ForceNewWindow,
}

/// Decide the window action from observed state. Pure.
pub(crate) fn decide_window(
    session_exists: bool,
    window_exists: bool,
    force_new: bool,
) -> WinAction {
    match (session_exists, window_exists, force_new) {
        (false, _, _) => WinAction::CreateSession,
        (true, false, _) => WinAction::NewWindow,
        (true, true, false) => WinAction::SelectExisting,
        (true, true, true) => WinAction::ForceNewWindow,
    }
}

/// The tmux argv(s) that realize a `WinAction`. Pure → unit-testable. Excludes the interactive
/// "follow" (switch-client/select-window), which only fires for a human at a tty.
///
/// `session` is the tmux session the window lives in — normally the repo (session-per-repo), but
/// [`ORCHESTRATOR_SESSION`] for an orchestrator/planning launch (#498). `cmd`, when set, is the
/// shell command the new window runs (e.g. an agent launch) instead of an interactive shell —
/// appended as `new-session`/`new-window`'s trailing command argument. It's only meaningful for
/// the creating actions; `SelectExisting` reuses an existing window untouched.
pub(crate) fn window_tmux_cmds(
    action: &WinAction,
    session: &str,
    branch: &str,
    wt: &str,
    cmd: Option<&str>,
) -> Vec<Vec<String>> {
    // `new-window -t` wants a *session* target with a trailing colon (`=bn:`) — tmux then appends
    // a new window and picks the index. The bare `=bn` form (fine for has-session/list-windows)
    // parses as a window/pane target and `new-window` rejects it with "can't specify pane here",
    // so every worktree after the first in a session silently got no window. The `=` keeps the
    // session match exact (so `bn` can't prefix-match `bn-foo`); the `:` is what makes it a session.
    let target = format!("={session}:");
    let argv = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let with_cmd = |mut v: Vec<String>| {
        if let Some(c) = cmd {
            v.push(c.to_string());
        }
        v
    };
    let create = match action {
        WinAction::CreateSession => with_cmd(argv(&[
            "new-session",
            "-d",
            "-s",
            session,
            "-c",
            wt,
            "-n",
            branch,
        ])),
        WinAction::NewWindow | WinAction::ForceNewWindow => with_cmd(argv(&[
            "new-window",
            "-d",
            "-t",
            &target,
            "-c",
            wt,
            "-n",
            branch,
        ])),
        WinAction::SelectExisting => return vec![],
    };
    let mut cmds = vec![create];
    // An agent window runs the harness as its pane root; with tmux's default `remain-on-exit off` a
    // harness that exits (a bad launch, a crash, a usage-limit bail-out) tears the pane down — and a
    // single-pane dispatch window closes with it, hiding the error at the exact moment you need it.
    // Keep command-backed (agent) panes on exit so a dead harness leaves an inspectable `[exited]`
    // pane with its final output. Plain shell windows (`cmd == None`) keep tmux's default.
    if cmd.is_some() {
        let win = format!("={session}:{branch}");
        cmds.push(argv(&[
            "set-window-option",
            "-t",
            &win,
            "remain-on-exit",
            "on",
        ]));
    }
    cmds
}

/// Ensure (or re-open) a worktree's tmux window: a window named after the branch in the repo's
/// session (session-per-repo, window-per-worktree — the layout `tat` expects), creating the session
/// if it's the first window. With `force_new`, always opens a fresh window even if one exists.
/// No-op when tmux is absent. Focus only follows for a human at an interactive terminal inside
/// tmux, so an agent/script invocation never yanks the attached client.
pub(crate) fn open_worktree_window(repo: &str, branch: &str, wt_path: &Path, force_new: bool) {
    open_worktree_window_cmd(repo, branch, wt_path, force_new, None, false, None);
}

/// The tmux session that hosts the orchestrator + fleet agents (controller, attention lanes,
/// dispatch orchestrator), distinct from the per-repo *ship* agent sessions. It holds **agents**,
/// never crons — crons run headlessly and never touch tmux at all. This is what `agent_live` (the
/// `bn jump` picker) surfaces and `reap` treats as sacrosanct.
pub(crate) const ORCHESTRATOR_SESSION: &str = "cron";

/// The tmux session that hosts the fleet agents — [`ORCHESTRATOR_SESSION`]. A function rather than
/// a bare const so every launch path and every protection/surfacing path (`reap`, `agent_live`,
/// `sessions`) resolves through one place and can never drift.
pub(crate) fn orchestrator_session() -> &'static str {
    ORCHESTRATOR_SESSION
}

/// Like [`open_worktree_window`], but the window runs `cmd` (an agent launch) instead of a shell,
/// and — with `prime_agent` — stamps the empty `@bn_agent` badge on the window so the agent's
/// lifecycle hooks can decorate it (working / waiting / done badges). This is the single disciplined
/// session-per-repo path `bn dispatch --agent` and `bn wt open` now share, so a dispatched worktree
/// lands in its repo's session (where `tat` expects it) whether or not you were already in tmux.
/// `session_override` places the window in a session other than the repo's — [`ORCHESTRATOR_SESSION`]
/// for an orchestrator/planning launch (#498); `None` keeps the default session-per-repo placement.
/// Returns whether a window is now available (created or already present). No-op → false when tmux
/// is absent.
pub(crate) fn open_worktree_window_cmd(
    repo: &str,
    branch: &str,
    wt_path: &Path,
    force_new: bool,
    cmd: Option<&str>,
    prime_agent: bool,
    session_override: Option<&str>,
) -> bool {
    open_worktree_window_cmd_named(
        repo,
        branch,
        branch,
        wt_path,
        force_new,
        cmd,
        prime_agent,
        session_override,
    )
}

/// Open a worktree using a readable window name while retaining the canonical repo/branch as
/// window metadata for fleet joins and cleanup.
#[allow(clippy::too_many_arguments)]
pub(crate) fn open_worktree_window_cmd_named(
    repo: &str,
    branch: &str,
    window_name: &str,
    wt_path: &Path,
    force_new: bool,
    cmd: Option<&str>,
    prime_agent: bool,
    session_override: Option<&str>,
) -> bool {
    open_worktree_window_cmd_impl(
        repo,
        branch,
        window_name,
        wt_path,
        force_new,
        cmd,
        prime_agent,
        session_override,
        true,
    )
}

/// Script-facing variant of [`open_worktree_window_cmd`] that keeps stdout machine-readable.
pub(crate) fn open_worktree_window_cmd_quiet(
    repo: &str,
    branch: &str,
    wt_path: &Path,
    force_new: bool,
    cmd: Option<&str>,
    prime_agent: bool,
    session_override: Option<&str>,
) -> bool {
    open_worktree_window_cmd_impl(
        repo,
        branch,
        branch,
        wt_path,
        force_new,
        cmd,
        prime_agent,
        session_override,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn open_worktree_window_cmd_named_quiet(
    repo: &str,
    branch: &str,
    window_name: &str,
    wt_path: &Path,
    force_new: bool,
    cmd: Option<&str>,
    prime_agent: bool,
    session_override: Option<&str>,
) -> bool {
    open_worktree_window_cmd_impl(
        repo,
        branch,
        window_name,
        wt_path,
        force_new,
        cmd,
        prime_agent,
        session_override,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn open_worktree_window_cmd_impl(
    repo: &str,
    branch: &str,
    window_name: &str,
    wt_path: &Path,
    force_new: bool,
    cmd: Option<&str>,
    prime_agent: bool,
    session_override: Option<&str>,
    announce: bool,
) -> bool {
    let session = session_override.unwrap_or(repo);
    let wt = wt_path.to_string_lossy().to_string();
    let target = format!("={session}");
    let session_exists = tmux_run(&["has-session", "-t", &target]);
    let window_exists = session_exists && {
        let windows =
            tmux(&["list-windows", "-t", &target, "-F", "#{window_name}"]).unwrap_or_default();
        windows.lines().any(|w| w == window_name)
    };
    let action = decide_window(session_exists, window_exists, force_new);
    for (i, argv) in window_tmux_cmds(&action, session, window_name, &wt, cmd)
        .into_iter()
        .enumerate()
    {
        let out = tmux_command(&argv).output();
        let ok = out.as_ref().map(|o| o.status.success()).unwrap_or(false);
        if ok {
            continue;
        }
        // The first command creates the session/window; a failure there means tmux is absent or
        // rejected the command. Surface it instead of bailing silently — a silent return here is
        // exactly how the `=bn` (no colon) target bug hid: the worktree got created, no window
        // opened, and nothing said why. Trailing commands only tune the window (e.g.
        // `remain-on-exit on`), so a failure there is best-effort — it mustn't claim the window
        // failed to open. (SelectExisting emits no commands, so this loop is skipped entirely.)
        if i == 0 {
            let detail = out
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "tmux unavailable".to_string());
            eprintln!(
                "{YELLOW}wt: could not open tmux window {session}:{window_name} — {detail}{NC}"
            );
            return false;
        }
    }
    // Target the window by its exact `=session:branch` name (it's named after the branch), so we can
    // prime the badge without capturing a window id. Harmless if a forced duplicate exists.
    let win = format!("={session}:{window_name}");
    let _ = tmux_run(&["set-window-option", "-t", &win, "@bn_repo", repo]);
    let _ = tmux_run(&["set-window-option", "-t", &win, "@bn_branch", branch]);
    let _ = tmux_run(&["set-window-option", "-t", &win, "@bn_worktree", &wt]);
    if prime_agent {
        let _ = tmux_run(&["set-window-option", "-t", &win, "@bn_agent", ""]);
    }
    if announce {
        println!("{GREEN}tmux window: {session}:{window_name}{NC}");
    }

    // Follow into it only for a human at an interactive terminal inside tmux.
    if std::env::var_os("TMUX").is_some() && std::io::stdout().is_terminal() {
        let _ = tmux_run(&["switch-client", "-t", &target]);
        let _ = tmux_run(&["select-window", "-t", &win]);
    }
    true
}

// --- tmux ------------------------------------------------------------------

/// tmux window ids belonging to a worktree. Matches windows whose active pane cwd is inside
/// `path`, OR whose name is the sanitized branch (covers windows that cd'd away). Unlike the zsh
/// version this does not require being inside tmux, so `cleanup --all`/cron can reach windows on
/// the running server.
pub(crate) fn tmux_windows_for(path: &Path, branch: &str) -> Vec<String> {
    let mut wins = std::collections::BTreeSet::new();
    let path_s = path.to_string_lossy().to_string();
    let prefix = format!("{path_s}/");

    if let Some(panes) = tmux(&[
        "list-panes",
        "-a",
        "-F",
        "#{window_id} #{pane_current_path}",
    ]) {
        for line in panes.lines() {
            if let Some((id, cwd)) = line.split_once(' ') {
                if cwd == path_s || cwd.starts_with(&prefix) {
                    wins.insert(id.to_string());
                }
            }
        }
    }
    if let Some(windows) = tmux(&[
        "list-windows",
        "-a",
        "-F",
        "#{window_id}\t#{window_name}\t#{@bn_branch}",
    ]) {
        for line in windows.lines() {
            let mut fields = line.splitn(3, '\t');
            let parsed = match (fields.next(), fields.next(), fields.next()) {
                (Some(id), Some(name), Some(recorded_branch)) => Some((id, name, recorded_branch)),
                _ => line.split_once(' ').map(|(id, name)| (id, name, "")),
            };
            if let Some((id, name, recorded_branch)) = parsed {
                let last = name.split_whitespace().last().unwrap_or(name);
                if name == branch || last == branch || recorded_branch == branch {
                    wins.insert(id.to_string());
                }
            }
        }
    }
    wins.into_iter().collect()
}

/// Kill the worktree's tmux windows. When invoked from inside tmux, the window we're sitting in is
/// killed LAST (it ends this shell) so the rest of cleanup completes first.
///
/// Each non-current window has its panes' **full process trees** terminated before the window is
/// killed (#503 gap 2): with `remain-on-exit on` a bare `kill-window` leaves the agent process
/// reparented and alive, so cleanup would leave a straggler burning CPU. The current window (our
/// own shell) is only killed — never tree-killed — so an interactive `bn close` from inside the
/// worktree doesn't shoot its own shell out mid-cleanup.
pub(crate) fn kill_tmux_windows(wins: &[String], cwd_inside: bool) {
    if wins.is_empty() {
        return;
    }
    // Kill each (non-current) window's pane process trees first — a bare `kill-window` under
    // `remain-on-exit on` would otherwise leave the agent reparented and alive.
    crate::reap::kill_window_trees(wins);
    let cur = std::env::var("TMUX")
        .ok()
        .and_then(|_| tmux(&["display-message", "-p", "#{window_id}"]));
    let mut current_matched = false;
    for win in wins {
        if cur.as_deref() == Some(win) {
            current_matched = true;
        } else {
            let _ = tmux_run(&["kill-window", "-t", win]);
        }
    }
    if current_matched {
        if let Some(cur) = cur {
            let _ = tmux_run(&["kill-window", "-t", &cur]);
        }
    } else if cwd_inside {
        println!("{YELLOW}Note: your shell is now in a removed directory — cd elsewhere{NC}");
    }
}
