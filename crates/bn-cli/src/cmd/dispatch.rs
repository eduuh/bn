//! `bn dispatch` — fork a worktree for a todo and launch an agent in it.
use crate::*;

pub(crate) mod policy;
use policy as dp;
pub(crate) use policy::{effective_launch_harness, normalize_agent};

/// Canonical argv builder for agent-bearing `bn dispatch` invocations.
///
/// Orchestrator and SSH-host callers deliberately execute dispatch in a child process, but they
/// must not independently encode its flags: seed construction, trust, and launch behavior belong
/// to [`run_dispatch`]. This type keeps those callers on one command shape.
pub(crate) struct DispatchInvocation<'a> {
    item: &'a str,
    repo: &'a str,
    agent: &'a str,
    branch: Option<&'a str>,
    skill: Option<&'a str>,
    scout: bool,
    verify: bool,
    yolo: bool,
    fresh: bool,
    json: bool,
}

impl<'a> DispatchInvocation<'a> {
    pub(crate) fn new(item: &'a str, repo: &'a str, agent: &'a str) -> Self {
        Self {
            item,
            repo,
            agent,
            branch: None,
            skill: None,
            scout: false,
            verify: false,
            yolo: false,
            fresh: false,
            json: false,
        }
    }

    pub(crate) fn branch(mut self, branch: Option<&'a str>) -> Self {
        self.branch = branch;
        self
    }

    pub(crate) fn skill(mut self, skill: Option<&'a str>) -> Self {
        self.skill = skill;
        self
    }

    pub(crate) fn scout(mut self, scout: bool) -> Self {
        self.scout = scout;
        self
    }

    pub(crate) fn verify(mut self, verify: bool) -> Self {
        self.verify = verify;
        self
    }

    pub(crate) fn yolo(mut self, yolo: bool) -> Self {
        self.yolo = yolo;
        self
    }

    pub(crate) fn fresh(mut self, fresh: bool) -> Self {
        self.fresh = fresh;
        self
    }

    pub(crate) fn json(mut self, json: bool) -> Self {
        self.json = json;
        self
    }

    pub(crate) fn args(&self) -> Vec<String> {
        let mut args = vec![
            "dispatch".to_string(),
            self.item.to_string(),
            "--repo".to_string(),
            self.repo.to_string(),
            "--agent".to_string(),
            self.agent.to_string(),
        ];
        if let Some(branch) = self.branch {
            args.extend(["--branch".to_string(), branch.to_string()]);
        }
        if self.scout {
            args.push("--scout".to_string());
        }
        if let Some(skill) = self.skill {
            args.extend(["--skill".to_string(), skill.to_string()]);
        }
        if self.verify {
            args.push("--verify".to_string());
        }
        if self.yolo {
            args.push("--yolo".to_string());
        }
        if self.fresh {
            args.push("--fresh".to_string());
        }
        // The orchestrator *is* the queue: queueing an item it just pulled off that queue would
        // loop. Its concurrency cap already bounds how many worktrees a run can grow, so the
        // daemon keeps the historical cold fallback while interactive/dashboard dispatches get the
        // queueing default.
        args.extend(["--on-pool-full".to_string(), "cold".to_string()]);
        if self.json {
            args.push("--json".to_string());
        }
        args
    }
}

/// Whether `s` looks like a todo id: `t` followed by digits (t3, t12).
pub(crate) fn is_dispatch_todo_id(s: &str) -> bool {
    s.strip_prefix('t')
        .is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit()))
}

/// Split a captured item's stored text into (title, optional url). `bn capture` stores items as
/// "<title> — <url>"; a bare trailing URL is also recognized. Otherwise it's all title.
pub(crate) fn split_item_text(text: &str) -> (String, Option<String>) {
    if let Some((title, url)) = text.rsplit_once(" — ") {
        let url = url.trim();
        if url.starts_with("http") && !url.contains(' ') {
            return (title.trim().to_string(), Some(url.to_string()));
        }
    }
    (text.trim().to_string(), None)
}

/// Resolve the dispatch target from a raw arg: an issue/PR URL, a todo id / unique substring
/// (looked up in the current note's open todos), or free text. Returns (title, url, source_todo_id).
pub(crate) fn resolve_dispatch_item(
    cfg: &Config,
    ctx: Option<&Resolved>,
    raw: &str,
) -> (String, Option<String>, Option<String>) {
    let raw = raw.trim();
    if raw.starts_with("http") {
        return (capture_title(raw), Some(raw.to_string()), None);
    }
    // Todo-id / substring resolution keys off the *current* note. When dispatching into another
    // repo from outside any repo (no cwd context), there's no note to consult — treat as free text.
    if let Some(note) = ctx.and_then(|c| bn_core::store::read(&cfg.note_file(c)).ok()) {
        let todos = &note.front.todos;
        let found = if is_dispatch_todo_id(raw) {
            todos.iter().find(|t| t.id == raw)
        } else {
            let rl = raw.to_lowercase();
            todos
                .iter()
                .find(|t| t.is_open_todo() && t.text.to_lowercase().contains(&rl))
        };
        if let Some(t) = found {
            let (title, url) = split_item_text(&t.text);
            return (title, url, Some(t.id.clone()));
        }
    }
    (raw.to_string(), None, None)
}

/// Slugify a title into a branch-safe segment: lowercase, runs of non-alphanumerics → one '-',
/// trimmed, capped so branch/dir names stay sane.
pub(crate) fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let capped: String = out.chars().take(40).collect();
    capped.trim_matches('-').to_string()
}

fn dispatch_branch_occupied(
    bare: &std::path::Path,
    worktree_dir: &std::path::Path,
    repo: &str,
    branch: &str,
) -> bool {
    let bare_s = bare.to_string_lossy().to_string();
    let ref_exists = ["heads", "remotes/origin"].iter().any(|kind| {
        crate::git::git_command(&[
            "--git-dir",
            &bare_s,
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/{kind}/{branch}"),
        ])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
    });
    ref_exists
        || worktree_dir
            .join(repo)
            .join(branch.replace('/', "-"))
            .exists()
}

fn may_reuse_occupied_worktree(fresh: bool, launching_agent: bool) -> bool {
    fresh && launching_agent
}

/// Queue `item` on `repo`'s orchestrate run because the warm pool has no free slot, then exit.
///
/// The alternative — what dispatch did for years — is to cut a cold worktree, which succeeds every
/// time and is exactly the problem: nothing reclaims those directories, so a pool that fills up
/// once quietly grows the disk from then on. Queueing says the true thing (there is no capacity
/// right now) and keeps the work: `RunState::extend` dedupes by derived id, so re-dispatching the
/// same item is idempotent rather than a second queue entry.
///
/// **Returns** only when there is no live run to queue onto — the caller then falls back to cold,
/// since refusing to start work on a repo that isn't orchestrated at all would be worse than a
/// directory. Otherwise it prints the envelope and exits 0: the dispatch was *handled*, not failed.
fn schedule_on_pool_full(repo: &str, item: &str, json: bool) {
    let path = bn_core::orchestrate::state_path_for(repo);
    let now = bn_core::util::now_epoch_secs();
    let queued = match bn_core::orchestrate::mutate(&path, |st| st.extend(&[item.to_string()], now))
    {
        Ok(Some(n)) => n,
        // No run for this repo — nothing to schedule onto.
        Ok(None) => return,
        Err(e) => {
            eprintln!("bn dispatch: could not queue on {repo}'s run ({e})");
            return;
        }
    };
    if json {
        let data = serde_json::json!({
            "repo": repo,
            "item": item,
            "dispatched": false,
            "scheduled": true,
            "reason": "pool_full",
            "queued": queued,
        });
        println!("{}", Envelope::success("dispatch", data).to_json());
    } else {
        println!(
            "bn dispatch: every warm slot for '{repo}' is leased — scheduled on the run instead \
             ({queued} newly queued). It starts as soon as a slot frees."
        );
    }
    std::process::exit(0);
}

/// A repo's primary branch that must never host a dispatched agent (its worktree would be the
/// main checkout, which reap/cleanup guards also protect). Canonical definition lives in
/// [`bn_core::agent::is_reserved_branch`]; kept here as a thin alias so dispatch, the lease layer,
/// reap.rs and wt/cleanup.rs never diverge on what "main" means.
pub(crate) fn is_reserved_branch(branch: &str) -> bool {
    bn_core::agent::is_reserved_branch(branch)
}

/// Is `p` an executable file? (On non-unix, just a file.)
pub(crate) fn is_executable(p: &std::path::Path) -> bool {
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

/// Create a worktree for <branch> at <wt_path> using <bare>. Returns Ok(true) when it created
/// one, Ok(false) when the path already existed. Mirrors the shell primitive: an existing branch
/// is checked out; otherwise a new branch is cut from main.
pub(crate) fn create_worktree(
    bare: &std::path::Path,
    wt_path: &std::path::Path,
    branch: &str,
    base: &str,
) -> Result<bool, String> {
    if wt_path.exists() {
        return Ok(false);
    }
    if let Some(parent) = wt_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bare_s = bare.to_string_lossy().to_string();
    let wt_s = wt_path.to_string_lossy().to_string();
    let git = |args: &[&str]| crate::git::git_command(args).output();
    let branch_exists = git(&[
        "--git-dir",
        &bare_s,
        "show-ref",
        "--verify",
        "--quiet",
        &format!("refs/heads/{branch}"),
    ])
    .map(|o| o.status.success())
    .unwrap_or(false);
    let out = if branch_exists {
        // Re-dispatch of an existing branch: check it out as-is — no base fork involved.
        git(&["--git-dir", &bare_s, "worktree", "add", &wt_s, branch])
    } else {
        // #304: fork the new branch from an *up-to-date* base. A stale local base (not updated
        // after sibling PRs merged on the remote) makes every dispatched branch miss those
        // commits, so its PR comes up CONFLICTING. Fetch `origin/<base>` best-effort (offline is
        // fine — we fall back to the local ref) and prefer forking from `origin/<base>` so the
        // branch starts from current remote base. `-c credential.useHttpPath` mirrors the wt.rs
        // fetch helper so ADO/GCM fetches can derive the org from the URL path.
        let origin_ref = format!("refs/remotes/origin/{base}");
        let _ = git(&[
            "--git-dir",
            &bare_s,
            "-c",
            "credential.useHttpPath=true",
            "fetch",
            "origin",
            &format!("refs/heads/{base}:{origin_ref}"),
        ]);
        let origin_exists = git(&[
            "--git-dir",
            &bare_s,
            "show-ref",
            "--verify",
            "--quiet",
            &origin_ref,
        ])
        .map(|o| o.status.success())
        .unwrap_or(false);
        let count = |range: &str| -> u32 {
            git(&["--git-dir", &bare_s, "rev-list", "--count", range])
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .trim()
                        .parse::<u32>()
                        .ok()
                })
                .unwrap_or(0)
        };
        let (behind, ahead) = if origin_exists {
            (
                count(&format!("refs/heads/{base}..{origin_ref}")),
                count(&format!("{origin_ref}..refs/heads/{base}")),
            )
        } else {
            (0, 0)
        };
        let (start, warn) = dispatch_fork_ref(base, origin_exists, behind, ahead);
        if let Some(w) = warn {
            eprintln!("bn dispatch: {w}");
        }
        git(&[
            "--git-dir",
            &bare_s,
            "worktree",
            "add",
            "-b",
            branch,
            &wt_s,
            &start,
        ])
    }
    .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(true)
}

#[derive(Debug)]
pub(crate) struct DispatchBase {
    pub(crate) branch: String,
    pub(crate) bootstrapped: bool,
    pub(crate) origin_backed: bool,
}

pub(crate) fn dispatch_base_bootstrap_message(repo: &str, base: &DispatchBase) -> Option<String> {
    (base.bootstrapped && base.origin_backed).then(|| {
        format!(
            "bn dispatch: bootstrapped {} worktree for {repo} from origin/{}",
            base.branch, base.branch
        )
    })
}

fn bare_git_args<'a>(bare: &'a str, tail: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["-c", "safe.bareRepository=all", "--git-dir", bare];
    args.extend_from_slice(tail);
    args
}

/// Ensure the dispatch base exists as a current local worktree. Fetching and resolving happen
/// against the bare clone so callers work identically from a repo worktree or the orchestrator.
pub(crate) fn ensure_dispatch_base(
    cfg: &Config,
    repo: &str,
    bare: &std::path::Path,
    requested: Option<&str>,
) -> Result<DispatchBase, String> {
    if !bare.is_dir() {
        return Err(format!(
            "bare repo is missing at {} — clone it with `bn wt clone <url>`",
            bare.display()
        ));
    }

    let bare_s = bare.to_string_lossy().to_string();
    let run = |args: &[&str]| -> Result<std::process::Output, String> {
        crate::git::git_command(args)
            .output()
            .map_err(|e| format!("couldn't run git for {}: {e}", bare.display()))
    };
    let checked = |args: &[&str], action: &str| -> Result<std::process::Output, String> {
        let out = run(args)?;
        if out.status.success() {
            Ok(out)
        } else {
            let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if detail.is_empty() {
                format!("{action} failed for {}", bare.display())
            } else {
                format!("{action} failed for {}: {detail}", bare.display())
            })
        }
    };

    let probe = checked(
        &bare_git_args(&bare_s, &["rev-parse", "--is-bare-repository"]),
        "bare repo validation",
    )?;
    if String::from_utf8_lossy(&probe.stdout).trim() != "true" {
        return Err(format!(
            "{} is not a valid bare repository — recreate it with `bn wt clone <url>`",
            bare.display()
        ));
    }

    checked(
        &bare_git_args(
            &bare_s,
            &[
                "-c",
                "credential.useHttpPath=true",
                "fetch",
                "origin",
                "+refs/heads/*:refs/remotes/origin/*",
            ],
        ),
        "fetch from origin",
    )?;

    let requested = requested.map(str::trim).filter(|s| !s.is_empty());
    let branch = match requested {
        Some(branch) => branch.to_string(),
        None => {
            let symbolic = run(&bare_git_args(
                &bare_s,
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    "refs/remotes/origin/HEAD",
                ],
            ))?;
            if symbolic.status.success() {
                String::from_utf8_lossy(&symbolic.stdout)
                    .trim()
                    .strip_prefix("origin/")
                    .unwrap_or_default()
                    .to_string()
            } else {
                let remote_head = checked(
                    &bare_git_args(&bare_s, &["ls-remote", "--symref", "origin", "HEAD"]),
                    "remote default-branch lookup",
                )?;
                String::from_utf8_lossy(&remote_head.stdout)
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("ref: refs/heads/")
                            .and_then(|rest| rest.split_whitespace().next())
                    })
                    .unwrap_or_default()
                    .to_string()
            }
        }
    };
    if branch.is_empty() {
        return Err(format!(
            "couldn't determine origin's default branch for {} — set origin/HEAD or pass --base",
            bare.display()
        ));
    }

    let origin_ref = format!("refs/remotes/origin/{branch}");
    let origin_backed = run(&bare_git_args(
        &bare_s,
        &["show-ref", "--verify", "--quiet", &origin_ref],
    ))?
    .status
    .success();
    let local_ref = format!("refs/heads/{branch}");
    let local_exists = run(&bare_git_args(
        &bare_s,
        &["show-ref", "--verify", "--quiet", &local_ref],
    ))?
    .status
    .success();
    if !origin_backed && (requested.is_none() || !local_exists) {
        return Err(format!(
            "origin/{branch} does not exist in {} — fetch it or pass an existing local --base",
            bare.display()
        ));
    }

    let base_path = cfg
        .worktree_dir
        .join(repo)
        .join(crate::wt::sanitize_branch(&branch));
    let base_s = base_path.to_string_lossy().to_string();
    let mut bootstrapped = false;
    if base_path.exists() {
        let current = checked(
            &["-C", &base_s, "symbolic-ref", "--quiet", "--short", "HEAD"],
            "base worktree validation",
        )?;
        let current = String::from_utf8_lossy(&current.stdout).trim().to_string();
        if current != branch {
            return Err(format!(
                "base worktree {} is on '{current}', expected '{branch}' — move it aside or check out '{branch}'",
                base_path.display()
            ));
        }
    } else {
        std::fs::create_dir_all(base_path.parent().unwrap_or(&cfg.worktree_dir))
            .map_err(|e| format!("couldn't create {}: {e}", base_path.display()))?;
        let mut args = bare_git_args(&bare_s, &["worktree", "add"]);
        if !local_exists {
            args.extend(["-b", branch.as_str()]);
        }
        args.extend([base_s.as_str()]);
        if !local_exists {
            args.push(origin_ref.as_str());
        } else {
            args.push(branch.as_str());
        }
        checked(&args, "base worktree bootstrap")?;
        bootstrapped = true;
    }

    if origin_backed {
        checked(
            &[
                "-C",
                &base_s,
                "merge",
                "--ff-only",
                &format!("origin/{branch}"),
            ],
            "base worktree fast-forward",
        )?;
    }

    Ok(DispatchBase {
        branch,
        bootstrapped,
        origin_backed,
    })
}

/// Decide the start ref for a freshly-forked dispatch branch, plus any advisory warning, from the
/// observed relationship between the local base and `origin/<base>` (#304, #456). `origin_exists`
/// is whether the `origin/<base>` remote-tracking ref resolved (false when offline or the base is
/// local-only). `behind` = commits `origin/<base>` has that the local base lacks; `ahead` =
/// commits the local base has that `origin/<base>` lacks.
///
/// A fresh dispatch branch should start from the repo's *current* remote base whenever origin is
/// reachable — a stale or **diverged** local base is exactly what makes the branch miss merged
/// sibling work and its PR come up CONFLICTING. So fork from `origin/<base>` unless:
///   - origin is unavailable (offline / local-only base) — fall back to the local base; or
///   - the local base is *purely ahead* (`ahead > 0 && behind == 0`) — it's a clean superset of
///     `origin/<base>` (contains it plus unpushed commits), so keep it rather than silently drop
///     that local work (#304).
///
/// The diverged case (`behind > 0 && ahead > 0`) — a squash-merge or a rewritten remote leaves the
/// local base with both missing and extra commits — now forks from `origin/<base>` too (#456): the
/// extra local-base commits are drift on a mirror's local `main`, and carrying them is what forced
/// the "its PR may need a rebase" fallback on a rarely-updated second repo.
pub(crate) fn dispatch_fork_ref(
    base: &str,
    origin_exists: bool,
    behind: u32,
    ahead: u32,
) -> (String, Option<String>) {
    // Offline / local-only base: nothing fresher to fork from.
    if !origin_exists {
        return (base.to_string(), None);
    }
    // Purely ahead: the local base already contains origin/<base> plus unpushed commits, so it's a
    // clean superset — keep it rather than drop that local work. Not stale, so no warning.
    if ahead > 0 && behind == 0 {
        return (base.to_string(), None);
    }
    // Otherwise fork from the CURRENT remote base.
    let warn = match (behind, ahead) {
        (0, _) => None,
        (b, 0) => Some(format!(
            "local {base} was {b} commit(s) behind origin/{base} — forking from origin/{base} \
             so this branch starts from current {base}"
        )),
        (b, a) => Some(format!(
            "local {base} had diverged from origin/{base} ({b} behind, {a} ahead) — forking from \
             origin/{base} so this branch starts from current {base} ({a} local-only commit(s) \
             on {base} left behind)"
        )),
    };
    (format!("origin/{base}"), warn)
}

/// Run the post-create lifecycle hook (mirrors lib/tmux-lib.zsh): `~/.config/bn/hooks/post-create`
/// then `<wt>/.bn/hooks/post-create`, both if present, with cwd = the worktree and BN_WORKTREE_PATH
/// / BN_BRANCH / BN_REPO / BN_MAIN_PATH set. A non-zero hook is reported but never fails dispatch.
pub(crate) fn run_post_create_hook(
    cfg: &Config,
    wt_path: &std::path::Path,
    repo: &str,
    branch: &str,
    base: &str,
) {
    let main_path = cfg
        .worktree_dir
        .join(repo)
        .join(crate::wt::sanitize_branch(base));
    let global = std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".config/bn/hooks/post-create"));
    let candidates = [global, Some(wt_path.join(".bn/hooks/post-create"))];
    for hook in candidates.into_iter().flatten() {
        if !is_executable(&hook) {
            continue;
        }
        println!("Running post-create hook: {}", hook.display());
        let ok = std::process::Command::new(&hook)
            .current_dir(wt_path)
            .env("BN_WORKTREE_PATH", wt_path)
            .env("BN_BRANCH", branch)
            .env("BN_REPO", repo)
            .env("BN_MAIN_PATH", &main_path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            println!("  post-create hook exited non-zero (continuing)");
        }
    }
}

pub(crate) fn plan_gate_enabled(explicit: bool, configured: bool, yolo: bool, scout: bool) -> bool {
    !scout && (explicit || configured || yolo)
}

fn render_workflow_body(skill: &bn_core::skills::Skill, title: &str) -> Result<String, String> {
    let mut args = serde_json::Map::new();
    for argument in &skill.arguments {
        let value = match argument.name.as_str() {
            "question" | "symptom" | "topic" => title,
            "continuous" => "true",
            _ => continue,
        };
        args.insert(
            argument.name.clone(),
            serde_json::Value::String(value.to_string()),
        );
    }
    skill
        .render_checked(Some(&args))
        .map_err(|error| format!("render workflow '{}': {error}", skill.name))
}

/// The concrete plan-review-gate protocol (#349) appended to a `bn dispatch --plan` ship seed. It
/// is the authoritative final instruction: the agent writes a plan, registers it with `bn review
/// submit` (raising a pending plan-review for `repo`/`branch` under session `sid`), and PAUSES —
/// polling `bn review status <id>` — implementing and opening a PR after low-risk agent consensus,
/// or after explicit human approval for high-risk/no-consensus plans. `verify` folds the `bn
/// verify` gate into the post-approval implement step.
pub(crate) fn plan_gate_seed(
    cfg: &Config,
    base_dir: &std::path::Path,
    ctx: PlanGateContext<'_>,
) -> Result<String, String> {
    let verification_requirement = if ctx.verify {
        bn_core::skills::render_required(cfg, base_dir, "dispatch-verify-gate", None)
            .map_err(|error| error.to_string())?
    } else {
        String::new()
    };
    let mut args = serde_json::Map::new();
    for (name, value) in [
        ("repo", ctx.repo.to_string()),
        ("branch", ctx.branch.to_string()),
        ("session_id", ctx.sid.to_string()),
        ("reviewers", ctx.reviewers.to_string()),
        ("quorum", ctx.quorum.to_string()),
        ("plan_file", ctx.plan_file.unwrap_or("plan.md").to_string()),
        ("verification_requirement", verification_requirement),
    ] {
        args.insert(name.into(), serde_json::Value::String(value));
    }
    bn_core::skills::render_required(cfg, base_dir, "dispatch-plan-review-gate", Some(&args))
        .map(|rendered| format!("\n\n{rendered}"))
        .map_err(|error| error.to_string())
}

pub(crate) struct PlanGateContext<'a> {
    pub(crate) repo: &'a str,
    pub(crate) branch: &'a str,
    pub(crate) sid: &'a str,
    pub(crate) verify: bool,
    pub(crate) reviewers: usize,
    pub(crate) quorum: usize,
    pub(crate) plan_file: Option<&'a str>,
}

fn append_plan_gate(
    cfg: &Config,
    base_dir: &std::path::Path,
    seed: String,
    plan_gate: bool,
    entering_investigate: bool,
    ctx: PlanGateContext<'_>,
) -> Result<String, String> {
    if plan_gate && !entering_investigate {
        Ok(format!("{seed}{}", plan_gate_seed(cfg, base_dir, ctx)?))
    } else {
        Ok(seed)
    }
}

/// The workflow skill a dispatch seeds its agent with (#321): an explicit `--skill` wins, else
/// the task type picks — a scout runs `investigate` (read-only findings), a ship runs `plan-work`
/// (burn the note's plan down to its done-when criteria). `debug` is the explicit pick for a
/// repro-first bugfix.
///
/// Deliberately **not** the same mapping as [`bn_core::pipeline::stage::Stage::default_skill`],
/// which answers a different question. This one is "what does an ad-hoc `bn dispatch` run?" — a
/// ship gets `plan-work`, because a dispatched lane writes its own plan and burns it down. That one
/// is "what does the nine-stage pipeline run at this stage?" — `execute` picks up an *already
/// approved* plan. Both are correct; folding them together would silently change what dispatch
/// launches.
pub(crate) fn dispatch_skill_name(explicit: Option<&str>, scout: bool) -> &str {
    explicit.unwrap_or(if scout { "investigate" } else { "plan-work" })
}

/// The seed for a skill-driven dispatch (#321): wraps the skill's rendered workflow body with
/// dispatch context (fresh worktree, plan-approval gate unless --yolo, scout read-only framing),
/// maps the workflow's MCP tool names onto the CLI for harnesses without the bn MCP server, and
/// closes with outcome ingestion, completion reporting, and the `bn reflect "skill:<name> — …"`
/// line that feeds evolve-skills. Self-contained: the caller appends nothing.
struct SkillSeedContext<'a> {
    title: &'a str,
    url: Option<&'a str>,
    scout: bool,
    yolo: bool,
    verify: bool,
    /// A plan-review gate will be appended to this seed. The gate then owns the plan file and
    /// the completion protocol, so the envelope suppresses its bootstrap/verification/completion
    /// blocks instead of issuing a second, contradicting copy.
    plan_gate: bool,
}

fn skill_seed_prompt(
    cfg: &Config,
    base_dir: &std::path::Path,
    ctx: SkillSeedContext<'_>,
    skill: &bn_core::skills::Skill,
) -> Result<String, String> {
    let body = render_workflow_body(skill, ctx.title)?;
    let name = &skill.name;
    let cli_map = bn_core::skills::render_required(cfg, base_dir, "dispatch-cli-map", None)
        .map_err(|error| error.to_string())?;
    let bootstrap = if !ctx.scout && name == "plan-work" && !ctx.plan_gate {
        bn_core::skills::render_required(cfg, base_dir, "dispatch-plan-work-bootstrap", None)
            .map_err(|error| error.to_string())?
    } else {
        String::new()
    };
    // The verify gate renders at most once per seed: the plan-review gate carries it when one is
    // appended, and `plan-work`/`execute` already gate step 6 on `bn verify` (rebase re-run
    // included), so only other workflows need the explicit clause.
    let verification =
        if ctx.verify && !ctx.scout && !ctx.plan_gate && name != "plan-work" && name != "execute" {
            bn_core::skills::render_required(cfg, base_dir, "dispatch-verify-gate", None)
                .map_err(|error| error.to_string())?
        } else {
            String::new()
        };
    let mut args = serde_json::Map::new();
    for (key, value) in [
        ("goal", ctx.title.to_string()),
        (
            "source",
            ctx.url.unwrap_or("(none; use the branch note)").to_string(),
        ),
        ("workflow_name", name.to_string()),
        ("workflow_body", body),
        ("cli_map", cli_map),
    ] {
        args.insert(key.into(), serde_json::Value::String(value));
    }
    if ctx.scout {
        return bn_core::skills::render_required(cfg, base_dir, "dispatch-scout", Some(&args))
            .map_err(|error| error.to_string());
    }
    let autonomy_name = if ctx.yolo {
        "dispatch-autonomy-unattended"
    } else {
        "dispatch-autonomy-interactive"
    };
    let autonomy = bn_core::skills::render_required(cfg, base_dir, autonomy_name, None)
        .map_err(|error| error.to_string())?;
    // With a plan gate appended, the gate's approved-branch is the completion protocol; an empty
    // slot here keeps the seed to a single copy.
    let completion = if ctx.plan_gate {
        String::new()
    } else {
        let mut completion_args = serde_json::Map::new();
        completion_args.insert(
            "workflow_name".into(),
            serde_json::Value::String(name.to_string()),
        );
        bn_core::skills::render_required(
            cfg,
            base_dir,
            "dispatch-completion",
            Some(&completion_args),
        )
        .map_err(|error| error.to_string())?
    };
    args.insert("autonomy".into(), serde_json::Value::String(autonomy));
    args.insert("bootstrap".into(), serde_json::Value::String(bootstrap));
    args.insert(
        "verification".into(),
        serde_json::Value::String(verification),
    );
    args.insert("completion".into(), serde_json::Value::String(completion));
    bn_core::skills::render_required(cfg, base_dir, "dispatch-ship", Some(&args))
        .map_err(|error| error.to_string())
}

fn pipeline_investigate_seed(
    cfg: &Config,
    base_dir: &std::path::Path,
    title: &str,
    url: Option<&str>,
    skill: &bn_core::skills::Skill,
) -> Result<String, String> {
    let body = render_workflow_body(skill, title)?;
    let cli_map = bn_core::skills::render_required(cfg, base_dir, "dispatch-cli-map", None)
        .map_err(|error| error.to_string())?;
    let mut args = serde_json::Map::new();
    args.insert("goal".into(), serde_json::Value::String(title.to_string()));
    args.insert(
        "source".into(),
        serde_json::Value::String(url.unwrap_or("(none; use the branch note)").to_string()),
    );
    args.insert("workflow_body".into(), serde_json::Value::String(body));
    args.insert("cli_map".into(), serde_json::Value::String(cli_map));
    bn_core::skills::render_required(cfg, base_dir, "pipeline-investigate-envelope", Some(&args))
        .map_err(|error| error.to_string())
}

/// The references section folded into a dispatched ship agent's seed (#623): the selected coding
/// standards by name, with a hint to run `bn ref <name>` for each one's full text — so the seed
/// stays small but the agent knows to consult them and write to your standards. Selection: an
/// empty `refs` attaches ALL references; explicit names (repeatable or comma-separated) pin a
/// subset; a literal `-` (or empty value) attaches none. Comma-splits and resolves each name
/// against the store (fuzzy), dedups, and preserves order. `None` when nothing is attached (no
/// references exist, or `-` was passed) so the caller appends nothing.
pub(crate) fn references_section(refs: &[String]) -> Option<String> {
    // Flatten comma-separated values; a `-` or empty token means "attach none".
    let mut tokens: Vec<String> = Vec::new();
    for r in refs {
        for part in r.split(',') {
            let t = part.trim();
            if t == "-" {
                return None;
            }
            if !t.is_empty() {
                tokens.push(t.to_string());
            }
        }
    }
    let names: Vec<String> = if tokens.is_empty() {
        // Default: every reference in the store.
        bn_core::references::names()
    } else {
        // Explicit subset: resolve each (fuzzy) to its canonical name, dedup, keep order.
        let mut out: Vec<String> = Vec::new();
        for t in &tokens {
            if let Some(r) = bn_core::references::resolve(t) {
                if !out.contains(&r.name) {
                    out.push(r.name);
                }
            }
        }
        out
    };
    if names.is_empty() {
        return None;
    }
    Some(format!(
        "\n\nApply these references (my coding standards) when writing code, and check your \
         changes against them — run `bn ref <name>` for the full text of each: {}.",
        names.join(", ")
    ))
}

/// A fresh lowercase UUID (via `uuidgen`, universal on macOS/Linux) to pin an agent's session id so
/// `bn agent send` can steer it via `-p --resume`. Falls back to a self-generated UUIDv4 when
/// `uuidgen` is absent (#646): copilot ≥1.0.71 rejects a `--session-id` that isn't a valid UUID, so
/// the old `bn-<pid>-<ts>` fallback broke steering entirely — the fallback must still be a real UUID.
pub(crate) fn gen_uuid() -> String {
    std::process::Command::new("uuidgen")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_lowercase())
        .filter(|s| is_uuid(s))
        .unwrap_or_else(fallback_uuid)
}

/// True if `s` is a canonical 8-4-4-4-12 hex UUID string (the shape copilot validates).
pub(crate) fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// A self-generated RFC 4122 v4 UUID for when `uuidgen` isn't on PATH. Draws entropy from
/// `/dev/urandom` when available, else derives 16 bytes from a splitmix64 seeded by wall-clock nanos
/// and pid — either way the version/variant bits are stamped so the result is a *valid* UUID.
fn fallback_uuid() -> String {
    let mut bytes = [0u8; 16];
    let filled = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| {
            use std::io::Read;
            f.read_exact(&mut bytes)
        })
        .is_ok();
    if !filled {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut state = nanos ^ ((std::process::id() as u64) << 32) ^ 0x9E37_79B9_7F4A_7C15;
        for chunk in bytes.chunks_mut(8) {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            let b = z.to_le_bytes();
            chunk.copy_from_slice(&b[..chunk.len()]);
        }
    }
    format_uuid_v4(bytes)
}

/// Stamp the version (4) and variant (RFC 4122) bits on 16 bytes and format as a canonical UUID.
fn format_uuid_v4(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let mut s = String::with_capacity(36);
    for (i, b) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Longest inline `--seed` we'll risk on the tmux launch command. A long multi-paragraph seed
/// (backticks, quotes, `#`, newlines) has been observed truncating in transit and leaving an
/// unbalanced quote that wedges the shell (#646), so past this — or on any newline — we refuse to
/// inline and force the caller to fail loud instead of garbling the command.
pub(crate) const MAX_INLINE_SEED: usize = 512;

/// Build the seed argument fragment for the launch command. Prefers a file-based seed (`--seed-file`,
/// always short); a short single-line inline `--seed` is fine; but a long/multiline seed with no
/// seed file is refused (#646) — a wedged shell is worse than a clear failure.
pub(crate) fn seed_launch_arg(
    seed_file: Option<&std::path::Path>,
    seed: &str,
) -> Result<String, String> {
    match seed_file {
        Some(f) => Ok(format!(" --seed-file {}", shq(&f.to_string_lossy()))),
        None if seed.len() <= MAX_INLINE_SEED && !seed.contains('\n') => {
            Ok(format!(" --seed {}", shq(seed)))
        }
        None => Err(format!(
            "seed-file write failed and the {}-byte seed is too long to inline safely — a long \
             inline seed truncates in transit and wedges the shell. Free space/permissions in the \
             temp dir (or set TMPDIR to a fast local dir) and re-dispatch.",
            seed.len()
        )),
    }
}

/// True if `s` is fully quote-balanced as a POSIX shell command: every `'`/`"` is closed and no
/// trailing backslash dangles. A truncated command (odd/unterminated quote) never ends its string
/// and drops the tmux window to a stuck shell (surfacing as `unknown option: --zsh`, #646); this
/// correctly accepts `shq`'s `'\''` escaping (close-escape-reopen) which a naive quote count would
/// mis-flag.
pub(crate) fn command_quoting_is_balanced(s: &str) -> bool {
    #[derive(PartialEq)]
    enum St {
        Bare,
        Single,
        Double,
    }
    let mut st = St::Bare;
    let mut escaped = false;
    for c in s.chars() {
        match st {
            St::Bare => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '\'' {
                    st = St::Single;
                } else if c == '"' {
                    st = St::Double;
                }
            }
            // In single quotes nothing is special except the closing quote (no backslash escaping).
            St::Single => {
                if c == '\'' {
                    st = St::Bare;
                }
            }
            St::Double => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    st = St::Bare;
                }
            }
        }
    }
    st == St::Bare && !escaped
}

/// The `bn agent launch` invocation prefix — the native replacement for the old `workflow/bin/agent`
/// zsh launcher (#683+). Points at the *installed* bn (`$HOME/.config/bn/bn`, the same stable path
/// tmux.conf uses for `bn jump` / `bn tmux-cockpit`), because the tmux server's PATH lacks the
/// bindir, so callers embed an absolute command. Callers append ` {kind} …flags` to this.
pub(crate) fn agent_launcher_bin() -> String {
    std::env::var("HOME")
        .map(|h| format!("{h}/.config/bn/bn agent launch"))
        .unwrap_or_else(|_| "bn agent launch".to_string())
}

fn worker_memory_wrapped_command(cmd: &str, memory_ceiling_mb: u64, systemd_run: bool) -> String {
    let inner = vec!["sh".to_string(), "-lc".to_string(), cmd.to_string()];
    bn_core::worker::spawn::wrap(
        bn_core::worker::spawn::MemoryCap::from_estimate_mb(memory_ceiling_mb),
        systemd_run,
        &inner,
    )
    .iter()
    .map(|arg| shq(arg))
    .collect::<Vec<_>>()
    .join(" ")
}

fn worker_completion_wrapped_command(
    cmd: &str,
    request_id: &str,
    executable: &std::path::Path,
) -> String {
    let script = r#"sh -lc "$1"
code=$?
if [ "$code" -eq 0 ]; then
  "$3" worker finish "$2" --exit-code "$code"
else
  "$3" worker finish "$2" --fail --exit-code "$code"
fi
finish_code=$?
if [ "$finish_code" -ne 0 ]; then
  exit "$finish_code"
fi
exit "$code""#;
    ["sh", "-c", script, "bn-worker-complete", cmd, request_id]
        .into_iter()
        .map(shq)
        .chain(std::iter::once(shq(&executable.to_string_lossy())))
        .collect::<Vec<_>>()
        .join(" ")
}

fn systemd_run_available() -> bool {
    std::process::Command::new("systemd-run")
        .args(["--user", "--scope", "--quiet", "--collect", "--", "true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// The shared gh config dir a pinned worktree copy is seeded from: `$GH_CONFIG_DIR` if set (so an
/// already-pinned parent shell propagates), else gh's default `~/.config/gh`.
fn gh_source_config_dir() -> std::path::PathBuf {
    if let Some(d) = std::env::var_os("GH_CONFIG_DIR").filter(|s| !s.is_empty()) {
        return std::path::PathBuf::from(d);
    }
    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".config")
        .join("gh")
}

/// Pin a worktree to its own `GH_CONFIG_DIR` so a dispatched agent's `gh auth switch` is scoped to
/// that worktree and can't flip a sibling lane's (or bn's own) gh account (#456). Returns the dir
/// to point `GH_CONFIG_DIR` at, or `None` to leave the agent on the shared config (no source config
/// to seed from — pinning to an empty, unauthed dir would break the agent's gh, which is worse).
///
/// Idempotent: an already-seeded dir is reused as-is so the agent's own switched account survives a
/// re-dispatch. `branch` is the sanitized (slash-flattened) form, matching the worktree dir.
fn pin_worktree_gh_account(repo: &str, branch: &str) -> Option<std::path::PathBuf> {
    // Machine-LOCAL base (~/.bn), never the synced notes tree — in gh's insecure mode the seeded
    // hosts.yml carries a token, which must not sync into git.
    let base = bn_core::resolver::bn_home().join("gh");
    let dir = bn_core::gh_pin::worktree_gh_config_dir(&base, repo, branch);
    // Already pinned by a prior dispatch — keep whatever account the agent switched to.
    if dir.join("hosts.yml").is_file() {
        return Some(dir);
    }
    let src = gh_source_config_dir();
    // No shared account to copy → don't pin (leave the agent on the global config).
    if !src.join("hosts.yml").is_file() {
        return None;
    }
    std::fs::create_dir_all(&dir).ok()?;
    for f in bn_core::gh_pin::GH_STATE_FILES {
        let from = src.join(f);
        if from.is_file() {
            // copy preserves the source's 0600 perms on unix, so a copied inline token stays owner-
            // only. A missing optional file (e.g. no config.yml) is fine.
            let _ = std::fs::copy(&from, dir.join(f));
        }
    }
    // Confirm the account file landed; otherwise don't pin (avoid an unauthed agent).
    dir.join("hosts.yml").is_file().then_some(dir)
}

/// Build the harness pass-through for a resolved pipeline model. The single leading `--` ends the
/// launcher's own options; model/effort flags and any Copilot MCP suppressions then reach the
/// harness directly. Every value is shell-quoted for the tmux command line.
pub(crate) fn agent_model_suffix(
    agent_kind: &str,
    model: &bn_core::resolver::PipelineModel,
    disabled_mcps: &[String],
) -> String {
    let mut suffix = String::from(" --");
    // Only pin `--model` when the configured id is one this harness actually accepts; otherwise
    // omit it and let the harness fall back to its own default. bn stores dotted catalog ids
    // (e.g. claude-opus-4.8) that Claude Code rejects, which parked every dispatched claude agent
    // on "model does not exist" (bn#963). Copilot accepts the dotted ids, so it is unaffected.
    if let Some(model_id) =
        bn_core::mcp::Harness::parse(agent_kind).and_then(|harness| harness.model_arg(&model.model))
    {
        suffix.push_str(" --model ");
        suffix.push_str(&shq(model_id));
    }
    suffix.push(' ');
    suffix.push_str(if agent_kind == "claude" {
        "--effort"
    } else {
        "--reasoning-effort"
    });
    suffix.push(' ');
    suffix.push_str(&shq(&model.effort));
    for name in disabled_mcps {
        suffix.push_str(" --disable-mcp-server ");
        suffix.push_str(&shq(name));
    }
    suffix
}

fn dispatch_pipeline_stage(scout: bool, skill: Option<&str>) -> bn_core::pipeline::stage::Stage {
    use bn_core::pipeline::stage::Stage;

    if scout || skill == Some("investigate") {
        Stage::Investigate
    } else if skill == Some("plan") {
        Stage::Plan
    } else {
        Stage::Execute
    }
}

/// Orchestrate `bn dispatch`: resolve the target repo (an explicit `--repo` name from anywhere, else
/// the cwd's), resolve the item, create a worktree off `base` for it, seed that branch's note (goal
/// = the item, plus a source link), run the post-create hook, mark the source todo done, and open a
/// window on it in the repo's tmux session — an agent window with `--agent`. The execution end of
/// `bn next`. `ctx` is the cwd's note context (for todo-id resolution / closing the source todo);
/// it's `None` only when dispatching into another repo from outside any repo.
fn enter_dispatched_investigate_stage(
    cfg: &Config,
    target: &Resolved,
) -> Result<bn_core::pipeline::gate::GateDecision, bn_core::store::StoreError> {
    bn_core::pipeline::stage::transition(
        cfg,
        &cfg.note_file(target),
        bn_core::pipeline::stage::Stage::Investigate,
    )
}

/// Run the fully-seeded harness command `cmd` in `dir` in the foreground, inheriting this process's
/// stdio/tty so the agent takes over the current pane, and block until it exits. Used by
/// `bn dispatch --attach` (the `claude`/`copilot` wrappers): control returns here on exit, which is
/// exactly where the leased pool slot gets handed back. Returns whether the harness exited cleanly.
fn run_agent_foreground(cmd: &str, dir: &std::path::Path) -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The repo whose session this dispatch is running in: an inherited `BN_WORKER_REPO` (set on every
/// dispatched agent/controller lane) takes precedence, else the cwd's repo. `None` means a neutral
/// origin — not inside any repo's session (a bare shell in a non-repo dir, or a daemon launched
/// from `/`).
fn current_session_repo(ctx: Option<&Resolved>) -> Option<String> {
    std::env::var("BN_WORKER_REPO")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| ctx.map(|c| c.repo.clone()))
}

/// Repo-session containment: a dispatch that *explicitly* targets `--repo X` must originate from
/// X's own session, never from a **different** repo's session — so an agent (or a human) working in
/// repo Y can't accidentally fan work into repo X and cross the streams. A neutral origin (no
/// session repo) and X's own session are both allowed, which preserves `--repo`-from-anywhere and
/// the orchestrator (its `sh_dispatch` declares `BN_WORKER_REPO=<target>`, so it always dispatches
/// as the target's own session). Returns the operator-facing rejection message on a cross-repo
/// dispatch, else `None`. Pure for tests.
fn cross_repo_session_block(
    explicit_repo: bool,
    session_repo: Option<&str>,
    target_repo: &str,
) -> Option<String> {
    if !explicit_repo {
        // No `--repo`: the dispatch targets the current session's own repo — never cross-repo.
        return None;
    }
    match session_repo {
        // Repo names are case-insensitive identifiers here — the cwd-derived `ctx.repo` is
        // canonically lowercased (see `resolver::context::normalize`) while a resolved `--repo`
        // target keeps its on-disk casing (e.g. `BigMono`). Compare case-insensitively so a repo with
        // uppercase letters isn't falsely blocked from dispatching to its own session.
        Some(s) if !s.eq_ignore_ascii_case(target_repo) => Some(format!(
            "bn dispatch: --repo {target_repo} can only run from {target_repo}'s session — \
             you're in {s}'s session. cd into a {target_repo} worktree (or dispatch from \
             {target_repo}'s session) and retry."
        )),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_dispatch(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo_override: Option<&str>,
    base: Option<&str>,
    raw: &str,
    name: Option<&str>,
    branch: Option<String>,
    no_tmux: bool,
    agent: Option<&str>,
    scout: bool,
    orchestrator: bool,
    skill: Option<&str>,
    verify: bool,
    yolo: bool,
    plan: bool,
    fresh: bool,
    keep_project_mcps: bool,
    no_pool: bool,
    on_pool_full: crate::cli::OnPoolFull,
    task_id: Option<&str>,
    attach: bool,
    refs: &[String],
    json: bool,
) {
    let controller_id = std::env::var("BN_CONTROLLER_ID")
        .ok()
        .map(|id| id.trim().to_string())
        .filter(|id| {
            !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        });
    // The investigate skill IS the scout workflow (read-only, findings to the note), so an
    // explicit `--skill investigate` gets scout semantics without needing --scout too.
    let scout = scout || skill == Some("investigate");
    // A scout ships nothing, so the verify gate only applies to ship tasks.
    let verify = verify && !scout;
    if raw.trim().is_empty() {
        fail_json(json, "dispatch", "empty-item", "bn dispatch: empty item");
    }
    // #503: opportunistic fleet GC before adding load. A dispatch wave is exactly when stale
    // stragglers from prior waves pile up (this is what drove load to 147), so reap the orphaned
    // process trees + prune dead windows first. Safe + quiet: never touches a working / WIP /
    // orchestrator agent, skips the per-branch PR network calls.
    crate::reap::sweep(cfg);
    // `--agent` present (bare or valued) launches an agent; omitted opens just a worktree/shell
    // window. A bare `--agent` arrives as the "auto" sentinel — launch, but let the launch stage's
    // configured harness decide. An explicit `--agent claude|copilot` PINS the harness (a user pin
    // wins over stage config). Validate the pin up front so a typo fails before we create a
    // worktree; the final harness (pin > stage default > copilot, then the capability gate) is
    // resolved below once repo + launch stage are known.
    let launch_agent = agent.is_some();
    let pinned_harness: Option<&'static str> = agent
        .map(str::trim)
        .filter(|a| !a.eq_ignore_ascii_case("auto"))
        .map(|a| {
            dp::normalize_agent(a).unwrap_or_else(|| {
                fail_json(
                    json,
                    "dispatch",
                    "bad-agent",
                    &format!("bn dispatch: unknown --agent '{a}' (use: copilot | claude)"),
                )
            })
        });

    // Resolve the target repo + its bare path: an explicit `--repo <name>` (from anywhere, cwd
    // ignored), else the cwd's repo. `resolve_bare_by_name` maps a GitHub name onto the local bare
    // and errors (never silently) on an unknown one — surfaced as a `--json` envelope, not empty
    // output (#452).
    let (repo, bare) = match wt::resolve_bare_by_name(cfg, repo_override) {
        Ok(Some((r, b))) => (r, b),
        Ok(None) => {
            let ctx = ctx.unwrap_or_else(|| {
                fail_json(
                    json,
                    "dispatch",
                    "no-repo",
                    "bn dispatch: not in a repo — pass --repo <name> to target one from anywhere",
                )
            });
            let bare = cfg.bare_dir().join(format!("{}.git", ctx.repo));
            if !bare.is_dir() {
                fail_json(
                    json,
                    "dispatch",
                    "no-bare-repo",
                    &format!(
                        "bn dispatch: no bare repo at {} — dispatch needs the bare+worktree layout",
                        bare.display()
                    ),
                );
            }
            (ctx.repo.clone(), bare)
        }
        Err(e) => fail_json(
            json,
            "dispatch",
            "unknown-repo",
            &format!("bn dispatch: {e}"),
        ),
    };
    // Repo-session containment: an explicit `--repo X` fired from a *different* repo's session is
    // refused (a neutral origin or X's own session pass through). Keeps each repo's work in its own
    // session without breaking `--repo`-from-anywhere or the orchestrator.
    if let Some(msg) = cross_repo_session_block(
        repo_override.is_some(),
        current_session_repo(ctx).as_deref(),
        &repo,
    ) {
        fail_json(json, "dispatch", "cross-repo-session", &msg);
    }
    let dispatch_base = ensure_dispatch_base(cfg, &repo, &bare, base).unwrap_or_else(|e| {
        fail_json(
            json,
            "dispatch",
            "base-bootstrap-failed",
            &format!("bn dispatch: base bootstrap failed: {e}"),
        )
    });
    if let Some(message) = dispatch_base_bootstrap_message(&repo, &dispatch_base) {
        eprintln!("{message}");
    }
    let base = dispatch_base.branch;

    let (title, url, source_todo) = resolve_dispatch_item(cfg, ctx, raw);
    let task_name = bn_core::agent::derive_task_name(name, &title);

    // Resolve the launch stage's model + harness. The stage (Investigate/Plan/Execute from the
    // scout/skill flags) selects `[pipeline.models.<stage>]`, whose `harness` is the stage-aware
    // default when the user didn't pin one with `--agent`. Precedence: pin > stage harness >
    // copilot, then the machine capability gate degrades an unavailable harness to copilot
    // (surfaced, never silent). A single dispatched run uses this one harness+model for its whole
    // pipeline, so the override only distinguishes separate dispatches by their launch stage.
    let stage = dispatch_pipeline_stage(scout, skill);
    let model = cfg.pipeline_model(&repo, stage);
    let agent_kind: Option<&'static str> = launch_agent.then(|| {
        let stage_harness: Option<&'static str> = model
            .harness
            .as_deref()
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(|h| {
                dp::normalize_agent(h).unwrap_or_else(|| {
                    fail_json(
                        json,
                        "dispatch",
                        "bad-stage-harness",
                        &format!(
                            "bn dispatch: unknown harness '{h}' in [pipeline.models.{}] (use: copilot | claude)",
                            stage.as_str()
                        ),
                    )
                })
            });
        let resolved = dp::effective_launch_harness(cfg, pinned_harness, stage_harness);
        if let Some((from, reason)) = &resolved.fallback_from {
            if !json {
                eprintln!(
                    "bn dispatch: {from} unavailable ({reason}); falling back to {}",
                    resolved.harness
                );
            }
        }
        resolved.harness
    });

    // Security: --yolo launches the agent with permission gating OFF. A free-text goal is the
    // user's own words — the same trust as an interactive launch, which already yolos — so that's
    // fine. But when the item resolved to an EXTERNAL url/issue, the seed embeds attacker-
    // influenceable content (a fetched issue/PR title), and running THAT unconfirmed is a prompt-
    // injection → command-execution path. So refuse to yolo an externally-sourced seed: it keeps
    // permission gating even under --yolo.
    let yolo_blocked_external = yolo && url.is_some();
    let yolo = yolo && url.is_none();
    if yolo_blocked_external {
        eprintln!(
            "bn dispatch: --yolo ignored — this item carries an external link, so its seed runs \
             WITH permission gating (an external title could inject commands)."
        );
    }
    // Approval-free by default: the launched harness drops the per-tool "allow this?" prompt for any
    // TRUSTED (non-external) dispatch — your own repo/goal — so an agent never wedges on approval
    // gating. This is decoupled from `--yolo`: `--yolo` additionally switches the *seed* to
    // autonomous framing + the consensus plan gate (below), whereas `launch_yolo` only drops the
    // per-tool gating while leaving the plan-first seed intact. An EXTERNAL seed (untrusted,
    // injection-prone) keeps full per-tool gating — the same guard as the `yolo` narrowing above.
    let launch_yolo = url.is_none();

    // Plan-review gate (#349): a ship agent proposes a plan via `bn review submit` and pauses for
    // approval before implementing / opening a PR. On by `--plan` or the `[dispatch].plan_review`
    // config default. Trusted `--yolo` lanes still pass through agent consensus so sensitive plans
    // cannot bypass a mandatory human decision; low-risk quorum approval remains unattended.
    let plan_gate = plan_gate_enabled(plan, cfg.dispatch_plan_review, yolo, scout);

    let branch = match branch {
        Some(branch) => branch,
        None => match dp::choose_implicit_dispatch_branch(
            &title,
            fresh,
            agent_kind.is_some(),
            |candidate| dispatch_branch_occupied(&bare, &cfg.worktree_dir, &repo, candidate),
        ) {
            dp::DispatchBranchChoice::Use(branch) => branch,
            dp::DispatchBranchChoice::AlreadyActive(branch) => fail_json(
                json,
                "dispatch",
                "dispatch-already-active",
                &format!(
                    "bn dispatch: '{branch}' is already active for this item; not creating a \
                     duplicate suffix branch. Use --fresh to recover, or --branch for separate \
                     same-title work."
                ),
            ),
        },
    };
    // Never dispatch an agent onto the repo's main/master branch — that would run (and later reap)
    // the primary worktree. Mirrors the guards in reap.rs and wt/cleanup.rs.
    if is_reserved_branch(&branch) || branch == base {
        fail_json(
            json,
            "dispatch",
            "refuse-main",
            &format!(
                "bn dispatch: refusing to dispatch onto '{branch}' — that's the repo's primary \
                 branch/worktree. Pick a feature branch (e.g. --branch dispatch/my-fix)."
            ),
        );
    }

    // Notes key the branch sanitized (resolver::normalize does slash→dash), and the worktree dir
    // uses the same form — so the note we seed here is the one the user sees on `cd`.
    let sanitized = branch.replace('/', "-");
    let pipeline_investigate = agent_kind.is_some() && !scout;

    // Pool-backed dispatch is the default (#457 generalized): every repo with a bn-managed bare
    // clone leases a warm, reap-safe slot (deps/build cache intact) instead of paying a cold
    // worktree checkout + install. `--no-pool` forces the legacy cold worktree. The pool-vs-cold
    // decision is pure policy in bn-core; the slot lease is the existing `wt pool` primitive. A repo
    // without a bn-managed bare clone, or a full pool, transparently falls back to a cold worktree —
    // so pooling is always a safe best-effort default, and `--no-pool` reproduces the old cold path
    // exactly. Pool size comes from `~/.config/wt/pools` (explicit or `default`), else the built-in
    // DEFAULT_DISPATCH_POOL_SIZE, so no per-repo registration is required.
    let mode = bn_core::pool::dispatch_worktree_mode(no_pool);
    // The cold path — a fresh worktree at the conventional dir — used both without `--pool` and as
    // the fallback when a warm slot can't be leased.
    let cold_worktree = || -> (std::path::PathBuf, bool) {
        let wt_path = cfg.worktree_dir.join(&repo).join(&sanitized);
        let created = match create_worktree(&bare, &wt_path, &branch, &base) {
            Ok(true) => true,
            Ok(false) if may_reuse_occupied_worktree(fresh, agent_kind.is_some()) => false,
            Ok(false) => fail_json(
                json,
                "dispatch",
                "worktree-occupied",
                &format!(
                    "bn dispatch: refusing to reuse occupied worktree {} — choose another \
                     --branch or omit --branch to allocate a unique dispatch branch",
                    wt_path.display()
                ),
            ),
            Err(e) => fail_json(
                json,
                "dispatch",
                "worktree-failed",
                &format!("bn dispatch: worktree creation failed: {e}"),
            ),
        };
        (wt_path, created)
    };
    let (wt_path, created, used_pool) = match mode {
        bn_core::pool::WorktreeMode::Pool => match wt_pool::lease_for_dispatch(cfg, &repo, &branch)
        {
            // A same-branch pooled slot is reusable only for explicit --fresh recovery; the agent
            // lease below reaps/replaces its prior owner before the new harness starts.
            Some((slot, true)) => (slot, true, true),
            Some((slot, false)) if may_reuse_occupied_worktree(fresh, agent_kind.is_some()) => {
                (slot, false, true)
            }
            Some((slot, false)) => {
                fail_json(
                    json,
                    "dispatch",
                    "worktree-occupied",
                    &format!(
                        "bn dispatch: refusing to reuse occupied pooled worktree {} for branch \
                         '{branch}' — choose another --branch or omit --branch to allocate a unique \
                         dispatch branch",
                        slot.display()
                    ),
                )
            }
            // Every worker slot is leased. Cutting a cold worktree here is what the code used to
            // do unconditionally, and it is unbounded — nothing ever reclaims those directories,
            // so a busy day left dozens behind. Capacity is finite; the honest answer is to
            // schedule the work and let it start when a slot frees.
            None => match on_pool_full {
                crate::cli::OnPoolFull::Queue => {
                    schedule_on_pool_full(&repo, raw, json);
                    // `schedule_on_pool_full` returns only when there was no run to queue onto.
                    eprintln!(
                        "bn dispatch: pool full for '{repo}' and no orchestrate run to queue onto \
                         — falling back to a cold worktree."
                    );
                    let (p, c) = cold_worktree();
                    (p, c, false)
                }
                crate::cli::OnPoolFull::Fail => fail_json(
                    json,
                    "dispatch",
                    "pool_full",
                    &format!(
                        "bn dispatch: every warm slot for '{repo}' is leased. Free one with \
                         `bn agent kill <repo:branch>` or `wt pool return`, or pass \
                         --on-pool-full cold to cut a throwaway worktree."
                    ),
                ),
                crate::cli::OnPoolFull::Cold => {
                    eprintln!(
                        "bn dispatch: couldn't lease a warm slot for '{repo}' (pool full? run \
                         `bn wt pool warm`, or pass --no-pool) — falling back to a cold worktree."
                    );
                    let (p, c) = cold_worktree();
                    (p, c, false)
                }
            },
        },
        bn_core::pool::WorktreeMode::Cold => {
            let (p, c) = cold_worktree();
            (p, c, false)
        }
    };

    if created {
        // A cold worktree pays the post-create install hook; a pooled slot is already warm (deps
        // installed by `bn wt pool warm`), so it skips the cold install — the whole point of the
        // pool (#457). MCP-extras wiring is cheap config (no install), so both paths get it.
        if !used_pool {
            run_post_create_hook(cfg, &wt_path, &repo, &branch, &base);
        }
        // Curated planning/productivity MCPs (#323): when `[mcp] extras` is configured, wire them
        // into the worktree so the dispatched agent boots with them available. Server
        // configs only — the instruction block would dirty a tracked CLAUDE.md into the agent's
        // diff. Best-effort: a wiring failure never blocks a dispatch.
        let extras = bn_core::resolver::mcp_extras();
        if !extras.is_empty() {
            let harness_name = agent_kind
                .map(str::to_string)
                .unwrap_or_else(|| wt::review_harness_name(cfg));
            if let Some(h) = bn_core::mcp::Harness::parse(&harness_name) {
                match bn_core::mcp::wire_servers(&wt_path, h, &extras) {
                    Ok(report) => {
                        for m in &report.missing_runners {
                            eprintln!(
                                "bn dispatch: MCP extra runner '{}' not on PATH — {}",
                                m.runner, m.fix
                            );
                        }
                    }
                    Err(e) => eprintln!("bn dispatch: MCP extras not wired: {e}"),
                }
            }
        }
    }

    // Resolve the workflow skill that will drive the agent (#321): explicit --skill, else by task
    // type (scout → investigate, ship → plan-work). Loaded against the fresh worktree so repo-tier
    // skills apply, falling back to the dispatcher's cwd (where the bn checkout / BN_MCP_SKILLS_DIR
    // may be visible). Runtime workflows are required: missing defaults fail loudly rather than
    // falling back to a compiled prompt.
    let skill_loaded = if agent_kind.is_some() {
        let name = if pipeline_investigate && skill.is_none() {
            "investigate"
        } else {
            dispatch_skill_name(skill, scout)
        };
        let found = bn_core::skills::load_skill(cfg, &wt_path, name).unwrap_or_else(|error| {
            fail_json(
                json,
                "dispatch",
                "invalid-skill-catalog",
                &format!("bn dispatch: {error}"),
            )
        });
        let found = if found.is_some() {
            found
        } else {
            std::env::current_dir().ok().and_then(|dir| {
                bn_core::skills::load_skill(cfg, &dir, name).unwrap_or_else(|error| {
                    fail_json(
                        json,
                        "dispatch",
                        "invalid-skill-catalog",
                        &format!("bn dispatch: {error}"),
                    )
                })
            })
        };
        if found.is_none() {
            fail_json(
                json,
                "dispatch",
                "skill-not-found",
                &format!(
                    "bn dispatch: required skill '{name}' not found — `bn skills` lists the available ones"
                ),
            );
        }
        found
    } else {
        if skill.is_some() {
            eprintln!(
                "bn dispatch: --skill shapes a dispatched agent's seed — pass --agent to launch one"
            );
        }
        None
    };

    // Seed the new branch's note: goal = the item; record the source link. Only on a *fresh*
    // worktree — re-dispatching an existing branch (or a fanout hitting one already in flight) must
    // not clobber the note a live agent has been writing. `ensure_note` is safe either way.
    let target = Resolved {
        repo: repo.clone(),
        branch: sanitized.clone(),
    };
    let _ = commands::ensure_note(cfg, &target, &today());

    // Bind the task to this worktree, so the fleet can say what the agent is working on and link
    // through to it. Done here — after the branch and worktree exist, before the agent launches —
    // so the very first supervisor poll after the launch already sees the binding. Not fatal: an
    // unknown id is worth a warning, not a refusal to start work that is otherwise ready.
    if let Some(id) = task_id {
        match bn_core::todo::claim(&cfg.tasks_file(), id, &repo, &sanitized) {
            Ok(true) => {}
            Ok(false) => eprintln!(
                "bn dispatch: no task '{id}' in the store — dispatching anyway, but the fleet \
                 won't show a task for this agent"
            ),
            Err(e) => eprintln!("bn dispatch: could not bind task '{id}': {e}"),
        }
    }

    if created {
        let _ = commands::set_goal(cfg, &target, &title, &today());
        // The goal says what the worktree is *for*; the todo makes it trackable work — it lands in
        // `bn todo` / `bn next` and can be closed with `bn done`, so a dispatched task is visible in
        // the same places as anything you captured by hand.
        // `if_absent` on both, matching the `decision:` writes just below. Without it a
        // re-dispatch (or two dispatches racing the same branch) seeds the same todo twice as `t1`
        // and `t2`, and the source link twice — the note then claims two pieces of work where
        // there is one.
        let _ = commands::add(cfg, &target, "todo", &title, Some(&title), &today());
        if let Some(u) = &url {
            let source = format!("source: {u}");
            let _ = commands::add(cfg, &target, "collab", &source, Some(&source), &today());
        }
        // Record the task type in the note itself (bn's own store — no new format): a scout
        // investigates and reports, a ship delivers a change. `bn promote` flips scout → ship.
        let task_line = if scout {
            "task: scout — investigate & report, no code change"
        } else {
            "task: ship — deliver a change"
        };
        let _ = commands::add(cfg, &target, "decision", task_line, Some("task:"), &today());
        if let Some(s) = &skill_loaded {
            let _ = commands::add(
                cfg,
                &target,
                "decision",
                &format!("skill: {} — the workflow this dispatch follows", s.name),
                Some("skill:"),
                &today(),
            );
        }
        if verify {
            let _ = commands::add(
                cfg,
                &target,
                "decision",
                "gate: run `bn verify` before done — only report done when the build/test/lint gate passes",
                Some("gate: run `bn verify`"),
                &today(),
            );
        }
        if plan_gate {
            let _ = commands::add(
                cfg,
                &target,
                "decision",
                "gate: plan-review — propose a plan via `bn review submit` and wait for approval before implementing / opening a PR (#349)",
                Some("gate: plan-review"),
                &today(),
            );
        }
        if pipeline_investigate {
            match enter_dispatched_investigate_stage(cfg, &target) {
                Err(e) => fail_json(
                    json,
                    "dispatch",
                    "pipeline-stage",
                    &format!("bn dispatch: failed to enter investigate stage: {e}"),
                ),
                // The SMART gate (#998) refused. This is a real stop, not a warning: dispatching
                // an unscoped task is exactly the wasted-worktree case the gate exists to prevent.
                // The task has been stamped and an attention item raised to get it fixed.
                Ok(decision) if !decision.admits() => {
                    let why = decision.refusal_line().unwrap_or_default();
                    fail_json(
                        json,
                        "dispatch",
                        "smart-gate",
                        &format!("bn dispatch: {why}"),
                    );
                }
                Ok(_) => {}
            }
        }
    }

    // Close the source todo in the originating (cwd) note — it's now in flight.
    if let (Some(ctx), Some(id)) = (ctx, &source_todo) {
        let _ = commands::done(cfg, ctx, id, &today(), None);
    }

    // Open the worktree's window in the *repo's* tmux session (the layout `tat` expects), creating
    // the session if needed — so this works whether or not you were already in tmux. With --agent,
    // the window runs the seeded agent instead of a shell. `--no-tmux` skips it entirely.
    let mut tmux_opened = false;
    let mut agent_launched = false;
    // Project MCP servers suppressed for a dispatched Copilot agent (#384) — surfaced in the JSON
    // envelope and the human summary so it's visible why a repo's MCP didn't load.
    let mut suppressed_mcps: Vec<String> = Vec::new();
    // The per-worktree gh config dir this agent is pinned to (#456), for the JSON envelope. `None`
    // when no agent launched or the shared gh config couldn't be found to seed one.
    let mut gh_config_dir: Option<String> = None;
    if !no_tmux {
        if let Some(kind) = agent_kind {
            // Pre-grant the harness's folder-trust for the new worktree — unconditionally, so NO
            // dispatched agent boots into a "trust this folder?" prompt (the yolo flags don't cover
            // it). Folder trust only silences that dialog; it never grants tool execution (per-tool
            // gating still applies for an untrusted/external seed), so it's safe for every dispatch.
            // In-process (no dependency on which `bn` the launcher finds).
            let _ = apply_folder_trust(&wt_path.to_string_lossy(), kind);
            // Pin a session id so `bn agent send` can steer this exact agent via `-p --resume`, and
            // record it after this launch wins the per-worktree lease.
            let sid = gen_uuid();
            let plan_path = if plan_gate && !pipeline_investigate {
                Some(
                    bn_core::plan::init_agent(cfg, &target, &sid).unwrap_or_else(|error| {
                        fail_json(
                            json,
                            "dispatch",
                            "plan-file",
                            &format!("bn dispatch: cannot create plan draft: {error}"),
                        )
                    }),
                )
            } else {
                None
            };
            // Attribute this run's work-class (#381) so `bn cost` categorizes it once the harness
            // session is ingested, and (#500) so the live fleet stamps its role. A scout is
            // research; an `--orchestrator` monitor is orchestration (which the dashboard pins as a
            // distinct fleet tier); every other dispatched ship agent is coding (orchestrated waves
            // shell out to `bn dispatch`, so they land here as coding too).
            let work_class = if scout {
                bn_core::cost::WorkClass::Research
            } else if orchestrator {
                bn_core::cost::WorkClass::Orchestration
            } else {
                bn_core::cost::WorkClass::Coding
            };
            let workflow = skill_loaded
                .as_ref()
                .expect("agent dispatch requires a loaded workflow");
            let seed = if pipeline_investigate {
                pipeline_investigate_seed(cfg, &wt_path, &title, url.as_deref(), workflow)
                    .unwrap_or_else(|error| {
                        fail_json(
                            json,
                            "dispatch",
                            "prompt-render",
                            &format!("bn dispatch: {error}"),
                        )
                    })
            } else {
                skill_seed_prompt(
                    cfg,
                    &wt_path,
                    SkillSeedContext {
                        title: &title,
                        url: url.as_deref(),
                        scout,
                        yolo,
                        verify,
                        plan_gate: plan_gate && !pipeline_investigate,
                    },
                    workflow,
                )
                .unwrap_or_else(|error| {
                    fail_json(
                        json,
                        "dispatch",
                        "prompt-render",
                        &format!("bn dispatch: {error}"),
                    )
                })
            };
            // Fold the coding-standards references (#623) and the branch's standing directives
            // (`bn agent instruct`) into the seed. See `seed_context::section`.
            let seed = format!(
                "{seed}{}",
                super::seed_context::section(cfg, &repo, &branch, scout, refs)
            );
            // No separate "## Plan artifact" section: the plan-review gate's step 1 names the
            // bn-owned draft file and forbids note-item decomposition itself.
            let plan_file_s = plan_path.as_ref().map(|path| path.display().to_string());
            // Append the concrete plan-review-gate protocol (#349) last, so it's the authoritative
            // final instruction: register a plan, poll, and don't ship until approved.
            let seed = append_plan_gate(
                cfg,
                &wt_path,
                seed,
                plan_gate,
                pipeline_investigate,
                PlanGateContext {
                    repo: &repo,
                    branch: &branch,
                    sid: &sid,
                    verify,
                    reviewers: cfg.dispatch_plan_reviewers,
                    quorum: cfg.dispatch_plan_review_quorum,
                    plan_file: plan_file_s.as_deref(),
                },
            )
            .unwrap_or_else(|error| {
                fail_json(
                    json,
                    "dispatch",
                    "prompt-render",
                    &format!("bn dispatch: {error}"),
                )
            });
            // `launch_yolo` (approval-free for trusted dispatches), NOT the seed-framing `yolo`:
            // this flag only drops the launched harness's per-tool gating. The plan-first vs.
            // autonomous seed and the consensus plan gate are decided separately by `yolo` above.
            let yolo_flag = if launch_yolo { " --yolo" } else { "" };
            // On a recovery re-dispatch, force a brand-new harness session instead of resuming the
            // worktree's idle/dead one (#383) — a resumed Copilot session comes back at an empty
            // prompt having done nothing; a fresh, re-seeded one actually resumes the work.
            let fresh_flag = if fresh { " --fresh" } else { "" };
            // File-based seed: keep the window's launch command short (a long inline seed has
            // been observed truncating in transit, leaving an unbalanced quote and a stuck
            // shell, #646). The launcher reads the file via --seed-file; a short single-line seed
            // may inline, but a long/multiline seed with no seed file is refused (fail loud) rather
            // than garbled onto the tmux command.
            let seed_file = wt::prompt_to_file("seed", &repo, &sanitized, &seed);
            let mut cmd = match seed_launch_arg(seed_file.as_deref(), &seed) {
                Ok(seed_arg) => format!(
                    "{} {kind}{yolo_flag}{fresh_flag} --session {sid}{seed_arg}",
                    agent_launcher_bin(),
                ),
                Err(msg) => {
                    if json {
                        println!(
                            "{}",
                            Envelope::failure("dispatch", "seed_write_failed", &msg).to_json()
                        );
                    } else {
                        eprintln!("dispatch: could not launch agent — {msg}");
                    }
                    return;
                }
            };
            // #384: an unattended Copilot agent hangs at boot when a repo's project MCP server
            // fails or wants interactive auth — it wedges on "Loading: … MCP servers" and never
            // reaches its prompt (0 AIC). Copilot has no blanket `--strict-mcp-config`, so we ask
            // it to skip each project server by name via the launcher's pass-through (`-- <args>`
            // become the agent's own argv). bn's own server is always kept. Off with
            // `--keep-project-mcps` (repo MCPs known-good and needed); no-op for Claude, which has
            // native strict handling.
            let suppressed = if kind == "copilot" && !keep_project_mcps {
                bn_core::mcp::copilot_project_servers(&wt_path)
            } else {
                Vec::new()
            };
            // `stage`/`model` were resolved up front (with the harness); reuse them here. The
            // suffix builder ignores `model.harness` — it only emits `--model`/`--effort`.
            cmd.push_str(&agent_model_suffix(kind, &model, &suppressed));
            suppressed_mcps = suppressed;
            // #456: pin this worktree to its own GH_CONFIG_DIR so the agent's `gh auth switch`
            // (e.g. work → personal to reach a private repo) is scoped here and can't flip a
            // sibling lane's — or bn's own — gh account. Seeded from the shared config; if that
            // can't be found we leave the agent on the global config (no worse than before).
            if let Some(dir) = pin_worktree_gh_account(&repo, &sanitized) {
                cmd = format!("GH_CONFIG_DIR={} {cmd}", shq(&dir.to_string_lossy()));
                gh_config_dir = Some(dir.to_string_lossy().to_string());
            }
            if let Some(controller_id) = &controller_id {
                cmd = format!(
                    "BN_CONTROLLER_ID={} BN_WORKER_SESSION_ID={} BN_WORKER_REPO={} \
                     BN_WORKER_BRANCH={} {cmd}",
                    shq(controller_id),
                    shq(&sid),
                    shq(&repo),
                    shq(&branch),
                );
            }
            if let Some(memory_ceiling_mb) = std::env::var("BN_AGENT_MEMORY_CEILING_MB")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
            {
                cmd =
                    worker_memory_wrapped_command(&cmd, memory_ceiling_mb, systemd_run_available());
            }
            if let Ok(request_id) = std::env::var("BN_WORKER_REQUEST_ID") {
                let executable =
                    std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("bn"));
                cmd = worker_completion_wrapped_command(&cmd, &request_id, &executable);
            }
            // #498: an orchestrator/planning-role agent lives in the shared cockpit session
            // (`wt::orchestrator_session()`, default `cron`), not the per-repo ship session — a
            // scout/ship agent stays in its repo's session.
            let launch_session = if orchestrator {
                Some(wt::orchestrator_session())
            } else {
                None
            };
            // #646: an odd number of single quotes in the launch command never terminates its
            // string, dropping the window to a stuck shell (surfacing as `unknown option: --zsh`).
            // Everything we append is `shq`-quoted, so this should always hold — but assert it before
            // send and refuse loud rather than wedge a shell.
            if !command_quoting_is_balanced(&cmd) {
                let msg = "assembled launch command has unbalanced quotes; refusing to launch \
                           (would wedge the shell)";
                if json {
                    println!(
                        "{}",
                        Envelope::failure("dispatch", "unbalanced_command", msg).to_json()
                    );
                } else {
                    eprintln!("dispatch: {msg}");
                }
                return;
            }
            // --attach (interactive foreground): run the investigation in THIS pane instead of a
            // background tmux lane. Record the session for cost/collab exactly like the lane path,
            // run the seeded harness in the foreground (it takes over the terminal), and — once it
            // exits — hand any leased pool slot straight back, so the `claude`/`copilot` wrappers
            // leave no residue. Deliberately skips the agent-lease, tmux window, and controller
            // events: those are background-lane bookkeeping with no meaning for a live pane.
            if attach {
                let worktree = wt_path.to_string_lossy().into_owned();
                crate::cmd::agent::record_launched_session(
                    cfg,
                    crate::cmd::agent::LaunchedSession {
                        ctx: &target,
                        harness: kind,
                        session_id: &sid,
                        worktree: &worktree,
                        work_class,
                        task_name: Some(&task_name),
                        controller_id: controller_id.as_deref(),
                    },
                );
                if !json {
                    eprintln!(
                        "bn dispatch: {kind} {} in this pane → {}{}",
                        if scout { "scout" } else { "ship" },
                        wt_path.display(),
                        if used_pool {
                            " (pooled slot; returns on exit)"
                        } else {
                            ""
                        }
                    );
                }
                let ran = run_agent_foreground(&cmd, &wt_path);
                // The launcher's placement guard leased this worktree under our `sid` (one agent per
                // checkout, even for a hand-typed `claude`). The pane is now free, so release it
                // here — the window it was keyed to is the user's own shell and outlives the agent,
                // so nothing else would ever notice it went idle.
                let _ = agent_lease::release(&sid);
                // A scout leaves the slot clean, so it returns to the warm pool; `Return` refuses a
                // dirty slot (a ship that wrote code) rather than discard work — surfaced, not forced.
                let slot_returned = used_pool
                    && wt_pool::run(
                        cfg,
                        wt::PoolCmd::Return {
                            target: Some(worktree.clone()),
                            force: false,
                        },
                    );
                if json {
                    let env = Envelope::success(
                        "dispatch",
                        serde_json::json!({
                            "attach": true,
                            "agent": kind,
                            "agent_launched": ran,
                            "repo": repo,
                            "branch": branch,
                            "task": if scout { "scout" } else { "ship" },
                            "worktree": worktree,
                            "pooled": used_pool,
                            "slot_returned": slot_returned,
                            "session_id": sid,
                        }),
                    );
                    println!("{}", env.to_json());
                } else {
                    let slot = if slot_returned {
                        format!("returned pool slot → {worktree}")
                    } else if used_pool {
                        format!("pool slot kept (dirty? `bn wt pool return --force`) → {worktree}")
                    } else {
                        format!("worktree → {worktree}")
                    };
                    eprintln!(
                        "bn dispatch: {kind} exited {}; {slot}",
                        if ran { "cleanly" } else { "non-zero" }
                    );
                }
                return;
            }
            // The lane's tmux window MUST be named by the sanitized branch — the single contract
            // every orchestrate consumer assumes (engine `QueueItem::tmux_window`, CLI `observe`/
            // `discover_lanes`, reap, cockpit, cross-repo sweep all reconstruct `<repo>:<sanitized
            // branch>`). A readable "<title> · <hash>" name here made the daemon fail to match its
            // own live lanes → `has_window=false` → misclassified Dead → hard-restart churn.
            let window_name = bn_core::orchestrate::tmux_window_name(&branch);
            let lease_session = launch_session.unwrap_or(&repo);
            let lease_target = format!("{lease_session}:{window_name}");
            let reservation = match agent_lease::reserve(agent_lease::AgentLeaseRequest {
                repo: &repo,
                branch: &branch,
                worktree: &wt_path,
                session_id: &sid,
                harness: kind,
                kind: bn_core::agent::AgentLeaseKind::Dispatch,
                tmux_target: &lease_target,
                replace: fresh,
                in_place: false,
            }) {
                Ok(lease) => lease,
                Err(error) => {
                    fail_json(
                        json,
                        "dispatch",
                        "agent-lease-held",
                        &format!("bn dispatch: refusing agent launch — {error}"),
                    );
                }
            };
            if let Some(controller_id) = &controller_id {
                let _ = commands::add(
                    cfg,
                    &target,
                    "collab",
                    &format!("controller-id: {controller_id}"),
                    Some("controller-id:"),
                    &today(),
                );
            }
            let worktree = wt_path.to_string_lossy().into_owned();
            crate::cmd::agent::record_launched_session(
                cfg,
                crate::cmd::agent::LaunchedSession {
                    ctx: &target,
                    harness: kind,
                    session_id: &sid,
                    worktree: &worktree,
                    work_class,
                    task_name: Some(&task_name),
                    controller_id: controller_id.as_deref(),
                },
            );
            agent_launched = wt::open_worktree_window_cmd_named(
                &repo,
                &sanitized,
                &window_name,
                &wt_path,
                false,
                Some(&cmd),
                true,
                launch_session,
            );
            if !agent_launched {
                if let Err(error) = reservation.rollback() {
                    eprintln!("dispatch: agent launch failed; {error}");
                }
            } else {
                agent_lease::mark_window(reservation.lease());
                reservation.commit();
                if let Some(path) = &plan_path {
                    let split_session = launch_session.unwrap_or(&repo);
                    let _ = wt::open_plan_split(split_session, &window_name, path);
                }
            }
            tmux_opened = agent_launched;
            if agent_launched {
                if let Some(controller_id) = &controller_id {
                    if let Err(error) = append_controller_worker_event(
                        controller_id,
                        &sid,
                        &repo,
                        &branch,
                        bn_core::controller::ControllerWorkerEventType::Progress,
                        &format!("worker dispatched: {title}"),
                        url.as_deref(),
                    ) {
                        eprintln!(
                            "dispatch: worker launched but controller event append failed: {error}"
                        );
                    }
                }
            }
        } else {
            let launch_session = if orchestrator {
                Some(wt::orchestrator_session())
            } else {
                None
            };
            // Same lane-window naming contract as the agent-launch branch above: the sanitized
            // branch, so `bn orchestrate` can adopt/observe this lane by name.
            let window_name = bn_core::orchestrate::tmux_window_name(&branch);
            tmux_opened = wt::open_worktree_window_cmd_named(
                &repo,
                &sanitized,
                &window_name,
                &wt_path,
                false,
                None,
                false,
                launch_session,
            );
        }
    }

    if json {
        let env = Envelope::success(
            "dispatch",
            serde_json::json!({
                "title": title,
                "name": task_name,
                "url": url,
                "repo": repo,
                "branch": branch,
                "base": base,
                "task": if scout { "scout" } else { "ship" },
                "skill": skill_loaded.as_ref().map(|s| s.name.clone()),
                "verify": verify,
                "yolo": yolo,
                "yolo_blocked_external": yolo_blocked_external,
                "launch_yolo": launch_yolo,
                "worktree": wt_path.to_string_lossy(),
                "created": created,
                "pooled": used_pool,
                "tmux_opened": tmux_opened,
                "agent": agent_kind,
                "agent_launched": agent_launched,
                "suppressed_mcps": suppressed_mcps,
                "gh_config_dir": gh_config_dir,
                "source_todo": source_todo,
                "controller_id": controller_id,
            }),
        );
        println!("{}", env.to_json());
        return;
    }

    if created {
        println!("Dispatched → {}", wt_path.display());
    } else {
        println!("Worktree already exists → {}", wt_path.display());
    }
    let task = if scout { "scout" } else { "ship" };
    println!("  repo: {repo}  base: {base}  task: {task}");
    if let Some(controller_id) = &controller_id {
        println!("  controller: {controller_id}");
    }
    if used_pool {
        println!(
            "  pool: leased a warm slot (no cold install) — {}",
            wt_path.display()
        );
    }
    if let Some(s) = &skill_loaded {
        println!("  skill: {}", s.name);
    }
    println!("  goal: {title}");
    println!("  name: {task_name}");
    if let Some(u) = &url {
        println!("  source: {u}");
    }
    if let Some(id) = &source_todo {
        println!("  closed source todo [{id}]");
    }
    if !suppressed_mcps.is_empty() {
        println!(
            "  suppressed {} project MCP server(s) so Copilot boots clean: {} (keep with --keep-project-mcps)",
            suppressed_mcps.len(),
            suppressed_mcps.join(", ")
        );
    }
    if agent_launched {
        let kind = agent_kind.unwrap_or("agent");
        println!(
            "  launched {kind} in task-named tmux window — \u{f013} working · \u{f0f3} waiting · \u{f00c} done"
        );
    } else if agent_kind.is_some() {
        println!(
            "  agent requested but tmux is unavailable; cd {} and run the agent",
            wt_path.display()
        );
    } else if tmux_opened {
        println!("  task-named tmux window opened");
    } else {
        println!("  cd {}", wt_path.display());
    }
}
