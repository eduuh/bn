//! Pausing the queue — the stop button, and the several smaller stop buttons next to it.
//!
//! State is two append-only events in the same log as everything else (`queue_paused` /
//! `queue_resumed`), mirroring [`crate::afk`] and [`crate::focus`]. No new store, and it survives a
//! restart because the log is on disk. What this generalizes over `afk` is a **scope key**, so
//! several independent pauses coexist: stopping one misbehaving producer without stopping the
//! machine is the thing that actually happens at 2am.
//!
//! Two decisions worth knowing before reading the code:
//!
//! * **Expiry is folded at read time, not fired by a timer.** A machine that was asleep past a
//!   pause's `until` wakes up running, rather than waiting for a timer that never fired to
//!   un-stick it.
//! * **A pause stops admission, not execution.** Work already running finishes normally. Killing is
//!   a separate, explicit verb (`bn agent kill`), so there is deliberately no `--kill` flag here —
//!   a stop button that also destroys in-flight work is one nobody dares press.
use crate::events::{self, Event};
use crate::queue::kind::{BacklogKind, WorkClass};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The event kinds this module folds. Public so `bn doctor` and the event reader can name them.
pub const PAUSED_EVENT: &str = "queue_paused";
/// See [`PAUSED_EVENT`].
pub const RESUMED_EVENT: &str = "queue_resumed";

/// What a pause covers.
///
/// [`All`](PauseScope::All) is the big red button; the narrower scopes exist so one bad producer,
/// one sick node or one noisy repo can be stopped on its own.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "kebab-case")]
pub enum PauseScope {
    /// Every kind, every lane, every node, every repo.
    All,
    /// One work kind — e.g. hold PR review while the reviewer prompt is being changed.
    Kind { kind: BacklogKind },
    /// One concurrency lane.
    Lane { lane: WorkClass },
    /// One node — e.g. a box you are about to reboot.
    Node { node_id: String },
    /// One repo.
    Repo { repo: String },
}

impl PauseScope {
    /// The stable key a `queue_paused`/`queue_resumed` pair is matched on, and the `--scope`
    /// argument form: `all`, `kind:cron`, `lane:coding`, `node:fedora-box`, `repo:bn`.
    pub fn key(&self) -> String {
        match self {
            PauseScope::All => "all".to_string(),
            PauseScope::Kind { kind } => format!("kind:{}", kind.label()),
            PauseScope::Lane { lane } => format!("lane:{}", lane.label()),
            PauseScope::Node { node_id } => format!("node:{node_id}"),
            PauseScope::Repo { repo } => format!("repo:{repo}"),
        }
    }

    /// Parse a `--scope` argument. `None` for an unrecognized axis or an unknown kind/lane, so a
    /// typo fails loudly rather than silently pausing nothing (or everything).
    pub fn parse(s: &str) -> Option<PauseScope> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("all") {
            return Some(PauseScope::All);
        }
        let (axis, value) = s.split_once(':')?;
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        match axis.trim().to_ascii_lowercase().as_str() {
            "kind" => BacklogKind::parse(value).map(|kind| PauseScope::Kind { kind }),
            "lane" => match value.to_ascii_lowercase().as_str() {
                "coding" => Some(PauseScope::Lane {
                    lane: WorkClass::Coding,
                }),
                "research" => Some(PauseScope::Lane {
                    lane: WorkClass::Research,
                }),
                _ => None,
            },
            "node" => Some(PauseScope::Node {
                node_id: value.to_string(),
            }),
            "repo" => Some(PauseScope::Repo {
                repo: value.to_string(),
            }),
            _ => None,
        }
    }

    /// Whether this scope covers a candidate. [`All`](PauseScope::All) covers everything; the rest
    /// match only their own axis. A candidate with no node (the implicit local box) is never
    /// covered by a node-scoped pause — pausing a named node must not accidentally stop local work.
    pub fn covers(
        &self,
        repo: &str,
        kind: BacklogKind,
        lane: WorkClass,
        node: Option<&str>,
    ) -> bool {
        match self {
            PauseScope::All => true,
            PauseScope::Kind { kind: want } => *want == kind,
            PauseScope::Lane { lane: want } => *want == lane,
            PauseScope::Node { node_id } => node.is_some_and(|n| n.eq_ignore_ascii_case(node_id)),
            PauseScope::Repo { repo: want } => want.eq_ignore_ascii_case(repo),
        }
    }
}

/// One pause in force.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pause {
    pub scope: PauseScope,
    /// Epoch secs the pause began.
    pub since: i64,
    /// Free text: why. Shown by `bn queue status`, on the timeline banner, and on every deferred
    /// row it caused — so a pause is never a silent stall.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// Epoch secs it auto-expires at. `None` = until explicitly resumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
}

impl Pause {
    /// Whether this pause has expired as of `now`.
    pub fn expired(&self, now: i64) -> bool {
        self.until.is_some_and(|until| now >= until)
    }
}

/// Every pause in force — the tick's gate input and the status surface's whole answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PauseState {
    pub pauses: Vec<Pause>,
}

impl PauseState {
    /// Fold `queue_paused` / `queue_resumed` into the set in force at `now`.
    ///
    /// Log-structured, like the backlog: the latest event per [`PauseScope::key`] wins, and a pause
    /// whose `until` has passed is dropped here rather than by a timer. Pure over the event list,
    /// so the whole gate is decidable from a captured snapshot.
    pub fn fold(events: &[Event], now: i64) -> PauseState {
        let mut open: Vec<(String, Option<Pause>)> = Vec::new();
        for event in events {
            let is_pause = event.kind == PAUSED_EVENT;
            if !is_pause && event.kind != RESUMED_EVENT {
                continue;
            }
            let Some(scope) = event
                .fields
                .get("scope")
                .and_then(Value::as_str)
                .and_then(PauseScope::parse)
            else {
                continue;
            };
            let key = scope.key();
            let entry = if is_pause {
                Some(Pause {
                    scope,
                    since: event.ts,
                    reason: event
                        .fields
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    until: event.fields.get("until").and_then(Value::as_i64),
                })
            } else {
                None
            };
            match open.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = entry,
                None => open.push((key, entry)),
            }
        }
        PauseState {
            pauses: open
                .into_iter()
                .filter_map(|(_, pause)| pause)
                .filter(|pause| !pause.expired(now))
                .collect(),
        }
    }

    /// Read the log and fold it — the IO-bound convenience over [`fold`](PauseState::fold).
    pub fn load(now: i64) -> PauseState {
        PauseState::fold(&events::read_all(), now)
    }

    /// The pause blocking this candidate, if any — the tick's single question.
    ///
    /// Checks [`All`](PauseScope::All) first so the reported scope is the **broadest** that
    /// applies: told "the queue is paused", nobody should have to discover that separately.
    pub fn blocks(
        &self,
        repo: &str,
        kind: BacklogKind,
        lane: WorkClass,
        node: Option<&str>,
    ) -> Option<&Pause> {
        self.pauses
            .iter()
            .find(|p| p.scope == PauseScope::All)
            .or_else(|| {
                self.pauses
                    .iter()
                    .find(|p| p.scope.covers(repo, kind, lane, node))
            })
    }

    /// Whether anything at all is paused.
    pub fn is_paused(&self) -> bool {
        !self.pauses.is_empty()
    }

    /// Whether *everything* is stopped — the banner's "the queue is paused" versus "3 scopes
    /// paused".
    pub fn is_fully_paused(&self) -> bool {
        self.pauses.iter().any(|p| p.scope == PauseScope::All)
    }

    /// The pause on exactly `scope`, if one is in force.
    pub fn get(&self, scope: &PauseScope) -> Option<&Pause> {
        self.pauses.iter().find(|p| p.scope == *scope)
    }
}

/// Append `queue_paused`.
///
/// A second pause on the same scope re-opens it with the new reason/expiry (latest wins on read),
/// so calling it repeatedly is safe — the [`crate::afk::start`] contract.
pub fn pause(
    scope: &PauseScope,
    reason: &str,
    until: Option<i64>,
    now: i64,
) -> std::io::Result<()> {
    let mut fields = Map::new();
    fields.insert("scope".to_string(), Value::String(scope.key()));
    if !reason.trim().is_empty() {
        fields.insert(
            "reason".to_string(),
            Value::String(reason.trim().to_string()),
        );
    }
    if let Some(until) = until {
        fields.insert("until".to_string(), Value::from(until));
    }
    events::append_at(PAUSED_EVENT, now, fields)
}

/// Append `queue_resumed`.
///
/// Returns the epoch the pause began (for the "paused for 4h" line), or `None` when that scope was
/// not paused — so `bn queue resume` can say so without writing a dangling event. The
/// [`crate::afk::end`] contract.
pub fn resume(scope: &PauseScope, now: i64) -> std::io::Result<Option<i64>> {
    let Some(since) = PauseState::load(now).get(scope).map(|p| p.since) else {
        return Ok(None);
    };
    let mut fields = Map::new();
    fields.insert("scope".to_string(), Value::String(scope.key()));
    events::append_at(RESUMED_EVENT, now, fields)?;
    Ok(Some(since))
}
