//! Scout configuration — the `[scout]` table of `config.toml`, resolved into [`Config::scout`].
//!
//! The Scout's collectors (the deterministic gather half behind `bn staff gather scout`) were
//! removed along with the machine queue that ran them. The config table survives so an existing
//! `config.toml` still parses; nothing reads it yet.

/// The Scout's own configuration, from the `[scout]` table of `config.toml`.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoutConfig {
    /// Public `owner/repo` watchlist — the upstreams to scan for help-wanted issues.
    pub watch: Vec<String>,
    /// The tools in your own stack, as public `owner/repo` GitHub slugs.
    pub tools: Vec<String>,
    /// Hard cap on findings the agent may keep (default [`DEFAULT_MAX_FINDINGS`]).
    pub max_findings: usize,
}

impl Default for ScoutConfig {
    fn default() -> Self {
        ScoutConfig {
            watch: Vec::new(),
            tools: Vec::new(),
            max_findings: DEFAULT_MAX_FINDINGS,
        }
    }
}

/// The findings ceiling the Scout enforces.
pub const DEFAULT_MAX_FINDINGS: usize = 5;
