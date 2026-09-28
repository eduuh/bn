//! Merge machinery for `wt`: `merge`, the file-overlap-aware `merge --all`
//! (clusters, auto-rebase, union conflict resolution). The pre-land gate is in [`super::gate`].
//! Moved verbatim from the former wt.rs god file (issue #351).

use super::*;

// --- merge (local-action trigger for cleanup) ------------------------------

/// The `gh pr merge` method flag for the chosen strategy. Default is squash; `--merge` and
/// `--rebase` opt into a merge commit or rebase-merge respectively (they're mutually exclusive at
/// the CLI layer).
pub(crate) fn merge_method(merge: bool, rebase: bool) -> &'static str {
    if rebase {
        "--rebase"
    } else if merge {
        "--merge"
    } else {
        "--squash"
    }
}

/// Merge one GitHub PR by its head `branch` via `gh pr merge`, run OUTSIDE any worktree
/// (`--repo` makes it API-driven) so the `--delete-branch` step never trips over the branch being
/// checked out locally. Shared by the single-PR `cmd_merge` and the `--all` driver. Returns
/// whether gh reported success.
pub(crate) fn merge_pr_via_gh(
    slug: &str,
    branch: &str,
    method: &str,
    delete_branch: bool,
    admin: bool,
) -> bool {
    let mut args = vec!["pr", "merge", branch, "--repo", slug, method];
    if delete_branch {
        args.push("--delete-branch");
    }
    if admin {
        args.push("--admin");
    }
    crate::gh::gh_run_in(&home(), &args)
}

pub(crate) fn cmd_merge(cfg: &Config, flags: MergeFlags) -> bool {
    let MergeFlags {
        merge,
        rebase,
        no_delete_branch,
        admin,
        no_gate,
        ci,
        ..
    } = flags;
    let wt_path = cwd();
    let Some(bare) = find_bare_repo(&wt_path) else {
        ceprintln!("{RED}Not inside a worktree{NC}");
        return false;
    };
    let Some(branch) = git_out(&["-C", &wt_path.to_string_lossy(), "branch", "--show-current"])
    else {
        ceprintln!("{RED}Could not determine current branch{NC}");
        return false;
    };
    if bn_core::agent::is_reserved_branch(&branch) {
        ceprintln!("{RED}Refusing to merge '{branch}'{NC}");
        return false;
    }
    let slug = match remote_for(&bare) {
        Remote::GitHub { slug } => slug,
        _ => {
            ceprintln!(
                "{YELLOW}bn wt merge supports GitHub (gh). For ADO, complete the PR in the UI/az — \
                 the `cleanup --all` poller will reclaim the worktree.{NC}"
            );
            return false;
        }
    };

    // Pre-merge gate: rebase origin/<branch> onto current origin/main in a disposable worktree and
    // verify THAT head, so a red or stale-base PR can't land (the class that broke main). Repos
    // with no build/test/lint job aren't blocked; `--no-gate` is the deliberate bypass.
    if !no_gate {
        match resolver::resolve(cfg, &wt_path) {
            Ok(ctx) => {
                let head_ref = format!("origin/{branch}");
                if let GateOutcome::Ran(r) = gate_head(cfg, &ctx, &bare, &head_ref, None, true, false)
                {
                    let color = if r.pass { GREEN } else { RED };
                    cprintln!("{color}{}{NC}", gate_summary(&r));
                    let bullet =
                        format!("{} — {} (pre-merge)", bn_core::util::now_stamp(), gate_summary(&r));
                    let _ = bn_core::commands::append_body(cfg, &ctx, "progress", &bullet, &today());
                    if !r.pass {
                        ceprintln!(
                            "{RED}Refusing to merge {slug}/{branch} — gate failed. Fix and re-push, \
                             or bypass with `bn wt merge --no-gate`.{NC}"
                        );
                        return false;
                    }
                }
            }
            Err(_) => ceprintln!(
                "{YELLOW}bn wt merge: could not resolve repo context — skipping the pre-merge gate.{NC}"
            ),
        }
    }

    // The published CI verdict. The gate above proves the head builds *here*; this proves the
    // checks the repo actually publishes agreed. Checked after it, so a PR that can't pass locally
    // never costs the wait budget.
    if ci.enabled && !ci_gate_open(&slug, &branch, &format!("{slug}/{branch}"), ci.wait) {
        return false;
    }

    let method = merge_method(merge, rebase);
    cprintln!(
        "{BLUE}Merging PR for {slug}/{branch} ({}){NC}",
        &method[2..]
    );
    if !merge_pr_via_gh(&slug, &branch, method, !no_delete_branch, admin) {
        ceprintln!("{RED}gh pr merge failed — leaving the worktree in place{NC}");
        return false;
    }

    // The merge succeeded — that's the win. Cleanup is best-effort: if it's held back (open
    // todos/blockers, or uncommitted changes), say so but still report success.
    let (_, worktree_dir) = layout(cfg);
    if cleanup_one(cfg, &wt_path, &worktree_dir, false).is_err() {
        cprintln!(
            "{YELLOW}Merged — worktree kept (see above). Clean later with `bn wt cleanup`.{NC}"
        );
    }
    true
}

// --- merge --all (file-overlap-aware batch merge) --------------------------

/// A PR's mergeability as GitHub reports it. `Unknown` is the transient state GitHub returns while
/// it recomputes the merge commit (e.g. right after another PR lands on the base) — the caller
/// polls past it before treating it as a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mergeable {
    Yes,
    Conflicting,
    Unknown,
}

/// A PR's changed-file paths, via `gh pr view <n> --json files`. Falls back to a base...head diff
/// (`origin/main...origin/<branch>`) when the API list is empty — e.g. a brand-new PR GitHub hasn't
/// computed yet. Empty on total failure, so an unreadable PR conservatively looks disjoint (it
/// won't be wrongly forced into a serialize cluster) — a later mergeability re-check still guards
/// the actual merge.
pub(crate) fn pr_changed_files(slug: &str, number: i64, branch: &str) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct FilesView {
        #[serde(default)]
        files: Vec<FileEntry>,
    }
    #[derive(serde::Deserialize)]
    struct FileEntry {
        #[serde(default)]
        path: String,
    }
    let number = number.to_string();
    let files: Vec<String> = bn_core::runner::gh_json::<FilesView>(
        &bn_core::runner::RealRunner,
        &["pr", "view", &number, "--repo", slug, "--json", "files"],
    )
    .map(|v| {
        v.files
            .into_iter()
            .map(|f| f.path.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect()
    })
    .unwrap_or_default();
    if !files.is_empty() {
        return files;
    }
    git_out(&[
        "diff",
        "--name-only",
        &format!("origin/main...origin/{branch}"),
    ])
    .map(|d| {
        d.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

/// Current mergeability of PR `number`, via `gh pr view --json mergeable`. Anything other than a
/// clean `MERGEABLE`/`CONFLICTING` (including a failed call) maps to `Unknown`, so the caller keeps
/// polling rather than mistaking a hiccup for a verdict.
pub(crate) fn pr_mergeable(slug: &str, number: i64) -> Mergeable {
    #[derive(serde::Deserialize)]
    struct MergeableView {
        mergeable: String,
    }
    let number = number.to_string();
    match bn_core::runner::gh_json::<MergeableView>(
        &bn_core::runner::RealRunner,
        &["pr", "view", &number, "--repo", slug, "--json", "mergeable"],
    ) {
        Some(v) => match v.mergeable.as_str() {
            "MERGEABLE" => Mergeable::Yes,
            "CONFLICTING" => Mergeable::Conflicting,
            _ => Mergeable::Unknown,
        },
        None => Mergeable::Unknown,
    }
}

/// The CLI flags every `bn wt merge` entry point shares. Carried as one value rather than eight
/// positional bools: the single-PR merge, `--all`, and `--all-repos` all take the same set, and a
/// positional list that long is one transposed argument away from landing with the wrong gate.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MergeFlags {
    pub merge: bool,
    pub rebase: bool,
    pub no_delete_branch: bool,
    pub admin: bool,
    pub no_auto_rebase: bool,
    pub no_gate: bool,
    pub ci: CiGate,
}

/// The CI-gate setting, threaded from the CLI through every merge path so the single-PR merge,
/// `--all`, and `--all-repos` can't drift into disagreeing about whether checks are required.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CiGate {
    pub enabled: bool,
    pub wait: Duration,
}

impl Default for CiGate {
    fn default() -> Self {
        Self {
            enabled: true,
            wait: Duration::from_secs(600),
        }
    }
}

/// What the PR's GitHub checks say right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Checks {
    /// Every check finished green (or was skipped/neutral) — safe to land.
    Green,
    /// At least one check is still queued or running. Not a verdict: poll, then skip if it stays.
    Pending,
    /// At least one check finished red (failure, timeout, or cancelled).
    Red,
    /// The PR has no checks at all — nothing to gate on, so this never blocks a landing.
    None,
}

/// Classify one rollup entry. Split out from the `gh` call so the state machine — which strings
/// mean "still running", which mean "red" — is unit-testable without the network.
fn classify_check(status: &str, conclusion: &str, state: &str) -> Checks {
    let status = status.trim().to_ascii_uppercase();
    let conclusion = conclusion.trim().to_ascii_uppercase();
    let state = state.trim().to_ascii_uppercase();
    // A check run still in flight, or a legacy commit status still pending.
    if matches!(
        status.as_str(),
        "QUEUED" | "IN_PROGRESS" | "WAITING" | "PENDING" | "REQUESTED"
    ) || state == "PENDING"
    {
        return Checks::Pending;
    }
    let verdict = if conclusion.is_empty() {
        state.as_str()
    } else {
        conclusion.as_str()
    };
    match verdict {
        "SUCCESS" | "SKIPPED" | "NEUTRAL" => Checks::Green,
        // Nothing decided yet — treat as still running rather than as a pass.
        "" => Checks::Pending,
        _ => Checks::Red,
    }
}

/// The PR's check state via `gh pr view --json statusCheckRollup`.
///
/// A **cancelled** check counts as [`Checks::Red`], not as "no result". That is exactly the shape a
/// merge-then-cancel loop produces: this repo cancels in-flight CI the moment a PR merges, so a run
/// that was killed proves only that nobody waited for it. Treating it as green would let the gate
/// agree with whatever raced it.
///
/// A failed `gh` call reads as [`Checks::Pending`] rather than green: an unreadable rollup is a
/// reason to wait and retry, never a reason to land.
pub(crate) fn pr_checks(slug: &str, selector: &str) -> Checks {
    #[derive(serde::Deserialize)]
    struct RollupView {
        #[serde(default, rename = "statusCheckRollup")]
        rollup: Vec<CheckRun>,
    }
    #[derive(serde::Deserialize)]
    struct CheckRun {
        /// Check runs: QUEUED / IN_PROGRESS / COMPLETED. Empty for a legacy commit status.
        #[serde(default)]
        status: String,
        /// SUCCESS / FAILURE / CANCELLED / SKIPPED / NEUTRAL / TIMED_OUT, once completed.
        #[serde(default)]
        conclusion: String,
        /// Legacy commit-status state (SUCCESS / PENDING / FAILURE) when `status` is empty.
        #[serde(default)]
        state: String,
    }
    let Some(view) = bn_core::runner::gh_json::<RollupView>(
        &bn_core::runner::RealRunner,
        &[
            "pr",
            "view",
            selector,
            "--repo",
            slug,
            "--json",
            "statusCheckRollup",
        ],
    ) else {
        return Checks::Pending;
    };
    rollup_verdict(
        view.rollup
            .iter()
            .map(|r| classify_check(&r.status, &r.conclusion, &r.state)),
    )
}

/// Fold per-check verdicts into one: any red is red (report the worst), else any pending is
/// pending, else green — and an empty rollup is [`Checks::None`].
fn rollup_verdict(checks: impl Iterator<Item = Checks>) -> Checks {
    let mut seen = false;
    let mut pending = false;
    for c in checks {
        seen = true;
        match c {
            Checks::Red => return Checks::Red,
            Checks::Pending => pending = true,
            _ => {}
        }
    }
    if !seen {
        Checks::None
    } else if pending {
        Checks::Pending
    } else {
        Checks::Green
    }
}

/// Poll [`pr_checks`] until CI settles: up to `attempts` tries, sleeping `delay` between them,
/// returning as soon as the rollup is no longer [`Checks::Pending`]. A run that never settles
/// returns `Pending` so the caller skips the PR — a poller that blocks forever on one PR stops
/// being a poller, and the next pass picks it up anyway.
pub(crate) fn wait_for_checks(slug: &str, selector: &str, attempts: u32, delay: Duration) -> Checks {
    let mut last = Checks::Pending;
    for i in 0..attempts.max(1) {
        last = pr_checks(slug, selector);
        if last != Checks::Pending {
            return last;
        }
        if i + 1 < attempts.max(1) {
            std::thread::sleep(delay);
        }
    }
    last
}

/// Whether the PR may land as far as GitHub CI is concerned, printing why when it may not.
///
/// `wait` bounds how long we sit on a still-running run. A human merge gets the full budget (the
/// answer is worth waiting for); the orchestrate poller passes `0` — one look, then move on — so a
/// single unfinished PR can't stall a tick that has other work to do.
pub(crate) fn ci_gate_open(slug: &str, selector: &str, label: &str, wait: Duration) -> bool {
    // ~One poll every 10s across the budget: infrequent enough not to hammer the API, frequent
    // enough that a run finishing mid-wait is noticed promptly. A zero budget is a single check.
    let delay = Duration::from_secs(10);
    let attempts = (wait.as_secs() / delay.as_secs()).max(1) as u32;
    match wait_for_checks(slug, selector, attempts, delay) {
        Checks::Green | Checks::None => true,
        Checks::Red => {
            ceprintln!(
                "{RED}Holding {label} — CI is red. Fix and re-push, or bypass with \
                 `--no-ci-gate`.{NC}"
            );
            false
        }
        Checks::Pending => {
            let waited = if wait.as_secs() == 0 {
                String::new()
            } else {
                format!(" after {}s", wait.as_secs())
            };
            ceprintln!(
                "{YELLOW}Holding {label} — CI hasn't finished{waited}; it'll land on a later \
                 pass.{NC}"
            );
            false
        }
    }
}

/// Poll `pr_mergeable` past GitHub's transient `UNKNOWN` recompute window: up to `attempts` tries,
/// sleeping `delay` between them, returning as soon as GitHub settles on `Yes`/`Conflicting`. If it
/// never settles, the last (still-`Unknown`) verdict is returned so the caller can decide.
pub(crate) fn wait_until_mergeable(
    slug: &str,
    number: i64,
    attempts: u32,
    delay: Duration,
) -> Mergeable {
    let mut last = Mergeable::Unknown;
    for i in 0..attempts.max(1) {
        last = pr_mergeable(slug, number);
        if last != Mergeable::Unknown {
            return last;
        }
        if i + 1 < attempts.max(1) {
            std::thread::sleep(delay);
        }
    }
    last
}

/// Group PRs by transitive changed-file overlap. Pure and deterministic (no I/O) so the
/// overlap→ordering logic is unit-testable. Input is `(branch, changed_files)` per PR; two PRs are
/// unioned when they share ≥1 file, and the relation is transitive (A–B share x, B–C share y ⇒
/// A,B,C one cluster). A singleton cluster is a PR whose files are disjoint from every other PR —
/// safe to merge in any order / batched. A multi-member cluster must be merged one at a time, with
/// a mergeability re-check between each. Branches within a cluster and the clusters themselves are
/// sorted for a stable, reproducible plan.
pub(crate) fn plan_merge_clusters(prs: &[(String, Vec<String>)]) -> Vec<Vec<String>> {
    let n = prs.len();
    // Union-find over PR indices.
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]]; // path halving
            i = parent[i];
        }
        i
    }
    // Map each file to the first PR index that touched it; union on collision.
    let mut owner: HashMap<&str, usize> = HashMap::new();
    for (i, (_, files)) in prs.iter().enumerate() {
        for f in files {
            match owner.get(f.as_str()) {
                Some(&j) => {
                    let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                    if ri != rj {
                        parent[ri] = rj;
                    }
                }
                None => {
                    owner.insert(f.as_str(), i);
                }
            }
        }
    }
    // Gather branches by cluster root.
    let mut groups: HashMap<usize, Vec<String>> = HashMap::new();
    for (i, (branch, _)) in prs.iter().enumerate() {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(branch.clone());
    }
    let mut clusters: Vec<Vec<String>> = groups
        .into_values()
        .map(|mut g| {
            g.sort();
            g
        })
        .collect();
    clusters.sort();
    clusters
}

/// Resolve a diff3-marked conflicted file by KEEPING BOTH SIDES — but only when every conflict
/// hunk is a pure insert/insert (empty base): both sides added lines at the same spot, nobody
/// rewrote an existing line. That's the parallel-dispatch signature — N PRs each adding a variant
/// to the same `Commands` enum (or an arm to its match). Any hunk with a non-empty base is a
/// genuine both-modified conflict, so the whole file returns `None` — too risky to auto-resolve.
/// Also `None` when there are no markers at all or they aren't diff3-style (the caller rebases
/// with `merge.conflictStyle=diff3`; an unmerged file without proper markers means something
/// non-textual — binary, delete/modify — is going on). Pure, so the union rule is unit-testable.
pub(crate) fn union_resolve_conflicts(text: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    let mut lines = text.lines();
    let mut resolved_any = false;
    while let Some(line) = lines.next() {
        if !line.starts_with("<<<<<<<") {
            out.push(line);
            continue;
        }
        // Ours (the side already on origin/main), up to the `|||||||` base marker. Hitting
        // `=======` first means merge-style markers (no base section) — refuse rather than guess.
        let mut ours: Vec<&str> = Vec::new();
        loop {
            let l = lines.next()?;
            if l.starts_with("|||||||") {
                break;
            }
            if l.starts_with("=======") || l.starts_with(">>>>>>>") {
                return None;
            }
            ours.push(l);
        }
        // Base section must be empty (`=======` immediately follows the base marker): any content
        // here means the sides rewrote existing lines — bail.
        if !lines.next()?.starts_with("=======") {
            return None;
        }
        // Theirs (the branch being rebased), up to the closing marker.
        let mut theirs: Vec<&str> = Vec::new();
        loop {
            let l = lines.next()?;
            if l.starts_with(">>>>>>>") {
                break;
            }
            theirs.push(l);
        }
        out.extend(ours);
        out.extend(theirs);
        resolved_any = true;
    }
    if !resolved_any {
        return None;
    }
    let mut s = out.join("\n");
    if text.ends_with('\n') {
        s.push('\n');
    }
    Some(s)
}

/// Try to rebase `branch` onto origin/main in a throwaway detached worktree off the bare repo,
/// resolving only pure insert/insert conflicts (see [`union_resolve_conflicts`]), then publish the
/// result with `push --force-with-lease`. This is the merge-queue step `cmd_merge_all` runs when a
/// serialized PR flips CONFLICTING after a sibling lands. Detached (`origin/<branch>`) so it never
/// collides with the branch being checked out in its dispatch worktree. On any bail the branch is
/// left untouched: the rebase is aborted and the temp worktree removed. Returns a short reason on
/// failure for the skip message.
pub(crate) fn auto_rebase_branch(bare: &Path, branch: &str) -> Result<(), String> {
    let bare_s = bare.to_string_lossy().to_string();
    if !run_git(&["--git-dir", &bare_s, "fetch", "origin"]) {
        return Err("git fetch failed".into());
    }
    let tmp = std::env::temp_dir().join(format!(
        "bn-merge-queue-{}-{}",
        std::process::id(),
        branch.replace(['/', '\\'], "-")
    ));
    let tmp_s = tmp.to_string_lossy().to_string();
    let origin_branch = format!("origin/{branch}");
    if !run_git(&[
        "--git-dir",
        &bare_s,
        "worktree",
        "add",
        "--detach",
        &tmp_s,
        &origin_branch,
    ]) {
        return Err("could not create temp worktree".into());
    }
    let result = rebase_keeping_insertions(&tmp_s, branch);
    // Always reclaim the temp worktree; --force also drops any half-done rebase state.
    let _ = run_git(&[
        "--git-dir",
        &bare_s,
        "worktree",
        "remove",
        "--force",
        &tmp_s,
    ]);
    result
}

/// Rebase the temp worktree's HEAD onto origin/main with diff3 markers, union-resolving every
/// conflicted file at each step or aborting the whole rebase. A step cap keeps a pathological
/// rebase from spinning forever. Does NOT push — the caller decides what to do with the rebased
/// head: the merge-queue ([`rebase_keeping_insertions`]) force-pushes it, the pre-land gate
/// ([`gate_head`]) verifies it. Neither should rewrite existing lines, hence the shared
/// insert/insert-only resolution.
pub(crate) fn rebase_onto_main_resolving(wt: &str) -> Result<(), String> {
    // diff3 markers are what union_resolve_conflicts requires; GIT_EDITOR=true keeps any
    // message-editing stop non-interactive.
    let rebase_step = |args: &[&str]| {
        let mut full = vec!["-C", wt, "-c", "merge.conflictStyle=diff3"];
        full.extend_from_slice(args);
        crate::git::git_command(&full)
            .env("GIT_EDITOR", "true")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    let abort = || {
        let _ = run_git(&["-C", wt, "rebase", "--abort"]);
    };
    let mut ok = rebase_step(&["rebase", "origin/main"]);
    let mut steps = 0;
    while !ok {
        steps += 1;
        if steps > 50 {
            abort();
            return Err("too many conflicted steps".into());
        }
        let unmerged =
            git_out(&["-C", wt, "diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
        let files: Vec<&str> = unmerged.lines().filter(|l| !l.is_empty()).collect();
        if files.is_empty() {
            abort();
            return Err("rebase stopped without unmerged files".into());
        }
        for f in &files {
            let path = Path::new(wt).join(f);
            let resolved = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| union_resolve_conflicts(&t));
            let Some(resolved) = resolved else {
                abort();
                return Err(format!("{f}: not a pure insert/insert conflict"));
            };
            if std::fs::write(&path, resolved).is_err() || !run_git(&["-C", wt, "add", "--", f]) {
                abort();
                return Err(format!("{f}: could not stage resolution"));
            }
        }
        ok = rebase_step(&["rebase", "--continue"]);
    }
    Ok(())
}

/// Rebase inside the temp worktree, then publish. The merge-queue step: rebase clean (keeping both
/// sides of pure insert/insert conflicts) then force-push the result to origin/<branch>.
pub(crate) fn rebase_keeping_insertions(wt: &str, branch: &str) -> Result<(), String> {
    rebase_onto_main_resolving(wt)?;
    // Rebased clean — publish. --force-with-lease keys off origin/<branch>, which we just fetched,
    // so a push that raced in from elsewhere is refused rather than clobbered.
    let refspec = format!("HEAD:refs/heads/{branch}");
    if !run_git(&["-C", wt, "push", "--force-with-lease", "origin", &refspec]) {
        return Err("push --force-with-lease refused".into());
    }
    Ok(())
}

/// Options for a `--all` batch landing, shared by the single-repo ([`cmd_merge_all`]) and cross-repo
/// ([`cmd_merge_all_repos`]) drivers so both agree on merge strategy + gating.
#[derive(Clone, Copy)]
pub(crate) struct MergeAllOpts {
    pub merge: bool,
    pub rebase: bool,
    pub no_delete_branch: bool,
    pub admin: bool,
    pub no_auto_rebase: bool,
    /// Gate each PR on THIS repo's own verify (#454) — rebase onto origin/main + run the repo's
    /// `[repo].verify`/`test`/`build` in a disposable worktree — before merging. On for the
    /// cross-repo rollup (a deliberate, safe, human-initiated land across repos); off for the
    /// single-repo `--all` fast path the supervisor lands green PRs with each tick.
    pub gate: bool,
    /// #607: enforce the adversarial diff-only review gate before landing each PR — a ship PR may
    /// not land until N independent diff-only reviewers approved its current head with every
    /// blocking finding resolved. Set from `[review].diff_gate` AND `!--no-gate` (the deliberate
    /// bypass), so an unconfigured land behaves exactly as before.
    pub review_gate: bool,
    /// Require the PR's **GitHub** checks to have finished green before landing it. The local
    /// `gate` proves the head builds on this machine; this proves the checks the repo actually
    /// publishes agreed — the thing a reviewer reads before approving. Without it a poller merges
    /// whatever is mergeable *now*, and since merging cancels in-flight CI, the run never gets to
    /// report: the checks stop being a gate and become a race the merge usually wins. `--no-ci-gate`
    /// is the deliberate bypass.
    pub ci_gate: bool,
    /// How long to sit on a still-running CI run before skipping the PR for a later pass.
    pub ci_wait: Duration,
}

/// Running tally of a `--all` landing pass, so the cross-repo rollup can sum per-repo outcomes.
#[derive(Default, Clone, Copy)]
pub(crate) struct LandTally {
    pub merged: usize,
    pub skipped: usize,
    pub failed: usize,
    pub rebased: usize,
    pub gated_out: usize,
}

impl LandTally {
    fn add(&mut self, o: LandTally) {
        self.merged += o.merged;
        self.skipped += o.skipped;
        self.failed += o.failed;
        self.rebased += o.rebased;
        self.gated_out += o.gated_out;
    }
}

pub(crate) fn cmd_merge_all(cfg: &Config, flags: MergeFlags) -> bool {
    let MergeFlags {
        merge,
        rebase,
        no_delete_branch,
        admin,
        no_auto_rebase,
        no_gate,
        ci,
    } = flags;
    let wt_path = cwd();
    let Some(bare) = find_bare_repo(&wt_path) else {
        ceprintln!("{RED}Not inside a worktree — run `bn wt merge --all` from within the repo{NC}");
        return false;
    };
    let slug = match remote_for(&bare) {
        Remote::GitHub { slug } => slug,
        _ => {
            ceprintln!(
                "{YELLOW}bn wt merge --all supports GitHub (gh). For ADO, complete PRs in the \
                 UI/az — the `cleanup --all` poller will reclaim the worktrees.{NC}"
            );
            return false;
        }
    };
    let repo = repo_name(&bare);
    // Single-repo `--all` keeps its historical fast path: no per-PR verify gate (orchestrate lands
    // green PRs cheaply each tick; the cross-repo rollup below is the gated, deliberate variant).
    // The adversarial diff-only review gate (#607) still applies here when `[review].diff_gate` is
    // on (this IS the ship-lane land path) — unless `--no-gate` opts out.
    let opts = MergeAllOpts {
        merge,
        rebase,
        no_delete_branch,
        admin,
        no_auto_rebase,
        gate: false,
        review_gate: cfg.review_diff_gate && !no_gate,
        ci_gate: ci.enabled,
        ci_wait: ci.wait,
    };
    let t = merge_all_in_repo(cfg, &bare, &slug, &repo, &opts);
    let rebased_note = if t.rebased > 0 {
        format!(" ({} auto-rebased)", t.rebased)
    } else {
        String::new()
    };
    cprintln!(
        "{GREEN}merge --all: {} merged{rebased_note}, {} skipped, {} failed.{NC}",
        t.merged, t.skipped, t.failed
    );
    t.failed == 0
}

/// Land every open PR of mine for `repo` (bare `bare`, GitHub `slug`) via the file-overlap-aware
/// merge-queue, returning the outcome tally. Runs entirely off the passed repo identity — never the
/// cwd — so it serves both the cwd-scoped `bn wt merge --all` and the cross-repo rollup. Prints
/// per-PR progress but leaves the summary line to the caller.
pub(crate) fn merge_all_in_repo(
    cfg: &Config,
    bare: &Path,
    slug: &str,
    repo: &str,
    opts: &MergeAllOpts,
) -> LandTally {
    let remote = Remote::GitHub {
        slug: slug.to_string(),
    };
    cprintln!("{BLUE}Fetching your open PRs for {slug}...{NC}");
    let prs = my_open_prs(&remote);
    if prs.is_empty() {
        cprintln!("{GREEN}No open PRs of yours to merge for {slug}.{NC}");
        return LandTally::default();
    }

    // Changed files per PR → overlap clusters (pure). Then merge cluster by cluster, re-checking
    // mergeability before EVERY merge (each merge advances the base, so even disjoint PRs get a
    // transient UNKNOWN — and overlapping ones may flip to CONFLICTING).
    let with_files: Vec<(String, Vec<String>)> = prs
        .iter()
        .map(|p| {
            (
                p.branch.clone(),
                pr_changed_files(slug, p.number, &p.branch),
            )
        })
        .collect();
    let clusters = plan_merge_clusters(&with_files);
    let by_branch: HashMap<&str, &RemotePr> = prs.iter().map(|p| (p.branch.as_str(), p)).collect();

    let serialized = clusters.iter().filter(|c| c.len() > 1).count();
    let batched = clusters.iter().filter(|c| c.len() == 1).count();
    cprintln!(
        "{BLUE}{} open PR(s): {batched} independent, {serialized} overlap cluster(s) to serialize.{NC}",
        prs.len()
    );

    let method = merge_method(opts.merge, opts.rebase);
    let (_, worktree_dir) = layout(cfg);
    let checked_out = checked_out_branches(&bare.to_string_lossy());
    let mut t = LandTally::default();

    for cluster in &clusters {
        let serialize = cluster.len() > 1;
        for branch in cluster {
            let Some(pr) = by_branch.get(branch.as_str()) else {
                continue;
            };
            // Re-check mergeability, polling past GitHub's UNKNOWN recompute window, before merging.
            // A CONFLICTING verdict gets one auto-rebase attempt (the mini merge-queue) before we
            // give up and skip — unless --no-auto-rebase opted out.
            let mut verdict = wait_until_mergeable(slug, pr.number, 8, Duration::from_secs(3));
            if verdict == Mergeable::Conflicting && !opts.no_auto_rebase {
                cprintln!(
                    "{BLUE}#{} [{branch}] conflicting — auto-rebasing onto origin/main...{NC}",
                    pr.number
                );
                match auto_rebase_branch(bare, branch) {
                    Ok(()) => {
                        t.rebased += 1;
                        verdict = wait_until_mergeable(slug, pr.number, 8, Duration::from_secs(3));
                    }
                    Err(reason) => {
                        ceprintln!(
                            "{YELLOW}Skipping #{} [{branch}] — auto-rebase bailed ({reason}); \
                             rebase it manually, then re-run.{NC}",
                            pr.number
                        );
                        t.skipped += 1;
                        continue;
                    }
                }
            }
            match verdict {
                Mergeable::Yes => {}
                Mergeable::Conflicting => {
                    ceprintln!(
                        "{YELLOW}Skipping #{} [{branch}] — conflicting; rebase it, then re-run.{NC}",
                        pr.number
                    );
                    t.skipped += 1;
                    continue;
                }
                Mergeable::Unknown => {
                    ceprintln!(
                        "{YELLOW}Skipping #{} [{branch}] — mergeability still UNKNOWN after polling; \
                         try again shortly.{NC}",
                        pr.number
                    );
                    t.skipped += 1;
                    continue;
                }
            }
            // #454/#455: gate the mergeable PR on THIS repo's own verify before landing it. Rebases
            // onto current origin/main in a disposable worktree and runs the repo's verify there; a
            // red or stale-base PR is skipped (never lands), a repo with no verify job isn't blocked.
            if opts.gate && !gate_pr_head(cfg, bare, repo, branch) {
                t.gated_out += 1;
                continue;
            }
            // #607: the adversarial diff-only review gate. When `[review].diff_gate` is on, a ship
            // PR may not land until N independent reviewers approved its *current* head — seeing the
            // diff only, never the author's reasoning — and every blocking finding was resolved or
            // waived. The verify gate proves it builds/passes; this proves an adversarial human/agent
            // pass found no unaddressed correctness defect. Held-back PRs are counted as gated-out.
            if opts.review_gate && !adversarial_gate_open(cfg, slug, repo, pr.number) {
                t.gated_out += 1;
                continue;
            }
            // The published CI verdict, checked last — after the cheap local gates have already
            // ruled the PR out, so we never spend the wait budget on a PR that can't land anyway.
            if opts.ci_gate
                && !ci_gate_open(
                    slug,
                    &pr.number.to_string(),
                    &format!("#{} [{branch}]", pr.number),
                    opts.ci_wait,
                )
            {
                t.gated_out += 1;
                continue;
            }
            let tag = if serialize { " (serialized)" } else { "" };
            cprintln!(
                "{BLUE}Merging #{} [{branch}] ({}){tag}{NC}",
                pr.number,
                &method[2..]
            );
            if !merge_pr_via_gh(slug, branch, method, !opts.no_delete_branch, opts.admin) {
                ceprintln!("{RED}gh pr merge failed for #{} [{branch}]{NC}", pr.number);
                t.failed += 1;
                continue;
            }
            t.merged += 1;
            // Best-effort local cleanup: reclaim the worktree if one exists for this branch. The
            // `cleanup --all` poller reclaims any we can't (e.g. dirty or dispatched elsewhere).
            if let Some(dir) = existing_worktree_for(&checked_out, &worktree_dir, repo, branch) {
                let _ = cleanup_one(cfg, Path::new(&dir), &worktree_dir, false);
            }
        }
    }
    t
}

/// Gate one PR head on its repo's own verify (#454) as part of a cross-repo landing: build the
/// repo's context, rebase `origin/<branch>` onto current origin/main in a disposable worktree, and
/// run the resolved verify target there. Returns whether the PR may land: `true` on a green gate OR
/// when the repo has no verify job (nothing to gate); `false` only on a real FAIL. The verdict is
/// echoed so the operator sees exactly why a PR was held back.
fn gate_pr_head(cfg: &Config, bare: &Path, repo: &str, branch: &str) -> bool {
    let ctx = match resolver::resolve_target(
        cfg,
        &home(),
        Some(repo.to_string()),
        Some(branch.to_string()),
    ) {
        Ok(c) => c,
        Err(_) => {
            ceprintln!(
                "{YELLOW}Skipping [{branch}] in {repo} — could not resolve repo context to gate.{NC}"
            );
            return false;
        }
    };
    let head_ref = format!("origin/{branch}");
    match gate_head(cfg, &ctx, bare, &head_ref, None, true, false) {
        GateOutcome::Ran(r) => {
            let color = if r.pass { GREEN } else { RED };
            cprintln!("{color}{} [{repo}/{branch}]{NC}", gate_summary(&r));
            if !r.pass {
                ceprintln!(
                    "{YELLOW}Holding {repo}/{branch} — gate failed. Fix + re-push, or land with \
                     `bn wt merge --all-repos --no-gate`.{NC}"
                );
            }
            r.pass
        }
        // No verify job for the repo — nothing to gate, so don't block the land.
        GateOutcome::NoJob => true,
    }
}

/// The PR's current head sha via `gh pr view --json headRefOid`. `None` if gh can't resolve it —
/// the caller treats an unknown head as "not approved yet" (fail-closed), so a resolution hiccup
/// can't sneak an unreviewed PR past the gate.
fn pr_head_sha(slug: &str, number: i64) -> Option<String> {
    crate::gh::gh_capture(&[
        "pr",
        "view",
        &number.to_string(),
        "--repo",
        slug,
        "--json",
        "headRefOid",
        "--jq",
        ".headRefOid",
    ])
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

/// #607: whether the adversarial diff-only review gate is open for one PR — enough independent
/// approvals at the *current* head sha AND no open blocking finding. Fail-closed: if the head can't
/// be resolved (so approvals can't be pinned to it), the gate is treated as blocked. `repo` is the
/// bare repo name the reviewer keyed the gate on (not the `owner/repo` slug). The verdict is echoed
/// so the operator sees exactly why a PR was held back.
fn adversarial_gate_open(cfg: &Config, slug: &str, repo: &str, number: i64) -> bool {
    let required = cfg.review_diff_gate_reviews;
    let Some(head) = pr_head_sha(slug, number) else {
        ceprintln!(
            "{YELLOW}Holding {repo}#{number} — diff-only review gate on but the PR head couldn't be \
             resolved to check approvals.{NC}"
        );
        return false;
    };
    let gs = bn_core::review_gate::gate_status(repo, number, &head, required);
    if gs.open {
        cprintln!("{GREEN}review-gate {repo}#{number}: {}{NC}", gs.summary());
    } else {
        ceprintln!(
            "{YELLOW}Holding {repo}#{number} — review-gate {}. Run diff-only reviewers \
             (`bn review-gate prompt {repo} {number}`) and resolve blocking findings, or land with \
             `bn wt merge --no-gate`.{NC}",
            gs.summary()
        );
    }
    gs.open
}

/// `bn wt merge --all-repos` (#455): land every open PR of mine across **every configured repo** in
/// one invocation — the cross-repo rollup that closes the "cd into each repo's worktree N times"
/// gap. Rolls through repos in a deterministic order ([`bn_core::crossrepo::cross_repo_land_order`]),
/// gating each PR on that repo's own verify (unless `no_gate`), and prints a per-repo section plus a
/// grand total. Non-GitHub repos are noted + skipped (gh-only, like `--all`).
pub(crate) fn cmd_merge_all_repos(cfg: &Config, flags: MergeFlags) -> bool {
    let MergeFlags {
        merge,
        rebase,
        no_delete_branch,
        admin,
        no_auto_rebase,
        no_gate,
        ci,
    } = flags;
    let (bare_dir, _) = layout(cfg);
    let names: Vec<String> = bare_repos(&bare_dir).iter().map(|b| repo_name(b)).collect();
    let order = bn_core::crossrepo::cross_repo_land_order(&names);
    if order.is_empty() {
        ceprintln!(
            "{YELLOW}bn wt merge --all-repos: no bare repos under {} — clone one first \
             (bn wt clone <url>).{NC}",
            bare_dir.display()
        );
        return true;
    }
    let opts = MergeAllOpts {
        merge,
        rebase,
        no_delete_branch,
        admin,
        no_auto_rebase,
        gate: !no_gate,
        review_gate: cfg.review_diff_gate && !no_gate,
        ci_gate: ci.enabled,
        ci_wait: ci.wait,
    };
    cprintln!(
        "{BLUE}merge --all-repos: rolling through {} repo(s): {}{NC}",
        order.len(),
        order.join(", ")
    );
    let mut total = LandTally::default();
    let mut repos_with_prs = 0usize;
    for repo in &order {
        let bare = bare_dir.join(format!("{repo}.git"));
        let slug = match remote_for(&bare) {
            Remote::GitHub { slug } => slug,
            _ => {
                cprintln!("{YELLOW}— {repo}: not a GitHub repo — skipping (complete ADO PRs in the UI/az).{NC}");
                continue;
            }
        };
        cprintln!("{BLUE}══ {repo} ({slug}) ══{NC}");
        let t = merge_all_in_repo(cfg, &bare, &slug, repo, &opts);
        if t.merged + t.skipped + t.failed + t.gated_out > 0 {
            repos_with_prs += 1;
            let gated_note = if t.gated_out > 0 {
                format!(", {} gated-out", t.gated_out)
            } else {
                String::new()
            };
            let rebased_note = if t.rebased > 0 {
                format!(" ({} auto-rebased)", t.rebased)
            } else {
                String::new()
            };
            cprintln!(
                "{GREEN}  {repo}: {} merged{rebased_note}, {} skipped{gated_note}, {} failed.{NC}",
                t.merged, t.skipped, t.failed
            );
        }
        total.add(t);
    }
    let gated_note = if total.gated_out > 0 {
        format!(", {} gated-out", total.gated_out)
    } else {
        String::new()
    };
    let rebased_note = if total.rebased > 0 {
        format!(" ({} auto-rebased)", total.rebased)
    } else {
        String::new()
    };
    cprintln!(
        "{GREEN}merge --all-repos: {} merged{rebased_note}, {} skipped{gated_note}, {} failed across {} repo(s).{NC}",
        total.merged, total.skipped, total.failed, repos_with_prs
    );
    total.failed == 0
}


