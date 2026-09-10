//! Worktree creation for `wt`: `clone`, `add`, `create_worktree`/`ff_pull`, and the
//! manifest-driven `clone-all`. Moved verbatim from the former wt.rs god file (issue #351).

use super::*;

// --- commands --------------------------------------------------------------

pub(crate) fn cmd_clone(cfg: &Config, url: &str, name: Option<&str>) -> bool {
    let (bare_dir, worktree_dir) = layout(cfg);
    clone_into(url, name, &bare_dir, &worktree_dir)
}

/// Like [`cmd_clone`] but into an explicit `root` (`<root>/bare`, `<root>/worktree`) instead of the
/// `$WT_ROOT`-or-config layout — how the `managed-repos` job pins clones under the managed root
/// without mutating process env.
pub(crate) fn cmd_clone_root(url: &str, root: &Path) -> bool {
    clone_into(url, None, &root.join("bare"), &root.join("worktree"))
}

fn clone_into(url: &str, name: Option<&str>, bare_dir: &Path, worktree_dir: &Path) -> bool {
    if url.is_empty() {
        ceprintln!("{RED}Error: URL required{NC}");
        return false;
    }
    let name = match name {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => Path::new(url)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
    };

    if is_regular_clone(&name) {
        let root = worktree_dir.parent().unwrap_or(worktree_dir).to_path_buf();
        let clone_path = root.join(&name);
        if clone_path.exists() {
            ceprintln!("{RED}Error: {} already exists{NC}", clone_path.display());
            return false;
        }
        cprintln!("{BLUE}Cloning {url} as regular repo (not bare)...{NC}");
        if !run_git(&["clone", url, &clone_path.to_string_lossy()]) {
            return false;
        }
        cprintln!("{GREEN}Cloned to {}{NC}", clone_path.display());
        cprintln!("{YELLOW}This is a regular repo (not bare). Use standard git commands.{NC}");
        return true;
    }

    let bare = bare_dir.join(format!("{name}.git"));
    if bare.exists() {
        ceprintln!("{RED}Error: {} already exists{NC}", bare.display());
        return false;
    }
    let _ = std::fs::create_dir_all(bare_dir);
    let _ = std::fs::create_dir_all(worktree_dir);

    cprintln!("{BLUE}Cloning {url} as bare repo...{NC}");
    if !run_git(&["clone", "--bare", url, &bare.to_string_lossy()]) {
        return false;
    }
    let bare_s = bare.to_string_lossy().to_string();
    run_git(&[
        "--git-dir",
        &bare_s,
        "config",
        "remote.origin.fetch",
        "+refs/heads/*:refs/remotes/origin/*",
    ]);
    run_git(&["--git-dir", &bare_s, "fetch", "origin"]);

    let default_branch = resolve_default_branch(&bare_s);
    let Some(default_branch) = default_branch else {
        ceprintln!("{RED}Error: {url} has no branches{NC}");
        return false;
    };
    // Point the local head at the fetched remote branch so `worktree add` checks out the tip.
    run_git(&[
        "--git-dir",
        &bare_s,
        "branch",
        "-f",
        &default_branch,
        &format!("origin/{default_branch}"),
    ]);
    let wt_path = worktree_dir.join(&name).join(&default_branch);
    let _ = std::fs::create_dir_all(worktree_dir.join(&name));
    cprintln!("{BLUE}Creating worktree for {default_branch}...{NC}");
    if !run_git(&[
        "--git-dir",
        &bare_s,
        "worktree",
        "add",
        &wt_path.to_string_lossy(),
        &default_branch,
    ]) {
        return false;
    }
    // A cloned repo's first worktree gets what `wt add` gives every new worktree: the BN-Note
    // commit-trailer hook and the repo-tier skills.
    crate::hooks::install_on_worktree_create(&wt_path);
    seed_skills(&wt_path);
    cprintln!("{GREEN}Cloned to {}{NC}", bare.display());
    cprintln!("{GREEN}Worktree: {}{NC}", wt_path.display());
    println!("cd {}", wt_path.display());
    true
}

/// The default branch of a freshly-fetched bare clone: the remote HEAD when it names a branch that
/// actually exists, else `main`/`master`, else the first branch present. Mirrors the bash
/// `clone_repo` fallback so a repo whose HEAD is unset (or isn't main/master) still gets a
/// worktree. `None` only when the clone has no branches at all.
fn resolve_default_branch(bare_s: &str) -> Option<String> {
    let has = |b: &str| run_git(&["--git-dir", bare_s, "show-ref", "--verify", "--quiet", b]);
    if let Some(def) = git_out(&["--git-dir", bare_s, "symbolic-ref", "--short", "HEAD"]) {
        if !def.is_empty() && has(&format!("refs/heads/{def}")) {
            return Some(def);
        }
    }
    for b in ["main", "master"] {
        if has(&format!("refs/heads/{b}")) {
            return Some(b.to_string());
        }
    }
    git_out(&[
        "--git-dir",
        bare_s,
        "for-each-ref",
        "--count=1",
        "--format=%(refname:short)",
        "refs/heads",
    ])
    .filter(|s| !s.is_empty())
}

pub(crate) fn cmd_add(
    cfg: &Config,
    branch: Option<String>,
    new_branch: Option<String>,
    base: Option<String>,
    no_tmux: bool,
) -> bool {
    let (bare_dir, worktree_dir) = layout(cfg);
    let bare = match resolve_bare_or_pick(&bare_dir) {
        Some(b) => b,
        None => return false,
    };
    let name = repo_name(&bare);
    let bare_s = bare.to_string_lossy().to_string();

    // -b <new> [base]: positional `branch` is treated as the base when creating a new branch.
    let (branch, create_new, base) = if let Some(nb) = new_branch {
        let base = base.or(branch).unwrap_or_else(|| base_ref(&bare_s));
        (nb, true, base)
    } else {
        let b = match branch {
            Some(b) => b,
            None => {
                let refs = git_out(&[
                    "--git-dir",
                    &bare_s,
                    "branch",
                    "-a",
                    "--format=%(refname:short)",
                ])
                .unwrap_or_default();
                let mut items: Vec<String> = refs
                    .lines()
                    .map(|l| l.trim().trim_start_matches("origin/").to_string())
                    .filter(|l| !l.is_empty() && l != "HEAD")
                    .collect();
                items.sort();
                items.dedup();
                match fzf_select(&items, "Select branch") {
                    Some(b) => b,
                    None => return true,
                }
            }
        };
        (b, false, String::new())
    };

    // Asking for main is asking for the repo's main slot. Git would refuse to check `main` out a
    // second time anyway (it is already in the main worktree), and that refusal used to be the
    // whole answer — an error where the useful reply is "here is where main-branch work happens".
    if !create_new && crate::main_slot::is_main_selection(&branch) {
        let landing = crate::main_slot::resolve(&bare_dir, &worktree_dir, &name);
        if !no_tmux {
            open_worktree_window(&name, "main", &landing, false);
        }
        println!("cd {}", landing.display());
        return true;
    }

    let sanitized = sanitize_branch(&branch);
    let wt_path = worktree_dir.join(&name).join(&sanitized);
    if wt_path.is_dir() {
        cprintln!("{YELLOW}Worktree already exists: {}{NC}", wt_path.display());
        println!("cd {}", wt_path.display());
        return true;
    }
    let _ = std::fs::create_dir_all(worktree_dir.join(&name));
    cprintln!("{BLUE}Creating worktree: {}{NC}", wt_path.display());

    let wt_s = wt_path.to_string_lossy().to_string();
    let created = if create_new {
        let ok = run_git(&[
            "--git-dir",
            &bare_s,
            "worktree",
            "add",
            "-b",
            &branch,
            &wt_s,
            &base,
        ]);
        if ok {
            ff_pull(&wt_s)
        } else {
            false
        }
    } else {
        create_worktree(&bare_s, &branch, &wt_s, true)
    };
    if !created {
        return false;
    }
    cprintln!("{GREEN}Created worktree: {}{NC}", wt_path.display());
    if !no_tmux {
        open_worktree_window(&name, &sanitized, &wt_path, false);
    }
    println!("cd {}", wt_path.display());
    true
}

/// Create a worktree for `branch` at `wt_s` off the bare repo: check out an existing local/remote
/// branch, else branch off main. Then fast-forward pull. Returns whether it was created.
pub(crate) fn create_worktree(bare_s: &str, branch: &str, wt_s: &str, pull: bool) -> bool {
    let exists = |kind: &str| {
        git_out(&[
            "--git-dir",
            bare_s,
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/{kind}/{branch}"),
        ])
        .is_some()
    };
    let created = if exists("heads") {
        run_git(&[
            "-c",
            "safe.bareRepository=all",
            "--git-dir",
            bare_s,
            "worktree",
            "add",
            wt_s,
            branch,
        ])
    } else if exists("remotes/origin") {
        // Remote-only ref → create the local branch explicitly from origin/<branch>. A bare
        // `worktree add <path> <branch>` won't DWIM a slash-named remote branch under a limited
        // fetch refspec (e.g. Office/BigMono tracks only main) — it errors "invalid reference".
        run_git(&[
            "-c",
            "safe.bareRepository=all",
            "--git-dir",
            bare_s,
            "worktree",
            "add",
            "-B",
            branch,
            wt_s,
            &format!("origin/{branch}"),
        ])
    } else {
        // Cut from the resolved base ref — `origin/main` where it exists — not from the literal
        // local `main`. The local branch lives in the main worktree and is only as current as the
        // last sync; branching off it silently starts new work behind the remote.
        let base = base_ref(bare_s);
        cprintln!("{YELLOW}Branch '{branch}' not found. Creating it from {base}...{NC}");
        run_git(&[
            "-c",
            "safe.bareRepository=all",
            "--git-dir",
            bare_s,
            "worktree",
            "add",
            "-b",
            branch,
            wt_s,
            &base,
        ])
    };
    if created {
        // New worktrees get the BN-Note trailer stamp for free (#248). Hooks live in the git
        // common dir, so after the repo's first worktree this is a quiet no-op refresh.
        crate::hooks::install_on_worktree_create(Path::new(wt_s));
        seed_skills(Path::new(wt_s));
    }
    if created && pull && !ff_pull(wt_s) {
        return false;
    }
    created
}

/// Give a freshly created worktree this repo's repo-tier skills, so an agent dispatched into it
/// can reach them.
///
/// Hooked here rather than in `bn dispatch` because every worktree deserves them, not just the
/// dispatched ones — and because creation is the one moment there is no prior state to clobber.
/// [`bn_core::skills::seed_worktree_skills`] owns both preconditions (the repo has repo-tier
/// skills; the native root is gitignored) and refuses rather than writing when either fails.
///
/// Best-effort and quiet on the common paths: a worktree without repo skills is the normal case,
/// and a skipped root is worth one line, not a failed `wt add`.
fn seed_skills(wt: &Path) {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let report = bn_core::skills::seed_worktree_skills(&bn_core::resolver::Config::load(), wt, &home);
    if report.nothing_to_seed {
        return;
    }
    if !report.is_empty() {
        cprintln!("{BLUE}Seeded {} repo skill file(s){NC}", report.changes.len());
    }
    for skip in &report.skipped {
        cprintln!(
            "{YELLOW}Skipped {} skills: {}{NC}",
            skip.harness.label(),
            skip.reason
        );
    }
}

/// The ref new work should be cut from: `origin/HEAD`'s branch, else the local default, via the
/// one base-branch resolver every other part of bn already uses. Falls back to `main` only when
/// the repo offers no answer at all — the same last resort the resolver's own callers take.
fn base_ref(bare_s: &str) -> String {
    bn_core::gitbase::base_ref(
        &bn_core::runner::RealRunner,
        bn_core::gitbase::BaseScope::BareRepo(Path::new(bare_s)),
    )
    .unwrap_or_else(|| "main".to_string())
}

/// Fast-forward a freshly-created worktree before reporting it ready.
///
/// A branch with no upstream — every `-b <new>` branch, and any local-only branch cut from a local
/// `main` — makes a bare `git pull --ff-only` die with "no tracking information". There is nothing
/// to fast-forward in that case, so skip the pull instead of failing the whole `wt add` and telling
/// the user the (perfectly usable) worktree "is not ready".
pub(crate) fn ff_pull(wt_s: &str) -> bool {
    if !crate::git::git_ok(&["-C", wt_s, "rev-parse", "--git-dir"]) {
        ceprintln!("{RED}Not a git worktree: {wt_s}{NC}");
        return false;
    }
    if !crate::git::git_ok(&["-C", wt_s, "rev-parse", "--verify", "--quiet", "@{u}"]) {
        cprintln!("{YELLOW}No upstream yet; skipping pull{NC}");
        return true;
    }
    cprintln!("{BLUE}Pulling latest...{NC}");
    if crate::git::git_ok(&["-C", wt_s, "pull", "--ff-only"]) {
        true
    } else {
        ceprintln!("{RED}Fast-forward pull failed; worktree is not ready{NC}");
        false
    }
}

// --- clone-all -------------------------------------------------------------

pub(crate) fn cmd_clone_all(cfg: &Config, here: bool) -> bool {
    let manifest = std::env::var("WT_MANIFEST")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let cfgdir = std::env::var("XDG_CONFIG_HOME")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("{}/.config", home_str()));
            format!("{cfgdir}/wt/repos")
        });
    if !Path::new(&manifest).is_file() {
        ceprintln!("{RED}No clone manifest: {manifest}{NC}");
        eprintln!("Add one git URL per line (comments with #), then re-run.");
        return false;
    }
    // Inside tmux (and not --here): run in a fresh window so a slow clone never blocks the caller.
    if !here
        && std::env::var("TMUX")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    {
        let (_, worktree_dir) = layout(cfg);
        let root = worktree_dir.parent().unwrap_or(&worktree_dir);
        let label = format!(
            "clone:{}",
            root.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        );
        let self_exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "bn".to_string());
        let runner = format!(
            "WT_MANIFEST={} {} wt clone-all --here; echo; echo '── clone-all done — press enter to close ──'; read",
            shq(&manifest),
            shq(&self_exe)
        );
        let _ = tmux_run(&["new-window", "-n", &label, &runner]);
        cprintln!("{GREEN}Cloning in new tmux window: {label} (non-blocking){NC}");
        return true;
    }
    run_clone_all(cfg, &manifest)
}

pub(crate) fn run_clone_all(cfg: &Config, manifest: &str) -> bool {
    let (bare_dir, worktree_dir) = layout(cfg);
    let root = worktree_dir.parent().unwrap_or(&worktree_dir).to_path_buf();
    let content = std::fs::read_to_string(manifest).unwrap_or_default();
    let (mut cloned, mut skipped, mut failed) = (0, 0, 0);
    for line in content.lines() {
        let url: String = line
            .split('#')
            .next()
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if url.is_empty() {
            continue;
        }
        let name = Path::new(&url)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if bare_dir.join(format!("{name}.git")).is_dir() || root.join(&name).is_dir() {
            cprintln!("{YELLOW}[{name}] already present — skip{NC}");
            skipped += 1;
            continue;
        }
        cprintln!("{BLUE}[{name}] cloning → {}{NC}", root.display());
        if cmd_clone(cfg, &url, None) {
            cloned += 1;
        } else {
            ceprintln!("{RED}[{name}] failed{NC}");
            failed += 1;
        }
    }
    cprintln!("{GREEN}Done: cloned {cloned}, skipped {skipped}, failed {failed}{NC}");
    true
}

