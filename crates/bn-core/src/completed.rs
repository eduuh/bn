//! The **Completed** fold (#561) — the counterpart to [`crate::action_center`]. Where the action
//! center folds every *open* signal into "what needs you", this folds every *finished* one into a
//! history of "what got done": completed branch-note todos (`bn done`), achieved daily goals,
//! resolved attention items, and landed/merged orchestrate work.
//!
//! Each [`CompletedItem`] carries a [`NoteLink`] so a finished item is traceable back to its full
//! context from anywhere — the branch note's **local path** (open on this machine) *and* its
//! **remote URL** (the synced branch-notes remote). The fold is pure over its inputs plus a
//! [`NoteLinker`] (the git/fs resolver, injected so the ranking + kind-mapping stay unit-testable);
//! the web layer gathers the source viewmodels and supplies an [`crate::rollups::FsNoteLinker`].

use crate::attention::AttnItem;
use crate::model::Item;
use crate::orchestrate::{ItemStatus, QueueItem};
use crate::util;
use serde::Serialize;

/// A branch note's two locations, so a completed item can jump back to its full context from
/// anywhere. Both are best-effort — a note that no longer exists locally, or a notes repo with no
/// git remote, simply leaves the corresponding field `None` and the UI omits that jump.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct NoteLink {
    /// The absolute path of the branch note on **this** machine (`~/projects/branch-notes/…`),
    /// for the "open locally" jump. `None` when the note isn't present on this host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    /// A web URL to the branch note in the synced **remote** (a GitHub blob link), for the "open
    /// remotely" jump. `None` when the notes repo has no resolvable remote.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
}

impl NoteLink {
    /// Whether either jump is available — the UI hides the note-links row entirely when neither is.
    pub fn is_empty(&self) -> bool {
        self.local_path.is_none() && self.remote_url.is_none()
    }
}

/// Resolves a `(repo, branch)` to its [`NoteLink`]. Injected into [`build_completed`] so the fold
/// itself stays pure and unit-testable (tests pass a stub); the real
/// [`crate::rollups::FsNoteLinker`] shells git + touches the filesystem.
pub trait NoteLinker {
    /// The note-link for `repo`/`branch`. `branch` is the note-key form (slashes already dashed).
    fn link(&self, repo: &str, branch: &str) -> NoteLink;
}

/// A no-op linker — every note-link comes back empty. The default when no note location is known
/// (and a convenient stub for tests that don't exercise linking).
pub struct NoLinker;
impl NoteLinker for NoLinker {
    fn link(&self, _repo: &str, _branch: &str) -> NoteLink {
        NoteLink::default()
    }
}

/// What a given [`CompletedItem`] came from — its `type`, driving the row's icon + group label.
/// Serialized kebab-case to match the rest of the web contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompletedKind {
    /// A branch-note todo closed via `bn done`.
    Todo,
    /// A daily goal marked achieved.
    Goal,
    /// An attention item that reached a terminal state (acked / done).
    Attention,
    /// An orchestrate work item whose PR landed (merged).
    MergedPr,
}

impl CompletedKind {
    /// A short, stable machine token (also what `type` serializes to).
    pub fn as_str(self) -> &'static str {
        match self {
            CompletedKind::Todo => "todo",
            CompletedKind::Goal => "goal",
            CompletedKind::Attention => "attention",
            CompletedKind::MergedPr => "merged-pr",
        }
    }
}

/// One finished task, unified across sources. `completed` is epoch-seconds of completion (the sort
/// key, most-recent-first); `note` carries the local + remote jumps back to its branch note.
#[derive(Debug, Clone, Serialize)]
pub struct CompletedItem {
    /// Stable, type-prefixed id (`done-todo:repo/branch/t1`, `merged:repo/338`, …), unique across
    /// kinds so the list can key on it.
    pub id: String,
    /// The item's origin kind (serialized as `type`).
    #[serde(rename = "type")]
    pub kind: CompletedKind,
    /// One-line headline for the row.
    pub title: String,
    /// Human-readable provenance (`note · repo/branch`, `today · goal`, `attention`, …).
    pub source: String,
    /// The repo this finished in, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The branch this finished on, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// A deep link when the item has one (a merged PR url, a goal's attached link).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Extra multi-line context for the expanded row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Epoch-seconds the item completed — the recency sort key.
    pub completed: i64,
    /// The branch note's local path + remote URL (the "open locally / remotely" jumps).
    pub note: NoteLink,
}

/// A completed branch-note todo, gathered from the notes (see [`crate::rollups::completed_todos`]).
/// Kept as its own input row — [`crate::rollups::SummaryEntry`] carries only *open* todos.
#[derive(Debug, Clone)]
pub struct CompletedTodo {
    pub repo: String,
    pub branch: String,
    pub id: String,
    pub text: String,
    /// The `done_at` stamp as recorded on the note (`YYYY-MM-DD`); parsed for the sort key.
    pub done_at: String,
}

/// The already-read source viewmodels the completed fold unifies. Every field is borrowed, so the
/// caller owns the reads.
#[derive(Default)]
pub struct CompletedSources<'a> {
    /// Closed branch-note todos across active notes ([`crate::rollups::completed_todos`]).
    pub todos: &'a [CompletedTodo],
    /// Today's daily goals (`daily::read(...).front.goals`) — only achieved ones become items.
    pub goals: &'a [Item],
    /// Terminal attention items (an agent's completion push / acked items) — the review lane.
    pub attn: &'a [AttnItem],
    /// Every orchestrate queue item across live runs (flattened) — only `Merged` ones become items.
    pub merged: &'a [QueueItem],
}

/// Truncate a possibly-multiline body to a short single line for a row title.
fn one_line(text: &str, max: usize) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    if first.chars().count() > max {
        let mut s: String = first.chars().take(max.saturating_sub(1)).collect();
        s.push('…');
        s
    } else {
        first.to_string()
    }
}

/// Build the unified, most-recent-first list of completed items from every source, resolving each
/// item's branch-note jump through `linker`. Pure over `(src, linker)`.
pub fn build_completed(src: &CompletedSources, linker: &dyn NoteLinker) -> Vec<CompletedItem> {
    let mut out: Vec<CompletedItem> = Vec::new();

    // Completed branch-note todos ----------------------------------------
    for t in src.todos {
        out.push(CompletedItem {
            id: format!("done-todo:{}/{}/{}", t.repo, t.branch, t.id),
            kind: CompletedKind::Todo,
            title: one_line(&t.text, 90),
            source: format!("note · {}/{}", t.repo, t.branch),
            repo: Some(t.repo.clone()),
            branch: Some(t.branch.clone()),
            link: None,
            detail: if t.text.contains('\n') {
                Some(t.text.clone())
            } else {
                None
            },
            completed: util::parse_ts(&t.done_at).unwrap_or(0),
            note: linker.link(&t.repo, &t.branch),
        });
    }

    // Achieved daily goals -----------------------------------------------
    for g in src.goals {
        if g.done != Some(true) {
            continue;
        }
        // A goal may carry a linked branch note (#564); use the first one for the jump + context.
        let linked = g
            .notes
            .iter()
            .find_map(|n| match (n.repo.as_deref(), n.branch.as_deref()) {
                (Some(r), Some(b)) if !r.is_empty() && !b.is_empty() => Some((r, b)),
                _ => None,
            });
        let (repo, branch, note) = match linked {
            Some((r, b)) => (Some(r.to_string()), Some(b.to_string()), linker.link(r, b)),
            None => (None, None, NoteLink::default()),
        };
        out.push(CompletedItem {
            id: format!("goal:{}", g.id),
            kind: CompletedKind::Goal,
            title: one_line(&g.text, 90),
            source: "today · goal".to_string(),
            repo,
            branch,
            link: g.link.clone(),
            detail: None,
            completed: g.done_at.as_deref().and_then(util::parse_ts).unwrap_or(0),
            note,
        });
    }

    // Resolved attention items -------------------------------------------
    for a in src.attn {
        let note = match (a.repo.as_deref(), a.branch.as_deref()) {
            (Some(r), Some(b)) if !r.is_empty() && !b.is_empty() => linker.link(r, b),
            _ => NoteLink::default(),
        };
        out.push(CompletedItem {
            id: format!("attn:{}", a.id),
            kind: CompletedKind::Attention,
            title: one_line(&a.title, 90),
            source: format!("attention · {}", a.source),
            repo: a.repo.clone(),
            branch: a.branch.clone(),
            link: a.link.clone(),
            detail: a.detail.clone(),
            completed: a.created,
            note,
        });
    }

    // Landed / merged orchestrate work -----------------------------------
    for item in src.merged {
        if item.status != ItemStatus::Merged {
            continue;
        }
        let note = match item.branch.as_deref() {
            Some(b) if !b.is_empty() => linker.link(&item.repo, b),
            _ => NoteLink::default(),
        };
        out.push(CompletedItem {
            id: format!("merged:{}/{}", item.repo, item.id),
            kind: CompletedKind::MergedPr,
            title: format!("Merged · {} {}", item.repo, one_line(&item.item, 70)),
            source: format!("orchestrate · {}", item.repo),
            repo: Some(item.repo.clone()),
            branch: item.branch.clone(),
            link: item.pr.clone(),
            detail: Some(format!("Item: {}", item.item)),
            completed: item.updated_at,
            note,
        });
    }

    // Most-recent-first; a missing/unparseable completion stamp sorts to epoch 0 (the bottom).
    out.sort_by(|a, b| b.completed.cmp(&a.completed).then(a.id.cmp(&b.id)));
    out
}


