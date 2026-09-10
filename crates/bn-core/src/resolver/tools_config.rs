//! Managed-tools configuration. Built-in tool specs are opt-in by machine: a host declares which
//! bn-shipped specs it wants, while user-declared `tools.toml` entries are always active.

use super::config_file::ConfigFile;

/// Resolved config for the managed-tools subsystem.
///
/// `[tools].builtins` is an allowlist of bn-shipped tool specs to include in the effective
/// registry. It defaults empty so personal services like n8n never start on a work machine unless
/// that machine explicitly opts in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolsConfig {
    pub builtins: Vec<String>,
}

impl ToolsConfig {
    /// Production entry: the compiled defaults, then the config file layers.
    pub fn load() -> ToolsConfig {
        ToolsConfig::resolve(ConfigFile::load().as_ref())
    }

    pub(super) fn resolve(file: Option<&ConfigFile>) -> ToolsConfig {
        let builtins = file
            .and_then(|f| f.tools_builtins())
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        ToolsConfig { builtins }
    }
}
