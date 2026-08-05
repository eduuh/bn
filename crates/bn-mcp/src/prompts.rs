//! rmcp adapter for bn-core skills. The skill engine (discovery, parsing, rendering, tier
//! precedence) lives in `bn_core::skills`; this module only maps a [`bn_core::skills::Skill`]
//! into the `rmcp` `Prompt` type the server advertises in `prompts/list`.

use bn_core::skills::Skill;
use rmcp::model::{Prompt, PromptArgument};

/// The MCP `Prompt` advertised for a skill (name, description, declared arguments). The skill's
/// `when:` trigger is folded into the description as "Use when: …" so the client's slash-command
/// picker — which only shows name + description — still surfaces *when* to reach for the skill.
pub fn to_prompt(skill: &Skill) -> Prompt {
    let args: Vec<PromptArgument> = skill
        .arguments
        .iter()
        .map(|a| {
            let mut p = PromptArgument::new(a.name.clone()).with_required(a.required);
            if let Some(d) = &a.description {
                p = p.with_description(d.clone());
            }
            p
        })
        .collect();
    let args = if args.is_empty() { None } else { Some(args) };
    Prompt::new(&skill.name, Some(&prompt_description(skill)), args)
}

/// The advertised description: the skill's own description, plus its `when` trigger appended as
/// "Use when: …" when present.
fn prompt_description(skill: &Skill) -> String {
    match &skill.when {
        Some(when) if !when.trim().is_empty() => {
            format!("{} Use when: {}", skill.description, when.trim())
        }
        _ => skill.description.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(when: Option<&str>) -> Skill {
        Skill {
            name: "wrap-session".into(),
            description: "Wrap up the session.".into(),
            when: when.map(str::to_string),
            keywords: vec![],
            arguments: vec![],
            body: "body".into(),
        }
    }

    #[test]
    fn description_folds_in_when_trigger() {
        let p = to_prompt(&skill(Some("at the end of a work session")));
        assert_eq!(
            p.description.as_deref(),
            Some("Wrap up the session. Use when: at the end of a work session")
        );
    }

    #[test]
    fn description_unchanged_without_when() {
        let p = to_prompt(&skill(None));
        assert_eq!(p.description.as_deref(), Some("Wrap up the session."));
    }
}
