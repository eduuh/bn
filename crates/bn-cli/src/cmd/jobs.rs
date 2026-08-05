//! Native cron-job implementations + the `native_job` registry (run_job dispatches here).
use crate::*;
use std::cell::RefCell;

thread_local! {
    static NATIVE_JOB_OUTPUT: RefCell<String> = const { RefCell::new(String::new()) };
}

fn reset_native_job_output() {
    NATIVE_JOB_OUTPUT.with(|output| output.borrow_mut().clear());
}

fn take_native_job_output() -> String {
    NATIVE_JOB_OUTPUT.with(|output| std::mem::take(&mut *output.borrow_mut()))
}

fn native_job_line(line: &str, error: bool) {
    if error {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
    NATIVE_JOB_OUTPUT.with(|output| {
        let mut output = output.borrow_mut();
        output.push_str(line);
        output.push('\n');
    });
}

/// A natively-implemented job: `(cfg, ctx) -> exit code`.
pub(crate) type NativeJob = fn(&Config, Option<&Resolved>) -> i32;

/// The native implementation for `job`, or `None` if it hasn't been ported (→ fall back to `.sh`).
pub(crate) fn native_job(job: &str) -> Option<NativeJob> {
    match job {
        "wt-cleanup" => Some(job_wt_cleanup),
        "fleet-reap" => Some(job_fleet_reap),
        "attention-sync" => Some(job_attention_sync),
        "daily-rollover" => Some(job_daily_rollover),
        "metrics-snapshot" => Some(job_metrics_snapshot),
        "gather-findings" => Some(job_gather_findings),
        "dojo-cards" => Some(job_dojo_cards),
        "weekly-insights" => Some(job_weekly_insights),
        "agent-observe" => Some(job_agent_observe),
        "auto-review-poll" => Some(job_auto_review_poll),
        "ensure-tmux" => Some(job_ensure_tmux),
        "notify" => Some(job_notify),
        "standup-push" => Some(job_standup_push),
        "wrap-digest" => Some(job_wrap_digest),
        "auto-review-trigger" => Some(job_auto_review_trigger),
        "attention-auto-trigger" => Some(job_attention_auto_trigger),
        "agent-supervise" => Some(job_agent_supervise),
        "pr-queue" => Some(job_pr_queue),
        "night-shift" => Some(job_night_shift),
        "pr-observe" => Some(job_pr_observe),
        "quarter-review" => Some(job_quarter_review),
        "staff-chief-am" => Some(job_staff_chief_am),
        "staff-chief-pm" => Some(job_staff_chief_pm),
        "staff-archivist" => Some(job_staff_archivist),
        "staff-curriculum" => Some(job_staff_curriculum),
        "staff-interviewer" => Some(job_staff_interviewer),
        "sentinel" => Some(job_sentinel),
        "staff-mentor" => Some(job_staff_mentor),
        "staff-eye-daily" => Some(job_staff_eye_daily),
        "staff-eye-weekly" => Some(job_staff_eye_weekly),
        "routine-notify" => Some(crate::cmd::routine::job_routine_notify),
        "routine-tick" => Some(crate::cmd::routine::job_routine_tick),
        "today-agent" => Some(job_today_agent),
        "staff-archive" => Some(job_staff_archive),
        "commit-notes" => Some(job_commit_notes),
        "watch" => Some(job_watch),
        "recall-index" => Some(job_recall_index),
        "managed-repos" => Some(job_managed_repos),
        "refresh-all" => Some(job_refresh_all),
        "self-update" => Some(job_self_update),
        "tools-reconcile" => Some(job_tools_reconcile),
        "keep-warm" => Some(crate::cmd::orchestrate::job_keep_warm),
        "pool-warm" => Some(crate::wt_pool::job_pool_warm),
        "cockpit-ensure" => Some(crate::cmd::cockpit::job_cockpit_ensure),
        "cron-health" => Some(crate::cmd::attn_agent::job_cron_health),
        "attention-agent" => Some(crate::cmd::attn_agent::job_attention_agent),
        "info-gather" => Some(crate::cmd::cron::job_info_gather),
        _ => None,
    }
}

/// What the committer should do for one notes repo, given its git state. Pure so it's testable
/// without a real remote: `dirty` = uncommitted/untracked changes present, `has_upstream` = the
/// branch tracks a remote, `ahead` = local commits not yet pushed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NotesAction {
    /// Nothing uncommitted and nothing unpushed — no commit, no network.
    Skip,
    /// Commit the dirty tree, but there's no upstream to push to (commit-only).
    CommitOnly,
    /// Commit if dirty, then rebase-on-remote and push (also runs when only unpushed commits
    /// remain from a prior run that committed but couldn't push).
    CommitAndPush,
}

pub(crate) fn notes_action(dirty: bool, has_upstream: bool, ahead: u64) -> NotesAction {
    if !dirty && ahead == 0 {
        return NotesAction::Skip;
    }
    if has_upstream {
        NotesAction::CommitAndPush
    } else {
        NotesAction::CommitOnly
    }
}

/// A timestamped, host-tagged conventional message for an auto-checkpoint commit. Kept simple and
/// deterministic (no Claude round-trip) so the committer never hangs on an unattended run.
pub(crate) fn notes_commit_message(host: &str, stamp: &str) -> String {
    format!("notes: auto-checkpoint {stamp} ({host})")
}

/// `commit-notes` — auto-commit (and push, where an upstream exists) the branch-notes repos. This
/// is the cross-platform, bn-native committer that replaced the Linux-only systemd unit (#471): as
/// a native job it runs from `bn cron register`'s launchd agents on macOS and Windows tasks too,
/// so notes can't silently stop syncing on a platform without systemd.
///
/// Only attempts work when there's something to do (a clean tree with nothing unpushed is skipped —
/// no commit, no network). When dirty it stages everything (`git add -A`, so untracked dispatch
/// note dirs land too) and commits with a timestamped message. When the branch has an upstream it
/// rebases on the remote first (multi-host: several machines push here) so a push from another
/// machine doesn't reject us, then pushes. Never blocks on credentials. Returns non-zero if any
/// repo's commit or push failed, so the scheduler/`bn cron` surfaces the stall.
pub(crate) fn job_commit_notes(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use std::path::PathBuf;

    let host = cfg.machine();
    let stamp = bn_core::util::now_stamp();

    // `git -C <dir> <args...>` → (success, trimmed stdout), routed through the shared git seam
    // (`crate::git`) so it stays testable and the raw-subprocess guard is satisfied. A commit/push
    // that succeeds yields `(true, "")` (empty stdout); a failure yields `(false, "")`.
    let git = |dir: &std::path::Path, args: &[&str]| -> (bool, String) {
        match crate::git::git_capture(args, Some(dir), true) {
            Some(out) => (true, out),
            None => (false, String::new()),
        }
    };

    let mut rc = 0;
    let mut seen: Vec<PathBuf> = Vec::new();
    for dir in [cfg.personal_root.clone(), cfg.work_root.clone()] {
        if dir.as_os_str().is_empty() || !dir.exists() {
            continue;
        }
        let (ok, top) = git(&dir, &["rev-parse", "--show-toplevel"]);
        if !ok || top.is_empty() {
            continue; // not a git repo
        }
        let root = PathBuf::from(top);
        if seen.contains(&root) {
            continue; // dedupe: personal + work may share a root
        }
        seen.push(root.clone());

        // Self-heal any stale lock / interrupted rebase before touching the repo, so one crashed
        // run can't wedge notes-sync indefinitely (the chronic commit-notes failure trap).
        heal_stale_git_state(&root);

        let dirty = !git(&root, &["status", "--porcelain"]).1.is_empty();
        let has_upstream = git(&root, &["rev-parse", "--abbrev-ref", "@{u}"]).0;
        let ahead: u64 = if has_upstream {
            git(&root, &["rev-list", "--count", "@{u}..HEAD"])
                .1
                .parse()
                .unwrap_or(0)
        } else {
            0
        };

        match notes_action(dirty, has_upstream, ahead) {
            NotesAction::Skip => {
                println!("clean: {}", root.display());
                continue;
            }
            NotesAction::CommitOnly => {
                if commit_all(&git, &root, host, &stamp) {
                    println!("no upstream; commit-only: {}", root.display());
                } else {
                    rc = 1;
                }
                continue;
            }
            NotesAction::CommitAndPush => {
                if dirty && !commit_all(&git, &root, host, &stamp) {
                    rc = 1;
                    continue;
                }
                let branch = git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]).1;
                if git(&root, &["pull", "--rebase", "--autostash", "-q"]).0 {
                    if git(&root, &["push", "-q"]).0 {
                        println!("pushed: {} ({branch})", root.display());
                    } else {
                        eprintln!(
                            "push failed (auth/network?); local commit kept: {} ({branch})",
                            root.display()
                        );
                        rc = 1;
                    }
                } else {
                    let _ = git(&root, &["rebase", "--abort"]);
                    eprintln!(
                        "rebase failed (conflict?); local commit kept, push skipped: {} ({branch})",
                        root.display()
                    );
                    rc = 1;
                }
            }
        }
    }
    rc
}

/// Stage everything (`git add -A`) and commit with a timestamped message. Returns false (and logs)
/// if the commit failed. Split out so both the commit-only and commit+push paths share it.
/// How old `.git/index.lock` must be before commit-notes treats it as abandoned. A notes
/// auto-committer never legitimately holds the index for minutes, so a lock older than this is a
/// crashed/killed git — clearing it is what stops a single dead run from silently wedging
/// notes-sync forever (the exact stall that ran commit-notes 4/5 failed on a real host).
pub(crate) const STALE_LOCK_AGE: std::time::Duration = std::time::Duration::from_secs(120);

/// True when `.git/index.lock`'s age means it is an abandoned lock we should clear: at or past
/// [`STALE_LOCK_AGE`], or of unknown age (no mtime → treat as stale rather than wedge forever).
pub(crate) fn index_lock_is_stale(age: Option<std::time::Duration>) -> bool {
    age.map(|a| a >= STALE_LOCK_AGE).unwrap_or(true)
}

/// Self-heal the stale git state that otherwise wedges *every* subsequent commit-notes run: an
/// abandoned `.git/index.lock` (older than [`STALE_LOCK_AGE`]) and a leftover interrupted rebase
/// (`rebase-merge`/`rebase-apply`, left behind when a prior run was killed mid-`pull --rebase`).
/// Both are self-heal but never silent — each action is printed. Returns the messages (for tests).
pub(crate) fn heal_stale_git_state(root: &std::path::Path) -> Vec<String> {
    let git_dir = root.join(".git");
    // A worktree's `.git` is a file, and a non-repo has none — nothing to heal in either case.
    if !git_dir.is_dir() {
        return Vec::new();
    }
    let mut healed = Vec::new();

    let lock = git_dir.join("index.lock");
    if let Ok(md) = std::fs::metadata(&lock) {
        let age = md.modified().ok().and_then(|m| m.elapsed().ok());
        if index_lock_is_stale(age) && std::fs::remove_file(&lock).is_ok() {
            let msg = format!("healed: cleared stale index.lock: {}", root.display());
            println!("{msg}");
            healed.push(msg);
        }
    }

    // An interrupted rebase leaves these dirs; the commit-notes rebase path aborts on its own
    // failures, so a leftover means a crash/kill. Abort to restore HEAD, then force-remove if the
    // abort itself couldn't (e.g. an untracked file blocked the checkout).
    for name in ["rebase-merge", "rebase-apply"] {
        let dir = git_dir.join(name);
        if dir.exists() {
            let _ = crate::git::git_capture(&["rebase", "--abort"], Some(root), true);
            let _ = std::fs::remove_dir_all(&dir);
            if !dir.exists() {
                let msg = format!(
                    "healed: cleared interrupted rebase ({name}): {}",
                    root.display()
                );
                println!("{msg}");
                healed.push(msg);
            }
        }
    }
    healed
}

fn commit_all(
    git: &impl Fn(&std::path::Path, &[&str]) -> (bool, String),
    root: &std::path::Path,
    host: &str,
    stamp: &str,
) -> bool {
    git(root, &["add", "-A"]);
    let msg = notes_commit_message(host, stamp);
    if git(root, &["commit", "-q", "-m", &msg]).0 {
        println!("committed: {} — {msg}", root.display());
        true
    } else {
        eprintln!("commit failed: {}", root.display());
        false
    }
}

/// `watch` — one pass of the wake reactor (the cron form of `bn watch --once`). Toasts anything
/// newly needing you (agents waiting/blocked/stuck, agent completions, urgent attention) since the
/// last run; quiet when nothing changed. Native so it toasts identically on every platform.
pub(crate) fn job_watch(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    emit_watch(cfg, true, 30, bn_core::agent::DEFAULT_STUCK_MINS, false);
    0
}

/// `recall-index` — keep the machine-local semantic-recall index warm (#249): (re)embed only the
/// notes/investigation docs whose fingerprint changed since the last run, so it's cheap on a
/// schedule. The cron form of `bn recall --index`; fully offline and idempotent.
pub(crate) fn job_recall_index(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    emit_recall(cfg, "", None, true, 20, false);
    0
}

/// `managed-repos` — clone + refresh the repos listed in `[managed_repos]` under the bn-managed
/// root (`~/.bn/repos`), using bn's own bare+worktree layout so `wt`/`tat` can pick them up. This
/// is how dotfiles (and any always-checked-out repo) land on a new machine without a manual
/// `git clone`. Idempotent: clone-if-absent, else fetch + fast-forward each worktree. Best-effort —
/// a single repo failing never aborts the rest, so it always exits 0 (matching the shell job it
/// replaced). Native so the git fan-out runs identically on Linux/macOS/Windows.
pub(crate) fn job_managed_repos(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let root = resolver::managed_repos_dir();
    if root.as_os_str().is_empty() {
        eprintln!("managed-repos: empty managed root");
        return 1;
    }
    let bare_root = root.join("bare");

    for url in &cfg.managed_repos {
        if url.is_empty() {
            continue;
        }
        let name = url.rsplit('/').next().unwrap_or(url);
        let name = name.strip_suffix(".git").unwrap_or(name);
        let bare = bare_root.join(format!("{name}.git"));

        if bare.is_dir() {
            let bare_s = bare.to_string_lossy();
            if !crate::git::git_ok(&[
                "--git-dir",
                &bare_s,
                "fetch",
                "--quiet",
                "--prune",
                "origin",
            ]) {
                eprintln!("managed-repos: fetch failed for {name}");
            }
            fast_forward_worktrees(&bare_s);
            println!("managed-repos: refreshed {name}");
        } else {
            println!("managed-repos: cloning {name}…");
            // Pin the clone under the managed root (mirrors the shell job's `WT_ROOT=$root`), but
            // via an explicit-root arg rather than mutating process env.
            if wt::cmd_clone_root(url, &root) {
                println!("managed-repos: cloned {name} -> {}", bare.display());
            } else {
                eprintln!("managed-repos: clone failed for {name} ({url})");
            }
        }
    }
    0
}

/// Fast-forward every worktree of a managed bare repo to its `origin/<branch>` counterpart
/// (best-effort — dirty or diverged worktrees are skipped, and a failed merge is ignored). Mirrors
/// the shell job's per-worktree `git merge --ff-only origin/$branch` fan-out.
fn fast_forward_worktrees(bare_s: &str) {
    let listing = crate::git::git_capture(
        &["--git-dir", bare_s, "worktree", "list", "--porcelain"],
        None,
        true,
    )
    .unwrap_or_default();

    // Parse `worktree <path>` / `branch refs/heads/<name>` pairs; detached/bare entries (no branch
    // line) are skipped.
    let mut path: Option<String> = None;
    for line in listing.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(p.trim().to_string());
        } else if let Some(b) = line.strip_prefix("branch ") {
            let branch = b.trim().strip_prefix("refs/heads/").unwrap_or(b.trim());
            if let Some(p) = path.take() {
                if std::path::Path::new(&p).is_dir() {
                    let _ = crate::git::git_ok(&[
                        "-C",
                        &p,
                        "merge",
                        "--ff-only",
                        &format!("origin/{branch}"),
                    ]);
                }
            }
        } else if line.trim().is_empty() {
            path = None;
        }
    }
}

/// `refresh-all` — the daily keep-current sweep. Clones/fast-forwards the bn-managed repos
/// (`[managed_repos]`, so dotfiles & co. stay current) and reconciles the cron timers with the
/// manifest (so a `crons.toml` change pulled into the repo takes effect without a manual
/// `bn cron register`). Both steps are idempotent and best-effort; always exits 0.
///
/// Note: the shell job it replaced also ran `bn refresh-all` up front, but no such subcommand ever
/// existed — that line errored to a no-op — so the native handler drops it and does the two real
/// steps only (#485).
pub(crate) fn job_refresh_all(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    let _ = job_managed_repos(cfg, ctx);
    emit_cron_register(cfg, ctx, false, false);
    0
}

/// Locate bn's `bn/main` worktree in `git worktree list --porcelain` output — the first
/// `worktree <path>` line whose path ends in `/bn/main`. Pure so it's unit-testable.
fn parse_main_worktree(porcelain: &str) -> Option<String> {
    porcelain
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(str::trim)
        .find(|p| p.ends_with("/bn/main"))
        .map(str::to_string)
}

/// True if `cargo` is on `PATH` — the native form of the shell job's `command -v cargo`.
fn cargo_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let c = dir.join("cargo");
        c.is_file() || c.with_extension("exe").is_file()
    })
}

/// True if `p` is an executable file (unix exec bit), mirroring the shell job's `[ -x ... ]`.
fn is_executable_file(p: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// Resolve bn's `bn/main` worktree from the install anchor (`~/.config/bn/repo`, a symlink to the
/// checkout `install.sh` ran from), falling back to `$BN_MAIN_WORKTREE` and then the worktree-dir
/// convention. Used by `self-update` so the committed `install.sh` is resolved regardless of the
/// cron's arbitrary cwd, and it stays robust even when bn was installed from a feature worktree.
fn resolve_bn_main_worktree() -> String {
    let home = wt::home_str();
    let anchor = std::fs::canonicalize(format!("{home}/.config/bn/repo")).ok();
    anchor
        .and_then(|a| {
            let a = a.to_string_lossy().into_owned();
            crate::git::git_capture(&["-C", &a, "worktree", "list", "--porcelain"], None, true)
        })
        .and_then(|out| parse_main_worktree(&out))
        .or_else(|| {
            std::env::var("BN_MAIN_WORKTREE")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| format!("{home}/projects/worktree/bn/main"))
}

/// `self-update` — keep the installed `bn` in lock-step with `bn/main` by re-running the repo's own
/// `install.sh` whenever main advances. install.sh is the single source of truth for how bn is
/// built and wired (binaries, the bn-bash fallback, the `~/.config/bn` anchors, cron registration,
/// MCP registration), so we delegate to it rather than reimplementing a cargo build here — that
/// would drift on a new cron/MCP/anchor a fresh main needs. Cheap to run often: it fast-forwards
/// main and only re-installs when HEAD actually moved past the last build (tracked in a stamp
/// file). No cargo (e.g. Codespaces, which run a prebuilt release) → it no-ops. Native port of
/// `jobs/self-update.sh`: the git fan-out and stamp bookkeeping run in-process; only the final
/// build+wire step still delegates to `install.sh --core` (which stays shell — it bootstraps bn
/// before bn exists). `--core` skips the tmux layer, whose helpers are symlinks into this worktree
/// and so already track main without re-linking.
pub(crate) fn job_self_update(_cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use std::path::{Path, PathBuf};
    let home = wt::home_str();

    let bindir = std::env::var_os("BN_BIN_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{home}/.bin")));
    // Stamp lives at the legacy `~/.config/bn/.built-at` (where install.sh anchors bn), matching
    // the shell job so a first native run doesn't force a spurious rebuild.
    let cfg_base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{home}/.config")));
    let stamp = cfg_base.join("bn").join(".built-at");

    // Resolve bn's main worktree — robust even when bn was installed
    // from a feature worktree.
    let main_wt = resolve_bn_main_worktree();

    if !Path::new(&main_wt).is_dir() {
        eprintln!("self-update: bn main worktree not found ({main_wt})");
        return 1;
    }

    if !cargo_on_path() {
        println!("self-update: cargo absent — leaving the prebuilt binary in place");
        return 0;
    }

    // Best-effort fast-forward of main (matches the shell's `|| true`).
    let _ = crate::git::git_ok(&["-C", &main_wt, "fetch", "--quiet", "origin", "main"]);
    let _ = crate::git::git_ok(&["-C", &main_wt, "merge", "--ff-only", "origin/main"]);

    let head = match crate::git::git_capture(
        &["-C", &main_wt, "rev-parse", "--short", "HEAD"],
        None,
        true,
    ) {
        Some(h) if !h.is_empty() => h,
        _ => {
            eprintln!("self-update: cannot resolve HEAD in {main_wt}");
            return 1;
        }
    };

    // Skip the (re)install when the installed binary already matches the current HEAD.
    let built = std::fs::read_to_string(&stamp)
        .ok()
        .map(|s| s.trim().to_string());
    if built.as_deref() == Some(head.as_str()) && is_executable_file(&bindir.join("bn")) {
        return 0;
    }

    // Single source of truth: install.sh rebuilds from source and re-wires everything. It is not a
    // git/gh/tmux binary, so spawning it directly is fine (the subprocess guard only covers those).
    let installer = Path::new(&main_wt).join("install.sh");
    match std::process::Command::new(&installer)
        .arg("--core")
        .current_dir(&main_wt)
        .status()
    {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("self-update: install.sh --core failed ({s})");
            return s.code().unwrap_or(1);
        }
        Err(e) => {
            eprintln!("self-update: cannot run {}: {e}", installer.display());
            return 1;
        }
    }

    if let Some(dir) = stamp.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&stamp, format!("{head}\n"));
    println!("self-update: ran install.sh --core @ {head}");
    0
}

/// `tools-reconcile` cron (#653) — the keep-alive loop for the managed "tools" subsystem: for
/// every declared docker-backed tool (opted-in built-ins + `<config_dir>/tools.toml`), re-assert
/// the "up" desired state (create if absent, start if stopped, no-op if running). Best-effort per
/// tool — one failing tool doesn't stop the others; the exit code is non-zero only if at least one
/// tool couldn't be brought up, so the scheduler / `bn cron` surfaces the stall.
pub(crate) fn job_tools_reconcile(_cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use bn_core::runner::RealRunner;
    use bn_core::tools::{self, DeployOutcome};
    let specs = tools::all_specs();
    if specs.is_empty() {
        native_job_line("tools-reconcile: no tools declared", false);
        return 0;
    }
    // All managed tools are docker-backed. On a host with no docker on PATH (a builder without
    // Docker Desktop, a WSL box with the daemon down) every `docker inspect`/`run` fails, so the
    // job would return non-zero on EVERY tick forever — the chronic-failure trap (539/540 silent
    // fails). Skip cleanly instead: nothing to reconcile without a daemon isn't a failure.
    let path = std::env::var("PATH").unwrap_or_default();
    if !bn_core::mcp::command_on_path("docker", &path)
        && !bn_core::mcp::command_on_path("docker.exe", &path)
    {
        println!(
            "tools-reconcile: docker not on PATH — skipping ({} tool(s))",
            specs.len()
        );
        return 0;
    }
    let mut rc = 0;
    for spec in &specs {
        match tools::reconcile(&RealRunner, spec) {
            DeployOutcome::Created => {
                native_job_line(&format!("tools-reconcile: {} created", spec.name), false)
            }
            DeployOutcome::Started => {
                native_job_line(&format!("tools-reconcile: {} started", spec.name), false)
            }
            DeployOutcome::AlreadyRunning => native_job_line(
                &format!("tools-reconcile: {} already running", spec.name),
                false,
            ),
            DeployOutcome::Failed(e) => {
                native_job_line(
                    &format!("tools-reconcile: {} FAILED — {e}", spec.name),
                    true,
                );
                rc = 1;
            }
        }
    }
    rc
}

/// Run a native job with the same bookkeeping as the script path: the per-machine `only_on` gate,
/// the `bn cron running` marker, and a `cron_run` event with duration + exit code. Native jobs are
/// global scope (they live in the binary, not a note dir), so the event is tagged `global`. The
/// native jobs stream straight to stdout/stderr; jobs that opt into [`native_job_line`] also
/// retain the same text in the event tail for `bn cron show` / `bn cron failures`.
pub(crate) fn run_native_job(
    cfg: &Config,
    ctx: Option<&Resolved>,
    job: &str,
    f: impl FnOnce(&Config, Option<&Resolved>) -> i32,
) -> i32 {
    use std::time::Instant;
    if !cfg.job_allowed_here(job) {
        println!(
            "bn run: skipping '{job}' — not configured to run on '{}'",
            cfg.machine()
        );
        return 0;
    }
    let start = today_epoch();
    let t0 = Instant::now();
    let _running = bn_core::events::running_marker(job);
    reset_native_job_output();
    let code = f(cfg, ctx);
    let tail = tail_lines(&take_native_job_output(), 15);
    let duration_ms = t0.elapsed().as_millis() as i64;
    let _ = bn_core::events::log_cron_run(
        job,
        resolver::JobScope::Global.label(),
        None,
        None,
        start,
        duration_ms,
        code,
        &tail,
    );
    code
}

/// `staff-archive` cron — the #275 store-retention sweep: compact fully-elapsed quarters of feed
/// cards into a per-quarter headline index and roll up `staff_run` ledger rows older than a year
/// into monthly aggregates. Idempotent, so a weekly (or caught-up-after-a-miss) run is always safe;
/// chronicles and retro transcripts are exempt. Always exits 0 — retention is best-effort
/// housekeeping, never a page.
pub(crate) fn job_staff_archive(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    emit_staff_archive(cfg, false, false);
    0
}

/// `jobs/wt-cleanup.sh` — reclaim finished worktrees: merged-PR ships (cleanup), then done+clean
/// scouts (reap). Both sweep all repos; the reap keeps the note (findings) while removing the
/// disposable scout worktree, so investigation worktrees don't pile up.
pub(crate) fn job_wt_cleanup(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    // #534: clear orphaned merged-but-dirty notices whose worktrees were reaped out of band before
    // reclaiming the current crop — decoupled from the single `cleanup_one` removal path.
    wt::reconcile_stuck(cfg);
    let cleanup = wt::run_code(
        cfg,
        wt::WtCmd::Cleanup {
            all: true,
            force: false,
            worktree: None,
        },
    );
    let reap = wt::run_code(
        cfg,
        wt::WtCmd::Reap {
            all: true,
            yes: true,
        },
    );
    cleanup.max(reap)
}

/// `fleet-reap` (#503) — the fleet garbage-collector: terminate orphaned/stale agent process trees
/// (window gone / worktree removed / PR merged/closed) and prune dead tmux windows. This is what
/// makes the reaper automatic on macOS, where the systemd `wt-cleanup` timer never fires (#471):
/// `bn cron register` materializes it as a launchd timer. Acts (kills), checks PR state (the cron
/// can afford the network), and honors every safety gate — never a working / WIP / orchestrator
/// agent.
pub(crate) fn job_fleet_reap(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    reap::run_reap(
        cfg,
        reap::ReapOpts {
            kill: true,
            check_pr: true,
            ..reap::ReapOpts::default()
        },
    )
}

/// `jobs/attention-sync.sh` — recompute + rewrite the attention cache (`bn next --refresh`).
pub(crate) fn job_attention_sync(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    // refresh=true, no_momentum=true: the cache write is always punch-free (momentum is a
    // read-time re-rank), so the sync job never bakes the current punch into the snapshot.
    emit_next(cfg, 5, false, false, true, false, true, false, false);
    println!("attention-sync: attention cache refreshed");
    0
}

/// `jobs/daily-rollover.sh` — roll today's daily goals forward (`bn daily rollover`).
pub(crate) fn job_daily_rollover(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    match bn_core::daily::rollover(cfg, &today()) {
        Ok(r) => {
            print!("{}", payload::rollover_text(&r));
            0
        }
        Err(e) => {
            eprintln!("daily-rollover: {e}");
            1
        }
    }
}

/// `jobs/metrics-snapshot.sh` — write today's scorecard snapshot (`bn metrics --snapshot`). Calls
/// the core writer directly (rather than `emit_metrics`) so a failure is recorded, not `exit`ed.
pub(crate) fn job_metrics_snapshot(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use bn_core::clock::Clock;
    let clock = bn_core::clock::RealClock;
    let now_today = clock.today();
    let s = bn_core::metrics::scorecard(
        cfg,
        &bn_core::runner::RealRunner,
        clock.now_epoch_secs(),
        30,
        &now_today,
    );
    match bn_core::metrics::write_snapshot(cfg, &s, &now_today) {
        Ok(p) => {
            println!("metrics-snapshot: snapshot written: {}", p.display());
            0
        }
        Err(e) => {
            eprintln!("metrics-snapshot: {e}");
            1
        }
    }
}

/// The `gather-findings` cron (#897) — snapshot the local morning-planning caches into the
/// findings store ahead of `today-agent`. Runs the same offline adapters as `bn findings gather`
/// (PR review queue, your open PRs, inbound feed, stale/blocked branches, night-shift reviewables,
/// failing crons, sentinel, quarter goals, dojo cards), recording one durable run per non-empty
/// source. Additive and read-only over the caches: no network, no re-fetch, and no
/// feed/attention/daily side effect. A gather error is a non-fatal exit 1.
pub(crate) fn job_gather_findings(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    match bn_core::findings_gather::run(cfg) {
        Ok(report) => {
            native_job_line(
                &format!(
                    "gather-findings: {} source(s) recorded, {} item(s)",
                    report.recorded(),
                    report.total_items()
                ),
                false,
            );
            for s in report.sources.iter().filter(|s| s.recorded) {
                native_job_line(
                    &format!("  [{}] {} — {} item(s)", s.tier, s.source, s.items),
                    false,
                );
            }
            0
        }
        Err(e) => {
            native_job_line(&format!("gather-findings: {e}"), true);
            1
        }
    }
}

/// The `dojo-cards` cron (#253) — mine new learning cards for the current repo from its recent
/// history and structure, so `bn dojo` always has fresh, pre-prepared material. Opt-in via
/// `[dojo].enabled`; honors the `[dojo] repos` rotation allowlist (empty = mine wherever it runs).
/// Reads `git log` for recent `fix:`/`feat:` commits (bug mysteries + walkthroughs) and the
/// module tree (architecture tours), appending only cards whose id is new. Read-only over the
/// repo; a git failure yields zero cards rather than an error.
pub(crate) fn job_dojo_cards(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use bn_core::runner::RealRunner;
    let jc = resolver::JobConfig::load();
    if !jc.dojo_enabled {
        println!("dojo-cards: mining disabled — set [dojo] enabled = true (or $BN_DOJO_ENABLED=1)");
        return 0;
    }
    let Some(repo) = dojo_current_repo(cfg) else {
        println!("dojo-cards: not inside a resolvable repo — nothing to mine");
        return 0;
    };
    if !jc.dojo_repos.is_empty() && !jc.dojo_repos.iter().any(|r| r == &repo) {
        println!("dojo-cards: '{repo}' not in [dojo] repos — skipping");
        return 0;
    }

    let now = bn_core::util::now_epoch_secs();
    let since = bn_core::util::local_date_of(now - DOJO_MINE_WINDOW_DAYS * 86_400);
    let commits = bn_core::dojo::gather_commits(&RealRunner, &since, DOJO_MINE_MAX_COMMITS);
    let mut cards = bn_core::dojo::mine_from_commits(&repo, &commits, now);

    // Architecture tours from the module tree. Stamped one second later than the commit-derived
    // drills so a brand-new deck leads with the meatier bug/feature cards, not the tours.
    let files: Vec<String> = {
        use bn_core::runner::CommandRunner;
        RealRunner
            .git(&["ls-files", "*.rs"])
            .ok_stdout()
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    };
    cards.extend(bn_core::dojo::mine_architecture_from_paths(
        &repo, &files, now,
    ));

    let store = bn_core::dojo::DojoStore::from_config(cfg);
    match store.append_cards(&cards) {
        Ok(added) => {
            println!(
                "dojo-cards: mined {repo} — {added} new card(s) from {} candidate(s)",
                cards.len()
            );
            0
        }
        Err(e) => {
            eprintln!("dojo-cards: {e}");
            1
        }
    }
}

/// How far back the `dojo-cards` miner reads commit history, in days.
pub(crate) const DOJO_MINE_WINDOW_DAYS: i64 = 45;

/// Cap on `fix:`/`feat:` commits turned into cards per mining run, newest first.
pub(crate) const DOJO_MINE_MAX_COMMITS: usize = 40;

/// The `weekly-insights` cron — snapshot the current anchored week's `bn insights` retro into
/// `<notes>/<machine>/insights/<YYYY-Www>.md` so history accumulates and syncs. Sibling of
/// `metrics-snapshot`; writes directly (rather than `emit_insights`) so a failure is recorded,
/// not `exit`ed. Read-only over telemetry — never fetches.
pub(crate) fn job_weekly_insights(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use bn_core::clock::Clock;
    let today = bn_core::clock::RealClock.today();
    let ins = bn_core::insights::insights(cfg, &today, None);
    let Some(week) = bn_core::util::iso_week(&ins.end) else {
        eprintln!("weekly-insights: can't derive the ISO week for {today}");
        return 1;
    };
    let path = cfg.insights_file(&week);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("weekly-insights: {e}");
            return 1;
        }
    }
    match std::fs::write(&path, bn_core::insights::render(&ins)) {
        Ok(()) => {
            println!("weekly-insights: snapshot written: {}", path.display());
            0
        }
        Err(e) => {
            eprintln!("weekly-insights: {e}");
            1
        }
    }
}

/// `jobs/agent-observe.sh` — ingest agent session state (`bn agent ingest`).
pub(crate) fn job_agent_observe(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    emit_agent_ingest(cfg, false);
    0
}

/// `jobs/auto-review-poll.sh` — auto-review lifecycle poll (`bn review-pr poll`).
pub(crate) fn job_auto_review_poll(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    if review_pr::run(cfg, review_pr::ReviewPrCmd::Poll, false) {
        0
    } else {
        1
    }
}

/// `jobs/notify.sh` — push an OS toast for each urgent attention item, then mark it notified.
pub(crate) fn job_notify(_cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let items = bn_core::attention::to_notify(&today());
    if items.is_empty() {
        println!("notify: nothing new to push");
        return 0;
    }
    for it in &items {
        let body = match it.link.as_deref() {
            Some(l) if !l.is_empty() => l,
            _ => "urgent — needs attention",
        };
        bn_core::notify::toast(&format!("bn: {}", it.title), body);
        let _ = bn_core::attention::mark_notified(&it.id);
        println!("notify: toasted {} ({})", it.id, it.title);
    }
    0
}

/// `jobs/quarter-review.sh` — the recurring impact agent (#252). Opt-in via `[quarter].enabled`;
/// runs `bn quarter review --cron`, which scores the window's shipped work against the quarter
/// goals, appends a validated review, and flags at-risk goals (a malformed run records nothing and
/// raises a warn attention item). Native so the weekly cron drives it with `bn run quarter-review`.
pub(crate) fn job_quarter_review(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let jc = resolver::JobConfig::load();
    if !jc.quarter_review_enabled {
        println!("quarter-review: impact agent disabled — set [quarter].enabled to opt in");
        return 0;
    }
    let harness = jc
        .quarter_harness
        .clone()
        .or_else(|| cfg.review_harness.clone());
    match bn_core::quarter::review(
        cfg,
        &bn_core::runner::RealRunner,
        harness.as_deref(),
        &today(),
        jc.quarter_review_window_days,
    ) {
        Ok(bn_core::quarter::ReviewOutcome::Appended { review, raised }) => {
            println!(
                "quarter-review: appended review ({} goals, {raised} flagged)",
                review.assessments.len()
            );
            0
        }
        Ok(bn_core::quarter::ReviewOutcome::Malformed { reason }) => {
            println!("quarter-review: agent output failed validation — warn item raised: {reason}");
            0
        }
        Ok(bn_core::quarter::ReviewOutcome::Skipped { reason }) => {
            println!("quarter-review: skipped ({reason})");
            0
        }
        Err(e) => {
            eprintln!("quarter-review: {e}");
            1
        }
    }
}

/// `jobs/standup-push.sh` — copy today's standup line to the clipboard and toast it (autopilot).
pub(crate) fn job_standup_push(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let jc = resolver::JobConfig::load();
    if !jc.standup_enabled {
        println!("standup-push: standup autopilot disabled — nothing to push");
        return 0;
    }
    let s = bn_core::reports::standup(cfg, &today());
    let mut line = bn_core::reports::standup_line(&s);
    if jc.standup_fetch_delta {
        if let Some(d) = bn_core::reports::pr_delta(&bn_core::runner::RealRunner) {
            line.push_str(&format!(" · {d}"));
        }
    }
    if line.trim().is_empty() {
        println!("standup-push: nothing to push");
        return 0;
    }
    bn_core::notify::clipboard(&line);
    bn_core::notify::toast("bn standup", &line);
    println!("{line}");
    0
}

/// `jobs/wrap-digest.sh` — end-of-day toast: the wrap digest plus a "still on you" attention tail.
pub(crate) fn job_wrap_digest(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    if !resolver::JobConfig::load().wrap_enabled {
        println!("wrap-digest: digest disabled — nothing to push");
        return 0;
    }
    let today = today();
    // Mirror emit_wrap: with no note in cwd, date un-noted commits at `today` so that section
    // stays empty rather than flagging every commit.
    let note_since = target_ctx(cfg, None, None)
        .and_then(|ctx| bn_core::store::read(&cfg.note_file(&ctx)).ok())
        .map(|note| bn_core::reports::note_last_activity(&note))
        .unwrap_or_else(|| today.clone());
    let w = bn_core::reports::wrap(cfg, &today, &note_since, &bn_core::runner::RealRunner);
    let mut digest = if w.is_empty() {
        String::new()
    } else {
        bn_core::reports::render_wrap(&w).trim_end().to_string()
    };

    if let Some(tail) = attn_source_tail(&bn_core::attention::open_items(&today)) {
        if digest.is_empty() {
            digest = tail;
        } else {
            digest.push('\n');
            digest.push_str(&tail);
        }
    }

    if digest.trim().is_empty() {
        println!("wrap-digest: clean day (or disabled) — nothing to push");
        return 0;
    }
    bn_core::notify::toast("bn wrap — end of day", &digest);
    println!("{digest}");
    0
}

/// "Still on you: N emails, N Teams replies, N meetings" from open attention items, or `None` when
/// none of those sources are present. Replaces the jq pipeline in `wrap-digest.sh`.
pub(crate) fn attn_source_tail(items: &[bn_core::attention::AttnItem]) -> Option<String> {
    source_tail(items.iter().map(|i| i.source.as_str()))
}

/// The pure counting/pluralizing core of [`attn_source_tail`], over a stream of source names.
pub(crate) fn source_tail<'a>(sources: impl Iterator<Item = &'a str>) -> Option<String> {
    let (mut e, mut t, mut m) = (0usize, 0usize, 0usize);
    for s in sources {
        match s {
            "email" => e += 1,
            "teams" => t += 1,
            "meetings" => m += 1,
            _ => {}
        }
    }
    let mut parts = Vec::new();
    if e > 0 {
        parts.push(format!("{e} email{}", if e == 1 { "" } else { "s" }));
    }
    if t > 0 {
        parts.push(format!(
            "{t} Teams repl{}",
            if t == 1 { "y" } else { "ies" }
        ));
    }
    if m > 0 {
        parts.push(format!("{m} meeting{}", if m == 1 { "" } else { "s" }));
    }
    (!parts.is_empty()).then(|| format!("Still on you: {}", parts.join(", ")))
}

/// `jobs/auto-review-trigger.sh` — when idle, prep the next queued PR unattended.
pub(crate) fn job_auto_review_trigger(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    review_pr::auto_trigger(cfg)
}

/// The last `lines` lines of the active pane in the tmux window for `repo`/`branch`, matched by
/// the same canonical key the board join uses (glyph-stripped). `None` when the window is gone or
/// tmux is absent — the stuck-capture is best-effort.
pub(crate) fn pane_tail_for(repo: &str, branch: &str, lines: usize) -> Option<String> {
    let want = bn_core::attention::window_key_parts(repo, branch);
    let listing = crate::proc::tmux_raw(&[
        "list-windows",
        "-a",
        "-F",
        "#{window_id}\t#{session_name}\t#{window_name}",
    ])?;
    let wid = listing.lines().find_map(|l| {
        let c: Vec<&str> = l.split('\t').collect();
        let name = c
            .get(2)?
            .trim_start_matches(|ch: char| !ch.is_ascii_alphanumeric());
        (bn_core::attention::window_key_parts(c.get(1)?, name) == want)
            .then(|| c.first().map(|s| s.to_string()))?
    })?;
    // `-t <window_id>` captures the window's active pane.
    let text = crate::proc::tmux_raw(&["capture-pane", "-p", "-t", &wid])?;
    let all: Vec<&str> = text.lines().collect();
    let trimmed_len = all
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map(|p| p + 1)
        .unwrap_or(0);
    let start = trimmed_len.saturating_sub(lines);
    let tail = all[start..trimmed_len].join("\n");
    (!tail.trim().is_empty()).then_some(tail)
}

/// window). Delegates to [`emit_night_run`], which gates on the kill switch, `[night] enabled`, and
/// the window; its text output is captured in the run's event tail. Always exits 0 — a gate that
/// declines (kill switch, disabled, off-window) is a clean no-op, not a failure.
pub(crate) fn job_night_shift(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    emit_night_run(cfg, None, false, false);
    0
}

/// Fold each *stuck* in-flight item's pane tail into its `detail` — a wedged run explains itself
/// on the board instead of silently sitting there. Overwrites on each sweep (latest state wins);
/// best-effort throughout.
pub(crate) fn capture_stuck_details() {
    use bn_core::attention::WorkStatus;
    let (classified, _) = attn_lanes();
    for (s, st) in classified {
        if st != WorkStatus::Stuck {
            continue;
        }
        let (Some(repo), Some(branch)) = (s.item.repo.as_deref(), s.item.branch.as_deref()) else {
            continue;
        };
        if let Some(tail) = pane_tail_for(repo, branch, 15) {
            let detail = format!("stuck — pane tail:\n{tail}");
            let _ = bn_core::attention::update(
                &s.item.id,
                None,
                None,
                None,
                None,
                None,
                Some(&detail),
                None,
                None,
            );
            println!(
                "attention-auto-trigger: captured pane tail for stuck {}",
                s.item.id
            );
        }
    }
}

/// `attention-auto-trigger` — the autonomous pickup loop. When idle, fill every free concurrency
/// slot with the next eligible `auto` attention items (self-authored, prompted, with a repo/branch),
/// each launched with its agent. Concurrency is capped by `BN_ATTN_AUTO_MAX` (default 1 → one at a
/// time): `next_auto_batch` returns at most (cap − already-running) items, so the live fleet never
/// exceeds the cap; a *done* item awaiting review frees its slot, so the queue keeps draining across
/// sweeps. Also folds stuck runs' pane tails into their detail each sweep, so a wedged item explains
/// itself. The trust boundary (auto only for `source=manual`) lives in `next_auto_batch`, so no
/// untrusted input ever launches an unattended agent.
pub(crate) fn job_attention_auto_trigger(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let now = bn_core::util::now_epoch_secs();
    // Stuck-capture runs regardless of the idle gate — it's read-only on tmux and makes the board
    // truthful even while you're active.
    capture_stuck_details();
    let idle_mins = env_i64("BN_ATTN_AUTO_IDLE_MINS", 5);
    if !bn_core::idle::is_idle(cfg, now, idle_mins) {
        return 0; // active — don't interrupt
    }
    // Fill every free concurrency slot in one idle sweep, highest-priority first. The batch is
    // capped at (BN_ATTN_AUTO_MAX − already-running), so we never overshoot; each start marks its
    // item in-flight, and later sweeps top the fleet back up as runs finish (done frees a slot).
    let batch = bn_core::attention::next_auto_batch(&today(), attn_auto_max());
    if batch.is_empty() {
        return 0; // at the concurrency cap, or nothing eligible
    }
    let mut rc = 0;
    for item in &batch {
        println!(
            "attention-auto-trigger: starting {} — {}",
            item.id, item.title
        );
        match crate::cmd::attn::run_attn_start_attributed(
            cfg,
            item,
            true,
            Some(bn_core::cost::WorkClass::Scheduled),
        ) {
            Some(o) if o.launched => {}
            Some(_) => {
                eprintln!(
                    "attention-auto-trigger: opened the window but the agent launch failed for {}",
                    item.id
                );
                rc = 1;
            }
            None => {
                eprintln!(
                    "attention-auto-trigger: could not open a worktree for {} ({}/{})",
                    item.id,
                    item.repo.as_deref().unwrap_or("?"),
                    item.branch.as_deref().unwrap_or("?")
                );
                rc = 1;
            }
        }
    }
    rc
}

/// `jobs/agent-supervise.sh` — report fleet health and, when `BN_SUPERVISE_NUDGE=1`, nudge stuck
/// agents via tmux send-keys. The jq selection of stuck agents becomes a Rust filter.
pub(crate) fn job_agent_supervise(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let idle = env_i64("BN_SUPERVISE_IDLE_MINS", 5);
    let stuck_mins = env_i64("BN_SUPERVISE_STUCK_MINS", 15);
    if let Err(error) =
        agent_live::publish_actionable_wakes(cfg, idle, stuck_mins, bn_core::util::now_epoch_secs())
    {
        eprintln!("agent-supervise: wake queue: {error}");
        return 1;
    }
    let fleet = bn_core::agent::supervise(cfg, idle, stuck_mins);
    println!(
        "agent-supervise: {} running, {} stuck (idle>{stuck_mins}m)",
        fleet.agents.len(),
        fleet.stuck
    );
    if fleet.stuck > 0 && std::env::var("BN_SUPERVISE_NUDGE").as_deref() == Ok("1") {
        let msg = std::env::var("BN_SUPERVISE_NUDGE_MSG").unwrap_or_else(|_| {
            "bn supervise: still there? continue with the current task, or commit + open a PR if it is done.".to_string()
        });
        for a in fleet
            .agents
            .iter()
            .filter(|a| a.health == bn_core::agent::AgentHealth::Stuck)
        {
            if tmux_send_keys(&a.target, &msg) {
                println!("agent-supervise: nudged {}", a.target);
            } else {
                eprintln!("agent-supervise: could not nudge {}", a.target);
            }
        }
    }
    0
}

/// Type `msg` then Enter into a tmux window (`bn agent send` mechanics) — true on delivery.
/// Exact-name target ([`tmux_exact_target`]) so a renumbered/prefix-matched window can't receive
/// another agent's nudge.
pub(crate) fn tmux_send_keys(target: &str, msg: &str) -> bool {
    let target = tmux_exact_target(target);
    let send = |key: &str| crate::proc::tmux_ok(&["send-keys", "-t", &target, key]);
    send(msg) && send("Enter")
}

/// `jobs/pr-queue.sh` — rebuild the review-queue cache from GitHub: PRs awaiting your review and
/// your own open PRs, each stamped with state + reviewDecision. Replaces the gh+jq pipeline.
pub(crate) fn job_pr_queue(_cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    if !gh_ready() {
        eprintln!("pr-queue: gh not found or not authenticated");
        return 0; // non-fatal, like the shim
    }
    let mut prs = Vec::new();
    for (query, tag) in [
        ("--review-requested=@me", "review"),
        ("--author=@me", "mine"),
    ] {
        prs.extend(gh_search_bucket(query, tag));
    }
    if prs.is_empty() {
        println!("pr-queue: nothing to review and no open PRs of mine — leaving cache as-is");
        return 0;
    }
    let cache = bn_core::reviews::ReviewCache {
        generated_at: bn_core::util::now_rfc3339_utc(),
        prs,
    };
    let path = bn_core::reviews::cache_path();
    let json = match serde_json::to_string_pretty(&cache) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("pr-queue: {e}");
            return 1;
        }
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, json) {
        Ok(()) => {
            println!(
                "pr-queue: wrote {} PR(s) to {}",
                cache.prs.len(),
                path.display()
            );
            0
        }
        Err(e) => {
            eprintln!("pr-queue: {e}");
            1
        }
    }
}

/// Trailing window `pr-observe` sweeps: PRs updated within this many days get (re)checked.
/// 30 matches the metrics scorecard window, so a fresh install backfills what the card shows.
pub(crate) const PR_OBSERVE_WINDOW_DAYS: i64 = 30;

/// `pr-observe` — fold the GitHub state of your own PRs into the metrics log as
/// `pr_opened`/`pr_merged`/`pr_closed`/`pr_review` events (the producer `bn metrics` was
/// missing, #213). One `gh search prs --author=@me` for the window, then one `gh pr view`
/// only for PRs whose terminal/review events are still unknown. Idempotent by
/// `(kind, repo, number)` against the whole-person log; events are stamped at their GitHub
/// timestamps, so the first run backfills the window truthfully.
pub(crate) fn job_pr_observe(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use bn_core::pr_observe as po;
    if !gh_ready() {
        eprintln!("pr-observe: gh not found or not authenticated");
        return 0; // non-fatal, like pr-queue
    }
    let seen = po::seen_keys(&bn_core::events::read_events_across(
        &cfg.metrics_logs_all_machines(),
    ));
    let since = bn_core::util::add_days(&today(), -PR_OBSERVE_WINDOW_DAYS).unwrap_or_else(today);
    let rows = gh_my_prs(&since);
    if rows.is_empty() {
        println!("pr-observe: no PRs of mine updated since {since}");
        return 0;
    }
    let mut prs = Vec::new();
    for row in rows {
        // Scorecard-excluded repos never surface — skip them rather than spend gh calls.
        if bn_core::metrics::is_excluded_repo(&row.repo) {
            continue;
        }
        let mut pr = po::ObservedPr {
            repo: row.repo,
            number: row.number,
            created_ts: bn_core::util::parse_ts(&row.created_at),
            ..Default::default()
        };
        if po::needs_enrichment(&pr.repo, pr.number, row.closed, &seen) {
            gh_enrich_pr(&mut pr, &row.url);
        }
        prs.push(pr);
    }
    let pending = po::derive_events(&prs, &seen);
    if pending.is_empty() {
        println!("pr-observe: metrics log already up to date");
        return 0;
    }
    let n = pending.len();
    for ev in pending {
        if let Err(e) =
            bn_core::events::append_metric_at(&cfg.metrics_log_path(), ev.kind, ev.ts, ev.fields)
        {
            eprintln!("pr-observe: {e}");
            return 1;
        }
    }
    println!("pr-observe: appended {n} PR event(s)");
    0
}

/// One row from the `pr-observe` search sweep.
pub(crate) struct MyPrRow {
    /// gh `owner/repo`.
    repo: String,
    number: i64,
    created_at: String,
    /// Search-level state is only open/closed — merged needs the per-PR view.
    closed: bool,
    url: String,
}

/// Your own PRs updated since `since` (`YYYY-MM-DD`), any state, via `gh search prs`.
pub(crate) fn gh_my_prs(since: &str) -> Vec<MyPrRow> {
    #[derive(serde::Deserialize)]
    struct SearchPr {
        repository: Repo,
        number: i64,
        #[serde(default, rename = "createdAt")]
        created_at: String,
        #[serde(default)]
        state: String,
        url: String,
    }
    #[derive(serde::Deserialize)]
    struct Repo {
        #[serde(rename = "nameWithOwner")]
        name_with_owner: String,
    }
    let updated = format!(">={since}");
    bn_core::runner::gh_json::<Vec<SearchPr>>(
        &bn_core::runner::RealRunner,
        &[
            "search",
            "prs",
            "--author=@me",
            "--updated",
            &updated,
            "--limit",
            "100",
            "--json",
            "repository,number,createdAt,state,url",
        ],
    )
    .unwrap_or_default()
    .into_iter()
    .map(|pr| MyPrRow {
        repo: pr.repository.name_with_owner,
        number: pr.number,
        created_at: pr.created_at,
        closed: pr.state.eq_ignore_ascii_case("closed"),
        url: pr.url,
    })
    .collect()
}

/// Fill a PR's terminal/review/lead fields from `gh pr view` (the one per-PR call). Best-effort:
/// on any gh failure the PR keeps only its search-row fields. The first review excludes the
/// author's own review-comments; the lead anchor is the earliest commit's authored date.
pub(crate) fn gh_enrich_pr(pr: &mut bn_core::pr_observe::ObservedPr, url: &str) {
    #[derive(serde::Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    struct View {
        #[serde(default)]
        merged_at: Option<String>,
        #[serde(default)]
        closed_at: Option<String>,
        #[serde(default)]
        author: Option<Login>,
        #[serde(default)]
        reviews: Vec<Review>,
        #[serde(default)]
        commits: Vec<Commit>,
    }
    #[derive(serde::Deserialize, Default)]
    struct Login {
        #[serde(default)]
        login: String,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Review {
        #[serde(default)]
        author: Option<Login>,
        #[serde(default)]
        submitted_at: Option<String>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Commit {
        #[serde(default)]
        authored_date: Option<String>,
        #[serde(default)]
        committed_date: Option<String>,
    }
    let Some(v) = bn_core::runner::gh_json::<View>(
        &bn_core::runner::RealRunner,
        &[
            "pr",
            "view",
            url,
            "--json",
            "mergedAt,closedAt,author,reviews,commits",
        ],
    ) else {
        return;
    };
    let ts = |s: &Option<String>| s.as_deref().and_then(bn_core::util::parse_ts);
    pr.merged_ts = ts(&v.merged_at);
    pr.closed_ts = ts(&v.closed_at);
    let me = v.author.map(|a| a.login).unwrap_or_default();
    pr.first_review_ts = v
        .reviews
        .iter()
        .filter(|r| {
            r.author
                .as_ref()
                .map(|a| !a.login.eq_ignore_ascii_case(&me))
                .unwrap_or(true)
        })
        .filter_map(|r| ts(&r.submitted_at))
        .min();
    pr.first_commit_ts = v
        .commits
        .iter()
        .filter_map(|c| ts(&c.authored_date).or_else(|| ts(&c.committed_date)))
        .min();
}

/// `gh` present and authenticated.
pub(crate) fn gh_ready() -> bool {
    crate::gh::gh_ok(&["auth", "status"])
}

/// One `gh search prs <query>` bucket → `ReviewPr`s, each enriched with its state + reviewDecision
/// via `gh pr view` (matching the shim's per-PR verdict lookup).
pub(crate) fn gh_search_bucket(query: &str, tag: &str) -> Vec<bn_core::reviews::ReviewPr> {
    #[derive(serde::Deserialize)]
    struct SearchPr {
        repository: Repo,
        number: i64,
        #[serde(default)]
        title: String,
        author: Option<Author>,
        url: String,
        #[serde(default, rename = "createdAt")]
        created_at: String,
        #[serde(default, rename = "updatedAt")]
        updated_at: String,
    }
    #[derive(serde::Deserialize)]
    struct Repo {
        #[serde(rename = "nameWithOwner")]
        name_with_owner: String,
    }
    #[derive(serde::Deserialize, Default)]
    struct Author {
        #[serde(default)]
        login: String,
    }
    bn_core::runner::gh_json::<Vec<SearchPr>>(
        &bn_core::runner::RealRunner,
        &[
            "search",
            "prs",
            query,
            "--state=open",
            "--limit",
            "50",
            "--json",
            "repository,number,title,author,url,createdAt,updatedAt",
        ],
    )
    .unwrap_or_default()
    .into_iter()
    .map(|pr| {
        let (state, review_decision) = gh_verdict(&pr.url);
        bn_core::reviews::ReviewPr {
            repo: pr.repository.name_with_owner,
            number: pr.number,
            title: pr.title,
            author: pr.author.unwrap_or_default().login,
            url: pr.url,
            updated_at: pr.updated_at,
            created_at: pr.created_at,
            tag: tag.to_string(),
            state,
            review_decision,
        }
    })
    .collect()
}

/// A PR's `(state, reviewDecision)` from `gh pr view`; state lowercased and defaulted to `open`.
pub(crate) fn gh_verdict(url: &str) -> (String, Option<String>) {
    #[derive(serde::Deserialize, Default)]
    struct VerdictView {
        state: Option<String>,
        #[serde(rename = "reviewDecision")]
        review_decision: Option<String>,
    }
    let v = bn_core::runner::gh_json::<VerdictView>(
        &bn_core::runner::RealRunner,
        &["pr", "view", url, "--json", "state,reviewDecision"],
    )
    .unwrap_or_default();
    let state = v
        .state
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "open".to_string());
    let rd = v.review_decision.filter(|s| !s.is_empty());
    (state, rd)
}

/// Read an `i64` env var, falling back to `default` when unset or unparseable.
pub(crate) fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Max concurrent unattended `auto` agents the autonomous loop may run at once — the
/// `attention-auto-trigger` concurrency cap, from `BN_ATTN_AUTO_MAX` (default 1 → one at a time;
/// raise it to fan an overnight queue out across several agents). Clamped to at least 1.
pub(crate) fn attn_auto_max() -> usize {
    env_i64("BN_ATTN_AUTO_MAX", 1).max(1) as usize
}

/// `jobs/ensure-tmux.sh` — ensure the keepalive tmux session exists.
pub(crate) fn job_ensure_tmux(_cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let session = std::env::var("BN_TMUX_SESSION").unwrap_or_else(|_| "main".to_string());
    let has = crate::proc::tmux_run(&["has-session", "-t", &session]);
    if has {
        println!("tmux session '{session}' already running");
        return 0;
    }
    if crate::proc::tmux_run(&["new-session", "-d", "-s", &session]) {
        println!("started detached tmux session '{session}'");
        0
    } else {
        eprintln!("ensure-tmux: failed to start tmux session '{session}'");
        1
    }
}

#[cfg(test)]
mod native_job_tests {
    use super::{job_tools_reconcile, native_job, native_job_line, run_native_job};
    use std::path::PathBuf;

    #[test]
    fn ported_jobs_are_native_unported_fall_through() {
        for j in [
            "wt-cleanup",
            "fleet-reap",
            "attention-sync",
            "daily-rollover",
            "metrics-snapshot",
            "gather-findings",
            "dojo-cards",
            "weekly-insights",
            "agent-observe",
            "auto-review-poll",
            "ensure-tmux",
            "notify",
            "standup-push",
            "wrap-digest",
            "auto-review-trigger",
            "attention-auto-trigger",
            "agent-supervise",
            "pr-queue",
            "night-shift",
            "pr-observe",
            "quarter-review",
            "staff-chief-am",
            "staff-chief-pm",
            "staff-archivist",
            "staff-curriculum",
            "staff-interviewer",
            "sentinel",
            "staff-mentor",
            "staff-eye-daily",
            "staff-eye-weekly",
            "routine-notify",
            "routine-tick",
            "today-agent",
            "staff-archive",
            "commit-notes",
            "watch",
            "recall-index",
            "managed-repos",
            "refresh-all",
            "self-update",
            "tools-reconcile",
            "keep-warm",
            "pool-warm",
            "cockpit-ensure",
        ] {
            assert!(native_job(j).is_some(), "{j} should be native");
        }
        // Not portable: m365-sweep/meetings shell out to workiq (external, work-laptop-only) and
        // stay as user-customizable .sh crons — they fall through.
        assert!(native_job("m365-sweep").is_none());
        assert!(native_job("meetings").is_none());
        assert!(native_job("definitely-not-a-job").is_none());
    }

    #[test]
    fn tools_reconcile_failure_text_is_persisted_for_cron_views() {
        let _lock = bn_core::test_support::env_lock();
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", &state);
        let cfg = bn_core::test_support::config(root.path());
        let expected = "tools-reconcile: n8n FAILED — partial podman stderr\ncommand timed out after 45s: `docker run -d --name n8n image`";

        let code = run_native_job(&cfg, None, "tools-reconcile", |_, _| {
            native_job_line(expected, true);
            1
        });

        assert_eq!(code, 1);
        let latest = bn_core::events::last_run("tools-reconcile").unwrap();
        assert_eq!(latest.output_tail, expected);
        let failures = bn_core::events::recent_failures(1);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].output_tail, expected);
    }

    #[test]
    fn tools_reconcile_noops_when_no_tools_declared() {
        let _lock = bn_core::test_support::env_lock();
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch")
            .join(format!("cli-tools-no-opt-in-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _config_dir = bn_core::test_support::EnvGuard::set("BN_CONFIG_DIR", &dir);
        let _config = bn_core::test_support::EnvGuard::remove("BN_CONFIG");
        let _builtins = bn_core::test_support::EnvGuard::remove("BN_TOOLS_BUILTINS");
        let cfg = bn_core::test_support::config(&dir);

        assert_eq!(job_tools_reconcile(&cfg, None), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_main_worktree_picks_the_bn_main_entry() {
        let porcelain = "\
worktree /home/me/projects/worktree/bn/feature-x
HEAD abc
branch refs/heads/feature-x

worktree /home/me/projects/worktree/bn/main
HEAD def
branch refs/heads/main
";
        assert_eq!(
            super::parse_main_worktree(porcelain).as_deref(),
            Some("/home/me/projects/worktree/bn/main")
        );
        assert_eq!(super::parse_main_worktree("worktree /x/bn/dev\n"), None);
        assert_eq!(super::parse_main_worktree(""), None);
    }

    #[test]
    fn rank_to_start_follows_next_order_then_sinks_unranked() {
        let item = |id: &str| bn_core::attention::AttnItem {
            id: id.into(),
            source: "manual".into(),
            title: id.into(),
            severity: "warn".into(),
            repo: None,
            branch: None,
            link: None,
            key: None,
            detail: None,
            mode: None,
            prompt: None,
            task_type: None,
            worktree_path: None,
            uncommitted_files: Vec::new(),
            created: 0,
        };
        // Board order a1,a2,a3; bn next ranks a3 first, a1 second; a2 unranked (aged out).
        let out = crate::rank_to_start(
            vec![item("a1"), item("a2"), item("a3")],
            &["a3".to_string(), "a1".to_string()],
        );
        let ids: Vec<&str> = out.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            ["a3", "a1", "a2"],
            "ranked first in next order, unranked sink"
        );
    }

    #[test]
    fn source_tail_counts_and_pluralizes() {
        use super::source_tail;
        assert_eq!(source_tail([].into_iter()), None);
        assert_eq!(source_tail(["chat"].into_iter()), None); // unknown sources ignored
        assert_eq!(
            source_tail(["email"].into_iter()).as_deref(),
            Some("Still on you: 1 email")
        );
        assert_eq!(
            source_tail(["email", "email", "teams", "meetings", "meetings"].into_iter()).as_deref(),
            Some("Still on you: 2 emails, 1 Teams reply, 2 meetings")
        );
    }

    #[test]
    fn notes_action_skips_only_when_clean_and_pushed() {
        use super::{notes_action, NotesAction};
        // Clean tree, nothing unpushed → no commit, no network.
        assert_eq!(notes_action(false, true, 0), NotesAction::Skip);
        assert_eq!(notes_action(false, false, 0), NotesAction::Skip);
        // Dirty with an upstream → commit + push.
        assert_eq!(notes_action(true, true, 0), NotesAction::CommitAndPush);
        // Dirty without an upstream → commit-only (personal repo before a remote is added).
        assert_eq!(notes_action(true, false, 0), NotesAction::CommitOnly);
        // Clean but unpushed (a prior run committed but couldn't push) → retry the push.
        assert_eq!(notes_action(false, true, 3), NotesAction::CommitAndPush);
    }

    #[test]
    fn index_lock_staleness_clears_old_and_unknown_only() {
        use super::{index_lock_is_stale, STALE_LOCK_AGE};
        use std::time::Duration;
        // A fresh lock (a git op in flight right now) is left alone.
        assert!(!index_lock_is_stale(Some(Duration::from_secs(1))));
        assert!(!index_lock_is_stale(Some(
            STALE_LOCK_AGE - Duration::from_secs(1)
        )));
        // At/past the threshold → abandoned, clear it.
        assert!(index_lock_is_stale(Some(STALE_LOCK_AGE)));
        assert!(index_lock_is_stale(Some(Duration::from_secs(3600))));
        // Unknown age (no mtime) → treat as stale rather than wedge forever.
        assert!(index_lock_is_stale(None));
    }

    #[test]
    fn heal_stale_git_state_clears_rebase_leftovers_and_reports() {
        use super::heal_stale_git_state;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let git_dir = root.join(".git");
        // A leftover interrupted rebase dir (git rebase --abort is a harmless no-op on this fake
        // repo; the force-remove is what clears the wedge).
        std::fs::create_dir_all(git_dir.join("rebase-merge")).unwrap();

        let healed = heal_stale_git_state(root);

        assert!(
            !git_dir.join("rebase-merge").exists(),
            "rebase leftover is cleared"
        );
        assert!(healed.iter().any(|m| m.contains("rebase")));
    }

    #[test]
    fn heal_stale_git_state_keeps_a_fresh_lock_and_skips_worktrees() {
        use super::heal_stale_git_state;
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let lock = git_dir.join("index.lock");
        std::fs::write(&lock, b"").unwrap(); // just created → fresh, a live git op
        let healed = heal_stale_git_state(tmp.path());
        assert!(lock.exists(), "a fresh lock (live git op) is left in place");
        assert!(healed.is_empty());

        // A worktree's `.git` is a file, not a dir → nothing to heal, no panic.
        let wt = tempfile::tempdir().unwrap();
        std::fs::write(wt.path().join(".git"), b"gitdir: /elsewhere\n").unwrap();
        assert!(heal_stale_git_state(wt.path()).is_empty());
    }

    #[test]
    fn notes_commit_message_is_conventional_and_tagged() {
        use super::notes_commit_message;
        let msg = notes_commit_message("MyLaptop", "2026-07-15 14:38");
        assert_eq!(msg, "notes: auto-checkpoint 2026-07-15 14:38 (MyLaptop)");
        assert!(msg.starts_with("notes: "));
    }
}
