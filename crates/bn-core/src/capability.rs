//! Node capability label prefixes: how a box advertises the skills, harnesses and MCP servers it
//! offers in one flat label list.

/// Reserved label prefixes that project a **typed** requirement into the flat node-label namespace
/// the #728 capability matcher already understands: `skill:investigate`, `harness:claude`, `mcp:bn`.
///
/// A node advertises one flat `labels` list; every axis is a prefix within it, so there is exactly
/// one place a capability can be recorded.
pub const SKILL_PREFIX: &str = "skill:";
/// See [`SKILL_PREFIX`].
pub const HARNESS_PREFIX: &str = "harness:";
/// See [`SKILL_PREFIX`].
pub const MCP_PREFIX: &str = "mcp:";
