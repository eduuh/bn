//! Worktree listing and navigation for `wt`: `list`, `status`, `remove`, `go`,
//! `fetch`, `remote` (worktrees from PRs opened elsewhere), and `prune`. Moved
//! verbatim from the former wt.rs god file (issue #351).

use super::*;

pub(crate) fn cmd_list(cfg: &Config) -> bool {
    let (bare_dir, _) = layout(cfg);
    match find_bare_repo(&cwd()) {
        Some(bare) => run_git(&["--git-dir", &bare.to_string_lossy(), "worktree", "list"]),
        None => {
            println!("{BLUE}All worktrees:{NC}");
            for bare in bare_repos(&bare_dir) {
                println!("\n{YELLOW}{}:{NC}", repo_name(&bare));
                run_git(&["--git-dir", &bare.to_string_lossy(), "worktree", "list"]);
            }
            true
        }
    }
}

/// Count of uncommitted entries (modified + untracked) in a worktree — what `git worktree remove`
/// refuses on without `--force`, so a non-zero count is exactly what blocks auto-cleanup.
pub(crate) fn dirty_count(wt: &str) -> usize {
    git_out(&["-C", wt, "status", "--porcelain"])
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

/// The uncommitted entries (modified + untracked) in a worktree as a path list — the same set
/// [`dirty_count`] counts, but each `git status --porcelain` line stripped of its two-column XY
/// status prefix (and any `-> ` rename arrow) so a consumer gets bare paths. Empty on a clean or
/// unreadable worktree.
pub(crate) fn dirty_files(wt: &str) -> Vec<String> {
    git_out(&["-C", wt, "status", "--porcelain"])
        .map(|s| porcelain_paths(&s))
        .unwrap_or_default()
}

/// Parse the paths out of `git status --porcelain` output: drop blank lines, strip the leading
/// two-column `XY ` status prefix, and for a rename/copy (`old -> new`) keep the current (new)
/// path. Pure so the prefix/rename handling is unit-testable without a live repo.
fn porcelain_paths(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let path = l.get(3..).unwrap_or(l).trim();
            path.rsplit(" -> ").next().unwrap_or(path).to_string()
        })
        .collect()
}

/// Report worktrees with uncommitted changes across every bare repo. The dirty check is local and
/// fast; `pr` enriches each dirty worktree with its PR state (one gh/az call each) to flag the
/// merged-but-dirty case — the worktree the poller keeps refusing — so it surfaces instead of
/// looping silently. main/master are skipped (never cleanup candidates).
pub(crate) fn cmd_status(cfg: &Config, all: bool, pr: bool) -> bool {
    let (bare_dir, _) = layout(cfg);
    let (mut stuck, mut dirty, mut clean) = (0, 0, 0);
    for bare in bare_repos(&bare_dir) {
        let repo = repo_name(&bare);
        let remote = remote_for(&bare);
        for wt in list_worktree_paths(&bare_dir, Some(&bare)) {
            let Some(branch) = git_out(&["-C", &wt, "branch", "--show-current"]) else {
                continue;
            };
            if bn_core::agent::is_reserved_branch(&branch) {
                continue;
            }
            let changes = dirty_count(&wt);
            if changes == 0 {
                clean += 1;
                if all {
                    println!("  {GREEN}✓{NC} {repo}/{branch}  clean");
                }
                continue;
            }
            dirty += 1;
            if pr {
                let state = if remote == Remote::Other {
                    "—".to_string()
                } else {
                    pr_state(&remote, &branch)
                };
                let (mark, verdict) = if state == "MERGED" {
                    stuck += 1;
                    (
                        format!("{RED}⚠{NC}"),
                        "merged but dirty — commit or stash, then it auto-clears".to_string(),
                    )
                } else {
                    (
                        format!("{YELLOW}•{NC}"),
                        format!("uncommitted work (PR {state})"),
                    )
                };
                println!("{mark} {repo}/{branch}  {changes} changes  — {verdict}");
            } else {
                println!("{YELLOW}•{NC} {repo}/{branch}  {changes} changes — uncommitted");
            }
        }
    }
    if dirty == 0 {
        println!("{GREEN}No worktrees with uncommitted changes{NC} ({clean} clean)");
    } else if pr {
        println!("{stuck} stuck (merged+dirty), {dirty} with uncommitted changes, {clean} clean");
    } else {
        println!("{dirty} with uncommitted changes, {clean} clean  (use --pr to flag merged ones)");
    }
    true
}

/// Parse `git worktree list --porcelain` output, keeping the worktree paths that don't live
/// under `bare_dir` (the bare repo's own entry). Pure → unit-testable. Matches on path
/// components, so a sibling dir like `<bare_dir>-wt/...` isn't dropped by a raw string prefix.
pub(crate) fn worktree_paths_in(listing: &str, bare_dir: &Path) -> Vec<String> {
    listing
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .filter(|p| !Path::new(p).starts_with(bare_dir))
        // Warm pool slots (`<repo>/_pool/<n>`) are real worktrees but must never be swept by
        // cleanup/prune/status — they're managed by `wt pool`. Drop any path under a `_pool` dir.
        .filter(|p| !Path::new(p).components().any(|c| c.as_os_str() == "_pool"))
        .map(|p| p.to_string())
        .collect()
}

/// Worktree paths across the given bare repo (or all repos), excluding the bare dirs themselves.
pub(crate) fn list_worktree_paths(bare_dir: &Path, bare: Option<&Path>) -> Vec<String> {
    let bares: Vec<PathBuf> = match bare {
        Some(b) => vec![b.to_path_buf()],
        None => bare_repos(bare_dir),
    };
    // git prints symlink-resolved paths in `worktree list`, so the exclusion below must compare
    // against a resolved `bare_dir` too. Unresolved, the bare repo's OWN entry survives the
    // filter and reads as a removable worktree — and `reset --force` then rm -rf's the bare repo
    // when `git worktree remove` refuses it. Bites wherever the configured root reaches the real
    // directory through a symlink: every macOS `/var/...` path (`/private/var/...` to the
    // kernel), or a symlinked `$HOME` on Linux.
    let bare_dir = &std::fs::canonicalize(bare_dir).unwrap_or_else(|_| bare_dir.to_path_buf());
    let mut out = Vec::new();
    for b in bares {
        let listing = git_out(&[
            "--git-dir",
            &b.to_string_lossy(),
            "worktree",
            "list",
            "--porcelain",
        ])
        .unwrap_or_default();
        out.extend(worktree_paths_in(&listing, bare_dir));
    }
    out
}

pub(crate) fn cmd_remove(cfg: &Config, worktree: Option<String>, force: bool) -> bool {
    let (bare_dir, worktree_dir) = layout(cfg);
    let current_bare = find_bare_repo(&cwd());
    let worktree = match worktree {
        Some(w) => w,
        None => {
            let items = list_worktree_paths(&bare_dir, current_bare.as_deref());
            if items.is_empty() {
                println!("{YELLOW}No worktrees found{NC}");
                return true;
            }
            match fzf_select(&items, "Select worktree to remove") {
                Some(w) => w,
                None => return true,
            }
        }
    };
    let wt_path = abs_worktree(&worktree, &worktree_dir);
    if !wt_path.is_dir() {
        eprintln!("{RED}Error: Worktree not found: {}{NC}", wt_path.display());
        return false;
    }
    if find_bare_repo(&wt_path).is_none() {
        eprintln!("{RED}Error: Could not find bare repo for worktree{NC}");
        return false;
    }
    println!("{YELLOW}Removing worktree: {}{NC}", wt_path.display());
    // Straight through the one reclaim path — see [`crate::wt::reclaim`]. This used to run its own
    // `git worktree remove`, which skipped every gate the other paths apply: it could remove a
    // `_pool/<n>` slot out from under a live lease (leaving the ledger holding a slot that no longer
    // exists, so the pool ran permanently one short), and it left the branch note open. A picker is
    // no reason to reclaim a worktree differently from every other caller.
    //
    // `cleanup_one` kills the worktree's windows itself, under the session guard, so this no longer
    // captures them up front.
    cleanup_one(cfg, &target_root(&wt_path), &worktree_dir, force).is_ok()
}

pub(crate) fn cmd_go(cfg: &Config, worktree: Option<String>) -> bool {
    let (_, worktree_dir) = layout(cfg);
    let worktree = match worktree {
        Some(w) => w,
        None => {
            let mut items = Vec::new();
            if let Ok(repos) = std::fs::read_dir(&worktree_dir) {
                for repo in repos.flatten().filter(|e| e.path().is_dir()) {
                    let rn = repo.file_name().to_string_lossy().to_string();
                    if let Ok(branches) = std::fs::read_dir(repo.path()) {
                        for br in branches.flatten().filter(|e| e.path().is_dir()) {
                            let bn = br.file_name().to_string_lossy().to_string();
                            // `_pool` holds warm pool slots, not branch worktrees — skip it.
                            if bn == "_pool" {
                                continue;
                            }
                            items.push(format!("{rn}/{bn}"));
                        }
                    }
                }
            }
            items.sort();
            if items.is_empty() {
                eprintln!("{YELLOW}No worktrees found{NC}");
                return true;
            }
            match fzf_select(&items, "Select worktree") {
                Some(w) => w,
                None => return true,
            }
        }
    };
    let path = worktree_dir.join(&worktree);
    if !path.is_dir() {
        eprintln!("{RED}Error: {} not found{NC}", path.display());
        return false;
    }
    // The shell wrapper cd's into this (stdout only — keep it clean).
    println!("{}", path.display());
    true
}

pub(crate) fn cmd_fetch(_cfg: &Config) -> bool {
    match find_bare_repo(&cwd()) {
        Some(bare) => {
            println!("{BLUE}Fetching all remotes...{NC}");
            let ok = run_git(&[
                "--git-dir",
                &bare.to_string_lossy(),
                "fetch",
                "--all",
                "--prune",
            ]);
            if ok {
                println!("{GREEN}Fetch complete{NC}");
            }
            ok
        }
        None => {
            eprintln!("{RED}Error: Not in a worktree context{NC}");
            false
        }
    }
}

// --- remote (create worktrees from my PRs opened on other machines) ---------

/// Map of `branch → worktree path` for every branch currently checked out across `bare`'s
/// worktrees, parsed from `git worktree list --porcelain`. Pure (testable). Bare/detached entries
/// (which carry no `branch` line) are skipped. This is what tells whether a PR already has a local
/// worktree even when that worktree lives under a non-canonical directory name.
pub(crate) fn parse_checked_out_branches(listing: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut path: Option<String> = None;
    for line in listing.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(p.to_string());
        } else if let Some(b) = line.strip_prefix("branch ") {
            let branch = b.trim().strip_prefix("refs/heads/").unwrap_or(b.trim());
            if let Some(p) = path.take() {
                map.insert(branch.to_string(), p);
            }
        } else if line.is_empty() {
            path = None;
        }
    }
    map
}

/// Branches checked out across `bare`'s worktrees → their paths (see [`parse_checked_out_branches`]).
pub(crate) fn checked_out_branches(bare_s: &str) -> HashMap<String, String> {
    let listing =
        git_out(&["--git-dir", bare_s, "worktree", "list", "--porcelain"]).unwrap_or_default();
    parse_checked_out_branches(&listing)
}

/// The path of an existing local worktree for `branch`, or `None` if the PR has no worktree yet: a
/// worktree with `branch` checked out under *any* directory name (so a PR fetched under a custom
/// name isn't offered again), else the canonical `<repo>/<sanitized-branch>` dir. The "already
/// fetched?" test for `wt remote`.
pub(crate) fn existing_worktree_for(
    checked_out: &HashMap<String, String>,
    worktree_dir: &Path,
    repo: &str,
    branch: &str,
) -> Option<String> {
    if let Some(path) = checked_out.get(branch) {
        return Some(path.clone());
    }
    let canon = worktree_dir.join(repo).join(sanitize_branch(branch));
    canon.is_dir().then(|| canon.to_string_lossy().to_string())
}

/// `bn wt remote` — fetch my open PRs for the current repo (opened from any machine) and create
/// local worktrees for them. Provider (GitHub/ADO) is detected from origin. With a `target` (PR
/// number/branch) it creates that one; with `--all` it creates every PR lacking a worktree; with
/// neither it lists candidates and, on a terminal, opens an fzf multi-picker.
pub(crate) fn cmd_remote(cfg: &Config, target: Option<String>, all: bool, no_tmux: bool) -> bool {
    let (bare_dir, worktree_dir) = layout(cfg);
    let bare = match resolve_bare_or_pick(&bare_dir) {
        Some(b) => b,
        None => return false,
    };
    let repo = repo_name(&bare);
    let bare_s = bare.to_string_lossy().to_string();
    let remote = remote_for(&bare);
    if remote == Remote::Other {
        eprintln!("{RED}wt remote: origin is not GitHub or Azure DevOps — nothing to fetch{NC}");
        return false;
    }

    println!(
        "{BLUE}Fetching your open PRs for {} ({})...{NC}",
        repo,
        remote.label()
    );
    let prs = my_open_prs(&remote);
    if prs.is_empty() {
        println!(
            "{YELLOW}No open PRs of yours found for {}{NC}",
            remote.label()
        );
        return true;
    }
    // Branches already checked out anywhere under this repo — a PR with one of these already has a
    // local worktree (possibly under a non-canonical dir name), so it's not a fetch candidate.
    let checked_out = checked_out_branches(&bare_s);

    // Explicit target: create exactly that PR (a no-op via create_remote_worktree if its worktree
    // already exists). Otherwise work over the PRs that lack a local worktree.
    let chosen: Vec<RemotePr> = if let Some(t) = target {
        match prs.iter().find(|p| p.matches(&t)) {
            Some(p) => vec![p.clone()],
            None => {
                eprintln!("{RED}wt remote: no open PR of yours matches '{t}'{NC}");
                return false;
            }
        }
    } else {
        let candidates: Vec<RemotePr> = prs
            .iter()
            .filter(|p| {
                existing_worktree_for(&checked_out, &worktree_dir, &repo, &p.branch).is_none()
            })
            .cloned()
            .collect();
        if candidates.is_empty() {
            println!("{GREEN}All your open PRs already have local worktrees.{NC}");
            return true;
        }
        if all {
            candidates
        } else if std::io::stdout().is_terminal() {
            let items: Vec<String> = candidates.iter().map(RemotePr::fzf_line).collect();
            let picked = fzf_select_multi(&items, "Select PRs → worktrees (Tab to multi-select)");
            let numbers: Vec<i64> = picked
                .iter()
                .filter_map(|l| number_from_fzf_line(l))
                .collect();
            if numbers.is_empty() {
                return true; // cancelled / nothing picked
            }
            candidates
                .into_iter()
                .filter(|p| numbers.contains(&p.number))
                .collect()
        } else {
            // Not a terminal (and no --all/target): just list the candidates so the command is
            // useful in scripts and pipelines — "fetch them in the worktree list".
            println!(
                "{BLUE}Your open PRs without a local worktree (pass a number, --all, or run interactively):{NC}"
            );
            for p in &candidates {
                println!("  #{}  {}  [{}]  {}", p.number, p.title, p.branch, p.url);
            }
            return true;
        }
    };

    let mut ok = true;
    for pr in &chosen {
        ok &= create_remote_worktree(
            &remote,
            &bare_s,
            &repo,
            &worktree_dir,
            &checked_out,
            pr,
            no_tmux,
        );
    }
    ok
}

/// Materialize a worktree for one of my PRs: favourite the branch (ADO limited-ref repos only),
/// fetch it into the bare repo, then `worktree add` it via the shared [`create_worktree`] path and
/// open its tmux window (unless `no_tmux`). Idempotent: a no-op (with a pointer to the existing
/// path) if the PR's branch is already checked out — under any directory name.
pub(crate) fn create_remote_worktree(
    remote: &Remote,
    bare_s: &str,
    repo: &str,
    worktree_dir: &Path,
    checked_out: &HashMap<String, String>,
    pr: &RemotePr,
    no_tmux: bool,
) -> bool {
    if let Some(existing) = existing_worktree_for(checked_out, worktree_dir, repo, &pr.branch) {
        println!(
            "{YELLOW}PR #{} ({}) already has a worktree: {}{NC}",
            pr.number, pr.branch, existing
        );
        println!("cd {existing}");
        return true;
    }
    let sanitized = sanitize_branch(&pr.branch);
    let wt_path = worktree_dir.join(repo).join(&sanitized);
    let _ = std::fs::create_dir_all(worktree_dir.join(repo));

    if !prepare_remote_pr_branch(remote, bare_s, pr) {
        return false;
    }
    let wt_s = wt_path.to_string_lossy().to_string();
    // pull = false: we just fetched the exact tip, so a follow-up pull is redundant.
    if !create_worktree(bare_s, &pr.branch, &wt_s, false) {
        return false;
    }
    println!(
        "{GREEN}Created worktree for PR #{}: {}{NC}",
        pr.number,
        wt_path.display()
    );
    if !no_tmux {
        open_worktree_window(repo, &sanitized, &wt_path, false);
    }
    println!("cd {}", wt_path.display());
    true
}

pub(crate) fn cmd_prune(cfg: &Config) -> bool {
    let (bare_dir, _) = layout(cfg);
    match find_bare_repo(&cwd()) {
        Some(bare) => {
            run_git(&["--git-dir", &bare.to_string_lossy(), "worktree", "prune"]);
        }
        None => {
            println!("{BLUE}Pruning all bare repos...{NC}");
            for bare in bare_repos(&bare_dir) {
                run_git(&["--git-dir", &bare.to_string_lossy(), "worktree", "prune"]);
            }
        }
    }
    println!("{GREEN}Pruned stale worktree references{NC}");
    true
}
