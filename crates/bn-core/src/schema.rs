//! The published shape of bn's `--json` output: the envelope contract, and what each
//! script-facing command puts in `data`.
//!
//! This exists because a consistent envelope is not the same as a *promised* one. Every command
//! already emitted `{ok, command, data, error}`, but only by convention — nothing said so, nothing
//! checked it, and a script had no way to tell a renamed field from an absent one. `bn schema`
//! publishes the contract and [`ENVELOPE_VERSION`](crate::render::ENVELOPE_VERSION) versions it.
//!
//! The registry is deliberately curated rather than generated. bn emits well over a hundred
//! command tags, most of them write paths whose `data` is an acknowledgement nobody scripts
//! against; listing all of them would be a wall of noise that rots. What is here is the read side
//! — the commands you would actually build a dashboard or a cron on — and each entry is verified
//! against the live command by `schema_matches_reality` in the CLI's test suite.

/// One command's `data` contract.
pub struct CommandSchema {
    /// The envelope's `command` tag.
    pub tag: &'static str,
    /// The CLI invocation that produces it.
    pub invocation: &'static str,
    /// What the command answers.
    pub summary: &'static str,
    /// The keys `data` carries, each as `name: type — meaning`.
    pub fields: &'static [&'static str],
}

/// The envelope's own fields, in the order they serialize.
pub const ENVELOPE_FIELDS: &[&str] = &[
    "v: number — the contract version; reject an envelope whose `v` you do not understand",
    "ok: bool — whether the command succeeded",
    "command: string — the command tag, one of the `tag`s below",
    "data: object|null — the command's payload; null on failure",
    "error: object|null — {code, message} on failure, null on success",
];

/// The script-facing read commands and their `data` shapes.
pub const COMMANDS: &[CommandSchema] = &[
    CommandSchema {
        tag: "verify",
        invocation: "bn verify --dry-run --json",
        summary: "the resolved verification gate without executing it or recording a result",
        fields: &[
            "job: string — the gate label, shared by previews and executed runs",
            "dry_run: bool — true for a preview; absent for an executed run",
            "cwd: string — the execution directory; preview only",
            "steps: array — resolved targets in execution order; preview only",
            "steps[].kind: string — command | script",
            "steps[].label: string — the target label",
            "steps[].command: string — configured shell command; command targets only",
            "steps[].path: string — resolved job script path; script targets only",
            "pass: bool — whether verification passed; executed runs only",
            "code: number — verification exit code; executed runs only",
            "proof: string|null — captured proof id; executed runs only",
        ],
    },
    CommandSchema {
        tag: "discover",
        invocation: "bn discover --json [task...]",
        summary: "read-only branch context and ranked workflow suggestions for a task",
        fields: &[
            "task: string — the trimmed task text, empty when omitted",
            "context: object|null — current task/execution orientation, null outside a repo or detached HEAD",
            "context.repo: string — the repo key when context exists",
            "context.branch: string — the branch name when context exists",
            "context.goal: string|null — compact goal text, null when unset",
            "context.task_id: string|null — governing task id",
            "context.doc: string|null — task-owned context document path",
            "context.stage: string|null — latest recorded pipeline stage",
            "context.plan: object — current plan artifacts and execution status",
            "workflows: array — ranked matching workflows, up to three",
            "workflows[].name: string — workflow or skill name",
            "workflows[].description: string — compact description",
            "workflows[].when: string|null — compact guidance on when to use it",
            "workflows[].scope: string — source scope such as repo-shared or global-shared",
            "workflows[].score: number|null — match score from workflow ranking",
            "workflows[].source: string — absolute source path",
            "workflows[].invoke: object — harness-specific invocations",
            "workflows[].invoke.copilot: string|null — Copilot invocation when installed",
            "workflows[].invoke.claude: string|null — Claude invocation when installed",
            "workflows[].read_command: string — command to inspect the full workflow body",
            "workflows_truncated: bool — true when more than three workflows matched",
            "workflows_filtered: bool — true when low-scoring lexical hits were omitted",
            "commands: array — suggested next commands, already shell-quoted",
        ],
    },
    CommandSchema {
        tag: "worktrees",
        invocation: "bn worktrees --json",
        summary: "task scopes joined to their on-disk worktree dirs",
        fields: &[
            "worktrees: array — `list`'s row shape plus:",
            "worktrees[].worktree: string|null — the checkout dir, null when it is gone",
        ],
    },
    CommandSchema {
        tag: "repos",
        invocation: "bn repos list --json",
        summary: "every cloned repo under the bare root",
        fields: &[
            "dir: string — the bare root the fleet was read from",
            "repos: array — one row per clone",
            "repos[].repo: string — the repo name",
            "repos[].git_dir: string — absolute path to <name>.git",
            "repos[].default_branch: string — the bare repo's HEAD",
            "repos[].branches: number — local branch count",
            "repos[].worktrees: number — registered worktree count",
            "repos[].last_commit: string — relative age of the default branch's tip",
        ],
    },
    CommandSchema {
        tag: "repos-foreach",
        invocation: "bn --json repos foreach <argv...>",
        summary: "one command's result in every repo (note: --json precedes `foreach`)",
        fields: &[
            "repos: array — one row per repo, sorted by name",
            "repos[].repo: string — the repo name",
            "repos[].dir: string — where the command actually ran",
            "repos[].exit: number|null — the exit code, null when skipped",
            "repos[].ok: bool — exit == 0",
            "repos[].skipped: bool — no worktree on the requested branch",
            "repos[].stdout: string — captured stdout, trailing newline trimmed",
            "repos[].stderr: string — captured stderr, trailing newline trimmed",
            "failed: number — repos that ran and exited non-zero",
            "skipped: number — repos that never ran",
        ],
    },
    CommandSchema {
        tag: "wt-list",
        invocation: "bn wt list --json",
        summary: "git worktrees for the current repo, or every repo when outside one",
        fields: &[
            "repos: array — one row per repo",
            "repos[].repo: string — the repo name",
            "repos[].git_dir: string — absolute path to <name>.git",
            "repos[].worktrees: array — parsed `git worktree list --porcelain` rows",
            "repos[].worktrees[].path: string — the checkout dir",
            "repos[].worktrees[].head: string — the commit sha, empty for the bare entry",
            "repos[].worktrees[].branch: string|null — null when bare or detached",
            "repos[].worktrees[].bare: bool — this is the bare repo itself",
            "repos[].worktrees[].detached: bool — checked out on no branch (a pool slot)",
        ],
    },
    CommandSchema {
        tag: "links",
        invocation: "bn links --json",
        summary:
            "the stored links, in picker order (bare `bn links --json` lists; it does not open)",
        fields: &[
            "items: array — one row per link",
            "items[].id: string — the short id `bn links rm` takes",
            "items[].name: string — the display name",
            "items[].url: string — the link",
            "items[].repo: string|null — the owning repo, null when repo-agnostic",
            "items[].tags: array — tag strings",
            "items[].created: string — YYYY-MM-DD",
            "items[].last: string|null — last-used date, null when never used",
            "items[].uses: number — use count",
        ],
    },
    CommandSchema {
        tag: "findings-sources",
        invocation: "bn findings sources --json",
        summary: "the sources the findings store holds history for",
        fields: &["sources: array — source name strings"],
    },
    CommandSchema {
        tag: "harness-models",
        invocation: "bn harness models --json",
        summary: "what each agent harness accepts, cached for a day",
        fields: &[
            "harnesses: array — one row per harness",
            "harnesses[].harness: string — copilot | claude",
            "harnesses[].models: array — accepted model ids",
            "harnesses[].efforts: array — accepted reasoning-effort levels",
            "harnesses[].discovered: bool — read from the harness, not the fallback list",
            "harnesses[].fetched_at: number — unix seconds",
        ],
    },
];

/// The entry for `tag`, if it is registered.
pub fn find(tag: &str) -> Option<&'static CommandSchema> {
    COMMANDS.iter().find(|c| c.tag == tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tag_is_unique() {
        let mut tags: Vec<&str> = COMMANDS.iter().map(|c| c.tag).collect();
        let before = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), before, "duplicate tag in the schema registry");
    }

    #[test]
    fn every_entry_documents_at_least_one_field() {
        for c in COMMANDS {
            assert!(!c.fields.is_empty(), "{} documents no fields", c.tag);
            assert!(!c.summary.is_empty(), "{} has no summary", c.tag);
            assert!(
                c.invocation.starts_with("bn "),
                "{} invocation should be a bn command line",
                c.tag
            );
        }
    }

    /// Each field reads `name: type — meaning`, so a consumer can split on the first colon.
    #[test]
    fn every_field_is_name_type_meaning() {
        for c in COMMANDS {
            for f in c.fields {
                assert!(f.contains('—'), "{}: {f:?} has no name/meaning dash", c.tag);
                assert!(f.contains(':'), "{}: {f:?} has no name/type colon", c.tag);
            }
        }
    }

    #[test]
    fn find_resolves_a_registered_tag_and_rejects_an_unknown_one() {
        assert!(find("goal").is_none());
        assert_eq!(find("discover").map(|c| c.tag), Some("discover"));
        assert!(find("definitely-not-a-command").is_none());
    }
}
