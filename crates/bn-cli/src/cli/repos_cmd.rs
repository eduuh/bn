//! `bn repos` — the repo fleet as one addressable thing.
//!
//! Every other bn command is scoped to the repo you are standing in, and the note layer is
//! already global (`bn list` spans every repo). What was missing is the *git* side: a way to ask
//! all twelve clones a question at once. `bn repos list` is the inventory; `bn repos foreach` is
//! the fan-out, and its `--json` array (one row per repo, each with its own exit code) is the
//! scripting surface — a shell loop over `bn repos list` cannot report per-repo failure without
//! reinventing it.
use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum ReposCmd {
    /// The inventory: every bare clone under the worktree layout's bare root, with its default
    /// branch, branch/worktree counts and last-commit age. The default when no subcommand is given.
    List,
    /// Run a command once per repo, in parallel, and report every repo's exit code.
    ///
    /// Runs in each repo's default-branch worktree (`<worktree_dir>/<repo>/main`), which every
    /// managed clone keeps; `--worktree` picks a different branch and `--bare` the git dir.
    ///
    /// Nothing is interpreted: the words after `foreach` are the argv, run directly (no shell).
    /// Pipe through `sh -c '…'` yourself when you want shell syntax.
    ///
    /// Note that `--json` is a global flag: it must come before `foreach`
    /// (`bn --json repos foreach git status`), since everything after `foreach` is the argv.
    #[command(trailing_var_arg = true, allow_hyphen_values = true)]
    Foreach {
        /// Run in the worktree checked out on this branch instead of the default-branch one; a
        /// repo with no such worktree is skipped (reported as skipped, never as a failure).
        #[arg(long, value_name = "BRANCH", conflicts_with = "bare")]
        worktree: Option<String>,
        /// Run in the bare git dir instead of a worktree. Plumbing (`fetch`, `log`,
        /// `for-each-ref`) works there; anything needing a work tree does not.
        #[arg(long)]
        bare: bool,
        /// Restrict to these repos by name (repeatable). Default: every cloned repo.
        #[arg(long, value_name = "NAME")]
        repo: Vec<String>,
        /// How many repos to run at once (default 6).
        #[arg(long, default_value_t = 6, value_name = "N")]
        jobs: usize,
        /// The command and its arguments.
        #[arg(required = true)]
        argv: Vec<String>,
    },
}
