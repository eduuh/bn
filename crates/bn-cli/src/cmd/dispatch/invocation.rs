//! The canonical argv builder for agent-bearing `bn dispatch` invocations.
//!
//! Its own module because `dispatch.rs` is at the file-size ratchet's mercy: the tests that guard the
//! command shape belong beside the builder, not wedged into the file it is trying not to grow.

/// Canonical argv builder for agent-bearing `bn dispatch` invocations.
///
/// Orchestrator and SSH-host callers deliberately execute dispatch in a child process, but they
/// must not independently encode its flags: seed construction, trust, and launch behavior belong
/// to [`run_dispatch`]. This type keeps those callers on one command shape.
pub(crate) struct DispatchInvocation<'a> {
    item: &'a str,
    repo: &'a str,
    agent: &'a str,
    name: Option<&'a str>,
    task: Option<&'a str>,
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
            name: None,
            task: None,
            scout: false,
            verify: false,
            yolo: false,
            fresh: false,
            json: false,
        }
    }

    /// The fleet task name — how a running agent is recognised by the guards that stop a duplicate
    /// starting.
    pub(crate) fn name(mut self, name: Option<&'a str>) -> Self {
        self.name = name;
        self
    }

    /// Bind the launch to a task id (`--task`), so the fleet shows which task the agent is on.
    pub(crate) fn task(mut self, task: Option<&'a str>) -> Self {
        self.task = task.filter(|t| !t.trim().is_empty());
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
        if let Some(name) = self.name {
            args.extend(["--name".to_string(), name.to_string()]);
        }
        if self.scout {
            args.push("--scout".to_string());
        }
        if let Some(task) = self.task {
            args.extend(["--task".to_string(), task.to_string()]);
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
        // A machine-started launch must not park itself on the orchestrate queue when the pool is
        // full: nothing would pick it up. The cold fallback starts it in a fresh checkout instead.
        args.extend(["--on-pool-full".to_string(), "cold".to_string()]);
        if self.json {
            args.push("--json".to_string());
        }
        args
    }

    /// Run this `bn dispatch` as a child process and wait for it to return.
    ///
    /// The door for agents the machine starts on its own behalf: a cron's repair agent, keep-warm's
    /// brainstorm scouts, an attention item, a decomposed issue's children. A child rather than
    /// [`super::run_dispatch`] in-process, because dispatch exits the process on a refusal, and one
    /// refused launch must not take the caller's remaining work with it.
    ///
    /// `BN_WORKER_REPO` declares the target as the child's session, the same declaration the web
    /// dashboard and the orchestrator make, so the cross-repo session guard does not refuse work
    /// the machine chose for that repo.
    pub(crate) fn launch(&self) -> Result<(), String> {
        #[cfg(test)]
        {
            // The unit-test executable is not the `bn` CLI; re-entering it would re-run the suite.
            LAUNCHED.with(|launched| launched.borrow_mut().push(self.args()));
            Ok(())
        }
        #[cfg(not(test))]
        {
            let exe = std::env::current_exe()
                .map_err(|e| format!("cannot resolve the bn executable: {e}"))?;
            let status = std::process::Command::new(exe)
                .args(self.args())
                .env("BN_WORKER_REPO", self.repo)
                .stdin(std::process::Stdio::null())
                .status()
                .map_err(|e| format!("could not run bn dispatch: {e}"))?;
            if status.success() {
                Ok(())
            } else {
                Err(format!("bn dispatch exited {}", status.code().unwrap_or(-1)))
            }
        }
    }
}

#[cfg(test)]
thread_local! {
    static LAUNCHED: std::cell::RefCell<Vec<Vec<String>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The argv of every [`DispatchInvocation::launch`] this test thread made, drained.
#[cfg(test)]
pub(crate) fn take_launched() -> Vec<Vec<String>> {
    LAUNCHED.with(|launched| std::mem::take(&mut *launched.borrow_mut()))
}

#[cfg(test)]
mod invocation_tests {
    //! The argv builder is the last place an agent's identity can be lost: a caller can carry a
    //! name faithfully and the agent still runs as the wrong work if the command line drops it.
    use super::*;

    fn args_of(inv: DispatchInvocation<'_>) -> Vec<String> {
        inv.args()
    }

    fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        let at = args.iter().position(|a| a == flag)?;
        args.get(at + 1).map(String::as_str)
    }

    #[test]
    fn a_declared_name_and_task_reach_the_command_line() {
        let args = args_of(
            DispatchInvocation::new("set up nvim", "nvim", "copilot")
                .name(Some("repo-setup"))
                .task(Some("t12"))
                .yolo(true),
        );
        assert_eq!(flag_value(&args, "--name"), Some("repo-setup"));
        assert_eq!(flag_value(&args, "--task"), Some("t12"));
        assert!(args.iter().any(|a| a == "--yolo"));
    }

    #[test]
    fn an_undeclared_identity_adds_no_flags() {
        // A bare dispatch must be byte-identical to what it always was: dispatch derives the branch
        // and name from the item.
        let args = args_of(DispatchInvocation::new("338", "bn", "copilot"));
        assert_eq!(flag_value(&args, "--name"), None);
        assert_eq!(flag_value(&args, "--task"), None);
        assert!(!args.iter().any(|a| a == "--yolo"));
    }
}
