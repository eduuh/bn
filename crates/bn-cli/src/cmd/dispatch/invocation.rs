//! The canonical argv builder for agent-bearing `bn dispatch` invocations.
//!
//! Its own module because it is the last place a queued agent's identity can be lost, and because
//! `dispatch.rs` is at the file-size ratchet's mercy — the tests that guard the invariant belong
//! beside the builder, not wedged into the file it is trying not to grow.

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
    name: Option<&'a str>,
    skill: Option<&'a str>,
    scout: bool,
    verify: bool,
    yolo: bool,
    no_plan_review: bool,
    with_main: bool,
    stage: Option<bn_core::pipeline::stage::Stage>,
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
            name: None,
            skill: None,
            scout: false,
            verify: false,
            yolo: false,
            no_plan_review: false,
            with_main: false,
            stage: None,
            fresh: false,
            json: false,
        }
    }

    pub(crate) fn branch(mut self, branch: Option<&'a str>) -> Self {
        self.branch = branch;
        self
    }

    /// The fleet task name — how a running agent is recognised by the guards that stop a duplicate
    /// starting.
    pub(crate) fn name(mut self, name: Option<&'a str>) -> Self {
        self.name = name;
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

    /// Decline the plan-review gate — the stance a queued entry recorded, replayed verbatim.
    pub(crate) fn no_plan_review(mut self, no_plan_review: bool) -> Self {
        self.no_plan_review = no_plan_review;
        self
    }

    pub(crate) fn with_main(mut self, with_main: bool) -> Self {
        self.with_main = with_main;
        self
    }

    /// Declare the pipeline stage, so the launch resolves *that* stage's model/effort/harness rather
    /// than the one dispatch would infer from `--scout`/`--skill`.
    pub(crate) fn stage(mut self, stage: Option<bn_core::pipeline::stage::Stage>) -> Self {
        self.stage = stage;
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
        if let Some(name) = self.name {
            args.extend(["--name".to_string(), name.to_string()]);
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
        if self.no_plan_review {
            args.push("--no-plan-review".to_string());
        }
        if self.with_main {
            args.push("--with-main".to_string());
        }
        if let Some(stage) = self.stage {
            args.extend(["--stage".to_string(), stage.as_str().to_string()]);
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

#[cfg(test)]
mod invocation_tests {
    //! The argv builder is the last place a queued agent's identity can be lost. Everything
    //! upstream — the entry, the tick's plan — can carry a branch faithfully and the agent still
    //! runs as the wrong work if the command line does not say so.
    use super::*;

    fn args_of(inv: DispatchInvocation<'_>) -> Vec<String> {
        inv.args()
    }

    fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        let at = args.iter().position(|a| a == flag)?;
        args.get(at + 1).map(String::as_str)
    }

    #[test]
    fn a_declared_branch_and_name_reach_the_command_line() {
        let args = args_of(
            DispatchInvocation::new("set up nvim", "nvim", "copilot")
                .branch(Some("repo-setup"))
                .name(Some("repo-setup"))
                .yolo(true)
                .no_plan_review(true)
                .with_main(true),
        );
        assert_eq!(flag_value(&args, "--branch"), Some("repo-setup"));
        assert_eq!(flag_value(&args, "--name"), Some("repo-setup"));
        assert!(args.iter().any(|a| a == "--yolo"));
        // A queued entry that declined the gate must be relaunched having declined it: rebuilding
        // the command without this is how the tick would quietly reinstate a plan review.
        assert!(args.iter().any(|a| a == "--no-plan-review"));
        assert!(args.iter().any(|a| a == "--with-main"));
    }

    #[test]
    fn an_undeclared_identity_adds_no_flags() {
        // A bare dispatch must be byte-identical to what it always was: dispatch derives the branch
        // from the item, and an accidental empty `--branch` would be a very confusing way to fail.
        let args = args_of(DispatchInvocation::new("338", "bn", "copilot"));
        assert_eq!(flag_value(&args, "--branch"), None);
        assert_eq!(flag_value(&args, "--name"), None);
        assert!(!args.iter().any(|a| a == "--yolo"));
        assert!(!args.iter().any(|a| a == "--no-plan-review"));
        assert!(!args.iter().any(|a| a == "--with-main"));
    }
}
