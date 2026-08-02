//! Worktree teardown for `wt`: `cleanup` (worktree+note+tmux), `reap`, and the
//! top-level `bn reset` / `bn rename` flows. Moved verbatim from the former
//! wt.rs god file (issue #351).

use super::*;

/// How long after a scout reports done (its note's last write) its worktree is left in place before
/// an automatic sweep reclaims it. The grace exists purely for the human: without it a finished
/// scout's tmux window vanishes the instant the next `wt-cleanup` tick runs, which reads as a crash.
/// The findings are already safe in the synced note, so this only delays disposal, never risks work.
const SCOUT_REAP_GRACE_SECS: u64 = 20 * 60;

/// True if `path` was modified within the last `within_secs`. Best-effort: any error (missing file,
/// unreadable mtime, clock skew) reads as "not recent" so the caller falls back to prior behavior.
fn recently_modified(path: &Path, within_secs: u64) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() < within_secs)
}

/// A best-effort desktop toast when a worktree is auto-reclaimed by a background sweep, so a tmux
/// window disappearing is *explained* rather than mistaken for a killed process. No work is ever
/// lost: a scout's findings live in the synced branch note, a merged ship's code is on `main`.
fn announce_reap(name: &str, reason: &str) {
    let body = match reason {
        "scout done" => {
            "Scout finished — worktree reclaimed; findings are safe in the branch note (bn cat / bn recall)."
        }
        "merged PR" => "PR merged — worktree reclaimed; the code is on main.",
        _ => "Worktree reclaimed by the cleanup sweep.",
    };
    bn_core::notify::toast(&format!("bn: reclaimed {name}"), body);
}

// --- cleanup (the worktree+note+tmux teardown) -----------------------------

/// Close-triggered cleanup: after `bn close` closes the note, reclaim the *current* worktree (and
/// its tmux window) using the same guards as `wt cleanup` — PR merged + clean tree, unless `force`.
/// Quietly no-ops (returns false) when the cwd isn't a reclaimable worktree — not under a managed
/// bare repo, or sitting on main/master — so a plain `bn close` outside a feature worktree just
/// closes the note. `cleanup_one` re-runs `bn close` internally, which is now idempotent.
pub fn cleanup_current(cfg: &Config, force: bool) -> bool {
    let (_, worktree_dir) = layout(cfg);
    let wt_path = cwd();
    if find_bare_repo(&wt_path).is_none() {
        return false; // not a managed worktree — nothing to reclaim
    }
    let branch = git_out(&["-C", &wt_path.to_string_lossy(), "branch", "--show-current"]);
    if matches!(branch.as_deref(), Some("main") | Some("master") | None) {
        return false; // never reclaim the main worktree on a note close
    }
    cleanup_one(cfg, &wt_path, &worktree_dir, force).is_ok()
}

pub(crate) fn cmd_cleanup(cfg: &Config, all: bool, force: bool, worktree: Option<String>) -> bool {
    let (bare_dir, worktree_dir) = layout(cfg);
    if all {
        return cleanup_all(cfg, &bare_dir, force);
    }
    let wt_path = match worktree {
        None => cwd(),
        Some(t) => abs_worktree(&t, &worktree_dir),
    };
    if !wt_path.is_dir() {
        eprintln!("{RED}Worktree not found: {}{NC}", wt_path.display());
        return false;
    }
    cleanup_one(cfg, &wt_path, &worktree_dir, force).is_ok()
}

pub(crate) fn cleanup_all(cfg: &Config, bare_dir: &Path, force: bool) -> bool {
    let (_, worktree_dir) = layout(cfg);
    let (mut cleaned, mut skipped) = (0, 0);
    for bare in bare_repos(bare_dir) {
        let remote = remote_for(&bare);
        if remote == Remote::Other {
            continue; // not a hosted repo we can check PR state for
        }
        let slug = remote.label();
        for wt in list_worktree_paths(bare_dir, Some(&bare)) {
            let wt_path = PathBuf::from(&wt);
            let Some(branch) = git_out(&["-C", &wt, "branch", "--show-current"]) else {
                continue;
            };
            if bn_core::agent::is_reserved_branch(&branch) {
                continue;
            }
            let state = pr_state(&remote, &branch);
            if state == "MERGED" {
                if cleanup_one(cfg, &wt_path, &worktree_dir, force).is_ok() {
                    cleaned += 1;
                } else {
                    skipped += 1;
                }
            } else {
                let label = if state == "NONE" {
                    "no PR".to_string()
                } else {
                    state
                };
                println!("{YELLOW}Skip {slug}/{branch} ({label}){NC}");
                skipped += 1;
            }
        }
    }
    println!("{GREEN}Cleaned {cleaned}, skipped {skipped}{NC}");
    true
}

/// Reap worktrees whose work is DONE and safe to remove: a scout worktree whose agent reported done
/// and is clean (findings live safely in the synced note), or a ship worktree with a merged PR.
/// Dry-run unless `execute`. Default target: current worktree; `all` sweeps every bare repo.
pub(crate) fn cmd_reap(cfg: &Config, all: bool, execute: bool) -> bool {
    let (bare_dir, worktree_dir) = layout(cfg);
    let paths: Vec<PathBuf> = if all {
        bare_repos(&bare_dir)
            .iter()
            .flat_map(|bare| list_worktree_paths(&bare_dir, Some(bare)))
            .map(PathBuf::from)
            .collect()
    } else {
        vec![cwd()]
    };
    let (mut reaped, mut candidates) = (0u32, 0u32);
    for wt_path in paths {
        if !wt_path.is_dir() {
            continue;
        }
        let Some((reason, force)) = reap_reason(cfg, &wt_path) else {
            continue;
        };
        candidates += 1;
        let name = wt_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if execute {
            if cleanup_one_for_reap(cfg, &wt_path, &worktree_dir, force).is_ok() {
                println!("{GREEN}reaped {name} ({reason}){NC}");
                announce_reap(&name, &reason);
                reaped += 1;
            } else {
                println!("{YELLOW}skip {name} — removal failed{NC}");
            }
        } else {
            println!("would reap {name} ({reason})");
        }
    }
    if candidates == 0 {
        println!("nothing to reap (no done + safe worktree)");
    } else if execute {
        println!("{GREEN}reaped {reaped}/{candidates}{NC}");
    } else {
        println!("{candidates} reapable — pass --yes to reap");
    }
    true
}

/// Why a worktree is reapable (with the `force` to pass cleanup), or `None` to keep it. "merged PR"
/// (a shipped ship — force=false so a dirty post-merge worktree is spared) or "scout done" (task=
/// scout, agent reported done, already clean, and past its [`SCOUT_REAP_GRACE_SECS`] grace — force=
/// true to bypass the no-PR check safely).
pub(crate) fn reap_reason(cfg: &Config, wt_path: &Path) -> Option<(String, bool)> {
    let wt = wt_path.to_string_lossy();
    let branch = git_out(&["-C", &wt, "branch", "--show-current"])?;
    if bn_core::agent::is_reserved_branch(&branch) {
        return None;
    }
    // A shipped ship: PR merged (reuse the existing check). force=false so cleanup still refuses a
    // dirty worktree (don't discard uncommitted post-merge edits).
    if let Some(bare) = find_bare_repo(wt_path) {
        let remote = remote_for(&bare);
        if remote != Remote::Other && pr_state(&remote, &branch) == "MERGED" {
            return Some(("merged PR".into(), false));
        }
    }
    // A finished scout: task=scout, the agent reported done, and nothing uncommitted/ahead — its
    // findings are safe in the (synced) note, so the worktree is disposable.
    let dir = wt_path.file_name()?.to_string_lossy().into_owned();
    let repo = wt_path
        .parent()?
        .file_name()?
        .to_string_lossy()
        .into_owned();
    let ctx = bn_core::resolver::Resolved { repo, branch: dir };
    let note_path = cfg.note_file(&ctx);
    let note = bn_core::store::read(&note_path).ok()?;
    let is_scout = note
        .front
        .decisions
        .iter()
        .any(|d| d.text.contains("task: scout"));
    let is_done = note.body.contains("agent done");
    let clean = dirty_count(&wt) == 0 && commits_ahead(wt_path) == 0;
    if is_scout && is_done && clean {
        // Grace: don't reclaim a *just*-finished scout — its tmux window would vanish while you're
        // still reading the result. Leave it a short while after `bn agent done` (the note's last
        // write) before disposing; the findings are already safe in the note either way.
        if recently_modified(&note_path, SCOUT_REAP_GRACE_SECS) {
            return None;
        }
        return Some(("scout done".to_string(), true));
    }
    None
}

/// Commits on the worktree ahead of the repo's default branch (main, else master); 0 if undetermined.
pub(crate) fn commits_ahead(wt: &Path) -> u32 {
    for base in ["main", "master"] {
        if let Some(n) = git_out(&[
            "-C",
            &wt.to_string_lossy(),
            "rev-list",
            "--count",
            &format!("{base}..HEAD"),
        ]) {
            if let Ok(n) = n.parse::<u32>() {
                return n;
            }
        }
    }
    0
}

/// Clean a single worktree: verify the PR, close the note, remove the worktree + branch, and
/// kill its tmux window(s).
pub(crate) fn cleanup_one(
    cfg: &Config,
    wt_path: &Path,
    worktree_dir: &Path,
    force: bool,
) -> Result<(), ()> {
    cleanup_one_with_policy(cfg, wt_path, worktree_dir, force, CleanupPolicy::Strict)
}

/// Automatic reap may discard clean commits made after a confirmed merged PR head, while keeping
/// every normal cleanup guard (dirty tree, open note work, and unmerged/rewritten divergence).
pub(crate) fn cleanup_one_for_reap(
    cfg: &Config,
    wt_path: &Path,
    worktree_dir: &Path,
    force: bool,
) -> Result<(), ()> {
    cleanup_one_with_policy(
        cfg,
        wt_path,
        worktree_dir,
        force,
        CleanupPolicy::MergedDescendant,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CleanupPolicy {
    Strict,
    MergedDescendant,
}

fn cleanup_one_with_policy(
    cfg: &Config,
    wt_path: &Path,
    worktree_dir: &Path,
    force: bool,
    policy: CleanupPolicy,
) -> Result<(), ()> {
    let Some(bare) = find_bare_repo(wt_path) else {
        eprintln!(
            "{RED}Could not find bare repo for: {}{NC}",
            wt_path.display()
        );
        return Err(());
    };
    // A detached worktree (`git branch --show-current` is empty) has no branch to map to a PR. This
    // happens when the branch was deleted out from under it — e.g. after a squash-merge + branch
    // delete, the worktree is left parked on a bare commit. Without this case it can never be
    // reclaimed: cleanup would query a PR for an empty branch and bail. Fall back to the directory
    // name as a label, and decide reclaimability by whether HEAD already landed in the default
    // branch instead of by PR state.
    let raw_branch = git_out(&["-C", &wt_path.to_string_lossy(), "branch", "--show-current"])
        .unwrap_or_default();
    let detached = raw_branch.is_empty();
    let dirname = wt_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let branch = if detached {
        dirname.clone()
    } else {
        raw_branch
    };
    if bn_core::agent::is_reserved_branch(&branch) {
        eprintln!("{RED}Refusing to clean up '{branch}' worktree{NC}");
        return Err(());
    }

    let remote = remote_for(&bare);
    let label = remote.label();
    let bare_s = bare.to_string_lossy().to_string();
    let wt_s = wt_path.to_string_lossy().to_string();

    let reclaimable = if detached {
        // No PR to query. Reclaimable iff HEAD is contained in the default branch — i.e. the work
        // has merged. Check the remote-tracking default first (reflects a just-merged PR), then the
        // local one. If neither contains it the worktree may hold unmerged commits — require --force.
        let default = git_out(&["--git-dir", &bare_s, "symbolic-ref", "--short", "HEAD"])
            .unwrap_or_else(|| "main".to_string());
        let merged = run_git(&[
            "-C",
            &wt_s,
            "merge-base",
            "--is-ancestor",
            "HEAD",
            &format!("origin/{default}"),
        ]) || run_git(&["-C", &wt_s, "merge-base", "--is-ancestor", "HEAD", &default]);
        if merged {
            println!("{GREEN}{label}/{dirname} — detached, HEAD already in {default}{NC}");
        } else {
            println!(
                "{YELLOW}{label}/{dirname} — detached worktree, HEAD not in {default} \
                 (unmerged commits?){NC}"
            );
        }
        merged
    } else {
        match pr_state(&remote, &branch).as_str() {
            "MERGED" => {
                // `pr_state` matched a merged PR by branch NAME only. Before force-deleting the
                // branch, confirm the worktree contains a merged PR head. Normal cleanup requires
                // an exact match; automatic reap also accepts clean post-merge descendants so
                // agent-authored wrap-up commits cannot wedge an orchestrate slot.
                let local_head = git_out(&["-C", &wt_s, "rev-parse", "HEAD"]);
                let merged_heads = merged_pr_heads(&remote, &branch);
                if merged_tip_reclaimable(&wt_s, local_head.as_deref(), &merged_heads, policy) {
                    println!("{GREEN}{label}/{branch} — PR merged{NC}");
                    true
                } else {
                    println!(
                        "{YELLOW}{label}/{branch} — a merged PR matches this branch name, but the \
                         local tip isn't what shipped (branch reused, or commits past the merge); \
                         use --force to clean anyway{NC}"
                    );
                    false
                }
            }
            "NONE" | "" => {
                println!("{YELLOW}{label}/{branch} — no PR found{NC}");
                false
            }
            other => {
                println!("{YELLOW}{label}/{branch} — PR is {other}, not merged{NC}");
                false
            }
        }
    };
    if !reclaimable && !force {
        println!("{YELLOW}Use --force to clean anyway{NC}");
        return Err(());
    }

    let repo = repo_name(&bare);
    let stuck_key = format!("wt-stuck:{repo}/{branch}");

    // Don't reclaim a worktree whose note still has open todos or blockers — unfinished tracked
    // work outlives the merge. --force overrides. (Resolve the note from the worktree path.)
    let ctx = resolver::resolve(cfg, wt_path).ok();
    if !force {
        if let Some(ctx) = &ctx {
            if let Ok(note) = bn_core::store::read(&cfg.note_file(ctx)) {
                let open_todos = note.front.todos.iter().filter(|t| t.is_open_todo()).count();
                let blockers = note.front.blockers.len();
                if open_todos > 0 || blockers > 0 {
                    println!(
                        "{YELLOW}{label}/{branch} — {open_todos} open todo(s), {blockers} blocker(s); \
                         skipping (finish them, or --force){NC}"
                    );
                    return Err(());
                }
            }
        }
    }

    // Merged but dirty: `git worktree remove` would refuse. Don't thrash on it every poll — leave
    // it, say why once, and raise a keyed (idempotent) attention item so `bn doctor` surfaces it
    // without re-notifying. The item is resolved below once the worktree is actually removed.
    if !force {
        let wt_str = wt_path.to_string_lossy();
        let files = dirty_files(&wt_str);
        let changes = files.len();
        if should_refuse_dirty(force, changes) {
            println!(
                "{YELLOW}{label}/{branch} — merged but {changes} uncommitted; left in place \
                 (commit or stash, then it auto-clears){NC}"
            );
            let _ = bn_core::attention::add_new(
                &bn_core::attention::NewAttn {
                    source: "wt-cleanup",
                    title: &format!(
                        "merged worktree {repo}/{branch} has {changes} uncommitted change(s) — \
                         commit or stash to reclaim it"
                    ),
                    severity: "warn",
                    repo: Some(&repo),
                    branch: Some(&branch),
                    key: Some(&stuck_key),
                    worktree_path: Some(&wt_str),
                    uncommitted_files: files,
                    ..Default::default()
                },
                &today(),
            );
            return Err(());
        }
    }

    // Resolve symlinks on both sides: a worktree path built from a symlinked config dir won't
    // string-prefix the realpath `current_dir()` returns, which would skip the step-out below
    // and leave the process sitting in a deleted directory.
    let cwd_inside = {
        let here = cwd();
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        canon(&here).starts_with(canon(wt_path)) || here.starts_with(wt_path)
    };
    let sanitized = sanitize_branch(&branch);

    // Complete + close the branch note while the worktree still resolves.
    if let Some(ctx) = &ctx {
        let _ = bn_core::commands::close(cfg, ctx, &today(), None);
    }
    // Capture the worktree's tmux window(s) BEFORE removal — panes keep a stale cwd afterward.
    let wins = tmux_windows_for(wt_path, &sanitized);

    // Kill the worktree's agent process trees up front (#503): before `git worktree remove`, so the
    // removal isn't racing a live agent still writing to the tree, and so no straggler survives even
    // if removal later fails. `kill_window_trees` skips the current window (our own shell).
    crate::reap::kill_window_trees(&wins);

    let mut remove = vec!["--git-dir", &bare_s, "worktree", "remove"];
    if force {
        remove.push("--force");
    }
    remove.push(&wt_s);
    if !run_git(&remove) {
        eprintln!("{RED}Worktree removal failed (uncommitted changes? try --force){NC}");
        return Err(());
    }

    // If we were inside the worktree, our cwd is now a deleted dir — step out before more work.
    if cwd_inside {
        let _ = std::env::set_current_dir(worktree_dir)
            .or_else(|_| std::env::set_current_dir(home()))
            .or_else(|_| std::env::set_current_dir("/"));
    }

    // No branch to delete on a detached worktree (it was already gone — that's why we're here).
    let branch_deleted = delete_local_branch(&bare_s, &branch, detached);
    rmdir_if_empty_repo(wt_path, worktree_dir);
    let _ = bn_core::lifecycle::prune(cfg); // safety net for other orphaned notes
    ack_stuck(&stuck_key); // it's gone now — clear any prior merged-but-dirty attention item
    kill_tmux_windows(&wins, cwd_inside);
    if branch_deleted {
        println!("{GREEN}Cleaned {branch}{NC}");
        Ok(())
    } else {
        eprintln!("{RED}Worktree removed, but local branch deletion failed: {branch}{NC}");
        Err(())
    }
}

fn delete_local_branch(bare_s: &str, branch: &str, detached: bool) -> bool {
    detached || crate::git::git_ok(&["--git-dir", bare_s, "branch", "-D", branch])
}

/// Resolve any open attention item carrying `key` (the merged-but-dirty notice for a worktree),
/// now that the worktree has been cleaned. No-op if none is open.
pub(crate) fn ack_stuck(key: &str) {
    for item in bn_core::attention::open_items(&today()) {
        if item.key.as_deref() == Some(key) {
            let _ = bn_core::attention::ack(&item.id);
        }
    }
}

/// Sweep away stale merged-but-dirty notices (#534). The `wt-stuck:<repo>/<branch>` item raised
/// when a merged worktree is left dirty is otherwise only cleared by [`ack_stuck`] inside
/// `cleanup_one`'s own success path — so a worktree reaped any other way (`git worktree remove
/// --force`, a manual `rm`, or removal on another host) orphans the item forever. Decoupled from
/// that single code path, this independently acks every live `wt-stuck:*` whose worktree is gone
/// from `git worktree list`, or is still present but now clean (nothing left to commit/stash).
/// Both the started/in-flight and open lanes are swept, since an orphaned notice may sit in either.
///
/// Safe to run on every cleanup poll and from `bn doctor`: a genuinely still-stuck worktree is
/// present *and* dirty, so it is never acked; re-acking an already-resolved item is a no-op. The
/// worktree-absent signal is primary — it's unambiguous — with the clean-tree signal catching the
/// case where the changes were committed/stashed out of band.
pub(crate) fn reconcile_stuck(cfg: &Config) {
    let is_stuck = |k: &Option<String>| k.as_deref().is_some_and(|k| k.starts_with("wt-stuck:"));
    let mut candidates: Vec<(String, String)> = Vec::new();
    for item in bn_core::attention::open_items(&today()) {
        if is_stuck(&item.key) {
            candidates.push((item.id, item.key.unwrap()));
        }
    }
    for s in bn_core::attention::started_items() {
        if is_stuck(&s.item.key) {
            candidates.push((s.item.id, s.item.key.unwrap()));
        }
    }
    if candidates.is_empty() {
        return;
    }

    // Map every live worktree to its stuck-key and dirty count. A key absent from this map means
    // the worktree is gone; present-with-zero means it was cleaned out of band.
    let (bare_dir, _) = layout(cfg);
    let mut live: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for bare in bare_repos(&bare_dir) {
        let repo = repo_name(&bare);
        for wt in list_worktree_paths(&bare_dir, Some(&bare)) {
            let branch = wt_branch_name(&wt);
            live.insert(format!("wt-stuck:{repo}/{branch}"), dirty_count(&wt));
        }
    }

    for (id, key) in candidates {
        let stale = match live.get(&key) {
            None => true,               // worktree gone — the notice can never self-clear
            Some(&dirty) => dirty == 0, // present but clean — nothing left to reclaim
        };
        if stale {
            let _ = bn_core::attention::ack(&id);
        }
    }
}

// --- reset (top-level `bn reset`) ------------------------------------------

/// `bn reset [--force]` — port of the bash `reset`. Enumerate every non-main worktree across all
/// bare repos; bare invocation is a dry run, `--force` rebases each repo's main, removes the
/// worktrees, closes their notes, prunes stale refs, and archives. Reuses the same git/note
/// machinery as `cleanup`, so note-status mutation goes through `commands::close` (event-logged)
/// instead of the oracle's in-place `sed`.
pub fn reset(cfg: &Config, force: bool) -> bool {
    let (bare_dir, worktree_dir) = layout(cfg);
    let bares = bare_repos(&bare_dir);
    if bares.is_empty() {
        println!("No bare repos found");
        return true;
    }

    // Non-main worktrees, across every repo. `list_worktree_paths` already drops the bare dirs.
    let mut to_remove: Vec<String> = Vec::new();
    for bare in &bares {
        for wt in list_worktree_paths(&bare_dir, Some(bare)) {
            if wt_branch_name(&wt) != "main" {
                to_remove.push(wt);
            }
        }
    }
    if to_remove.is_empty() {
        println!("No worktrees to remove (main worktrees are preserved)");
        return true;
    }

    println!("{RED}Worktrees to remove:{NC}");
    for wt in &to_remove {
        if dirty_count(wt) > 0 {
            println!("  {wt} {YELLOW}(has uncommitted changes){NC}");
        } else {
            println!("  {wt}");
        }
    }
    println!("\n{} worktree(s) will be removed", to_remove.len());

    if !force {
        println!("\n{YELLOW}Dry run. Use 'bn reset --force' to execute.{NC}");
        return true;
    }

    // Rebase each repo's main worktree onto its remote before tearing the rest down.
    println!("\n{BLUE}Rebasing main worktrees...{NC}");
    for bare in &bares {
        let repo = repo_name(bare);
        let main_wt = worktree_dir.join(&repo).join("main");
        if !main_wt.is_dir() {
            continue;
        }
        println!("  {BLUE}{repo}: fetching + updating main...{NC}");
        let _ = crate::git::git_ok(&["--git-dir", &bare.to_string_lossy(), "fetch", "origin"]);
        let ff = crate::git::git_ok(&[
            "-C",
            &main_wt.to_string_lossy(),
            "merge",
            "--ff-only",
            "origin/main",
        ]);
        if ff {
            println!("  {GREEN}{repo}: main updated{NC}");
        } else {
            println!("  {YELLOW}{repo}: main not fast-forwardable, skipped{NC}");
        }
    }

    println!();
    for wt in &to_remove {
        let wt_path = PathBuf::from(wt);
        let repo = wt_repo_name(wt);
        let branch = wt_branch_name(wt);
        println!("{YELLOW}Removing: {wt}{NC}");

        let bare = bare_dir.join(format!("{repo}.git"));
        let removed = run_git(&[
            "--git-dir",
            &bare.to_string_lossy(),
            "worktree",
            "remove",
            "--force",
            wt,
        ]);
        // Fall back to an unlink only for something that really is a worktree under the managed
        // root. `git worktree remove` refusing the path is exactly the case where a blind
        // `remove_dir_all` does the most damage — a misclassified bare repo (or anything else
        // outside `worktree_dir`) would be deleted with its history, unrecoverably, on a command
        // whose contract is "worktrees go, repos stay".
        if !removed {
            if under(&wt_path, &worktree_dir) {
                let _ = std::fs::remove_dir_all(&wt_path);
            } else {
                println!("  {YELLOW}Skipped: not under {}{NC}", worktree_dir.display());
            }
        }

        // Close the branch note (status + completion + achievement), if one exists. Derived from
        // the worktree path — its folder name is the sanitized branch the note dir is keyed on.
        let ctx = resolver::Resolved {
            repo: repo.clone(),
            branch: branch.clone(),
        };
        if cfg.note_file(&ctx).is_file() {
            let _ = bn_core::commands::close(cfg, &ctx, &today(), None);
            println!("  Closed note: {repo}/{branch}");
        }
    }

    for bare in &bares {
        run_git(&["--git-dir", &bare.to_string_lossy(), "worktree", "prune"]);
    }
    let _ = bn_core::lifecycle::archive(cfg, &today(), 30, false);

    println!(
        "{GREEN}Reset complete. Removed {} worktree(s).{NC}",
        to_remove.len()
    );
    true
}

/// Last path component of a worktree path — its sanitized branch / note folder name.
/// Whether `path` sits inside `root`, comparing symlink-resolved forms so a configured root that
/// reaches the real directory through a symlink (macOS `/var` → `/private/var`, a symlinked
/// `$HOME`) still recognizes its own worktrees. Falls back to the literal comparison when either
/// side can't be resolved. Guards destructive fallbacks, so an unresolvable path answers `false`
/// unless it matches literally — refusing to delete beats deleting the wrong tree.
pub(crate) fn under(path: &Path, root: &Path) -> bool {
    if path.starts_with(root) {
        return true;
    }
    match (std::fs::canonicalize(path), std::fs::canonicalize(root)) {
        (Ok(p), Ok(r)) => p.starts_with(r),
        _ => false,
    }
}

pub(crate) fn wt_branch_name(wt: &str) -> String {
    Path::new(wt)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Parent directory name of a worktree path — the repo it belongs to (`<wt_dir>/<repo>/<branch>`).
pub(crate) fn wt_repo_name(wt: &str) -> String {
    Path::new(wt)
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

// --- rename (top-level `bn rename` / `mv`) ---------------------------------

/// `bn rename <new>` (alias `mv`) — port of the bash `rename`. Renames the current branch and moves
/// its note folder atomically: `git branch -m`, move the note dir, rewrite the note's `branch`
/// field through `store` (vs. the oracle's `yq` on a `note.yaml` sidecar), then repush and delete
/// the old upstream ref. The note-data mutation is owned by Rust so a half-rename can't desync the
/// branch field from the folder name.
pub fn rename(cfg: &Config, new_branch: &str) -> bool {
    if new_branch.is_empty() {
        eprintln!("Usage: bn rename <new-branch>");
        return false;
    }
    if new_branch.contains(' ') || new_branch.contains("..") || new_branch.contains("//") {
        eprintln!("Invalid branch name: {new_branch}");
        return false;
    }

    let dir = cwd();
    let dir_s = dir.to_string_lossy().to_string();
    let Some(old_branch) =
        git_out(&["-C", &dir_s, "branch", "--show-current"]).filter(|b| !b.is_empty())
    else {
        eprintln!("Not on a branch (detached HEAD?)");
        return false;
    };
    if old_branch == new_branch {
        println!("Already named {new_branch} — nothing to do.");
        return true;
    }
    // refs/heads/<new> already present → refuse (git status 0 means the ref verified).
    if run_git(&[
        "-C",
        &dir_s,
        "show-ref",
        "--verify",
        "--quiet",
        &format!("refs/heads/{new_branch}"),
    ]) {
        eprintln!("Branch already exists: {new_branch}");
        return false;
    }

    // Resolve the current note dir, then derive the destination dir from the new (sanitized) branch.
    let Ok(old_ctx) = resolver::resolve(cfg, &dir) else {
        eprintln!("Not in a git repo");
        return false;
    };
    let new_ctx = resolver::Resolved {
        repo: old_ctx.repo.clone(),
        branch: sanitize_branch(new_branch),
    };
    let old_dir = cfg.note_dir(&old_ctx);
    let new_dir = cfg.note_dir(&new_ctx);
    if new_dir.exists() {
        eprintln!("Note folder already exists: {}", new_dir.display());
        return false;
    }

    // Upstream (for the repush + old-ref delete), e.g. "origin/old-branch".
    let upstream = git_out(&[
        "-C",
        &dir_s,
        "rev-parse",
        "--abbrev-ref",
        "--symbolic-full-name",
        "@{upstream}",
    ])
    .filter(|u| !u.is_empty());
    let remote = upstream.as_deref().and_then(|u| u.split_once('/'));

    println!("Renaming branch: {old_branch} → {new_branch}");
    if let Some(u) = &upstream {
        println!("  Upstream: {u}  (will be repushed)");
    }
    if old_dir.is_dir() {
        println!("  Note folder: {} → {}", old_ctx.branch, new_ctx.branch);
    }

    if !run_git(&["-C", &dir_s, "branch", "-m", &old_branch, new_branch]) {
        eprintln!("Failed to rename git branch");
        return false;
    }

    // Move the note folder and rewrite its `branch` field to match the new folder name.
    if old_dir.is_dir() {
        if let Some(parent) = new_dir.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::rename(&old_dir, &new_dir) {
            eprintln!("Warning: branch renamed but note move failed: {e}");
        } else if let Ok(mut note) = bn_core::store::read(&cfg.note_file(&new_ctx)) {
            note.front.branch = new_ctx.branch.clone();
            let _ = bn_core::store::save(&note, &cfg.note_file(&new_ctx));
        }
    }

    if let Some((remote_name, remote_branch)) = remote {
        println!("Pushing {new_branch} to {remote_name}...");
        if run_git(&["-C", &dir_s, "push", "-u", remote_name, new_branch]) {
            println!("Deleting old remote ref {remote_name}/{remote_branch}...");
            if !run_git(&["-C", &dir_s, "push", remote_name, "--delete", remote_branch]) {
                eprintln!("  (couldn't delete remote {remote_branch} — delete manually)");
            }
        } else {
            eprintln!("Note: failed to push {new_branch}. Push manually:");
            eprintln!("  git push -u {remote_name} {new_branch}");
            eprintln!("  git push {remote_name} --delete {remote_branch}");
        }
    }

    println!("\nDone. New branch: {new_branch}");
    true
}

fn merged_tip_reclaimable(
    wt: &str,
    local_head: Option<&str>,
    merged_heads: &[String],
    policy: CleanupPolicy,
) -> bool {
    let Some(local_head) = local_head else {
        return false;
    };
    merged_heads.iter().any(|head| {
        head == local_head
            || (policy == CleanupPolicy::MergedDescendant
                && run_git(&["-C", wt, "merge-base", "--is-ancestor", head, local_head]))
    })
}

fn should_refuse_dirty(force: bool, changes: usize) -> bool {
    !force && changes > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recently_modified_tracks_the_grace_window() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("note.md");
        // A missing file is never "recent" — the caller falls back to prior behavior.
        assert!(!recently_modified(&note, SCOUT_REAP_GRACE_SECS));
        // A freshly written note is inside any positive grace window...
        std::fs::write(&note, "agent done: findings").unwrap();
        assert!(recently_modified(&note, SCOUT_REAP_GRACE_SECS));
        // ...and outside a zero-second grace (nothing is newer than "now").
        assert!(!recently_modified(&note, 0));
    }

    #[test]
    fn branch_delete_failure_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!delete_local_branch(
            &dir.path().join("missing.git").to_string_lossy(),
            "dispatch/test",
            false,
        ));
        assert!(delete_local_branch("", "dispatch/test", true));
    }

    #[test]
    fn automatic_reap_accepts_only_exact_or_post_merge_descendant_tips() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_string_lossy().into_owned();
        let git = |args: &[&str]| assert!(crate::git::git_ok(args), "git {args:?} failed");
        git(&["-C", &repo, "init", "-q", "-b", "dispatch/test"]);
        git(&["-C", &repo, "config", "user.email", "t@example.com"]);
        git(&["-C", &repo, "config", "user.name", "t"]);

        std::fs::write(dir.path().join("f"), "base").unwrap();
        git(&["-C", &repo, "add", "f"]);
        git(&["-C", &repo, "commit", "-qm", "base"]);
        let base = git_out(&["-C", &repo, "rev-parse", "HEAD"]).unwrap();

        std::fs::write(dir.path().join("f"), "shipped").unwrap();
        git(&["-C", &repo, "add", "f"]);
        git(&["-C", &repo, "commit", "-qm", "shipped"]);
        let shipped = git_out(&["-C", &repo, "rev-parse", "HEAD"]).unwrap();
        let merged_heads = vec![shipped.clone()];
        assert!(merged_tip_reclaimable(
            &repo,
            Some(&shipped),
            &merged_heads,
            CleanupPolicy::Strict
        ));

        std::fs::write(dir.path().join("f"), "post-merge").unwrap();
        git(&["-C", &repo, "add", "f"]);
        git(&["-C", &repo, "commit", "-qm", "post-merge"]);
        let descendant = git_out(&["-C", &repo, "rev-parse", "HEAD"]).unwrap();
        assert!(!merged_tip_reclaimable(
            &repo,
            Some(&descendant),
            &merged_heads,
            CleanupPolicy::Strict
        ));
        assert!(merged_tip_reclaimable(
            &repo,
            Some(&descendant),
            &merged_heads,
            CleanupPolicy::MergedDescendant
        ));

        git(&["-C", &repo, "checkout", "-q", "-b", "diverged", &base]);
        std::fs::write(dir.path().join("f"), "unmerged").unwrap();
        git(&["-C", &repo, "add", "f"]);
        git(&["-C", &repo, "commit", "-qm", "unmerged"]);
        let diverged = git_out(&["-C", &repo, "rev-parse", "HEAD"]).unwrap();
        assert!(!merged_tip_reclaimable(
            &repo,
            Some(&diverged),
            &merged_heads,
            CleanupPolicy::MergedDescendant
        ));
    }

    #[test]
    fn automatic_reap_still_refuses_dirty_trees() {
        assert!(!should_refuse_dirty(false, 0));
        assert!(should_refuse_dirty(false, 1));
        assert!(!should_refuse_dirty(true, 1));
    }
}
