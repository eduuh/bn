use serde::{Deserialize, Serialize};

/// A single structured item in one of the six section lists.
/// Field order matches DECISIONS.md: `{id, text, created, done, done_at}`.
/// `done`/`done_at` are present only on todos (others serialize without them).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub text: String,
    #[serde(alias = "added")]
    pub created: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<String>,
    /// Optional URL attached to the item (an ADO work item, a doc, …). Used by daily goals;
    /// omitted from YAML when absent, so branch-note items round-trip unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Partial completion, 1–99, set by `bn done --pct N` (#213) — visible progress on
    /// long-running items before they close. Cleared on completion; absent when never tracked,
    /// so existing notes round-trip unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pct: Option<u8>,
    /// Optional time estimate stamped at creation (`--est 2h`), a human duration in the
    /// `bn focus start` grammar (`90m`, `2h`, `1h30m`). The estimate half of calibration
    /// ([`crate::calib`]); omitted from YAML when absent, so un-estimated items are untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub est: Option<String>,
    /// Optional actual duration stamped at close, summed from the punch log over the item's
    /// open window ([`crate::calib`]). Only set when an `est` was present and the punch log had
    /// matching time; omitted from YAML otherwise, so the default flow stays zero-friction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    /// Optional quarter-goal key (`--q <key>`) linking this item to a `bn quarter` goal.
    /// Like `link`, used by daily goals and note todos; omitted from YAML when absent so
    /// untagged items round-trip unchanged. Lets the impact agent tie shipped work to a goal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    /// Notes connected to this goal (#564): a linked branch note and/or attached free-text /
    /// uploaded content, so a goal carries its supporting context for the goals-alignment agent.
    /// Used by daily goals; carried inline (not an id-keyed side store) so the association survives
    /// daily carry-forward, where goal ids are renumbered. Omitted from YAML when empty, so every
    /// other note item round-trips byte-identically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<GoalNote>,
}

impl Item {
    /// True for an open (not-done) todo. Non-todo items (done == None) are never open.
    pub fn is_open_todo(&self) -> bool {
        self.done == Some(false)
    }
}

/// A note connected to a daily goal (#564). Two shapes, distinguished by which fields are set:
/// a **linked branch note** (`repo` + `branch`, jumpable to the Notes view) and/or **attached
/// content** (`text` — free-text or an uploaded doc's body). `title` is an optional human label
/// (an upload's filename, say). All-`Option` + skipped-when-absent so the shape stays minimal.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GoalNote {
    /// Per-goal id (`n1`, `n2`, …) — stable within the goal so a note can be disconnected by id.
    pub id: String,
    /// A linked branch note's repo basename (paired with `branch`); absent for a text attachment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The linked branch note's branch (note-key form, slashes already dashed); paired with `repo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Attached free-text / uploaded note content; absent for a pure branch-note link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Optional human label (an upload's filename, or a title for the attached text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Date the note was connected (`YYYY-MM-DD`).
    pub created: String,
}

impl GoalNote {
    /// True when this note links an existing branch note (both `repo` and `branch` present).
    pub fn is_link(&self) -> bool {
        self.repo.as_deref().is_some_and(|r| !r.is_empty())
            && self.branch.as_deref().is_some_and(|b| !b.is_empty())
    }
}
