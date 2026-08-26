//! State folding and the read lanes: open / completed / started items, outcome
//! rollups, and the auto-mode batch picker.

use super::model::{is_completion, severity_rank, AttnItem};
use super::store::{field_str, fold_item, merge_update};
use crate::events::{self, Event};
use serde::Serialize;
use serde_json::{Map, Value};

/// The lifecycle state of an attention item, folded from the latest event that referenced it.
/// Absence from the fold map means the item is still *open* (raised, never touched).
enum LifeState {
    /// Terminal — acked (done), removed (cancelled), or planned (promoted to a todo). Hidden from
    /// every lane.
    Terminal,
    /// Snoozed until `YYYY-MM-DD` — hidden only while that date is still in the future.
    Snoozed(String),
    /// In-flight, started at the given epoch secs — being worked on (out of the open list, into
    /// [`started_items`]).
    Started(i64),
    /// In-flight but finished, awaiting your review — entered at the given epoch secs, with the
    /// optional completion notes. Still visible (the needs-you lane) until acked.
    Done(i64, Option<String>),
}

/// Fold the event log into the latest [`LifeState`] per id, plus the merged `attn_update` field
/// overrides (latest value per field wins). Shared by [`open_items`] and [`started_items`] so the
/// two lanes agree on state. `read_all` is file order = chronological append, so the last matching
/// event per id wins (a snooze after an ack, a done after a started, and so on).
fn fold_states(
    events: &[Event],
) -> (
    std::collections::HashMap<String, LifeState>,
    std::collections::HashMap<String, Map<String, Value>>,
) {
    let mut states: std::collections::HashMap<String, LifeState> = Default::default();
    let mut overrides: std::collections::HashMap<String, Map<String, Value>> = Default::default();
    for ev in events {
        let Some(r) = field_str(ev, "ref") else {
            continue;
        };
        match ev.kind.as_str() {
            "attn_ack" | "attn_remove" | "attn_planned" | "attn_dispatched" => {
                states.insert(r, LifeState::Terminal);
            }
            "attn_snooze" => {
                if let Some(until) = field_str(ev, "until") {
                    states.insert(r, LifeState::Snoozed(until));
                }
            }
            "attn_started" => {
                states.insert(r, LifeState::Started(ev.ts));
            }
            "attn_done" => {
                states.insert(r, LifeState::Done(ev.ts, field_str(ev, "notes")));
            }
            "attn_update" => merge_update(overrides.entry(r).or_default(), ev),
            _ => {}
        }
    }
    (states, overrides)
}

/// Every *open* attention item, unsorted and unpartitioned — raised and not yet started, resolved,
/// or snoozed into the future. A lapsed snooze (its date on/before `today`) counts as open; a
/// started/done item does *not* — it lives in [`started_items`]. Any `attn_update` fields are
/// overlaid on the original `attn` event before the item is built. Callers split this into the
/// actionable [`open_items`] lane and the [`completed_items`] review lane.
pub(crate) fn open_raw(today: &str) -> Vec<AttnItem> {
    let events = events::read_all();
    let (states, overrides) = fold_states(&events);

    let mut out: Vec<AttnItem> = Vec::new();
    for ev in &events {
        if ev.kind != "attn" {
            continue;
        }
        let Some(id) = field_str(ev, "id") else {
            continue;
        };
        // Open == untouched, or a snooze whose date has passed. Terminal / started / done /
        // still-snoozed are all excluded from the open set.
        match states.get(&id) {
            None => {}
            Some(LifeState::Snoozed(until)) if until.as_str() <= today => {}
            _ => continue,
        }
        let ov = overrides.get(&id);
        out.push(fold_item(ev, id, ov));
    }
    out
}

/// The single *live* (non-terminal) attention item carrying `key`, if any. Unlike [`open_raw`],
/// this also sees items that were started, marked done-awaiting-review, or snoozed — they are all
/// still live, just out of the open lane. It's the basis of keyed-`add` idempotency across an
/// item's whole lifecycle: a re-firing keyed job (e.g. the merged-but-dirty `wt-stuck` notice)
/// refreshes this one record instead of stacking a duplicate the open-lane dedup can't see once
/// the item has been started (#534). Only a terminal ack/remove/planned frees the key for a fresh
/// occurrence. Latest matching `attn` event wins (chronological append order).
pub fn live_by_key(key: &str) -> Option<AttnItem> {
    let events = events::read_all();
    let (states, overrides) = fold_states(&events);
    let mut found = None;
    for ev in &events {
        if ev.kind != "attn" {
            continue;
        }
        let Some(id) = field_str(ev, "id") else {
            continue;
        };
        if matches!(states.get(&id), Some(LifeState::Terminal)) {
            continue;
        }
        let ov = overrides.get(&id);
        let item = fold_item(ev, id, ov);
        if item.key.as_deref() == Some(key) {
            found = Some(item);
        }
    }
    found
}

/// Every live item whose key starts with `prefix`, in append order.
///
/// The prefix scan behind cross-producer suppression: the deterministic producers namespace their
/// keys (`attn:cron-health:<job>`), so their live items are the canonical record of a subject and
/// can be recognised without hardcoding what subjects exist.
pub fn live_with_key_prefix(prefix: &str) -> Vec<AttnItem> {
    let events = events::read_all();
    let (states, overrides) = fold_states(&events);
    let mut out: Vec<AttnItem> = Vec::new();
    for ev in &events {
        if ev.kind != "attn" {
            continue;
        }
        let Some(id) = field_str(ev, "id") else {
            continue;
        };
        if matches!(states.get(&id), Some(LifeState::Terminal)) {
            continue;
        }
        let ov = overrides.get(&id);
        let item = fold_item(ev, id, ov);
        if item.key.as_deref().is_some_and(|k| k.starts_with(prefix)) {
            out.retain(|e: &AttnItem| e.key != item.key);
            out.push(item);
        }
    }
    out
}

/// The open attention items — the actionable "to start" lane: raised, needs-you, not yet started,
/// resolved, or snoozed into the future. Urgent-first then oldest-first. *Completions* (finished
/// work awaiting review, see [`is_completion`]) are excluded — they are outcomes, not actions, and
/// surface in [`completed_items`] instead. This is the single definition of "what belongs in the
/// attention list", so doctor / `bn next` / the badge all inherit the actionable-only rule.
pub fn open_items(today: &str) -> Vec<AttnItem> {
    let mut out: Vec<AttnItem> = open_raw(today)
        .into_iter()
        .filter(|i| !is_completion(i))
        .collect();
    out.sort_by(|a, b| {
        severity_rank(&b.severity)
            .cmp(&severity_rank(&a.severity))
            .then(a.created.cmp(&b.created))
    });
    out
}

/// The review inbox lane — completions (an agent's `bn agent done` push) that are finished and
/// awaiting your review, most-recent-first. These are *not* attention: they hold no action for you
/// beyond a look. Reviewing one is an [`ack`], which folds it terminal so it leaves this lane. The
/// source of truth is the same event log the completion was pushed to — this is a rendering view,
/// not a second store.
pub fn completed_items(today: &str) -> Vec<AttnItem> {
    let mut out: Vec<AttnItem> = open_raw(today).into_iter().filter(is_completion).collect();
    out.sort_by_key(|i| std::cmp::Reverse(i.created));
    out
}

/// One in-flight attention item — `mark_started` moved it off the open list and nothing has since
/// acked/removed it. `done` marks it finished-awaiting-review (an `attn_done` was recorded);
/// `since` is the epoch-secs moment it entered its current in-flight state, for the age badge and
/// the orphan-grace window. The live work status (working/idle/stuck/…) is derived by the caller
/// from the item's tmux window + agent — it is not stored here.
#[derive(Debug, Clone, Serialize)]
pub struct StartedItem {
    #[serde(flatten)]
    pub item: AttnItem,
    pub done: bool,
    pub since: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// The in-flight attention items — the "in progress / needs-you" lanes. Done-first (they await
/// your review), then most-recently-changed first. Field overrides from `attn_update` are applied,
/// same as [`open_items`].
pub fn started_items() -> Vec<StartedItem> {
    let events = events::read_all();
    let (states, overrides) = fold_states(&events);

    let mut out: Vec<StartedItem> = Vec::new();
    for ev in &events {
        if ev.kind != "attn" {
            continue;
        }
        let Some(id) = field_str(ev, "id") else {
            continue;
        };
        let (done, since, notes) = match states.get(&id) {
            Some(LifeState::Started(ts)) => (false, *ts, None),
            Some(LifeState::Done(ts, n)) => (true, *ts, n.clone()),
            _ => continue,
        };
        let ov = overrides.get(&id);
        out.push(StartedItem {
            item: fold_item(ev, id, ov),
            done,
            since,
            notes,
        });
    }
    out.sort_by(|a, b| b.done.cmp(&a.done).then(b.since.cmp(&a.since)));
    out
}

/// One raised attention item folded to its latest lifecycle outcome — for engagement analytics
/// (#268): which raised items actually got acted on vs left to rot. `resolution` is the kind of the
/// last lifecycle event that referenced it; `None` means still untouched-open.
#[derive(Debug, Clone, Serialize)]
pub struct AttnOutcome {
    pub id: String,
    /// Who raised it — a staff agent stamps `staff-<name>` here (see `bn_core::staff::job_name`).
    pub source: String,
    pub key: Option<String>,
    /// Epoch secs the item was raised (the `attn` event's ts).
    pub created: i64,
    /// Last transition: `ack` | `remove` | `planned` | `done` | `started` | `snooze`, or `None`
    /// while still open.
    pub resolution: Option<String>,
    /// Epoch secs of that transition, if any.
    pub resolved_ts: Option<i64>,
}

impl AttnOutcome {
    /// Acted on = engaged rather than ignored: acked (resolved), planned (promoted to a todo),
    /// dispatched (promoted to the orchestrate backlog), started, or done. A bare remove (cancelled)
    /// or snooze does not count — the item was dismissed or deferred, not acted on; still-open
    /// (`None`) is not acted on either.
    pub fn acted(&self) -> bool {
        matches!(
            self.resolution.as_deref(),
            Some("ack" | "planned" | "dispatched" | "started" | "done")
        )
    }
}

/// Fold every raised attention item to its latest lifecycle outcome. Unfiltered and unsorted —
/// callers window by `created`/`resolved_ts` and filter by `source` (e.g. staff engagement per
/// agent). Reads the whole event log once (file order = chronological append), so the last
/// lifecycle event per id wins.
pub fn outcomes() -> Vec<AttnOutcome> {
    let events = events::read_all();
    let mut base: std::collections::HashMap<String, (String, Option<String>, i64)> =
        Default::default();
    let mut last: std::collections::HashMap<String, (String, i64)> = Default::default();
    for ev in &events {
        match ev.kind.as_str() {
            "attn" => {
                if let Some(id) = field_str(ev, "id") {
                    let source = field_str(ev, "source").unwrap_or_default();
                    base.insert(id, (source, field_str(ev, "key"), ev.ts));
                }
            }
            "attn_ack" | "attn_remove" | "attn_planned" | "attn_done" | "attn_started"
            | "attn_snooze" | "attn_dispatched" => {
                if let Some(r) = field_str(ev, "ref") {
                    let kind = ev
                        .kind
                        .strip_prefix("attn_")
                        .unwrap_or(&ev.kind)
                        .to_string();
                    last.insert(r, (kind, ev.ts));
                }
            }
            _ => {}
        }
    }
    base.into_iter()
        .map(|(id, (source, key, created))| {
            let (resolution, resolved_ts) = match last.get(&id) {
                Some((k, ts)) => (Some(k.clone()), Some(*ts)),
                None => (None, None),
            };
            AttnOutcome {
                id,
                source,
                key,
                created,
                resolution,
                resolved_ts,
            }
        })
        .collect()
}

/// Whether an item may launch an agent unattended.
///
/// The trust boundary, now with two tiers (#1003): self-authored (`source: "manual"`) as before,
/// plus the narrower task tier (`source: "task"`) that additionally requires a passing SMART
/// verdict, a known repo, and a bn-authored prompt. A script/PR-sourced item is still never
/// auto-run even if it carries `mode=auto`. See [`super::trust`] for why the task tier exists and
/// why it is not simply `manual`.
fn is_auto_eligible(cfg: &crate::resolver::Config, i: &AttnItem, today: &str) -> bool {
    super::trust::auto_verdict(cfg, i, today).eligible()
}

/// The open, auto-eligible attention items — the promoted-todo queue that `bn attn promote` fuels
/// — in board order (urgent-first then oldest, same as [`open_items`]). Uncapped and with no
/// concurrency bookkeeping: the caller ranks and gates them (the night planner ranks by `bn next`,
/// then applies its allowlist / dirty / live-session / overlap / cap rails). Eligibility is
/// [`is_auto_eligible`], so the trust boundary (self-authored only) still holds.
pub fn open_auto_eligible(cfg: &crate::resolver::Config, today: &str) -> Vec<AttnItem> {
    open_items(today)
        .into_iter()
        .filter(|i| is_auto_eligible(cfg, i, today))
        .collect()
}

/// The eligible attention items the autonomous loop should start *now*, highest-priority first,
/// capped so that (already running + returned) never exceeds `max_running` — the concurrency knob.
/// Eligibility is [`is_auto_eligible`]. Returns empty while the running set is already at the cap.
/// A *done* item (awaiting your review) does NOT hold a concurrency slot, so an overnight queue
/// keeps draining task by task into done-awaiting-review instead of stalling. Branch-isolated: the
/// batch never repeats a repo/branch, nor picks one already occupied by a *running* auto agent, so
/// each concurrent run gets its own worktree/window and two agents never collide — e.g. two todos
/// promoted off the same branch note serialize instead of launching into one worktree at once.
/// `open_items` is urgent-first then oldest, so the batch is priority-ordered. `max_running` of 1
/// reproduces the historical one-at-a-time loop.
pub fn next_auto_batch(
    cfg: &crate::resolver::Config,
    today: &str,
    max_running: usize,
) -> Vec<AttnItem> {
    let started = started_items();
    // A *done* item frees its slot (awaits review, not a running agent), so only started-not-done
    // auto items count toward the cap.
    let running_auto = || {
        started
            .iter()
            .filter(|s| s.item.mode() == "auto" && !s.done)
    };
    let slots = max_running.saturating_sub(running_auto().count());
    if slots == 0 {
        return Vec::new();
    }
    // Repo/branch already occupied by a *running* auto agent — a new run must not reuse it, or two
    // agents would fight over one worktree/window. `insert` returning false also dedups the batch
    // itself, so two queued same-branch items never launch together.
    let mut taken: std::collections::HashSet<(String, String)> = running_auto()
        .filter_map(|s| Some((s.item.repo.clone()?, s.item.branch.clone()?)))
        .collect();
    let mut batch = Vec::new();
    for item in open_items(today) {
        if batch.len() >= slots {
            break;
        }
        if !is_auto_eligible(cfg, &item, today) {
            continue;
        }
        // is_auto_eligible guarantees repo and branch are Some.
        let (Some(repo), Some(branch)) = (item.repo.clone(), item.branch.clone()) else {
            continue;
        };
        if taken.insert((repo, branch)) {
            batch.push(item);
        }
    }
    batch
}

/// The single next attention item the autonomous loop would start under a `max_running` cap — the
/// head of [`next_auto_batch`]. `None` when the loop is at its concurrency cap or nothing is
/// eligible. Powers the `bn attn next-auto` dry-run.
pub fn next_auto(
    cfg: &crate::resolver::Config,
    today: &str,
    max_running: usize,
) -> Option<AttnItem> {
    next_auto_batch(cfg, today, max_running).into_iter().next()
}
