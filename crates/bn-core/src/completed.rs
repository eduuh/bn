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
    /// PRs merged on GitHub, discovered from the metrics log's `pr_merged` events
    /// (`crate::merged_prs::discover_merged`). This is the source that sees work done *outside* the
    /// orchestrate queue — which is nearly all of it, so without this the fold reports a day of
    /// shipped PRs as "quiet". Entries already represented in [`Self::merged`] are dropped by
    /// [`build_completed`] rather than counted twice.
    pub merged_prs: &'a [crate::merged_prs::MergedRef],
}

/// The repo basename a completed row groups under — `eduuh/bn` → `bn`. GitHub events carry the
/// `owner/repo` slug while branch-note todos carry the bare name; folding them to the same key is
/// what lets one repo's PRs and todos land in one group instead of two.
fn repo_base(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
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

    // Merged PRs observed on GitHub ---------------------------------------
    // Recorded first so the orchestrate pass below can drop its duplicates: when a queued item
    // lands, both sources describe the same PR, and the GitHub row is the better one (real merge
    // timestamp, real title, canonical link).
    let mut seen_prs: std::collections::HashSet<(String, i64)> = std::collections::HashSet::new();
    for pr in src.merged_prs {
        let base = repo_base(&pr.repo);
        if !seen_prs.insert((base.to_string(), pr.number)) {
            continue;
        }
        let title = if pr.title.is_empty() {
            format!("{} #{}", pr.repo, pr.number)
        } else {
            format!("{} (#{})", one_line(&pr.title, 80), pr.number)
        };
        out.push(CompletedItem {
            id: format!("merged-pr:{}/{}", pr.repo, pr.number),
            kind: CompletedKind::MergedPr,
            title,
            source: format!("github · {}", pr.repo),
            repo: Some(base.to_string()),
            branch: None,
            link: Some(format!("https://github.com/{}/pull/{}", pr.repo, pr.number)),
            detail: None,
            completed: pr.merged_ts,
            note: NoteLink::default(),
        });
    }

    // Landed / merged orchestrate work -----------------------------------
    for item in src.merged {
        if item.status != ItemStatus::Merged {
            continue;
        }
        // Already counted from its `pr_merged` event — one landed PR is one completed item.
        if item
            .pr
            .as_deref()
            .and_then(crate::pr_observe::parse_pr_link)
            .is_some_and(|(r, n)| seen_prs.contains(&(repo_base(&r).to_string(), n)))
        {
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
    use crate::merged_prs::MergedRef;

    fn merged_ref(repo: &str, number: i64, title: &str, ts: i64) -> MergedRef {
        MergedRef {
            repo: repo.to_string(),
            number,
            title: title.to_string(),
            merged_ts: ts,
        }
    }

    /// The regression this whole source exists for: work that never went through the orchestrate
    /// queue used to fold to nothing, so a day of shipped PRs read back as "quiet".
    #[test]
    fn a_pr_merged_outside_the_queue_is_completed_work() {
        let prs = [merged_ref(
            "eduuh/bn",
            1138,
            "never kill a watched agent",
            500,
        )];
        let src = CompletedSources {
            merged_prs: &prs,
            ..Default::default()
        };
        let out = build_completed(&src, &NoLinker);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, CompletedKind::MergedPr);
        assert_eq!(out[0].title, "never kill a watched agent (#1138)");
        assert_eq!(out[0].repo.as_deref(), Some("bn"));
        assert_eq!(
            out[0].link.as_deref(),
            Some("https://github.com/eduuh/bn/pull/1138")
        );
        assert_eq!(out[0].completed, 500);
    }

    /// A queued item and its `pr_merged` event describe one landed PR. Counting both would inflate
    /// every "how much got done" number by the share of work that happens to run through the queue.
    #[test]
    fn a_queued_item_and_its_merge_event_are_one_item() {
        let prs = [merged_ref("eduuh/bn", 1138, "landed", 500)];
        let queued = [QueueItem {
            id: "1138".into(),
            item: "1138".into(),
            repo: "bn".into(),
            status: ItemStatus::Merged,
            pr: Some("https://github.com/eduuh/bn/pull/1138".into()),
            updated_at: 400,
            ..Default::default()
        }];
        let src = CompletedSources {
            merged: &queued,
            merged_prs: &prs,
            ..Default::default()
        };
        let out = build_completed(&src, &NoLinker);
        assert_eq!(
            out.len(),
            1,
            "the GitHub row wins, the queue row folds away"
        );
        assert_eq!(out[0].id, "merged-pr:eduuh/bn/1138");
    }

    /// Orchestrate work with no corresponding merge event still counts — the dedupe must not
    /// swallow queue items just because the event log hasn't caught up.
    #[test]
    fn queued_work_without_a_merge_event_still_counts() {
        let queued = [QueueItem {
            id: "77".into(),
            item: "77".into(),
            repo: "atlas".into(),
            status: ItemStatus::Merged,
            pr: Some("https://github.com/eduuh/atlas/pull/77".into()),
            updated_at: 400,
            ..Default::default()
        }];
        let src = CompletedSources {
            merged: &queued,
            ..Default::default()
        };
        let out = build_completed(&src, &NoLinker);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "merged:atlas/77");
    }

    /// Events written before titles were carried must still render a usable row.
    #[test]
    fn a_titleless_event_falls_back_to_the_pr_reference() {
        let prs = [merged_ref("eduuh/wira360", 916, "", 500)];
        let src = CompletedSources {
            merged_prs: &prs,
            ..Default::default()
        };
        let out = build_completed(&src, &NoLinker);
        assert_eq!(out[0].title, "eduuh/wira360 #916");
    }

    /// GitHub says `eduuh/bn`, branch-note todos say `bn`. They must group under one repo.
    #[test]
    fn a_slug_and_a_bare_repo_name_group_together() {
        let prs = [merged_ref("eduuh/bn", 1, "x", 500)];
        let todos = [CompletedTodo {
            repo: "bn".into(),
            branch: "main".into(),
            id: "t1".into(),
            text: "a todo".into(),
            done_at: "2026-08-10".into(),
        }];
        let src = CompletedSources {
            todos: &todos,
            merged_prs: &prs,
            ..Default::default()
        };
        let out = build_completed(&src, &NoLinker);
        let repos: Vec<_> = out.iter().filter_map(|c| c.repo.as_deref()).collect();
        assert_eq!(repos, vec!["bn", "bn"]);
    }
}
