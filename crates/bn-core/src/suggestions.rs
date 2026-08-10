//! The suggestions store (#577): Today's agent synthesizes the cron findings store (#576) into a
//! reviewable list of **suggested tasks** — proposals, each carrying its source finding and a
//! rationale — for the owner to review. The store is the durable, reviewable substrate between the
//! findings and the work: the agent writes suggestions here, the owner reviews them on Today (web)
//! or `bn today`, and each carries a `status` so an accepted/dismissed proposal doesn't nag again.
//!
//! # Accepting is what feeds the queue
//!
//! A suggestion is a *proposal* until someone accepts it; accepting it is the moment it becomes
//! work, so that is where it enters the queue (the `today` producer — see
//! `bn_cli::cmd::queue_producers`). Each accepted suggestion therefore carries the identity of what
//! it produced: the governing [`Suggestion::task`], and the [`Suggestion::queued_repo`] /
//! [`Suggestion::queued_id`] of its backlog entry.
//!
//! Those three are a **cache, never the truth**. The backlog is the truth, findable by its
//! `source: today:<suggestion-id>` regardless of what is recorded here, because a crash between the
//! enqueue and the link would otherwise strand an entry that a later accept would then queue twice.
//! Read them to *display* the link; re-derive it from the backlog to *decide* anything.
//!
//! Unlike the findings store (append-only history), suggestions are *mutable* — a review marks one
//! accepted or dismissed — so they live in one read-modify-write JSON file under
//! `state_dir()/suggestions.json` rather than an append log. Each suggestion's `id` is derived from
//! its source + source item key, so re-running the agent over unchanged findings is idempotent: an
//! existing suggestion (in any state) is never re-created, so a dismissed proposal stays dismissed.
//! Nothing here touches the network.

use crate::resolver::state_dir;
use crate::util::now_epoch_secs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where a suggested task sits in its review lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SuggestionStatus {
    /// Freshly synthesized, awaiting review.
    Suggested,
    /// The owner wants to act on it, so it has been fed to the queue.
    Accepted,
    /// The owner declined it — stays recorded so the agent never re-proposes it.
    Dismissed,
    /// Its work landed. Recorded rather than deleted so the day's loop stays auditable: what Today
    /// proposed, what was accepted, and what actually shipped are all still readable afterwards.
    Done,
}

impl SuggestionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SuggestionStatus::Suggested => "suggested",
            SuggestionStatus::Accepted => "accepted",
            SuggestionStatus::Dismissed => "dismissed",
            SuggestionStatus::Done => "done",
        }
    }

    /// Parse a CLI/API status token; `None` for an unknown word.
    pub fn parse(s: &str) -> Option<SuggestionStatus> {
        match s.trim().to_ascii_lowercase().as_str() {
            "suggested" => Some(SuggestionStatus::Suggested),
            "accepted" => Some(SuggestionStatus::Accepted),
            "dismissed" => Some(SuggestionStatus::Dismissed),
            "done" => Some(SuggestionStatus::Done),
            _ => None,
        }
    }

    /// Whether this status is still awaiting an outcome — the states a reconcile sweep looks at.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            SuggestionStatus::Suggested | SuggestionStatus::Accepted
        )
    }
}

/// One proposed task: what to do (`title`), why (`rationale`), and where it came from (`source` +
/// `source_key`, plus an optional `url` to the finding). `id` is deterministic on
/// `source`+`source_key` so re-synthesis is idempotent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Suggestion {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub rationale: String,
    /// The findings source this was synthesized from.
    pub source: String,
    /// The finding item's key within that source (the dedupe axis).
    #[serde(default)]
    pub source_key: String,
    #[serde(default)]
    pub url: String,
    /// When this suggestion was first created (epoch seconds).
    pub created_at: i64,
    pub status: SuggestionStatus,
    /// The governing task id this became when accepted (`t42`), empty until then. The task is what
    /// the SMART gate judges and what close-out marks done.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task: String,
    /// The repo its backlog entry is booked against, empty until it is queued.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub queued_repo: String,
    /// Its backlog entry id, empty until it is queued. A **cache** of what the backlog already
    /// knows — see the module docs on why nothing decides from this field.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub queued_id: String,
    /// When it was queued (epoch seconds), `0` until then.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub queued_at: i64,
}

/// `skip_serializing_if` for an unset epoch stamp, so a suggestion that never reached the queue
/// serializes exactly as it did before these fields existed.
fn is_zero(n: &i64) -> bool {
    *n == 0
}

impl Suggestion {
    /// Build a fresh `Suggested` proposal, stamping `id` (from source+key) and `created_at` now.
    pub fn new(
        source: &str,
        source_key: &str,
        title: impl Into<String>,
        rationale: impl Into<String>,
        url: impl Into<String>,
    ) -> Suggestion {
        Suggestion {
            id: derive_id(source, source_key),
            title: title.into(),
            rationale: rationale.into(),
            source: source.to_string(),
            source_key: source_key.to_string(),
            url: url.into(),
            created_at: now_epoch_secs(),
            status: SuggestionStatus::Suggested,
            task: String::new(),
            queued_repo: String::new(),
            queued_id: String::new(),
            queued_at: 0,
        }
    }

    /// The `source:` a backlog entry produced from this suggestion carries.
    ///
    /// The **idempotency key of the whole tie**: an accept finds its own prior entry by this string
    /// rather than by the cached [`Suggestion::queued_id`], so a link that was never written cannot
    /// cause the same work to be queued twice.
    pub fn queue_source(&self) -> String {
        queue_source(&self.id)
    }
}

/// The `source:` for a suggestion id — the free function, for callers scanning a backlog they have
/// already loaded and holding only ids.
pub fn queue_source(id: &str) -> String {
    format!("today:{id}")
}

/// A filename/id-safe token: ASCII alphanumerics, `-`, `_` kept; everything else → `-`. Same shape
/// as [`crate::feed::sanitize_source`], reused so ids stay stable and printable.
fn sanitize(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    out.trim_matches('-').to_string()
}

/// The deterministic id for a `(source, source_key)` pair — the dedupe key across re-runs.
pub fn derive_id(source: &str, source_key: &str) -> String {
    let s = sanitize(source);
    let k = sanitize(source_key);
    if k.is_empty() {
        s
    } else {
        format!("{s}-{k}")
    }
}

fn store_path() -> PathBuf {
    state_dir().join("suggestions.json")
}

/// Every suggestion on disk, in stored order. Missing file → empty; a corrupt file reads as empty
/// rather than an error (the agent simply re-synthesizes on the next run).
pub fn read() -> Vec<Suggestion> {
    match std::fs::read_to_string(store_path()) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Persist the whole list (create the state dir on first write).
fn write(list: &[Suggestion]) -> std::io::Result<()> {
    let path = store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(list)?)
}

/// Merge freshly synthesized suggestions into the store, keeping existing ones untouched: only a
/// proposal whose `id` isn't already present is appended (in any state — so a dismissed one is
/// never resurrected). Returns how many were newly added.
pub fn upsert(fresh: Vec<Suggestion>) -> std::io::Result<usize> {
    let mut list = read();
    let existing: std::collections::HashSet<String> = list.iter().map(|s| s.id.clone()).collect();
    let mut added = 0;
    for s in fresh {
        if existing.contains(&s.id) {
            continue;
        }
        list.push(s);
        added += 1;
    }
    if added > 0 {
        write(&list)?;
    }
    Ok(added)
}

/// Move one suggestion to a new status. Returns `true` if it existed and changed (or already held
/// that status), `false` if no suggestion with `id` is stored.
pub fn set_status(id: &str, status: SuggestionStatus) -> std::io::Result<bool> {
    let mut list = read();
    let Some(s) = list.iter_mut().find(|s| s.id == id) else {
        return Ok(false);
    };
    s.status = status;
    write(&list)?;
    Ok(true)
}

/// Record the governing task an accepted suggestion became. Returns `false` when no suggestion with
/// `id` is stored. Idempotent — writing the same task id again changes nothing.
pub fn link_task(id: &str, task_id: &str) -> std::io::Result<bool> {
    let mut list = read();
    let Some(s) = list.iter_mut().find(|s| s.id == id) else {
        return Ok(false);
    };
    if s.task == task_id {
        return Ok(true);
    }
    s.task = task_id.to_string();
    write(&list)?;
    Ok(true)
}

/// Cache the backlog entry an accepted suggestion produced. Returns `false` when no suggestion with
/// `id` is stored.
///
/// `queued_at` is stamped only on the first link, so it keeps meaning "when this work entered the
/// queue" rather than drifting to the last time anything re-derived the same fact.
pub fn link_queued(id: &str, repo: &str, entry_id: &str, now: i64) -> std::io::Result<bool> {
    let mut list = read();
    let Some(s) = list.iter_mut().find(|s| s.id == id) else {
        return Ok(false);
    };
    if s.queued_repo == repo && s.queued_id == entry_id {
        return Ok(true);
    }
    s.queued_repo = repo.to_string();
    s.queued_id = entry_id.to_string();
    if s.queued_at == 0 {
        s.queued_at = now;
    }
    write(&list)?;
    Ok(true)
}

/// One suggestion by id.
pub fn get(id: &str) -> Option<Suggestion> {
    read().into_iter().find(|s| s.id == id)
}

/// The repo a suggestion's finding points at, or `None` when nothing in it names one.
///
/// Derived from the finding's `url` — a GitHub issue or PR link names its repo in the path — and
/// then **checked against the repos bn actually knows** (`known_repos`, lowercased, as
/// [`crate::smart::check`] takes them). Never a guess: booking work against a repo that has no
/// checkout produces an entry nothing can ever place, which is worse than admitting the repo is
/// unknown and letting the enhance agent resolve it.
///
/// Pure, so the whole policy is testable without a config.
pub fn repo_from_url(url: &str, known_repos: &std::collections::HashSet<String>) -> Option<String> {
    let rest = url
        .trim()
        .split_once("github.com")
        .map(|(_, rest)| rest.trim_start_matches([':', '/']))?;
    // `<owner>/<repo>/...` — the second segment, minus a `.git` suffix.
    let repo = rest.split('/').nth(1)?.trim();
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    if repo.is_empty() {
        return None;
    }
    let lower = repo.to_ascii_lowercase();
    known_repos.contains(&lower).then_some(lower)
}

/// What became of a suggestion after it was accepted — the round-trip half of the tie.
///
/// One builder for every surface: `bn today list`, `GET /api/suggestions`, and the action-feed fold.
/// The CLI and the dashboard disagreeing about whether the day's work is running would make both
/// untrustworthy, and the join is subtle enough (active vs. resolved, placement pass vs. the work it
/// unblocks) that two copies would drift.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueueView {
    pub repo: String,
    /// The backlog entry id.
    pub entry: String,
    /// `ship`, `enhance`, … — an `enhance` entry means this is still resolving *where* the work goes.
    pub kind: String,
    /// `pending` / `leased` / `done` / …
    pub status: String,
    /// Whether it is still in flight. The one field a caller should branch on before offering to
    /// start the work: `false` with a `Done` status means it shipped, `false` on a cancelled entry
    /// means nothing is coming and the work needs queueing again.
    pub live: bool,
}

/// Join one suggestion against the queue. `None` when nothing was ever queued for it.
pub fn queue_view(s: &Suggestion, backlog: &crate::backlog::Backlog) -> Option<QueueView> {
    let entry = backlog.for_source(&s.queue_source())?;
    Some(QueueView {
        repo: entry.repo.clone(),
        entry: entry.id.clone(),
        kind: entry.kind.label().to_string(),
        status: entry.status.label().to_string(),
        live: entry.status.is_active(),
    })
}

/// Suggestions newest-first, optionally filtered by status and capped. `limit == 0` is uncapped.
pub fn list(status: Option<SuggestionStatus>, limit: usize) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = read();
    if let Some(want) = status {
        out.retain(|s| s.status == want);
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
    if limit > 0 {
        out.truncate(limit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn known(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_finding_url_names_its_repo() {
        let k = known(&["bn", "atlas"]);
        for url in [
            "https://github.com/eduuh/bn/issues/577",
            "https://github.com/eduuh/bn/pull/1135",
            "https://github.com/eduuh/bn",
            "git@github.com:eduuh/bn.git",
        ] {
            assert_eq!(
                repo_from_url(url, &k).as_deref(),
                Some("bn"),
                "should resolve {url}"
            );
        }
    }

    #[test]
    fn a_repo_bn_does_not_know_is_not_resolved() {
        // The whole point of checking against `known_repos`: an entry booked against a repo with no
        // checkout can never be placed, so admitting we don't know is the useful answer.
        let k = known(&["bn"]);
        assert_eq!(
            repo_from_url("https://github.com/other/stranger/issues/1", &k),
            None
        );
    }

    #[test]
    fn nothing_resolvable_yields_none_rather_than_a_guess() {
        let k = known(&["bn"]);
        for url in [
            "",
            "   ",
            "https://example.com/bn/issues/1",
            "https://github.com/eduuh",
            "https://github.com/",
        ] {
            assert_eq!(repo_from_url(url, &k), None, "should not resolve {url:?}");
        }
    }

    #[test]
    fn repo_resolution_is_case_insensitive_and_normalizes_down() {
        let k = known(&["bn"]);
        assert_eq!(
            repo_from_url("https://github.com/Eduuh/BN/issues/577", &k).as_deref(),
            Some("bn")
        );
    }

    #[test]
    fn the_queue_source_is_the_dedupe_key() {
        let s = Suggestion::new("pr-queue", "577", "do the thing", "because", "");
        assert_eq!(s.queue_source(), format!("today:{}", s.id));
        assert_eq!(queue_source(&s.id), s.queue_source());
    }

    #[test]
    fn a_store_written_before_the_link_fields_existed_still_reads() {
        // The compatibility guarantee: every added field is `#[serde(default)]`, so an existing
        // suggestions.json is read unchanged rather than as an empty store.
        let old = r#"[{
            "id": "pr-queue-577",
            "title": "fix the flaky queue tick",
            "rationale": "surfaced by pr-queue",
            "source": "pr-queue",
            "source_key": "577",
            "url": "",
            "created_at": 100,
            "status": "accepted"
        }]"#;
        let list: Vec<Suggestion> = serde_json::from_str(old).expect("old store must still parse");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].status, SuggestionStatus::Accepted);
        assert!(list[0].task.is_empty());
        assert!(list[0].queued_id.is_empty());
        assert_eq!(list[0].queued_at, 0);
    }

    #[test]
    fn an_unqueued_suggestion_serializes_as_it_did_before() {
        // `skip_serializing_if` on every new field, so adding the tie does not rewrite the store for
        // suggestions that never reached the queue.
        let s = Suggestion::new("pr-queue", "577", "t", "r", "");
        let json = serde_json::to_string(&s).expect("serialize");
        for absent in ["task", "queued_repo", "queued_id", "queued_at"] {
            assert!(!json.contains(absent), "{absent} should be omitted: {json}");
        }
    }

    #[test]
    fn done_round_trips_and_is_not_open() {
        assert_eq!(
            SuggestionStatus::parse("done"),
            Some(SuggestionStatus::Done)
        );
        assert_eq!(SuggestionStatus::Done.as_str(), "done");
        assert!(!SuggestionStatus::Done.is_open());
        assert!(!SuggestionStatus::Dismissed.is_open());
        assert!(SuggestionStatus::Suggested.is_open());
        assert!(SuggestionStatus::Accepted.is_open());
    }
}
