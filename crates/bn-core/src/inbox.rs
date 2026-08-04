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


