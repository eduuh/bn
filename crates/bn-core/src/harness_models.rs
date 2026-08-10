//! What models and effort levels a harness actually offers — discovered, not hardcoded.
//!
//! An operator configuring a pipeline stage cannot be expected to know that Copilot serves
//! `claude-opus-4.8` while Claude Code needs `claude-opus-4-8`, or which of two dozen ids exist at
//! all. A hardcoded list in bn would answer that question wrongly within a month, and the failure is
//! silent: [`crate::mcp::Harness::model_arg`] drops the bad id and the agent runs on the harness's
//! own default, so the stage looks configured and isn't.
//!
//! # Where the truth lives
//!
//! The **Copilot CLI's shell-completion script** enumerates the accepted values for each flag, keyed
//! by the flag itself:
//!
//! ```text
//!     --model)
//!         local -a __vals=('auto' 'claude-opus-5' 'gpt-5.4' …)
//!     --effort|--reasoning-effort)
//!         local -a __vals=('none' 'minimal' 'low' 'medium' 'high' 'xhigh' 'max')
//! ```
//!
//! That is a real, machine-readable answer from the harness itself, and it costs one subprocess.
//! **Claude Code ships no such list**, so its models fall back to the documented alias set — which is
//! also exactly the set `model_arg` validates, so the picker and the guard cannot disagree.
//!
//! # Pure by construction
//!
//! Parsing lives here and is unit-tested against a fixture; *running* `copilot completion zsh` lives
//! in the CLI, which is where every other subprocess in bn lives. The interesting half — a brittle
//! text scrape — is therefore testable without the harness installed.

/// The tier aliases Claude Code accepts, strongest first.
///
/// Aliases rather than pinned ids on purpose: an alias tracks the latest model in its tier, so a
/// stage configured with `opus` keeps working when a new Opus ships. Pinning a specific version is
/// still possible by typing its dashed full name, which is why the picker allows free text too.
pub const CLAUDE_ALIASES: [&str; 4] = ["fable", "opus", "sonnet", "haiku"];

/// Extract the values a shell-completion `script` offers for `flag` (e.g. `--model`).
///
/// Returns them in the script's own order — which is the harness's preference order, newest first —
/// and empty when the flag is absent, the script is empty, or the format has changed. Empty is a
/// legitimate answer meaning "ask the harness a different way", never an error: a scrape that starts
/// failing must degrade to a free-text field, not block anyone from configuring a stage.
pub fn parse_completion_values(script: &str, flag: &str) -> Vec<String> {
    let mut lines = script.lines();
    while let Some(line) = lines.next() {
        if !is_case_arm_for(line, flag) {
            continue;
        }
        // The assignment follows within a line or two of its case arm; bounded so a malformed script
        // cannot make this scan the whole file for a `__vals=` belonging to a later flag.
        for candidate in lines.by_ref().take(3) {
            if let Some(values) = parse_vals(candidate) {
                return values;
            }
        }
        return Vec::new();
    }
    Vec::new()
}

/// Whether `line` is the `case` arm for `flag` — `--model)` or `--effort|--reasoning-effort)`.
///
/// Matched on the whole alternative rather than by substring, so `--model` does not also match a
/// hypothetical `--model-provider)` arm and return that flag's values instead.
fn is_case_arm_for(line: &str, flag: &str) -> bool {
    let trimmed = line.trim();
    let Some(pattern) = trimmed.strip_suffix(')') else {
        return false;
    };
    pattern.split('|').any(|alt| alt.trim() == flag)
}

/// The single-quoted tokens of a `local -a __vals=(…)` assignment, or `None` if this is not one.
fn parse_vals(line: &str) -> Option<Vec<String>> {
    let (_, rest) = line.split_once("__vals=(")?;
    let body = rest.split_once(')').map(|(inner, _)| inner).unwrap_or(rest);
    let values: Vec<String> = body
        .split('\'')
        // Quoted tokens land on odd indices: `a` 'x' ` ` 'y' … — so the separators are the evens.
        .skip(1)
        .step_by(2)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect();
    Some(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed-down copy of the real `copilot completion zsh` shape, including a second flag after
    /// the one under test — the arrangement that makes a naive "first `__vals` wins" parser wrong.
    const FIXTURE: &str = r#"
    case "$prev" in
        --model)
            local -a __vals=('auto' 'claude-opus-5' 'claude-opus-4.8' 'gpt-5.4')
            _describe -t values 'value' __vals
            return 0
            ;;
        --effort|--reasoning-effort)
            local -a __vals=('none' 'minimal' 'low' 'medium' 'high' 'xhigh' 'max')
            _describe -t values 'value' __vals
            return 0
            ;;
        --allow-tool)
            local -a __vals=('shell' 'write')
            ;;
    esac
"#;

    #[test]
    fn models_come_back_in_the_harnesss_own_order() {
        assert_eq!(
            parse_completion_values(FIXTURE, "--model"),
            vec!["auto", "claude-opus-5", "claude-opus-4.8", "gpt-5.4"]
        );
    }

    /// The flag we care about is not the first in the script, and an aliased arm has to match on the
    /// alternative — the two ways a substring match would return some other flag's values.
    #[test]
    fn an_aliased_arm_matches_either_of_its_names() {
        let expected = vec!["none", "minimal", "low", "medium", "high", "xhigh", "max"];
        assert_eq!(parse_completion_values(FIXTURE, "--effort"), expected);
        assert_eq!(
            parse_completion_values(FIXTURE, "--reasoning-effort"),
            expected
        );
    }

    #[test]
    fn a_flag_the_script_does_not_offer_yields_nothing() {
        assert!(parse_completion_values(FIXTURE, "--model-provider").is_empty());
        assert!(parse_completion_values(FIXTURE, "--nonesuch").is_empty());
    }

    /// A changed or absent script degrades to "no list" so the caller falls back to free text. This
    /// is the whole reason the parser returns a Vec rather than a Result.
    #[test]
    fn a_script_that_stops_looking_like_itself_degrades_quietly() {
        for script in [
            "",
            "   ",
            "case \"$prev\" in\n  --model)\n    esac\n",
            "garbage",
        ] {
            assert!(parse_completion_values(script, "--model").is_empty());
        }
    }

    #[test]
    fn the_claude_aliases_are_the_ones_the_launch_guard_accepts() {
        // The picker's list and `model_arg` must agree, or the UI offers an id that is silently
        // dropped at launch.
        for alias in CLAUDE_ALIASES {
            assert_eq!(
                crate::mcp::Harness::Claude.model_arg(alias),
                Some(alias),
                "{alias} must survive the guard"
            );
        }
    }
}
