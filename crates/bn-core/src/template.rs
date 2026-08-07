//! Strict named-argument templates shared by Markdown prompts and agents.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fmt;

/// One declared `{{name}}` template argument.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ArgumentDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
}

/// A structural or render-time template failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateError {
    InvalidArgumentName { name: String },
    DuplicateArgument { name: String },
    MissingRequiredValue { name: String },
    UnknownValue { name: String },
    UnresolvedPlaceholder { name: String },
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgumentName { name } => write!(
                f,
                "invalid argument name '{name}'; use 1-64 ASCII letters, digits, '-' or '_'"
            ),
            Self::DuplicateArgument { name } => {
                write!(f, "duplicate argument declaration '{name}'")
            }
            Self::MissingRequiredValue { name } => {
                write!(f, "missing required argument value '{name}'")
            }
            Self::UnknownValue { name } => {
                write!(f, "unknown argument value '{name}'")
            }
            Self::UnresolvedPlaceholder { name } => {
                write!(f, "unresolved template placeholder '{{{{{name}}}}}'")
            }
        }
    }
}

impl std::error::Error for TemplateError {}

/// Whether a declared argument name is safe to embed in a `{{name}}` marker.
pub fn safe_argument_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Validate argument declarations and every placeholder in `body`.
pub fn validate(body: &str, arguments: &[ArgumentDef]) -> Result<(), TemplateError> {
    let mut declared = BTreeSet::new();
    for argument in arguments {
        if !safe_argument_name(&argument.name) {
            return Err(TemplateError::InvalidArgumentName {
                name: argument.name.clone(),
            });
        }
        if !declared.insert(argument.name.as_str()) {
            return Err(TemplateError::DuplicateArgument {
                name: argument.name.clone(),
            });
        }
    }

    for placeholder in placeholders(body)? {
        if !declared.contains(placeholder.as_str()) {
            return Err(TemplateError::UnresolvedPlaceholder { name: placeholder });
        }
    }
    Ok(())
}

/// Strictly render `body`: required values must be present and no placeholder may be undeclared.
///
/// Missing optional values become empty strings, matching public skill rendering.
pub fn render_checked(
    body: &str,
    arguments: &[ArgumentDef],
    values: Option<&Map<String, Value>>,
) -> Result<String, TemplateError> {
    render(body, arguments, values, true)
}

/// Compatibility renderer for public skills: every omitted value, including a required one, is
/// replaced with an empty string. Declarations and placeholders are still structurally checked.
pub fn render_compatible(
    body: &str,
    arguments: &[ArgumentDef],
    values: Option<&Map<String, Value>>,
) -> Result<String, TemplateError> {
    render(body, arguments, values, false)
}

fn render(
    body: &str,
    arguments: &[ArgumentDef],
    values: Option<&Map<String, Value>>,
    require_values: bool,
) -> Result<String, TemplateError> {
    validate(body, arguments)?;
    if require_values {
        if let Some(values) = values {
            for name in values.keys() {
                if !arguments.iter().any(|argument| argument.name == *name) {
                    return Err(TemplateError::UnknownValue { name: name.clone() });
                }
            }
        }
    }
    let mut rendered = body.to_string();
    for argument in arguments {
        let value = values.and_then(|map| map.get(&argument.name));
        if require_values && argument.required && value.is_none() {
            return Err(TemplateError::MissingRequiredValue {
                name: argument.name.clone(),
            });
        }
        let value = value.map(value_to_string).unwrap_or_default();
        rendered = rendered.replace(&format!("{{{{{}}}}}", argument.name), &value);
    }
    Ok(rendered)
}

fn placeholders(body: &str) -> Result<Vec<String>, TemplateError> {
    let mut names = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("{{") {
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find("}}") else {
            return Err(TemplateError::UnresolvedPlaceholder {
                name: after_open.trim().to_string(),
            });
        };
        let name = &after_open[..end];
        if !safe_argument_name(name) {
            return Err(TemplateError::UnresolvedPlaceholder {
                name: name.to_string(),
            });
        }
        names.push(name.to_string());
        rest = &after_open[end + 2..];
    }
    Ok(names)
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argument(name: &str, required: bool) -> ArgumentDef {
        ArgumentDef {
            name: name.to_string(),
            description: None,
            required,
        }
    }

    #[test]
    fn compatible_render_substitutes_and_blanks_omitted_values() {
        let arguments = vec![argument("name", true), argument("note", false)];
        let mut values = Map::new();
        values.insert("name".into(), Value::String("Ada".into()));

        let rendered = render_compatible(
            "Hello {{name}}. {{note}}/{{name}}",
            &arguments,
            Some(&values),
        );

        assert_eq!(rendered, Ok("Hello Ada. /Ada".to_string()));
    }

    #[test]
    fn checked_render_requires_required_values() {
        let error = render_checked("Hello {{name}}", &[argument("name", true)], None);

        assert_eq!(
            error,
            Err(TemplateError::MissingRequiredValue {
                name: "name".to_string()
            })
        );
    }

    #[test]
    fn checked_render_rejects_unknown_values() {
        let mut values = Map::new();
        values.insert("extra".into(), Value::String("no".into()));

        assert_eq!(
            render_checked("", &[], Some(&values)),
            Err(TemplateError::UnknownValue {
                name: "extra".to_string()
            })
        );
    }

    #[test]
    fn invalid_and_duplicate_declarations_are_typed() {
        assert_eq!(
            validate("", &[argument("../name", false)]),
            Err(TemplateError::InvalidArgumentName {
                name: "../name".to_string()
            })
        );
        assert_eq!(
            validate("", &[argument("name", false), argument("name", true)]),
            Err(TemplateError::DuplicateArgument {
                name: "name".to_string()
            })
        );
    }

    #[test]
    fn unresolved_placeholders_are_typed() {
        assert_eq!(
            validate("Hello {{unknown}}", &[argument("name", false)]),
            Err(TemplateError::UnresolvedPlaceholder {
                name: "unknown".to_string()
            })
        );
    }
}
