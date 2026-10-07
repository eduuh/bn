//! The no-mistake pre-land gate: `bn gate` and the [`gate_head`] core `bn wt merge` blocks on.
//! Split out of [`super::merge`] under the never-grow ratchet.

use super::*;

/// Outcome of the pre-land gate.
pub(crate) enum GateOutcome {
    /// The repo defines no `test`/`build` job — there is nothing to verify, so the gate does not
    /// block (a repo without a gate isn't held back).
    NoJob,
    /// The gate ran (or tried to). Fail-closed: any setup/rebase/verify failure yields
    /// `pass=false` with a `reason`, so a gate that could not prove green blocks the land.
    Ran(GateResult),
}

/// The verdict of a gate run: rebased-onto-current-main + the verify result on that rebased head.
pub(crate) struct GateResult {
    pub(crate) pass: bool,
    pub(crate) job: String,
    pub(crate) code: i32,
    pub(crate) base_short: String,
    pub(crate) rebased_short: String,
    pub(crate) behind: bool,
    pub(crate) reason: String,
}

/// First 12 chars of a sha — git's default short length for large repos. Pure.
pub(crate) fn short_sha(sha: &str) -> String {
    sha.chars().take(12).collect()
}

/// The one-line human verdict for a gate run. Pure, so the rendering branches are unit-tested.
pub(crate) fn gate_summary(r: &GateResult) -> String {
    let base = if r.behind {
        format!(
            "rebased onto origin/main@{} (base had advanced)",
            r.base_short
        )
    } else {
        format!("current with origin/main@{}", r.base_short)
    };
    if r.pass {
        format!(
            "gate {}: PASS ✓ — {base}, verify green on {}",
            r.job, r.rebased_short
        )
    } else if !r.reason.is_empty() {
        format!("gate {}: FAIL ✗ — {}", r.job, r.reason)
    } else {
        format!(
            "gate {}: FAIL ✗ — {base}, verify red (exit {}) on {}",
            r.job, r.code, r.rebased_short
        )
    }
}

/// Build a fail-closed `Ran` result before verify could run (setup/rebase failure).
pub(crate) fn gate_fail(
    job: &str,
    reason: String,
    base_short: String,
    behind: bool,
) -> GateOutcome {
    GateOutcome::Ran(GateResult {
        pass: false,
        job: job.to_string(),
        code: 1,
        base_short,
        rebased_short: String::new(),
        behind,
        reason,
    })
}

/// The pre-land gate core, shared by `bn gate` and the `bn wt merge` block. Rebase `head_ref` onto
/// a freshly-fetched `origin/main` in a **disposable** worktree, then run the repo's verification
/// gate on THAT rebased head. The caller's own worktree is never touched. This is
/// what kills the stale-base class: the suite runs against *current* main + the branch's changes,
/// not an old checkout. Fail-closed — any fetch/rebase/verify failure returns `Ran { pass: false }`
/// so a gate that cannot prove green blocks the land; `NoJob` is the sole non-blocking outcome.
pub(crate) fn gate_head(
    cfg: &Config,
    ctx: &resolver::Resolved,
    bare: &Path,
    head_ref: &str,
    job: Option<&str>,
    stream: bool,
) -> GateOutcome {
    gate_head_to(cfg, ctx, bare, head_ref, job, stream, None)
}

/// [`gate_head`], teeing the verify steps' output into `log` when given (see
/// [`crate::cmd::verify::run_verify_gate_to`]).
#[allow(clippy::too_many_arguments)]
pub(crate) fn gate_head_to(
    cfg: &Config,
    ctx: &resolver::Resolved,
    bare: &Path,
    head_ref: &str,
    job: Option<&str>,
    stream: bool,
    log: Option<&crate::cmd::verify::GateLog>,
) -> GateOutcome {
    // Resolve what to gate on first (#454, #758): an explicit --job, else the repo's native
    // `[repo].verify` command, else all available build/test/lint jobs. A repo with none has nothing
    // to gate (`NoJob`); an explicit --job that doesn't resolve is a hard fail (fail-closed).
    let target = match crate::resolve_verify_target(cfg, ctx, job) {
        Ok(Some(t)) => t,
        Ok(None) => return GateOutcome::NoJob,
        Err(reason) => return gate_fail(job.unwrap_or("verify"), reason, String::new(), false),
    };
    let job_name = target.label();

    let bare_s = bare.to_string_lossy().to_string();
    // Fresh fetch so origin/main + head_ref are current — the whole point of the gate. All bare-repo
    // ops go through `--git-dir` (not `-C <bare>`), which git refuses under `safe.bareRepository`.
    if !run_git(&["--git-dir", &bare_s, "fetch", "origin"]) {
        return gate_fail(
            &job_name,
            "git fetch origin failed".into(),
            String::new(),
            false,
        );
    }
    let Some(base_sha) = git_out(&["--git-dir", &bare_s, "rev-parse", "origin/main"]) else {
        return gate_fail(
            &job_name,
            "no origin/main to rebase onto".into(),
            String::new(),
            false,
        );
    };
    let base_short = short_sha(&base_sha);
    let Some(head_sha) = git_out(&["--git-dir", &bare_s, "rev-parse", head_ref]) else {
        return gate_fail(
            &job_name,
            format!("cannot resolve {head_ref} — push the branch first?"),
            base_short,
            false,
        );
    };
    // Behind = the head's merge-base with main isn't main's tip → main advanced under it. Exactly
    // the stale-base case the gate exists to catch.
    let behind = git_out(&["--git-dir", &bare_s, "merge-base", &head_sha, &base_sha])
        .map(|mb| mb != base_sha)
        .unwrap_or(true);

    // Disposable detached worktree off the head — the caller's worktree is never touched.
    let tmp = std::env::temp_dir().join(format!("bn-gate-{}-{}", std::process::id(), ctx.repo));
    let tmp_s = tmp.to_string_lossy().to_string();
    if !run_git(&[
        "--git-dir",
        &bare_s,
        "worktree",
        "add",
        "--detach",
        &tmp_s,
        &head_sha,
    ]) {
        return gate_fail(
            &job_name,
            "could not create disposable worktree".into(),
            base_short,
            behind,
        );
    }
    let cleanup_wt = || {
        let _ = run_git(&[
            "--git-dir",
            &bare_s,
            "worktree",
            "remove",
            "--force",
            &tmp_s,
        ]);
    };

    if let Err(reason) = rebase_onto_main_resolving(&tmp_s) {
        cleanup_wt();
        return gate_fail(
            &job_name,
            format!("rebase onto origin/main: {reason} — rebase manually (escalate)"),
            base_short,
            behind,
        );
    }
    let rebased_short = git_out(&["-C", &tmp_s, "rev-parse", "HEAD"])
        .map(|s| short_sha(&s))
        .unwrap_or_default();

    // Run the ordered verify gate on the rebased head, in the disposable worktree, with a shared,
    // repo-keyed CARGO_TARGET_DIR so a warm gate skips a full cold rebuild.
    let target_dir = std::env::temp_dir().join(format!("bn-gate-target-{}", ctx.repo));
    let code =
        crate::cmd::verify::run_verify_gate_to(&target, &tmp, stream, Some(&target_dir), log)
            .unwrap_or(1);
    cleanup_wt();

    GateOutcome::Ran(GateResult {
        pass: code == 0,
        job: job_name,
        code,
        base_short,
        rebased_short,
        behind,
        reason: String::new(),
    })
}

/// `bn gate` — run the pre-land gate on THIS worktree's HEAD and report the verdict, recording it
/// to the note's Progress. Returns whether the gate is green; the caller maps that to the process
/// exit code, so an agent can gate "done" on it and never report done on a stale-base verify.
pub(crate) fn cmd_gate(
    cfg: &Config,
    job: Option<String>,
    as_json: bool,
    for_: &[String],
) -> bool {
    let wt_path = cwd();
    let wt_s = wt_path.to_string_lossy().to_string();
    let Some(bare) = find_bare_repo(&wt_path) else {
        ceprintln!("{RED}bn gate: not inside a worktree{NC}");
        return false;
    };
    let Some(branch) = git_out(&["-C", &wt_s, "branch", "--show-current"]) else {
        ceprintln!("{RED}bn gate: could not determine current branch{NC}");
        return false;
    };
    if bn_core::agent::is_reserved_branch(&branch) {
        ceprintln!("{RED}bn gate: refusing to gate '{branch}' — gate a feature branch{NC}");
        return false;
    }
    let Ok(ctx) = resolver::resolve(cfg, &wt_path) else {
        ceprintln!("{RED}bn gate: not in a git repo{NC}");
        return false;
    };
    let Some(head_sha) = git_out(&["-C", &wt_s, "rev-parse", "HEAD"]) else {
        ceprintln!("{RED}bn gate: could not resolve HEAD{NC}");
        return false;
    };
    let log = crate::cmd::verify::GateLog::default();
    match gate_head_to(
        cfg,
        &ctx,
        &bare,
        &head_sha,
        job.as_deref(),
        !as_json,
        Some(&log),
    ) {
        GateOutcome::NoJob => {
            if as_json {
                let data =
                    serde_json::json!({ "pass": true, "skipped": "no-job", "repo": ctx.repo });
                println!(
                    "{}",
                    bn_core::render::Envelope::success("gate", data).to_json()
                );
            } else {
                cprintln!(
                    "{YELLOW}gate: no build/test/lint job for {} — nothing to gate{NC}",
                    ctx.repo
                );
            }
            true
        }
        GateOutcome::Ran(r) => {
            // The proof is for the branch head that was gated; the rebase onto main is the
            // method, recorded in the summary, not the commit the evidence speaks for.
            let dirty = git_out(&["-C", &wt_s, "status", "--porcelain"])
                .is_some_and(|s| !s.trim().is_empty());
            let proof = crate::cmd::proof::record_run(
                cfg,
                &ctx,
                crate::cmd::proof::Run {
                    kind: bn_core::proof::Kind::Gate,
                    job: &r.job,
                    code: r.code,
                    log: &log,
                    sha: Some(head_sha.trim().to_string()),
                    dirty,
                    for_,
                },
            );
            let bullet = format!(
                "{} — {}{}",
                bn_core::util::now_stamp(),
                gate_summary(&r),
                proof.as_deref().map(|p| format!(" → proof {p}")).unwrap_or_default()
            );
            let _ = bn_core::commands::append_body(cfg, &ctx, "progress", &bullet, &today());
            if as_json {
                let data = serde_json::json!({
                    "job": r.job,
                    "pass": r.pass,
                    "code": r.code,
                    "base": r.base_short,
                    "rebased_head": r.rebased_short,
                    "behind": r.behind,
                    "reason": r.reason,
                    "proof": proof,
                });
                println!(
                    "{}",
                    bn_core::render::Envelope::success("gate", data).to_json()
                );
            } else {
                let color = if r.pass { GREEN } else { RED };
                cprintln!("{color}{}{NC}", gate_summary(&r));
            }
            r.pass
        }
    }
}
