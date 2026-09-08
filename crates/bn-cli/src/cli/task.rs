//! `bn task …` — the clap surface for the task store (#995, EPIC #993).
//!
//! Split out of `cli.rs` so the file-size ratchet leaves room for the capture flags (`--body`,
//! `--link`, `--attach`) and the `attach` subcommand to exist. Re-exported by `cli.rs`, so
//! `TaskCmd` resolves exactly as before.

use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum TaskCmd {
    /// List open tasks (default — same as bare `bn task`): today's work first, then unscheduled.
    List,
    /// Add a task: bn task add "<text>" [-p A] [--due YYYY-MM-DD] [--src S]. The text may inline
    /// +project, @context, and any `key:value` meta — so `bn task add "fix it repo:atlas day:
    /// 2026-08-03 est:2h"` sets those directly. `repo:` defaults to the repo you're standing in.
    Add {
        #[arg(required = true)]
        text: Vec<String>,
        /// Priority A | B | C (= severity urgent | warn | info).
        #[arg(short = 'p', long)]
        priority: Option<char>,
        /// Due date, YYYY-MM-DD.
        #[arg(long)]
        due: Option<String>,
        /// Who's adding it (default manual). Agents/cron pass their name.
        #[arg(long, default_value = "manual")]
        src: String,
        /// Everything you already know, as markdown, into the doc's `## Context`. `-` reads stdin,
        /// so a failing command's output can be piped straight into the task that fixes it.
        #[arg(long)]
        body: Option<String>,
        /// A URL this came from, into the doc's `## Links`. Repeatable.
        #[arg(long = "link")]
        links: Vec<String>,
        /// A file to capture with the task — a screenshot, a log, a spec. Repeatable. Copied into
        /// the notes tree beside the task's doc, so it travels with the task.
        #[arg(long = "attach")]
        attach: Vec<std::path::PathBuf>,
    },
    /// Bind a task to the worktree working it, so the fleet can show which task an agent is on:
    /// bn task claim <id> [--repo R] [--branch B]. Defaults to the repo/branch you're standing in,
    /// which is what an agent runs from inside its own checkout. Idempotent.
    Claim {
        id: String,
        /// Repo to bind to (default: the cwd's).
        #[arg(long)]
        repo: Option<String>,
        /// Branch to bind to (default: the cwd's).
        #[arg(long)]
        branch: Option<String>,
    },
    /// Mark a task done: bn task do <id>.
    Do { id: String },
    /// Mark a task in progress (@wip): bn task wip <id>.
    Wip { id: String },
    /// Hide a task until a date: bn task snooze <id> <YYYY-MM-DD>.
    Snooze { id: String, until: String },
    /// Close a task as cancelled (distinct from done): bn task cancel <id> [reason…].
    Cancel {
        id: String,
        #[arg(trailing_var_arg = true)]
        reason: Vec<String>,
    },
    /// Batch-add todos from stdin (JSON array/object or JSONL). Fields: text/title, priority
    /// (A|B|C) or severity, key, due, snooze, branch, src. Idempotent by key (namespaced by
    /// source); with --replace, open keyed todos from the source(s) absent from the batch close.
    Ingest {
        /// Default source for rows without `src` (usually the job/script name).
        #[arg(long, default_value = "script")]
        source: String,
        /// Default branch tag for rows without `branch` (else the current branch).
        #[arg(long)]
        branch: Option<String>,
        /// Treat stdin as the source's complete current set: mark done any open keyed todo from
        /// this source that's absent from the batch.
        #[arg(long)]
        replace: bool,
    },
    /// Open a task's context doc — the long form the one-line task can't hold: why it matters,
    /// the `## Done When` checklist the SMART gate reads, investigation findings, open questions.
    /// Created from a template on first use. Lives in the notes tree, beside the branch note once
    /// the task is picked up. Prints the path; `--open` launches $EDITOR.
    Doc {
        /// Task id (e.g. t42).
        id: String,
        /// Open it in $EDITOR instead of just printing the path.
        #[arg(long)]
        open: bool,
    },
    /// The files captured with a task — **this is how an agent resolves them**. The doc's
    /// `## Attachments` manifest lists names, never paths: a path is a property of the machine
    /// that wrote it, and the doc syncs everywhere and moves when the task is picked up. This
    /// prints where they actually are, here, now.
    ///
    /// `bn task attach t42` lists them; `--add` captures more; `--rm` drops one.
    Attach {
        /// Task id (e.g. t42).
        id: String,
        /// A file to capture with the task. Repeatable.
        #[arg(long = "add")]
        add: Vec<std::path::PathBuf>,
        /// The stored name of an attachment to remove. Repeatable.
        #[arg(long = "rm")]
        rm: Vec<String>,
    },
    /// Refill the task list from the GitHub-issue reservoir, bounded by a low-water mark: it tops
    /// each repo back up to N actionable tasks and no further, so the store stays readable.
    /// Idempotent — an issue already pulled is never minted twice. Pulled tasks carry no `smart:`
    /// verdict, so the gate routes them to the enhance agent before anything executes them.
    Pull {
        /// Tasks to keep actionable per repo (the low-water mark).
        #[arg(long, short = 'n', default_value_t = bn_core::keep_warm::DEFAULT_KEEP_WARM_LOW_WATER)]
        low_water: usize,
        /// Show what would be pulled and write nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Make a task executable: fill in why it matters, the `## Done When` the SMART gate reads,
    /// an estimate and a day. Renders the Markdown agent prompt with --dry-run. `--steer` passes
    /// your own "things to look out for" into its runtime packet.
    #[command(visible_alias = "improve")]
    Enhance {
        /// Task id (e.g. t42). Omit with --due to take the next task needing work.
        id: Option<String>,
        /// Things to look out for, passed to the agent verbatim.
        #[arg(long)]
        steer: Option<String>,
        /// Take the highest-ranked task that failed the gate, rather than a named one.
        #[arg(long)]
        due: bool,
        /// The agent manifest whose body is the prompt. The scheduled run passes its own name
        /// from its `run:` line, which is the only place that name should appear.
        #[arg(long, default_value = "task-enhance")]
        agent: String,
        /// Render the prompt instead of launching an agent.
        #[arg(long)]
        dry_run: bool,
    },
    /// Decide what is genuinely doable today: rank every open task deterministically, then let an
    /// agent reorder the shortlist and stamp `day:`. Writes its reasoning to the day's triage note.
    Triage {
        /// The day to triage for (default today).
        #[arg(long, value_name = "DATE")]
        day: Option<String>,
        /// The agent manifest whose body is the prompt. The scheduled run passes its own name
        /// from its `run:` line, which is the only place that name should appear.
        #[arg(long, default_value = "task-triage")]
        agent: String,
        /// Print the rendered prompt instead of launching an agent.
        #[arg(long)]
        dry_run: bool,
    },
    /// Absorb a legacy repo-local `TODO.txt` into the global store: bn task import <path>.
    /// Idempotent — re-running imports only what's new. Ids are reminted globally, and `repo:` is
    /// stamped from `--repo` or the source file's directory name. The original is left in place.
    Import {
        /// The legacy todo.txt file to read.
        path: std::path::PathBuf,
    },
}
