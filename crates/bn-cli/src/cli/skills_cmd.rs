//! `bn skills ...` clap surface, split from the top-level command catalog.

use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum SkillsCmd {
    /// List skills across all tiers (default).
    List,
    /// Rank skills by relevance to a task: bn skills find wrap up my work.
    Find {
        /// Free-text task/intent to match against skill names, keywords, when, and descriptions.
        #[arg(value_name = "QUERY", required = true)]
        query: Vec<String>,
    },
    /// Create a new skill: bn skills new <name> --desc <text> [--scope <tier>].
    New {
        name: String,
        /// One-line description shown in the prompt list.
        #[arg(long = "desc", value_name = "TEXT")]
        description: String,
        /// Tier: global-shared | global-personal | repo-shared | repo-personal-notes |
        /// repo-personal-local (default global-personal).
        #[arg(long, default_value = "global-personal")]
        scope: String,
        /// Replace an existing skill of the same name in that tier.
        #[arg(long)]
        overwrite: bool,
    },
    /// Render a skill's prompt body to stdout (headless — no MCP server needed), with
    /// `{{name}}` placeholders filled from repeatable `--arg key=value`. For cron/scripts:
    /// `prompt=$(bn skills render m365-email-query)`.
    Render {
        name: String,
        /// Fill a `{{key}}` placeholder: repeatable, e.g. `--arg branch=main`.
        #[arg(long = "arg", value_name = "KEY=VALUE")]
        args: Vec<String>,
    },
    /// Open a skill in $EDITOR, or the skills folder itself when no name is given:
    /// `bn skills edit` opens the global-shared skills dir; `bn skills edit debug` opens that
    /// skill's file. `--scope <tier>` picks a different tier's folder.
    Edit {
        /// Skill to open (its effective definition). Omit to open the skills folder.
        name: Option<String>,
        /// Which tier's folder to open when no name is given: global-shared | global-personal |
        /// repo-shared | repo-personal-notes | repo-personal-local (default global-shared).
        #[arg(long, default_value = "global-shared")]
        scope: String,
    },
    /// Internal (run by install.sh): (re)generate the native Agent Skills under `~/.copilot/skills`
    /// and `~/.claude/skills` from the global bn skills, so the shipped prompts + personal skills
    /// appear as `/name` in Copilot CLI and Claude Code — matching what the dashboard's skill CRUD
    /// writes on every edit. Idempotent; only ever writes or removes bn-managed files, and a
    /// harness whose root holds a same-named skill bn doesn't own is reported, not fatal.
    #[command(hide = true)]
    Install,
}
