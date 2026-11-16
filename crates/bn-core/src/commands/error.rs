//! Shared errors for task, daily-goal and inbox mutations.

use crate::fs::StoreError;

#[derive(Debug)]
pub enum CmdError {
    InvalidInput(String),
    Store(StoreError),
    EmptyText,
    NotFound(String),
    /// query, and the ids of the open todos it matched
    Ambiguous(String, Vec<String>),
    AlreadyDone(String),
    /// A date argument wasn't a valid `YYYY-MM-DD`.
    InvalidDate(String),
    /// A quarter-goal key (or other identifier) collided with an existing one.
    Duplicate(String),
}

impl CmdError {
    /// Stable error code for the JSON envelope.
    pub fn code(&self) -> &'static str {
        match self {
            CmdError::InvalidInput(_) => "invalid_input",
            CmdError::Store(_) => "io",
            CmdError::EmptyText => "empty_text",
            CmdError::NotFound(_) => "not_found",
            CmdError::Ambiguous(..) => "ambiguous",
            CmdError::AlreadyDone(_) => "already_done",
            CmdError::InvalidDate(_) => "invalid_date",
            CmdError::Duplicate(_) => "duplicate",
        }
    }

    pub fn message(&self) -> String {
        match self {
            CmdError::InvalidInput(message) => message.clone(),
            CmdError::Store(e) => e.to_string(),
            CmdError::EmptyText => "Text cannot be empty".into(),
            CmdError::NotFound(q) => format!("No match: {q}"),
            CmdError::Ambiguous(q, ids) => {
                format!(
                    "Multiple matches for '{q}': {} (use the id)",
                    ids.join(", ")
                )
            }
            CmdError::AlreadyDone(id) => format!("Already done: {id}"),
            CmdError::InvalidDate(d) => format!("Invalid date '{d}' (use YYYY-MM-DD)"),
            CmdError::Duplicate(k) => format!("Already exists: {k}"),
        }
    }
}

impl From<StoreError> for CmdError {
    fn from(e: StoreError) -> Self {
        CmdError::Store(e)
    }
}

impl From<std::io::Error> for CmdError {
    fn from(error: std::io::Error) -> Self {
        Self::Store(error.into())
    }
}
