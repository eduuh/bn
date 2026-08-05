//! The fresh-context critic (#649 Phase 3, per design r17 §1).
//!
//! A critic that shares the execution agent's conversation rationalizes the code it just
//! wrote. This module is the **leak-proof** heart of the critic stage: the seed builder
//! takes only `(diff, done_when, pr_url)` — it has *no parameter through which the approved
//! plan could leak* (r17 §1.3 guard) — and the verdict → next-stage decision that bounds the
//! execute↔critique loop. Everything here is pure and unit-tested; the CLI (`bn critic`)
//! cuts the fresh worktree, dispatches the `--fresh` lane, and wires the side effects.

/// Default bound on the execute↔critique loop before escalating to a human (r17 §1.4).
/// Not yet configurable — pipeline config is Phase 4/5; `bn critic` exposes it as a flag.
pub const DEFAULT_MAX_ITERATIONS: u32 = 3;

/// The critic's ruling on a diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every acceptance criterion is met — advance the task to shippable.
    Pass,
    /// At least one criterion is unmet / the diff is wrong — loop back to execution.
    Fail,
}

/// What the pipeline does next after a critic verdict (r17 §1.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CriticNext {
    /// PASS — advance the task to shippable / merge.
    Shippable,
    /// FAIL within budget — re-enter execution to fix the findings. `iteration` is the
    /// critique round just completed (1-based count of FAILs so far).
    ReExecute { iteration: u32 },
    /// FAIL with the iteration budget exhausted — raise a human-steer (`planning`) item so
    /// a person arbitrates instead of looping forever.
    Escalate,
}

/// Decide the next pipeline stage from a verdict and the critique round.
///
/// `iteration` is the 1-based count of FAIL verdicts *including this one* (so the first FAIL
/// is `1`). With `max_iterations = 3` the loop allows three critique rounds and then
/// escalates: FAIL@1 → ReExecute, FAIL@2 → ReExecute, FAIL@3 → Escalate. A `max_iterations`
/// of 0 escalates on the first FAIL (a degenerate "critic can't re-execute" config).
pub fn next_after_verdict(verdict: Verdict, iteration: u32, max_iterations: u32) -> CriticNext {
    match verdict {
        Verdict::Pass => CriticNext::Shippable,
        Verdict::Fail if iteration < max_iterations => CriticNext::ReExecute { iteration },
        Verdict::Fail => CriticNext::Escalate,
    }
}

/// The title of the `critique` attention item for a given FAIL round — the store-free carrier
/// of the iteration count (no attn-schema change). Read back by [`parse_iteration`].
pub fn critique_title(iteration: u32, max_iterations: u32) -> String {
    format!("Critique FAIL — iteration {iteration}/{max_iterations}")
}

/// Recover the iteration count from a live critique item's title (the inverse of
/// [`critique_title`]). Returns `0` when there is no prior/parseable critique item, so the
/// first FAIL computes `iteration = parse_iteration(..) + 1 = 1`.
pub fn parse_iteration(title: &str) -> u32 {
    // "... iteration N/M"
    title
        .rsplit("iteration")
        .next()
        .and_then(|tail| tail.trim().split('/').next())
        .and_then(|n| n.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

/// Build the critic's first-turn seed from **only** the diff under test, the acceptance
/// criteria (the note's `done-when` lines), and the PR url. This signature is the isolation
/// guard (r17 §1.3): there is deliberately no `plan` / `transcript` / `session` parameter, so
/// the approved plan text is structurally un-forwardable into the critic's context.
///
/// The seed frames a zero-context, adversarial review and closes by telling the critic to
/// render its ruling via `bn critic verdict`, which drives the loop-back.
pub fn critic_seed(diff: &str, done_when: &[String], pr_url: Option<&str>) -> String {
    let mut s = String::new();
    s.push_str(
        "You are a FRESH-CONTEXT CRITIC. You have NO context on why this change was built and \
         you have NOT seen the plan or the author's reasoning. Do NOT assume the author was \
         right. Your job is to rigorously and independently critique the diff below: check \
         correctness, tests, edge cases, and error handling, and verify that EACH acceptance \
         criterion is actually met by the code — reproduce and RUN the tests, don't take the \
         author's word. Be adversarial; look for what is missing or wrong.\n\n",
    );

    if let Some(url) = pr_url {
        s.push_str(&format!("PR under review: {url}\n\n"));
    }

    s.push_str("## Acceptance criteria (done-when)\n");
    if done_when.is_empty() {
        s.push_str(
            "(none recorded on the note — critique the diff for correctness, tests, and edge \
             cases on its own terms)\n",
        );
    } else {
        for (i, c) in done_when.iter().enumerate() {
            s.push_str(&format!("{}. {}\n", i + 1, c.trim()));
        }
    }

    s.push_str("\n## Diff under test\n```diff\n");
    s.push_str(diff.trim_end());
    s.push_str("\n```\n\n");

    s.push_str(
        "When you have finished reviewing, render your ruling so the pipeline can act on it:\n\
         - If every criterion is met and the diff is correct: run `bn critic verdict --pass`.\n\
         - Otherwise: write your concrete findings to a file and run \
         `bn critic verdict --fail --findings-file <path>` (or `--findings \"<text>\"`). List \
         each unmet criterion and each defect specifically enough that the implementer can fix \
         it without guessing.\n",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_advances_to_shippable() {
        assert_eq!(
            next_after_verdict(Verdict::Pass, 1, DEFAULT_MAX_ITERATIONS),
            CriticNext::Shippable
        );
    }

    #[test]
    fn fail_within_budget_re_executes() {
        assert_eq!(
            next_after_verdict(Verdict::Fail, 1, 3),
            CriticNext::ReExecute { iteration: 1 }
        );
        assert_eq!(
            next_after_verdict(Verdict::Fail, 2, 3),
            CriticNext::ReExecute { iteration: 2 }
        );
    }

    #[test]
    fn fail_at_budget_escalates() {
        assert_eq!(
            next_after_verdict(Verdict::Fail, 3, 3),
            CriticNext::Escalate
        );
        // A zero budget escalates on the very first FAIL.
        assert_eq!(
            next_after_verdict(Verdict::Fail, 1, 0),
            CriticNext::Escalate
        );
    }

    #[test]
    fn iteration_title_round_trips() {
        let t = critique_title(2, 3);
        assert_eq!(parse_iteration(&t), 2);
        // No prior item → 0, so the first FAIL computes iteration 1.
        assert_eq!(parse_iteration("done: bn/x — shipped"), 0);
        assert_eq!(parse_iteration(""), 0);
    }

    #[test]
    fn seed_carries_diff_criteria_and_pr() {
        let seed = critic_seed(
            "diff --git a/x b/x\n+added line",
            &["green CI".into(), "no clippy warnings".into()],
            Some("https://github.com/eduuh/bn/pull/999"),
        );
        assert!(seed.contains("+added line"));
        assert!(seed.contains("green CI"));
        assert!(seed.contains("no clippy warnings"));
        assert!(seed.contains("pull/999"));
        assert!(seed.contains("bn critic verdict --pass"));
        assert!(seed.contains("--findings-file"));
    }

    #[test]
    fn seed_is_leak_proof_by_signature() {
        // The plan text lives in the note's `## Plan`, never a parameter here — so it cannot
        // appear in the seed. Prove the builder never echoes an unrelated plan-shaped string
        // (it only has diff/done-when/pr to work with).
        let plan = "SECRET-PLAN-STEP: refactor the auth module first";
        let seed = critic_seed("diff --git a/y b/y\n+y", &["works".into()], None);
        // The only text that reaches the seed is the diff + done-when + pr; a plan handed to
        // any other stage can never appear here because it isn't an argument.
        assert!(!seed.contains(plan));
    }

    #[test]
    fn seed_without_criteria_is_still_valid() {
        let seed = critic_seed("diff --git a/z b/z", &[], None);
        assert!(seed.contains("none recorded"));
        assert!(seed.contains("bn critic verdict"));
    }
}
