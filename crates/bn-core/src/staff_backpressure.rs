//! Staff backpressure (#269): degrade toward silence, not noise.
//!
//! The staff framework (`bn staff`, see [`crate::staff`]) assumes you read the brief, do the
//! retro, review the night PRs. When you stop, the naive system turns into guilt: unread counts
//! climb, attention items pile up, `urgent` loses meaning. Attention inflation is the failure mode
//! that kills a system like this. The rule is **degrade toward silence, not noise** — three
//! structural throttles, all riding the *existing* staff/attention plumbing (no new store, no new
//! sink, focus-mode semantics untouched):
//!
//! 1. **Unread-streak self-throttle** ([`throttle_gate`]). An agent whose most-recent
//!    [`THROTTLE_UNREAD_STREAK`] cards are *all* unread skips its next run instead of writing yet
//!    another card onto the ignore stack, and posts one line saying so — *once*. While the streak
//!    holds the agent stays silent (never two pause notices in a row); reading or rating any of
//!    those cards shortens the streak and resumes it on the next run. The announce marker is
//!    machine-local, beside the read-marks.
//!
//! 2. **Per-source attention cap** ([`enforce_source_cap`]). At most [`MAX_OPEN_ATTN_PER_SOURCE`]
//!    open attention items may stand per `staff-<agent>` source. Before a new one lands, the
//!    *oldest info-severity* items from that source auto-resolve to make room — stale FYIs expire
//!    so genuine signal isn't buried. Warn/urgent items are never auto-resolved: the cap sheds
//!    noise, not signal, so a source pinned to the cap by real warnings simply stops shedding and
//!    the new item still lands.
//!
//! 3. **Urgent severity budget** ([`urgent_budget_severity`]). The framework caps `urgent` at
//!    [`MAX_URGENT_PER_DAY`] per agent per day: once spent, a further `urgent` is admitted as
//!    `warn`. Severity discipline is enforced structurally, not by trusting the prompt — the
//!    Interviewer's "escalate once, then drop" generalized to the whole roster.

use crate::attention::AttnItem;
use crate::resolver::{state_dir, Config};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// How many of an agent's most-recent cards must be unread, back-to-back, before it self-throttles
/// its next run. Three consecutive ignored cards is a clear "you've stopped reading me" signal
/// without over-reacting to a single skipped day.
pub const THROTTLE_UNREAD_STREAK: usize = 3;

/// The most open attention items one `staff-<agent>` source may hold. Past it, the oldest
/// info-severity items auto-resolve before a new one lands (see [`enforce_source_cap`]).
pub const MAX_OPEN_ATTN_PER_SOURCE: usize = 5;

/// The most `urgent` attention items one agent may raise per day. Beyond it, a would-be `urgent`
/// is admitted as `warn` (see [`urgent_budget_severity`]).
pub const MAX_URGENT_PER_DAY: usize = 1;

// --- unread-streak self-throttle -----------------------------------------------------------------

fn state_path() -> PathBuf {
    state_dir().join("staff").join("backpressure.json")
}

/// Per-agent throttle bookkeeping — whether the current unread-streak pause has already posted its
/// one-line notice. Machine-local (state dir), beside the read-marks and ratings; never synced.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ThrottleState {
    /// agent name → the pause notice for its current unread streak has been announced.
    #[serde(default)]
    announced: HashMap<String, bool>,
}

fn load_state() -> ThrottleState {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_state(state: &ThrottleState) -> std::io::Result<()> {
    let path = state_path();
    crate::store::save_json_atomic(&path, state)
}

/// The count of an agent's most-recent cards that are unread, back-to-back from the newest. Zero
/// when the newest card is already read (or the agent has no cards). Reads the same card store +
/// read-marks the feed does, so reading/rating any card in the streak immediately shortens it.
pub fn unread_streak(cfg: &Config, agent: &str) -> usize {
    let mut cards: Vec<crate::staff::CardRef> = crate::staff::list_cards(cfg)
        .into_iter()
        .filter(|c| c.agent == agent)
        .collect();
    // `list_cards` is unread-first; re-sort strictly newest-first for a true consecutive streak.
    cards.sort_by(|a, b| b.date.cmp(&a.date));
    cards.iter().take_while(|c| !c.read).count()
}

/// The pause decision from a streak length and whether this episode's notice already posted.
/// `(paused, announce)`: paused once the streak reaches [`THROTTLE_UNREAD_STREAK`]; announce only
/// on the *first* paused run of an episode — never two notices in a row. Pure, so the threshold
/// and announce-once logic are tested without touching the card store or state file.
pub fn decide(streak: usize, already_announced: bool) -> (bool, bool) {
    let paused = streak >= THROTTLE_UNREAD_STREAK;
    (paused, paused && !already_announced)
}

/// One self-throttle decision for a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Throttle {
    /// How many consecutive newest cards are unread — the streak that triggered the pause.
    pub streak: usize,
    /// Post the one-line pause notice for this run (the first paused run of the episode). Later
    /// paused runs leave this `false` so the agent goes quiet — "never two pause notices in a row".
    pub announce: bool,
}

impl Throttle {
    /// The one-line pause notice — the "posts one line saying so" surface (#269). Names the streak
    /// and how to resume; the caller prefixes the agent (e.g. `staff mentor: <notice>`). Reading
    /// (or rating) any card in the streak shortens it, so `bn staff feed <agent>` resumes.
    pub fn notice(&self, agent: &str) -> String {
        format!(
            "pausing — {} unread card{}; `bn staff feed {agent}` to resume",
            self.streak,
            if self.streak == 1 { "" } else { "s" },
        )
    }
}

/// The self-throttle gate, called at the top of a run. Returns `Some(throttle)` when the agent
/// should skip this run because its recent cards are all unread; `None` to proceed. Persists the
/// announce marker so the pause notice posts exactly once per unread streak, and clears it the
/// moment the streak breaks (a read/rate drops it below threshold) so a later streak announces
/// afresh.
pub fn throttle_gate(cfg: &Config, agent: &str, _today: &str) -> Option<Throttle> {
    let streak = unread_streak(cfg, agent);
    let mut state = load_state();
    let already = state.announced.get(agent).copied().unwrap_or(false);
    let (paused, announce) = decide(streak, already);
    if !paused {
        // Episode over (or never started) — forget any prior announce so the next streak speaks up.
        if state.announced.remove(agent).is_some() {
            crate::util::best_effort(save_state(&state), "staff: clear throttle marker");
        }
        return None;
    }
    if announce {
        state.announced.insert(agent.to_string(), true);
        crate::util::best_effort(save_state(&state), "staff: set throttle marker");
    }
    Some(Throttle { streak, announce })
}

// --- per-source attention cap --------------------------------------------------------------------

/// The ids to auto-resolve so `items` (one source's open attention set) drops below the cap:
/// oldest info-severity first, and only info. Pure — the I/O wrapper [`enforce_source_cap`] acks
/// them. Returns empty when the source is already under the cap or nothing is sheddable (all
/// remaining items are warn/urgent, which the cap never drops).
fn cap_victims(items: &[AttnItem]) -> Vec<String> {
    if items.len() < MAX_OPEN_ATTN_PER_SOURCE {
        return Vec::new();
    }
    // Shed down to one below the cap, leaving room for the incoming item to land at the cap.
    let need = items.len() + 1 - MAX_OPEN_ATTN_PER_SOURCE;
    let mut info: Vec<&AttnItem> = items.iter().filter(|i| i.severity == "info").collect();
    info.sort_by_key(|i| i.created); // oldest first; stable, so equal timestamps keep raise order
    info.into_iter().take(need).map(|i| i.id.clone()).collect()
}

/// Enforce the per-source open-attention cap before a new `staff-<agent>` item lands: auto-resolve
/// the oldest info-severity items from `source` until it sits below [`MAX_OPEN_ATTN_PER_SOURCE`],
/// so the incoming item lands at the cap rather than above it. Warn/urgent items are never
/// auto-resolved — the cap sheds stale FYIs, not genuine signal, so a source held at the cap by
/// real warnings simply stops shedding (the new item still lands). Returns how many items were
/// auto-resolved. Best-effort: a resolve error is logged, never fatal to routing.
pub fn enforce_source_cap(source: &str, today: &str) -> usize {
    let open: Vec<AttnItem> = crate::attention::open_items(today)
        .into_iter()
        .filter(|i| i.source == source)
        .collect();
    let mut resolved = 0;
    for id in cap_victims(&open) {
        match crate::attention::ack(&id) {
            Ok(true) => resolved += 1,
            Ok(false) => {}
            Err(e) => {
                crate::util::best_effort(Err::<(), _>(e), "staff: auto-resolve capped attn item")
            }
        }
    }
    resolved
}

// --- urgent severity budget ----------------------------------------------------------------------

/// The severity to actually admit for a would-be attention item, applying the per-agent daily
/// `urgent` budget. A non-urgent request passes through unchanged. An `urgent` request is admitted
/// as `urgent` only while the agent is under [`MAX_URGENT_PER_DAY`] urgents raised today; past the
/// budget it degrades to `warn` — severity discipline enforced structurally (#269), not by
/// trusting the prompt.
pub fn urgent_budget_severity(agent: &str, requested: &str, today: &str) -> String {
    if requested != "urgent" {
        return requested.to_string();
    }
    let source = crate::staff::job_name(agent);
    if urgent_raised_today(&source, today) < MAX_URGENT_PER_DAY {
        "urgent".to_string()
    } else {
        "warn".to_string()
    }
}

/// Count of open `urgent` attention items from `source` raised today — the day's spent urgent
/// budget. Reads the live attention set (no parallel counter to drift): within one card's routing
/// each admitted urgent is immediately visible to the next item, and a once-daily agent's urgents
/// all fall in the same day window. Windowed by `created >= today's midnight`, mirroring the
/// engagement review's windowing.
fn urgent_raised_today(source: &str, today: &str) -> usize {
    let start = crate::util::epoch_day(today).unwrap_or(0) * 86_400;
    crate::attention::open_items(today)
        .iter()
        .filter(|i| i.source == source && i.severity == "urgent" && i.created >= start)
        .count()
}
