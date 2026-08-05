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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GoalNote, Item};

    /// A stub linker that returns a deterministic local path + remote url per `(repo, branch)`, so
    /// the fold's linking can be asserted without touching git or the filesystem.
    struct StubLinker;
    impl NoteLinker for StubLinker {
        fn link(&self, repo: &str, branch: &str) -> NoteLink {
            NoteLink {
                local_path: Some(format!("/notes/{repo}/{branch}/note.md")),
                remote_url: Some(format!("https://host/blob/main/{repo}/{branch}/note.md")),
            }
        }
    }

    fn todo(repo: &str, branch: &str, id: &str, text: &str, done_at: &str) -> CompletedTodo {
        CompletedTodo {
            repo: repo.to_string(),
            branch: branch.to_string(),
            id: id.to_string(),
            text: text.to_string(),
            done_at: done_at.to_string(),
        }
    }

    fn goal(id: &str, text: &str, done: bool, done_at: &str) -> Item {
        Item {
            id: id.to_string(),
            text: text.to_string(),
            created: "2026-07-01".to_string(),
            done: Some(done),
            done_at: if done_at.is_empty() {
                None
            } else {
                Some(done_at.to_string())
            },
            link: None,
            pct: None,
            est: None,
            actual: None,
            q: None,
            notes: Vec::new(),
        }
    }

    fn attn(id: &str, title: &str, created: i64, repo: Option<&str>) -> AttnItem {
        AttnItem {
            id: id.to_string(),
            source: "agent".to_string(),
            title: title.to_string(),
            severity: "done".to_string(),
            repo: repo.map(str::to_string),
            branch: repo.map(|_| "main".to_string()),
            link: None,
            key: None,
            detail: None,
            mode: None,
            prompt: None,
            task_type: None,
            worktree_path: None,
            uncommitted_files: Vec::new(),
            created,
        }
    }

    fn merged(repo: &str, id: &str, item: &str, pr: &str, updated_at: i64) -> QueueItem {
        QueueItem {
            id: id.to_string(),
            item: item.to_string(),
            repo: repo.to_string(),
            status: ItemStatus::Merged,
            dispatch_phase: crate::orchestrate::DispatchPhase::Dispatched,
            branch: Some("feat".to_string()),
            pr: Some(pr.to_string()),
            updated_at,
            ..Default::default()
        }
    }

    #[test]
    fn folds_every_source_into_items() {
        let todos = [todo("bn", "main", "t1", "ship the thing", "2026-07-10")];
        let goals = [goal("g1", "achieved goal", true, "2026-07-11")];
        let attn = [attn("a1", "resolved item", 100, Some("bn"))];
        let merged = [merged("bn", "338", "issue #338", "https://x/pr/1", 200)];
        let src = CompletedSources {
            todos: &todos,
            goals: &goals,
            attn: &attn,
            merged: &merged,
        };
        let out = build_completed(&src, &StubLinker);
        assert_eq!(out.len(), 4);
        let kinds: Vec<_> = out.iter().map(|c| c.kind).collect();
        assert!(kinds.contains(&CompletedKind::Todo));
        assert!(kinds.contains(&CompletedKind::Goal));
        assert!(kinds.contains(&CompletedKind::Attention));
        assert!(kinds.contains(&CompletedKind::MergedPr));
    }

    #[test]
    fn only_achieved_goals_and_merged_prs_surface() {
        let goals = [
            goal("g1", "open goal", false, ""),
            goal("g2", "done goal", true, "2026-07-11"),
        ];
        let mut dispatched = merged("bn", "1", "item", "https://x/pr/1", 10);
        dispatched.status = ItemStatus::Dispatched;
        let merged_items = [dispatched, merged("bn", "2", "item2", "https://x/pr/2", 20)];
        let src = CompletedSources {
            goals: &goals,
            merged: &merged_items,
            ..Default::default()
        };
        let out = build_completed(&src, &NoLinker);
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|c| c.id == "goal:g2"));
        assert!(out.iter().any(|c| c.id == "merged:bn/2"));
        assert!(!out.iter().any(|c| c.id == "goal:g1"));
        assert!(!out.iter().any(|c| c.id == "merged:bn/1"));
    }

    #[test]
    fn sorted_most_recent_first() {
        let todos = [
            todo("bn", "main", "old", "old todo", "2026-07-01"),
            todo("bn", "main", "new", "new todo", "2026-07-14"),
        ];
        let src = CompletedSources {
            todos: &todos,
            ..Default::default()
        };
        let out = build_completed(&src, &NoLinker);
        assert_eq!(out[0].id, "done-todo:bn/main/new");
        assert_eq!(out[1].id, "done-todo:bn/main/old");
    }

    #[test]
    fn todos_carry_note_links() {
        let todos = [todo("bn", "main", "t1", "done", "2026-07-10")];
        let src = CompletedSources {
            todos: &todos,
            ..Default::default()
        };
        let out = build_completed(&src, &StubLinker);
        assert_eq!(
            out[0].note.local_path.as_deref(),
            Some("/notes/bn/main/note.md")
        );
        assert_eq!(
            out[0].note.remote_url.as_deref(),
            Some("https://host/blob/main/bn/main/note.md")
        );
    }

    #[test]
    fn goal_links_its_connected_note() {
        let mut g = goal("g1", "goal with note", true, "2026-07-11");
        g.notes = vec![GoalNote {
            id: "n1".to_string(),
            repo: Some("bn".to_string()),
            branch: Some("feat".to_string()),
            ..Default::default()
        }];
        let goals = [g];
        let src = CompletedSources {
            goals: &goals,
            ..Default::default()
        };
        let out = build_completed(&src, &StubLinker);
        assert_eq!(out[0].repo.as_deref(), Some("bn"));
        assert_eq!(out[0].branch.as_deref(), Some("feat"));
        assert!(!out[0].note.is_empty());
    }

    #[test]
    fn unlinked_goal_has_empty_note() {
        let goals = [goal("g1", "bare goal", true, "2026-07-11")];
        let src = CompletedSources {
            goals: &goals,
            ..Default::default()
        };
        let out = build_completed(&src, &StubLinker);
        assert!(out[0].note.is_empty());
        assert!(out[0].repo.is_none());
    }
}
