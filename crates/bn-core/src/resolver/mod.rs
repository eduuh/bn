//! cwd → (repo, branch) → note path. Port of `resolve_note_context`
//! (config/scripts/tmux-lib.zsh:151) + `notes_dir_for` / work classification (bn:101-116).
//!
//! Two derivation cases, kept distinct exactly as the oracle does:
//!   A. cwd under `$WORKTREE_DIR/` → repo = first path segment (from the *directory*),
//!      branch from git.
//!   B. otherwise → repo = basename(git toplevel), with a `.bare`-sibling override that
//!      uses the parent dir name; branch from git.
//! Then: repo lowercased, branch slashes→dashes; empty repo|branch ⇒ failure.

mod config;
mod config_file;
mod config_sections;
mod context;
mod dirs;
mod jobs;
mod machine_config;
mod paths;
mod tools_config;
mod util;

pub use config::{
    Config, ContextError, LogConfig, MachineProfile, PipelineConfig, PipelineModel, RepoBranch,
    RepoConfig, RepoPlacementPolicy, Resolved, StageConfig,
};
pub(crate) use config_file::merge_tables;
pub use context::{repo_root, resolve, resolve_hint, resolve_target};
pub use dirs::{
    agents_dir, approvals_file_path, bin_dir, bn_home, config_dir, config_layer_paths, config_root,
    jobs_dir, link_tags, link_tags_with_source, log_dir, machine_config_path, machine_file_path,
    machine_name, managed_repos_dir, notes_roots, references_dir, shared_config_path,
    shipped_config, shipped_root, state_dir, worktree_dir, TagSource,
};
pub use jobs::{
    agent_dirs, job_dirs, job_script, list_jobs, mcp_extras, mcp_user_servers, parse_script_header,
    script_header, JobConfig, JobRef, JobScope, ScriptHeader, SCRIPT_HEADER_LINES,
};
pub use tools_config::ToolsConfig;
