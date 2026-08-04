//! cwd → (repo, branch) → note path. Port of `resolve_note_context`
//! (config/scripts/tmux-lib.zsh:151) + `notes_dir_for` / work-repos classification (bn:101-116).
//!
//! Two derivation cases, kept distinct exactly as the oracle does:
//!   A. cwd under `$WORKTREE_DIR/` → repo = first path segment (from the *directory*),
//!      branch from git.
//!   B. otherwise → repo = basename(git toplevel), with a `.bare`-sibling override that
//!      uses the parent dir name; branch from git.
//! Then: repo lowercased, branch slashes→dashes; empty repo|branch ⇒ failure.

mod config;
mod config_file;
mod context;
mod dirs;
mod jobs;
mod paths;
mod tools_config;
mod util;

pub use config::{
    Config, ContextError, LogConfig, PipelineConfig, PipelineModel, RepoBranch, RepoConfig,
    RepoPlacementPolicy, Resolved, StageConfig, WebConfig,
};
pub use context::{repo_root, resolve, resolve_hint, resolve_target};
pub use dirs::{
    agents_dir, bin_dir, bn_home, config_dir, config_file_path, jobs_dir, log_dir,
    managed_repos_dir, references_dir, state_dir, web_token,
};
pub use jobs::{
    agent_dirs, job_dirs, job_script, list_jobs, mcp_extras, quarter_review_cron_override,
    JobConfig, JobRef, JobScope,
};
pub use tools_config::ToolsConfig;


