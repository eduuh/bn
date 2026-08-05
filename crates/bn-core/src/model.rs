//! Typed note model (D1). The frontmatter is the serde surface; the body is free
//! markdown (`## Goal`, `## Progress`) preserved verbatim.
//!
//! Item date field is `created` in the new single-file format; the legacy `added`
//! field name is accepted on read via a serde alias (DECISIONS.md D-item-date).

use crate::resolver::RepoBranch;
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

    /// True when this note carries attached free-text / uploaded content.
    pub fn has_text(&self) -> bool {
        self.text.as_deref().is_some_and(|t| !t.trim().is_empty())
    }
}

/// Some legacy notes carry `work_item`/`pr` as bare integers or strings; null otherwise.
/// This keeps either readable and re-serializes faithfully.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Scalar {
    Int(i64),
    Str(String),
}

impl Scalar {
    /// The scalar as a plain string (numbers stringified) — for URL building and display.
    pub fn display(&self) -> String {
        match self {
            Scalar::Int(n) => n.to_string(),
            Scalar::Str(s) => s.clone(),
        }
    }

    /// Parse a raw id/value: an all-digits string becomes `Int` (so a numeric work-item id
    /// round-trips as a YAML number, matching the store), otherwise `Str`.
    pub fn parse(s: &str) -> Scalar {
        if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(n) = s.parse::<i64>() {
                return Scalar::Int(n);
            }
        }
        Scalar::Str(s.to_string())
    }
}

/// The YAML frontmatter block, including the six embedded item lists (single-file D1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frontmatter {
    pub repo: String,
    pub branch: String,
    pub created: String,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<crate::pipeline::stage::Stage>,
    // Legacy notes write `machine: null`; the oracle coalesces null→"" (treated as local).
    // Deserialize null/missing → "" so the local filter matches the oracle.
    #[serde(default, deserialize_with = "null_to_empty")]
    pub machine: String,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item: Option<Scalar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<Scalar>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub collaborators: Vec<String>,
    // Harnesses (claude, copilot, …) and models (claude-opus-4-8, …) that have touched
    // this branch. Recorded on note mutations via `record_session`; omitted from YAML
    // while empty so plain-shell edits leave them off the note entirely.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub harnesses: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default)]
    pub todos: Vec<Item>,
    #[serde(default)]
    pub blockers: Vec<Item>,
    #[serde(default)]
    pub decisions: Vec<Item>,
    #[serde(default)]
    pub research: Vec<Item>,
    #[serde(default)]
    pub collab: Vec<Item>,
    #[serde(default)]
    pub ask: Vec<Item>,
    #[serde(default)]
    pub done_when: Vec<Item>,
}

fn default_status() -> String {
    "active".to_string()
}

/// Deserialize a string field tolerating an explicit YAML `null` (→ empty string).
fn null_to_empty<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

struct SectionDescriptor {
    section: Section,
    keywords: &'static [&'static str],
    id_prefix: char,
    label: &'static str,
    heading: &'static str,
    is_todo: bool,
    tracks_done: bool,
}

macro_rules! define_sections {
    (
        $(
            $(#[$meta:meta])*
            $variant:ident {
                keywords: [$($keyword:literal),+ $(,)?],
                id_prefix: $id_prefix:literal,
                label: $label:literal,
                is_todo: $is_todo:literal,
                tracks_done: $tracks_done:literal,
            }
        ),+ $(,)?
    ) => {
        /// Structured branch-note sections and their CLI/rendering metadata.
        #[repr(usize)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Section {
            $(
                $(#[$meta])*
                $variant,
            )+
        }

        const SECTION_DESCRIPTORS: &[SectionDescriptor] = &[
            $(
                SectionDescriptor {
                    section: Section::$variant,
                    keywords: &[$($keyword),+],
                    id_prefix: $id_prefix,
                    label: $label,
                    heading: concat!("## ", $label),
                    is_todo: $is_todo,
                    tracks_done: $tracks_done,
                },
            )+
        ];
    };
}

define_sections! {
    Todo {
        keywords: ["todo", "todos"],
        id_prefix: 't',
        label: "Todos",
        is_todo: true,
        tracks_done: true,
    },
    Blocker {
        keywords: ["blocker", "blockers"],
        id_prefix: 'b',
        label: "Blockers",
        is_todo: false,
        tracks_done: false,
    },
    Decision {
        keywords: ["decision", "decisions"],
        id_prefix: 'd',
        label: "Decisions",
        is_todo: false,
        tracks_done: false,
    },
    Research {
        keywords: ["research"],
        id_prefix: 'r',
        label: "To Research",
        is_todo: false,
        tracks_done: false,
    },
    Collab {
        keywords: ["collab"],
        id_prefix: 'c',
        label: "Collaboration",
        is_todo: false,
        tracks_done: false,
    },
    Ask {
        keywords: ["ask"],
        id_prefix: 'a',
        label: "To Ask",
        is_todo: false,
        tracks_done: false,
    },
    /// Acceptance criteria — a checklist of what "done" means for this branch. Checkable like
    /// todos, but kept out of the open-todo stats; `bn close` warns when any stay unmet.
    DoneWhen {
        keywords: ["done-when", "done_when", "donewhen", "criteria", "dod"],
        id_prefix: 'w',
        label: "Done When",
        is_todo: false,
        tracks_done: true,
    }
}

impl Section {
    fn descriptor(self) -> &'static SectionDescriptor {
        &SECTION_DESCRIPTORS[self as usize]
    }

    /// Resolve a CLI section keyword, including accepted aliases, to a section.
    pub fn from_keyword(s: &str) -> Option<Section> {
        SECTION_DESCRIPTORS
            .iter()
            .find(|descriptor| descriptor.keywords.contains(&s))
            .map(|descriptor| descriptor.section)
    }

    pub fn id_prefix(self) -> char {
        self.descriptor().id_prefix
    }

    /// Human label (heading without the `## ` prefix), used in `add` output.
    pub fn label(self) -> &'static str {
        self.descriptor().label
    }

    /// The markdown heading line (`## Todos`, …) used in legacy md-only notes.
    pub fn heading(self) -> &'static str {
        self.descriptor().heading
    }

    /// True for the Todo section only — the section whose open items count as "open todos"
    /// in summaries, doctor, and metrics.
    pub fn is_todo(self) -> bool {
        self.descriptor().is_todo
    }

    /// True for sections whose items carry checkbox state (`done`/`done_at`): todos and
    /// acceptance criteria. Drives whether `add` seeds `done: false` and whether `done` can
    /// check the item off.
    pub fn tracks_done(self) -> bool {
        self.descriptor().tracks_done
    }

    pub fn all() -> impl ExactSizeIterator<Item = Section> {
        SECTION_DESCRIPTORS
            .iter()
            .map(|descriptor| descriptor.section)
    }
}

/// A fully-read note: typed frontmatter + free-markdown body (prose only).
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub front: Frontmatter,
    /// Markdown after the frontmatter fence — Goal/Progress prose, item sections removed.
    pub body: String,
}

impl Note {
    /// A fresh, empty single-file note for `rb`'s repo/branch, created `created` on `machine`.
    /// Body seeds the `# repo / branch` title plus empty Goal/Progress (matches the
    /// oracle's branch-note template prose).
    pub fn new(rb: RepoBranch<'_>, created: &str, machine: &str) -> Note {
        let RepoBranch { repo, branch } = rb;
        let front = Frontmatter {
            repo: repo.to_string(),
            branch: branch.to_string(),
            created: created.to_string(),
            status: default_status(),
            stage: None,
            machine: machine.to_string(),
            type_: None,
            work_item: None,
            pr: None,
            tags: vec![],
            collaborators: vec![],
            harnesses: vec![],
            models: vec![],
            todos: vec![],
            blockers: vec![],
            decisions: vec![],
            research: vec![],
            collab: vec![],
            ask: vec![],
            done_when: vec![],
        };
        let body = format!("# {repo} / {branch}\n\n## Goal\n\n\n## Progress\n");
        Note { front, body }
    }
}

impl Frontmatter {
    /// Record the harness (claude/copilot/…) and its model into `harnesses`/`models`,
    /// each deduped. No-op for whichever isn't detectable, so a plain-shell edit (or a
    /// harness that doesn't expose its model) adds nothing.
    pub fn record_session(&mut self) {
        if let Some(h) = crate::util::detect_harness() {
            if !self.harnesses.iter().any(|x| x == &h) {
                self.harnesses.push(h);
            }
        }
        if let Some(m) = crate::util::detect_model() {
            if !self.models.iter().any(|x| x == &m) {
                self.models.push(m);
            }
        }
    }

    /// Borrow the item list for a section.
    pub fn items(&self, section: Section) -> &Vec<Item> {
        match section {
            Section::Todo => &self.todos,
            Section::Blocker => &self.blockers,
            Section::Decision => &self.decisions,
            Section::Research => &self.research,
            Section::Collab => &self.collab,
            Section::Ask => &self.ask,
            Section::DoneWhen => &self.done_when,
        }
    }

    /// Mutably borrow the item list for a section.
    pub fn items_mut(&mut self, section: Section) -> &mut Vec<Item> {
        match section {
            Section::Todo => &mut self.todos,
            Section::Blocker => &mut self.blockers,
            Section::Decision => &mut self.decisions,
            Section::Research => &mut self.research,
            Section::Collab => &mut self.collab,
            Section::Ask => &mut self.ask,
            Section::DoneWhen => &mut self.done_when,
        }
    }
}
