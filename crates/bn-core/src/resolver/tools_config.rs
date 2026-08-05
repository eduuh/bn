//! Managed-tools configuration. Built-in tool specs are opt-in by machine: a host declares which
//! bn-shipped specs it wants, while user-declared `tools.toml` entries are always active.

use super::config_file::ConfigFile;
use super::util::{non_empty_env, split_list};

/// Resolved config for the managed-tools subsystem.
///
/// `[tools].builtins` / `$BN_TOOLS_BUILTINS` is an allowlist of bn-shipped tool specs to include
/// in the effective registry. It defaults empty so personal services like n8n never start on a
/// work machine unless that machine explicitly opts in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolsConfig {
    pub builtins: Vec<String>,
}

impl ToolsConfig {
    /// Production entry: layer defaults → `config.toml` → `BN_*` env overrides.
    pub fn load() -> ToolsConfig {
        ToolsConfig::resolve(ConfigFile::load().as_ref())
    }

    pub(super) fn resolve(file: Option<&ConfigFile>) -> ToolsConfig {
        let builtins = non_empty_env("BN_TOOLS_BUILTINS")
            .map(|v| split_list(&v))
            .or_else(|| file.and_then(|f| f.tools_builtins()))
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        ToolsConfig { builtins }
    }
}
