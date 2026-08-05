//! Global capture inbox (`bn q`) — a zero-decision landing zone for stray thoughts.
//!
//! One synced file per machine (`<personal_root>/<machine>/inbox.md`), mirroring the daily
//! scheduled-queue format (fenced-YAML frontmatter over a regenerated title body) so the
//! existing frontmatter parser is reused. Capture stamps a timestamp plus a best-effort
//! repo/branch *hint* ([`crate::resolver::resolve_hint`] — git-free, so capture never pays
//! for a subprocess); routing is deferred to the drain (rollover / focus end), when
//! deciding is cheap. The inbox is never a home — doctor flags items that sit too long.

use crate::attention::{self, NewAttn};
use crate::commands::{self, CmdError};
use crate::daily;
use crate::resolver::{Config, Resolved};
use crate::store::StoreError;
use serde::{Deserialize, Serialize};

/// Id prefix for inbox captures (`q1`, `q2`, …).
const Q_PREFIX: char = 'q';

/// One captured thought. `created` is the capture date (drives the doctor staleness check),
/// `at` the wall-clock `HH:MM`; `repo`/`branch` record where the thought struck — context
/// for the drain, never a routing decision by themselves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxItem {
    pub id: String,
    pub text: String,
    pub created: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// The inbox file's frontmatter: just the list of pending captures, in capture order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct InboxFront {
    #[serde(default)]
    items: Vec<InboxItem>,
}

/// Result of [`capture`]: the banked item's id/text and the hint it was stamped with.
#[derive(Debug)]
pub struct CaptureOutcome {
    pub id: String,
    pub text: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
}

/// Read the inbox (empty if the file is missing or unreadable).
pub fn read(cfg: &Config) -> Vec<InboxItem> {
    let Ok(text) = std::fs::read_to_string(cfg.inbox_file()) else {
        return vec![];
    };
    crate::store::parse_frontmatter::<InboxFront>(&text)
        .map(|f| f.items)
        .unwrap_or_default()
}

/// Write the inbox, preserving capture order.
pub fn write(cfg: &Config, items: &[InboxItem]) -> Result<(), StoreError> {
    let front = InboxFront {
        items: items.to_vec(),
    };
    let fm = serde_yaml::to_string(&front).map_err(|e| StoreError::Parse(e.to_string()))?;
    let text = format!("---\n{fm}---\n\n# Inbox\n");
    let path = cfg.inbox_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, text)?;
    Ok(())
}

/// Bank a thought: append it with a fresh `q<N>` id, today's date, the wall-clock `at`, and
/// the repo/branch hint. No git ops, no prompts — the whole point is that capture costs
/// nothing beyond one file write.
pub fn capture(
    cfg: &Config,
    text: &str,
    hint: Option<&Resolved>,
    today: &str,
    at: Option<String>,
) -> Result<CaptureOutcome, CmdError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(CmdError::EmptyText);
    }
    let mut items = read(cfg);
    let id = next_q_id(&items);
    let (repo, branch) = match hint {
        Some(r) => (Some(r.repo.clone()), Some(r.branch.clone())),
        None => (None, None),
    };
    items.push(InboxItem {
        id: id.clone(),
        text: text.to_string(),
        created: today.to_string(),
        at,
        repo: repo.clone(),
        branch: branch.clone(),
    });
    write(cfg, &items)?;
    Ok(CaptureOutcome {
        id,
        text: text.to_string(),
        repo,
        branch,
    })
}

/// Where a capture can be routed out of the inbox at drain time.
#[derive(Debug, Clone)]
pub enum Route {
    /// Todo on a branch note.
    Todo(Resolved),
    /// Research item on a branch note.
    Research(Resolved),
    /// Goal on today's daily list.
    Goal,
    /// Attention-queue item.
    Attn,
    /// Discard.
    Drop,
}

/// One capture routed out of the inbox: where it went, for the drain report.
#[derive(Debug, Clone, Serialize)]
pub struct RoutedOutcome {
    pub id: String,
    pub text: String,
    pub dest: String,
}

/// Result of [`drain_auto`]: what routed, and how many captures stayed for human triage.
#[derive(Debug)]
pub struct AutoDrainOutcome {
    pub routed: Vec<RoutedOutcome>,
    pub left: usize,
}

/// Route one capture to its sink and remove it from the inbox. The sink write happens first,
/// so a failure leaves the capture in place — worst case it's re-routed, never lost.
pub fn route(
    cfg: &Config,
    item_id: &str,
    to: &Route,
    today: &str,
) -> Result<RoutedOutcome, CmdError> {
    let mut items = read(cfg);
    let idx = items
        .iter()
        .position(|i| i.id == item_id)
        .ok_or_else(|| CmdError::NotFound(item_id.to_string()))?;
    let item = items[idx].clone();
    let dest = deliver(cfg, &item, to, today)?;
    items.remove(idx);
    write(cfg, &items)?;
    Ok(RoutedOutcome {
        id: item.id,
        text: item.text,
        dest,
    })
}

/// Non-interactive drain (rollover, `bn q drain --auto`): route only the unambiguous
/// captures — one whose repo/branch hint points at a branch note that exists and is still
/// active becomes that branch's todo — and leave everything else for interactive triage.
/// Retry-safe: the todo add dedups by text, so a capture delivered but not yet removed
/// (a crash between the two writes) doesn't double up.
pub fn drain_auto(cfg: &Config, today: &str) -> Result<AutoDrainOutcome, CmdError> {
    let items = read(cfg);
    let mut routed = Vec::new();
    let mut remaining = Vec::new();
    for item in items {
        match auto_target(cfg, &item) {
            Some(ctx) => {
                let dest = deliver(cfg, &item, &Route::Todo(ctx), today)?;
                routed.push(RoutedOutcome {
                    id: item.id,
                    text: item.text,
                    dest,
                });
            }
            None => remaining.push(item),
        }
    }
    let left = remaining.len();
    if !routed.is_empty() {
        write(cfg, &remaining)?;
    }
    Ok(AutoDrainOutcome { routed, left })
}

/// The one auto-routable shape: a capture hinted at a branch note that exists and is still
/// `active`. Everything else (no hint, note missing, note closed) needs a human decision.
fn auto_target(cfg: &Config, item: &InboxItem) -> Option<Resolved> {
    let (Some(repo), Some(branch)) = (&item.repo, &item.branch) else {
        return None;
    };
    let ctx = Resolved {
        repo: repo.clone(),
        branch: branch.clone(),
    };
    let note = crate::store::read(&cfg.note_file(&ctx)).ok()?;
    (note.front.status == "active").then_some(ctx)
}

/// Write a capture into its sink; returns the human-readable destination label.
fn deliver(cfg: &Config, item: &InboxItem, to: &Route, today: &str) -> Result<String, CmdError> {
    Ok(match to {
        Route::Todo(ctx) => {
            commands::add(cfg, ctx, "todo", &item.text, Some(&item.text), today)?;
            format!("todo on {}/{}", ctx.repo, ctx.branch)
        }
        Route::Research(ctx) => {
            commands::add(cfg, ctx, "research", &item.text, Some(&item.text), today)?;
            format!("research on {}/{}", ctx.repo, ctx.branch)
        }
        Route::Goal => {
            daily::add(cfg, today, &item.text, None, None)?;
            "today's goals".to_string()
        }
        Route::Attn => {
            attention::add_new(
                &NewAttn {
                    source: "inbox",
                    title: &item.text,
                    severity: "info",
                    repo: item.repo.as_deref(),
                    branch: item.branch.as_deref(),
                    link: None,
                    key: None,
                    detail: None,
                    mode: None,
                    prompt: None,
                    ..Default::default()
                },
                today,
            )
            .map_err(|e| CmdError::Store(StoreError::from(e)))?;
            "attention".to_string()
        }
        Route::Drop => "dropped".to_string(),
    })
}

/// Next `q<N>` id. Tracks max (not count), so ids of drained items are never reused.
fn next_q_id(items: &[InboxItem]) -> String {
    let max = items
        .iter()
        .filter_map(|i| i.id.strip_prefix(Q_PREFIX))
        .filter(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|n| n.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    format!("{Q_PREFIX}{}", max + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    fn hint(repo: &str, branch: &str) -> Resolved {
        Resolved {
            repo: repo.into(),
            branch: branch.into(),
        }
    }

    #[test]
    fn capture_creates_file_and_allocates_ids() {
        let dir = tempfile::tempdir().unwrap();
        let c = test_support::config(dir.path());
        let a = capture(&c, "first thought", None, "2026-07-05", None).unwrap();
        assert_eq!(a.id, "q1");
        let b = capture(&c, "second thought", None, "2026-07-05", None).unwrap();
        assert_eq!(b.id, "q2");
        let items = read(&c);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "first thought");
        assert_eq!(items[0].created, "2026-07-05");
    }

    #[test]
    fn capture_stamps_hint_and_time() {
        let dir = tempfile::tempdir().unwrap();
        let c = test_support::config(dir.path());
        let h = hint("bn", "feat-x");
        let a = capture(
            &c,
            "check attn ingest",
            Some(&h),
            "2026-07-05",
            Some("09:41".into()),
        )
        .unwrap();
        assert_eq!(a.repo.as_deref(), Some("bn"));
        assert_eq!(a.branch.as_deref(), Some("feat-x"));
        let items = read(&c);
        assert_eq!(items[0].repo.as_deref(), Some("bn"));
        assert_eq!(items[0].branch.as_deref(), Some("feat-x"));
        assert_eq!(items[0].at.as_deref(), Some("09:41"));
    }

    #[test]
    fn hintless_capture_omits_hint_keys_from_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let c = test_support::config(dir.path());
        capture(&c, "desk-free thought", None, "2026-07-05", None).unwrap();
        let text = std::fs::read_to_string(c.inbox_file()).unwrap();
        assert!(
            !text.contains("repo:"),
            "unhinted item leaked repo key:\n{text}"
        );
        assert!(
            !text.contains("branch:"),
            "unhinted item leaked branch key:\n{text}"
        );
        assert!(
            !text.contains("at:"),
            "unhinted item leaked at key:\n{text}"
        );
    }

    #[test]
    fn file_format_is_fenced_yaml_over_title() {
        let dir = tempfile::tempdir().unwrap();
        let c = test_support::config(dir.path());
        capture(&c, "x", None, "2026-07-05", None).unwrap();
        let text = std::fs::read_to_string(c.inbox_file()).unwrap();
        assert!(text.starts_with("---\n"));
        assert!(
            text.ends_with("---\n\n# Inbox\n"),
            "unexpected tail:\n{text}"
        );
        // Lands in the per-machine subtree so it rides the notes sync.
        assert!(c.inbox_file().starts_with(dir.path().join("testhost")));
    }

    #[test]
    fn empty_or_whitespace_text_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let c = test_support::config(dir.path());
        assert!(matches!(
            capture(&c, "   ", None, "2026-07-05", None),
            Err(CmdError::EmptyText)
        ));
        assert!(!c.inbox_file().exists());
    }

    #[test]
    fn read_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let c = test_support::config(dir.path());
        assert!(read(&c).is_empty());
    }

    /// Create an (active) branch note for `ctx` by adding a seed todo to it.
    fn seed_note(cfg: &Config, ctx: &Resolved) {
        commands::add(cfg, ctx, "todo", "seed", None, "2026-07-01").unwrap();
    }

    #[test]
    fn drain_auto_routes_only_hints_at_active_notes() {
        let dir = tempfile::tempdir().unwrap();
        let c = test_support::config(dir.path());
        let live = hint("bn", "feat-live");
        let closed = hint("bn", "feat-closed");
        seed_note(&c, &live);
        seed_note(&c, &closed);
        commands::set_status(&c, &closed, "closed").unwrap();

        capture(&c, "routable", Some(&live), "2026-07-05", None).unwrap();
        capture(&c, "note closed", Some(&closed), "2026-07-05", None).unwrap();
        capture(
            &c,
            "note missing",
            Some(&hint("bn", "gone")),
            "2026-07-05",
            None,
        )
        .unwrap();
        capture(&c, "no hint", None, "2026-07-05", None).unwrap();

        let out = drain_auto(&c, "2026-07-05").unwrap();
        assert_eq!(out.routed.len(), 1);
        assert_eq!(out.routed[0].text, "routable");
        assert_eq!(out.routed[0].dest, "todo on bn/feat-live");
        assert_eq!(out.left, 3);

        // The routable capture became an open todo on the hinted note …
        let note = crate::store::read(&c.note_file(&live)).unwrap();
        assert!(note.front.todos.iter().any(|t| t.text == "routable"));
        // … and only the ambiguous three remain in the inbox.
        let left: Vec<_> = read(&c).into_iter().map(|i| i.text).collect();
        assert_eq!(left, ["note closed", "note missing", "no hint"]);
    }

    #[test]
    fn drain_auto_retry_never_duplicates_the_todo() {
        let dir = tempfile::tempdir().unwrap();
        let c = test_support::config(dir.path());
        let live = hint("bn", "feat-live");
        seed_note(&c, &live);
        capture(&c, "routable", Some(&live), "2026-07-05", None).unwrap();
        drain_auto(&c, "2026-07-05").unwrap();
        // Simulate a crash between deliver and inbox rewrite: the capture is back, drained again.
        capture(&c, "routable", Some(&live), "2026-07-05", None).unwrap();
        drain_auto(&c, "2026-07-05").unwrap();
        let note = crate::store::read(&c.note_file(&live)).unwrap();
        assert_eq!(
            note.front
                .todos
                .iter()
                .filter(|t| t.text == "routable")
                .count(),
            1
        );
    }

    #[test]
    fn route_goal_and_drop_remove_from_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let c = test_support::config(dir.path());
        capture(&c, "a goal", None, "2026-07-05", None).unwrap();
        capture(&c, "noise", None, "2026-07-05", None).unwrap();

        let g = route(&c, "q1", &Route::Goal, "2026-07-05").unwrap();
        assert_eq!(g.dest, "today's goals");
        let day = daily::read(&c.daily_file("2026-07-05")).unwrap();
        assert!(day.front.goals.iter().any(|i| i.text == "a goal"));

        let d = route(&c, "q2", &Route::Drop, "2026-07-05").unwrap();
        assert_eq!(d.dest, "dropped");
        assert!(read(&c).is_empty());
    }

    #[test]
    fn route_unknown_id_is_not_found_and_inbox_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let c = test_support::config(dir.path());
        capture(&c, "keep me", None, "2026-07-05", None).unwrap();
        assert!(matches!(
            route(&c, "q9", &Route::Drop, "2026-07-05"),
            Err(CmdError::NotFound(_))
        ));
        assert_eq!(read(&c).len(), 1);
    }

    #[test]
    fn route_attn_lands_in_attention_queue() {
        let dir = tempfile::tempdir().unwrap();
        let c = test_support::config(dir.path());
        test_support::with_state_dir(|| {
            let h = hint("bn", "feat-x");
            capture(&c, "flaky test?", Some(&h), "2026-07-05", None).unwrap();
            let r = route(&c, "q1", &Route::Attn, "2026-07-05").unwrap();
            assert_eq!(r.dest, "attention");
            let open = attention::open_items("2026-07-05");
            assert!(open
                .iter()
                .any(|i| i.title == "flaky test?" && i.source == "inbox"));
            assert!(read(&c).is_empty());
        });
    }

    #[test]
    fn ids_track_max_never_reused_after_drain() {
        let dir = tempfile::tempdir().unwrap();
        let c = test_support::config(dir.path());
        capture(&c, "one", None, "2026-07-05", None).unwrap();
        capture(&c, "two", None, "2026-07-05", None).unwrap();
        // Drain q1: q2 remains, next capture must be q3 (not a recycled q1).
        let items: Vec<_> = read(&c).into_iter().filter(|i| i.id != "q1").collect();
        write(&c, &items).unwrap();
        let n = capture(&c, "three", None, "2026-07-05", None).unwrap();
        assert_eq!(n.id, "q3");
    }
}
