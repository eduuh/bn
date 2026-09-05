//! The agent staff framework (`bn staff` / `bn staff feed`): every scheduled agent is the same machine

//! with a different prompt. A markdown roster (`config/agents/*.md`) declares each agent; a run is gather → dispatch → validate → route. Deterministic CLI code *gathers* a
//! JSON packet (the only world the agent sees), a pinned skill prompts a headless harness, the
//! output must parse as a **briefing card**, and card items are routed through existing surfaces
//! only — attention, daily goals, note sections, the dojo inbox. Malformed output lands nothing
//! and raises a warn attention item instead.
//!
//! **Private by design**: the sink set is closed ([`SINKS`]) and item kinds map onto it through
//! the fixed [`kind_sink`] table — there is no post/publish/comment sink and config cannot add
//! one. Cards live in the private notes repo, **person-scoped** at `<personal_root>/staff/feed/
//! <date>/<agent>` (see below), read-marks in machine-local state; nothing leaves either.
//!
//! **Multi-machine semantics** (#270). The staff is one team, not one-per-laptop, so two boxes
//! must not both run the 06:30 Mentor and duplicate-review the same PRs into the synced notes
//! repo. Three decisions settle it: (1) **homing** — `[staff].home` names the single machine the
//! staff runs on; [`home_gate`] no-ops `bn staff run` (and every `staff-<agent>` cron) loudly-once
//! elsewhere, with a per-agent `only_on` override for a box-specific agent, and unset = runs
//! anywhere. (2) **Person-scoped feed** — cards live once under `<personal_root>/staff/feed`
//! (not `<machine>/feed`), so whichever box is home writes and every box reads the same synced
//! cards; [`migrate_feed_layout`] folds any pre-#270 machine-scoped cards in. (3) **Machine-local
//! read-marks** (v1) — cheap, so the unread badge differs per box; the cross-machine "is the staff
//! alive?" signal instead comes from the synced feed's newest card date ([`newest_card_date`]),
//! which the doctor flags stale past [`STALE_STAFF_DAYS`] (a dead home machine).
//!
//! **Security posture** (#272): the gather path carries untrusted content from the public
//! internet (issue titles, PR descriptions from external contributors) onto cards you press `↵`
//! on — a prompt-injection surface. Three guards close it, all at the gather/sanitize boundary:
//! the packet is wrapped in an explicit untrusted-data frame ([`build_prompt`]) so a harness reads
//! it as data, never instructions, and is rejected past [`MAX_PACKET_BYTES`]; every human-facing
//! card string is stripped of terminal escapes / control characters and length-capped before it
//! is stored or rendered; and each card link must clear the domain allowlist
//! ([`DEFAULT_LINK_DOMAINS`] plus per-agent `link_domains`) — an off-list link is stripped and
//! noted, never followed ([`validate_card`]).
//!
//! **Run-status taxonomy** (adopted from the #271 headless-auth spike): a scheduled 05:30 run
//! with nobody at the keyboard fails in ways that are NOT interchangeable — an expired login
//! needs a human, a rate-limited run just needs a later clock, and only a genuine `fail` should
//! page anyone. The claude CLI exits 1 for all of these, so [`classify_run`] reads the run's
//! captured output against the CLI's actual error strings; `skipped` is policy-assigned, never
//! classified.

mod ledger;
mod outcome;
pub use ledger::*;

use crate::resolver::{state_dir, Config, Resolved};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

mod cost;
mod ratings;
pub use ratings::{
    performance_review, rate_card, read_ratings, resolve_card, AgentReview, Rating, RatingRecord,
    REVIEW_WINDOW_DAYS,
};
/// The card contract itself: its shape, and the validation every harness reply passes through.
mod card;
/// The briefing-card feed store (write / list / read / mark-read).
mod feed;
/// What an agent is told: the dispatch prompt and its equip preamble.
mod prompt;
/// Where a validated card's items land — the sinks, and the rules that keep re-derived cards from
/// piling up.
mod route;
pub use card::*;
pub use cost::{cost_report, AgentCost, CostReport};
pub use feed::{
    card_path, list_cards, mark_read, read_card, render_card_md, unread_count, write_card, CardRef,
};
pub use prompt::*;
pub use route::*;

/// The outcome of one scheduled staff run. Serialized into run records with kebab-case names
/// (`ok` / `fail` / `auth` / `rate-limited` / `skipped` / `off`) — the taxonomy #271 settled on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    /// The run completed and its output validated.
    Ok,
    /// The run genuinely broke — the only status that should raise a warn attention item.
    Fail,
    /// Auth is dead (expired / revoked / not logged in). No retry can fix this; it needs a
    /// human to run `/login`, so surface it distinctly and don't burn retries on it.
    Auth,
    /// The shared account hit its usage window. Not a failure — retry later or skip.
    RateLimited,
    /// Policy chose not to run (gate, or retry-later budget spent). Assigned by the
    /// scheduler, never by [`classify_run`].
    Skipped,
    /// The agent is switched **off** by configuration — its opt-in flag is unset, or its repo
    /// allowlist is empty. The scheduler fired it and it correctly did nothing.
    ///
    /// Distinct from [`Skipped`](RunStatus::Skipped), which is a *temporary* refusal a person can
    /// clear (backpressure, a retry budget). `Off` is a standing choice, so it must never be
    /// dressed up as a problem: an agent you deliberately turned off has not missed anything, and
    /// painting it red trains you to ignore the colour that means a real failure.
    Off,
}

impl RunStatus {
    /// The kebab-case wire name, matching the serde form — for panels and log lines.
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Ok => "ok",
            RunStatus::Fail => "fail",
            RunStatus::Auth => "auth",
            RunStatus::RateLimited => "rate-limited",
            RunStatus::Skipped => "skipped",
            RunStatus::Off => "off",
        }
    }

    /// Parse the wire name back. `None` for anything outside the taxonomy — a record with an
    /// unknown status should read as absent, not as some default.
    pub fn parse(s: &str) -> Option<RunStatus> {
        Some(match s {
            "ok" => RunStatus::Ok,
            "fail" => RunStatus::Fail,
            "auth" => RunStatus::Auth,
            "rate-limited" => RunStatus::RateLimited,
            "skipped" => RunStatus::Skipped,
            "off" => RunStatus::Off,
            _ => return None,
        })
    }

    /// Whether the retry-later policy applies: retry once at +30m, then record `skipped`.
    /// Only `rate-limited` qualifies — the window resets on its own. `auth` never does (a
    /// retry re-fails identically in ~2s), and a real `fail` should surface, not be papered
    /// over by retries.
    pub fn retry_later(self) -> bool {
        matches!(self, RunStatus::RateLimited)
    }
}

/// Auth-family markers in the claude CLI's error output. Every login-shaped failure the CLI
/// emits carries one of these ("Not logged in · Please run /login", "API Error: 401 Invalid
/// API key · Please run /login", "OAuth token revoked · Please run /login", "Session expired.
/// Please run /login to sign in again."). Matched case-insensitively.
const AUTH_MARKERS: &[&str] = &[
    "please run /login",
    "not logged in",
    "oauth token revoked",
    "oauth token has expired",
    "invalid api key",
    "authentication_error",
];

/// Rate-limit-family markers: the subscription window ("usage limit reached", legacy
/// "Claude AI usage limit reached|<epoch>") and the API's 429/529 error types. Overload is
/// grouped here because the remedy is identical — wait, don't page.
const RATE_LIMIT_MARKERS: &[&str] = &[
    "usage limit reached",
    "rate_limit_error",
    "overloaded_error",
];

/// Classify one finished harness run into the taxonomy. `exit_success` is the process exit;
/// `output` is the run's captured stdout+stderr (the CLI prints auth errors to stdout, so
/// feed it both). Success is `ok` no matter what the text says; on failure, auth markers win
/// over rate-limit markers (a 401 mentioning limits is still a login problem), and anything
/// unrecognized is an honest `fail`.
pub fn classify_run(exit_success: bool, output: &str) -> RunStatus {
    if exit_success {
        return RunStatus::Ok;
    }
    let lower = output.to_lowercase();
    if AUTH_MARKERS.iter().any(|m| lower.contains(m)) {
        return RunStatus::Auth;
    }
    if RATE_LIMIT_MARKERS.iter().any(|m| lower.contains(m)) {
        return RunStatus::RateLimited;
    }
    RunStatus::Fail
}

/// The closed sink set. Config (`sinks = [...]`) may only allowlist these; a card item routes to
/// exactly one of them via [`kind_sink`]. This is the privacy guard: no public sink exists, so
/// none can be configured.
pub const SINKS: &[&str] = &["attn", "daily", "note", "dojo", "research", "doc", "learn"];

/// Card item kinds — the vocabulary agents write in. Each maps to one sink via [`kind_sink`].
pub const KINDS: &[&str] = &[
    "attn",
    "goal",
    "todo",
    "research",
    "dojo-card",
    "note-append",
    "doc",
    "learning",
];

/// Kinds that need a route: their sink writes into a *branch* surface, so `repo` + `branch` name
/// the branch whose note the item lands on and an item without both is unroutable. The single
/// source of truth for that rule — [`validate_card`] enforces it and [`build_prompt`] advertises
/// it, so the contract the model is given can't drift from the one it's judged against (#985).
pub const ROUTED_KINDS: &[&str] = &["todo", "research", "note-append"];

/// Whether `kind` requires a non-empty `repo` + `branch`. See [`ROUTED_KINDS`].
pub fn kind_needs_route(kind: &str) -> bool {
    ROUTED_KINDS.contains(&kind)
}

/// The fixed kind → sink table. `None` for an unknown kind (a validate-time hard error).
pub fn kind_sink(kind: &str) -> Option<&'static str> {
    match kind {
        "attn" => Some("attn"),
        "goal" => Some("daily"),
        "todo" | "note-append" => Some("note"),
        "research" => Some("research"),
        "dojo-card" => Some("dojo"),
        "doc" => Some("doc"),
        "learning" => Some("learn"),
        _ => None,
    }
}

/// The card kinds an agent may emit, given its per-agent sink allowlist — the inverse of
/// [`kind_sink`] filtered to `allowed_sinks`, in [`KINDS`] order. This is what the dispatch
/// prompt's output contract advertises so the model is never invited to write a kind whose sink
/// [`validate_card`] would reject (e.g. a `[attn, daily]` chief is offered only `attn | goal`,
/// not `todo`). An agent with no sinks gets no kinds.
pub fn kinds_for_sinks(allowed_sinks: &[String]) -> Vec<&'static str> {
    KINDS
        .iter()
        .copied()
        .filter(|k| kind_sink(k).is_some_and(|s| allowed_sinks.iter().any(|a| a == s)))
        .collect()
}

// --- roster (.md agents) ------------------------------------------------------------------------

/// A staff agent is a self-describing markdown agent with a deterministic `gather:` command.
pub type ResolvedAgent = crate::agents::AgentDef;

/// All staff agents for `ctx`, folded by the agent discovery tier and filtered to staff agents.
pub fn discover(
    cfg: &Config,
    ctx: Option<&Resolved>,
) -> Result<Vec<ResolvedAgent>, crate::agents::AgentCatalogError> {
    Ok(crate::agents::discover(cfg, ctx)?
        .into_iter()
        .filter(ResolvedAgent::is_staff)
        .collect())
}

/// Find one staff agent by name.
pub fn find(
    cfg: &Config,
    ctx: Option<&Resolved>,
    name: &str,
) -> Result<Option<ResolvedAgent>, crate::agents::AgentCatalogError> {
    Ok(discover(cfg, ctx)?.into_iter().find(|a| a.name == name))
}

/// The attention/source name an agent records as.
pub fn job_name(agent: &str) -> String {
    format!("staff-{agent}")
}

// --- homing (#270) ------------------------------------------------------------------------------

/// Whether this machine may host staff agents, and why not when it may not. The staff is a
/// single team pinned to one home machine so two laptops never duplicate-review the same PRs into
/// the synced feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeGate {
    /// This machine hosts the agent — dispatch proceeds.
    RunsHere,
    /// Skipped: `[staff].home` names another machine. Carries the configured home for the
    /// loud-once notice.
    NotHome { home: String },
}

impl HomeGate {
    /// Does the gate allow a dispatch on this machine?
    pub fn allows(&self) -> bool {
        matches!(self, HomeGate::RunsHere)
    }
}

/// Decide whether staff may run on this machine (#270). `[staff].home`, when set, pins the whole
/// staff; otherwise the staff runs anywhere.
pub fn home_gate(cfg: &Config, _agent: &ResolvedAgent) -> HomeGate {
    match cfg.staff_home.as_deref() {
        Some(home) if home != cfg.machine() => HomeGate::NotHome {
            home: home.to_string(),
        },
        _ => HomeGate::RunsHere,
    }
}

fn home_skip_marker_path() -> PathBuf {
    state_dir().join("staff").join("home-skip")
}

/// Record that this machine has already emitted the loud "not the staff home" notice for `home`,
/// and report whether this is the *first* time (so the caller shouts once, then stays quiet).
/// Machine-local, keyed by the home value — re-homing to a new machine shouts again. Best-effort:
/// an unwritable marker just means the notice repeats (never silently wrong).
pub fn note_home_skip_once(home: &str) -> bool {
    let path = home_skip_marker_path();
    if std::fs::read_to_string(&path).is_ok_and(|prev| prev.trim() == home) {
        return false;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, home);
    true
}

// --- feed store + read-marks ---------------------------------------------------------------------

/// A card's identity in the feed store and read-mark set.
pub(super) fn card_key(date: &str, agent: &str) -> String {
    format!("{date}/{agent}")
}

/// `<feed>/<date>/<agent>.json` — the synced, append-only card store.
/// Best-effort, idempotent, merge-safe migration of any pre-#270 machine-scoped feed
/// (`<personal_root>/<machine>/feed`) into the person-scoped [`Config::staff_feed_dir`] (#270).
/// Each card file is *moved* into the shared feed only when the destination is absent (a card the
/// home machine already wrote wins), so re-runs and two boxes migrating never clobber. Emptied
/// legacy day-dirs and the legacy `feed` root are removed. A no-op — one `stat` — once migrated or
/// on a box that never wrote a card. Returns the count of `.json` cards moved.
pub(super) fn migrate_feed_layout(cfg: &Config) -> usize {
    let legacy = cfg.staff_feed_legacy_dir();
    let dest_root = cfg.staff_feed_dir();
    // Nothing to do when the legacy layout is absent, or (a misconfigured host where the machine
    // segment would collapse the two paths) it already *is* the person-scoped dir.
    if legacy == dest_root || !legacy.is_dir() {
        return 0;
    }
    let Ok(dates) = std::fs::read_dir(&legacy) else {
        return 0;
    };
    let mut moved = 0;
    for date_entry in dates.flatten() {
        let date_dir = date_entry.path();
        if !date_dir.is_dir() {
            continue;
        }
        if let Ok(files) = std::fs::read_dir(&date_dir) {
            let dest_date = dest_root.join(date_entry.file_name());
            for f in files.flatten() {
                let src = f.path();
                let dest = dest_date.join(f.file_name());
                if dest.exists() {
                    continue; // the shared feed already has this card — never clobber
                }
                if std::fs::create_dir_all(&dest_date).is_err() {
                    continue;
                }
                let is_card = src.extension().and_then(|e| e.to_str()) == Some("json");
                // A rename within one notes root is atomic; fall back to copy+remove if the roots
                // ever straddle a device boundary.
                let ok = std::fs::rename(&src, &dest).is_ok()
                    || (std::fs::copy(&src, &dest).is_ok() && std::fs::remove_file(&src).is_ok());
                if ok && is_card {
                    moved += 1;
                }
            }
        }
        let _ = std::fs::remove_dir(&date_dir); // best-effort; only when emptied
    }
    let _ = std::fs::remove_dir(&legacy); // best-effort; only when fully drained
    moved
}

/// The staff health board as one JSON view — the shared fold behind `bn staff board --json` and
/// the dashboard's `GET /api/staff/board` (same doctrine as [`crate::cost::rollup`]: one reusable
/// core, every surface serializes it). Joins the roster with the run ledger, the feed's unread
/// marks, missed-run detection, and the backpressure state, so a surface can explain *why* an
/// agent has gone quiet, not just that it has.
pub fn board_view(cfg: &Config, ctx: Option<&Resolved>) -> Result<serde_json::Value, String> {
    let agents = discover(cfg, ctx).map_err(|e| e.to_string())?;
    let last = last_runs();
    let unread: std::collections::HashSet<String> = list_cards(cfg)
        .into_iter()
        .filter(|c| !c.read)
        .map(|c| c.agent)
        .collect();
    let now = crate::util::now_epoch_secs();
    let missed: std::collections::HashMap<String, i64> = crate::staff_schedule::missed_runs(
        &agents,
        &last,
        now,
        crate::staff_schedule::DEFAULT_MISS_GRACE_SECS,
    )
    .into_iter()
    .map(|m| (m.agent, m.scheduled))
    .collect();
    let rows: Vec<_> = agents
        .iter()
        .map(|a| {
            let lr = last.get(&a.name);
            // An agent switched off by configuration cannot miss a slot it was never going to
            // run. Suppressing the flag here (not just the health word) keeps every consumer of
            // this row agreeing about whether anything is actually wrong.
            let off = lr.is_some_and(|r| r.status == RunStatus::Off);
            let missed_since = if off {
                None
            } else {
                missed.get(&a.name).copied()
            };
            let streak = crate::staff_backpressure::unread_streak(cfg, &a.name);
            let paused = crate::staff_backpressure::decide(streak, false).0;
            serde_json::json!({
                "name": a.name,
                "on_calendar": a.on_calendar,
                "sinks": a.sinks,
                "harness": a.harness.clone().unwrap_or_else(|| "copilot".into()),
                "scope": a.scope.label(),
                "last_status": lr.map(|r| r.status.as_str()),
                "last_ts": lr.map(|r| r.ts),
                "last_error": lr.and_then(|r| r.error.clone()),
                "unread_card": unread.contains(&a.name),
                "silent": lr.map(|r| r.silent).unwrap_or(false),
                "missed": missed_since.is_some(),
                "missed_since": missed_since,
                "health": crate::staff_schedule::health_label(lr, missed_since.is_some()),
                // Backpressure (#269): how many of the agent's newest cards sit unread, and
                // whether that streak has self-paused it — the answer to "why is it quiet".
                "unread_streak": streak,
                "paused": paused,
            })
        })
        .collect();
    Ok(serde_json::json!({ "agents": rows }))
}

/// The briefing-card feed as JSON index rows (unread-first then newest-first — [`list_cards`]
/// order) for the dashboard. Card bodies stay behind [`card_view`]; this is the index.
pub fn feed_view(cfg: &Config) -> serde_json::Value {
    let rows: Vec<_> = list_cards(cfg)
        .into_iter()
        .map(|c| {
            serde_json::json!({
                "agent": c.agent,
                "date": c.date,
                "headline": c.headline,
                "read": c.read,
            })
        })
        .collect();
    serde_json::json!({ "cards": rows })
}

/// One card rendered to Markdown for the dashboard, resolved through the feed listing so file
/// paths never cross the HTTP boundary. `None` when no such card exists.
pub fn card_view(cfg: &Config, date: &str, agent: &str) -> Option<serde_json::Value> {
    let card_ref = list_cards(cfg)
        .into_iter()
        .find(|c| c.date == date && c.agent == agent)?;
    let card = read_card(&card_ref.path)?;
    Some(serde_json::json!({
        "agent": card_ref.agent,
        "date": card_ref.date,
        "read": card_ref.read,
        "markdown": render_card_md(&card),
    }))
}

/// How long the staff may be silent before the doctor calls it stale (#270): the issue's "no runs
/// in > 2 days" failover trigger — a home machine that has been down long enough to notice.
pub const STALE_STAFF_DAYS: i64 = 2;

/// The most recent card date (`YYYY-MM-DD`) anywhere in the synced feed, or `None` when the staff
/// has never written one. Because the feed is person-scoped and synced (#270), this is the single
/// cross-machine "the staff last ran on <date>" signal every box agrees on — unlike the
/// machine-local `staff_run` event log, which each box only sees its own half of. Backs the
/// doctor's stale-staff check (a home machine down for days stops advancing this date everywhere).
pub fn newest_card_date(cfg: &Config) -> Option<String> {
    let mut newest: Option<String> = None;
    for d in std::fs::read_dir(cfg.staff_feed_dir()).ok()?.flatten() {
        let name = d.file_name().to_string_lossy().into_owned();
        if !plausible_date(&name) {
            continue;
        }
        // A date-dir counts as a run only if it actually holds a card (guards a stray empty dir).
        let Ok(files) = std::fs::read_dir(d.path()) else {
            continue;
        };
        let has_card = files
            .flatten()
            .any(|f| f.path().extension().and_then(|x| x.to_str()) == Some("json"));
        if !has_card {
            continue;
        }
        match &newest {
            Some(cur) if cur.as_str() >= name.as_str() => {}
            _ => newest = Some(name),
        }
    }
    newest
}

// --- pipeline ------------------------------------------------------------------------------------

/// The outcome of one `bn staff run` — what happened at each pipeline stage.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RunReport {
    pub agent: String,
    pub ok: bool,
    /// The run-status taxonomy outcome (`ok` / `fail` / `auth` / `rate-limited`). `ok` above is
    /// just `status == Ok`; `status` is what the health board and doctor read to say the true
    /// thing about a failed dispatch (an expired login is not a rate-limit is not a real bug).
    pub status: RunStatus,
    /// The validate/gather/dispatch failure, when `!ok`.
    pub error: Option<String>,
    pub card_path: Option<PathBuf>,
    pub routed: RouteSummary,
    /// True when this date's card already existed — the card file is refreshed but items are not
    /// re-routed (attention keys would dedup anyway; daily/notes would double).
    pub rerun: bool,
    /// Estimated tokens this run spent (see [`estimate_tokens`]); `0` for a run shed before
    /// dispatch or one that failed before producing output.
    #[serde(default)]
    pub tokens: u64,
    /// Wall-clock of the dispatch step in milliseconds; `0` when no dispatch happened.
    #[serde(default)]
    pub wall_ms: u64,
    /// A one-line human status to surface for this run, when there is something to say *once* —
    /// the backpressure pause notice on the first throttled run (#269) or a budget-shed line.
    /// `None` on an ordinary run and on a silent throttle repeat, so the CLI degrades toward
    /// silence rather than repeating itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// True when this run was `ok-silent` (#273): the agent dispatched and produced a valid card
    /// with a headline but **no routed items** — it ran and correctly decided there was nothing to
    /// say (a zero-merge Mentor day). Recorded on the run event so silence is distinguishable from
    /// a missed run; only meaningful when `ok`.
    #[serde(default)]
    pub silent: bool,
}

/// The most recent `staff_run` per agent — for the roster health board.
#[derive(Debug, Clone, PartialEq)]
pub struct LastRun {
    pub status: RunStatus,
    pub ts: i64,
    pub error: Option<String>,
    /// The run's recorded silence bit (#273): `true` when an `ok` run wrote a card with no routed
    /// items. Surfaces render this as `ok-silent` (dim) vs `ok-with-card`; `false` for any
    /// non-`ok` status. See [`crate::staff_schedule::health_label`].
    pub silent: bool,
}

/// The dispatch step's result, injected into [`run_pipeline`] (the CLI runs the real harness and
/// [`classify_run`]s its exit + output; tests pass a closure). Carrying [`RunStatus`] lets the
/// pipeline record an auth/rate-limited run distinctly from a genuine failure — and page only on
/// the latter.
pub enum Dispatched {
    /// The harness produced output to validate as a briefing card.
    Output(String),
    /// The harness run yielded no card; the status separates auth / rate-limited / fail, and the
    /// detail says *why* — see [`failure_detail`]. Empty when the caller had nothing to capture.
    Failed(RunStatus, String),
}

/// The tail of a failed harness run, for the recorded run error.
///
/// A bare `dispatch failed (fail)` is undiagnosable: the health board shows it days later with the
/// harness's own explanation — the thing that says whether this was a missing binary, a network
/// blip, or a real bug — already discarded. Keep the last few non-empty lines, which is where CLIs
/// put the error, sanitized and capped like any other untrusted card text.
pub fn failure_detail(output: &str) -> String {
    let mut tail: Vec<&str> = output
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(3)
        .collect();
    tail.reverse();
    sanitize_line(&tail.join(" · "))
}

/// The measured cost of one dispatch, folded onto its `staff_run` event so the health board and
/// the cost ledger are the same record.
#[derive(Debug, Clone, Default)]
pub struct RunCost {
    pub tokens: u64,
    pub wall_ms: u64,
}

fn log_run_event(
    agent: &str,
    date: &str,
    status: RunStatus,
    error: Option<&str>,
    silent: bool,
    cost: &RunCost,
) {
    let mut f = serde_json::Map::new();
    f.insert("agent".into(), serde_json::json!(agent));
    // The run's logical date (YYYY-MM-DD), so the ledger buckets by month without date math on
    // the event ts (which is the machine clock, not the run's `today`).
    f.insert("date".into(), serde_json::json!(date));
    f.insert("status".into(), serde_json::json!(status.as_str()));
    // Kept alongside `status` so any pre-taxonomy reader still sees the ok/not-ok bit.
    f.insert("ok".into(), serde_json::json!(status == RunStatus::Ok));
    // Silence bit (#273): an `ok` run that routed no items is `ok-silent`. Recorded so a surface
    // can distinguish a by-design quiet day from a missed run. Always false for a non-`ok` status.
    f.insert(
        "silent".into(),
        serde_json::json!(silent && status == RunStatus::Ok),
    );
    // Cost ledger fields. Always present so `ledger_entries` reads a full row.
    f.insert("tokens".into(), serde_json::json!(cost.tokens));
    f.insert("wall_ms".into(), serde_json::json!(cost.wall_ms));
    // Work-class (#381): staff runs are scheduled work, so the unified `bn cost` view categorizes
    // this ledger row under Scheduled without a per-path lookup.
    f.insert(
        "work_class".into(),
        serde_json::json!(crate::cost::WorkClass::Scheduled.as_str()),
    );
    if let Some(e) = error {
        f.insert("error".into(), serde_json::json!(e));
    }
    crate::util::best_effort(
        crate::events::append("staff_run", f),
        "staff: log staff_run event",
    );
}

/// Record a `staff_run` event for a bespoke staff member that runs its own pipeline rather than
/// [`run_pipeline`] (the Mentor, #258, needs a custom four-section output contract). A thin public
/// wrapper over the same event the pipeline logs, so a bespoke agent still lands on the health
/// board ([`last_runs`]) and the cost ledger ([`ledger_entries`]) with an identical row shape.
pub fn record_run(
    agent: &str,
    date: &str,
    status: RunStatus,
    error: Option<&str>,
    tokens: u64,
    wall_ms: u64,
) {
    log_run_event(
        agent,
        date,
        status,
        error,
        false,
        &RunCost { tokens, wall_ms },
    );
}

/// The declining gather's reason, if the packet is one. Only a top-level `skip` string counts, so
/// a packet that merely *contains* the word — a finding about a skipped test, say — still runs.
fn skip_reason(packet: &str) -> Option<String> {
    let value: Value = serde_json::from_str(packet).ok()?;
    let reason = value.get("skip")?.as_str()?.trim();
    Some(if reason.is_empty() {
        "gather declined the run".to_string()
    } else {
        sanitize_line(reason)
    })
}

/// Record that a scheduled agent is switched **off** by configuration, so the board says so
/// instead of reporting a hard miss.
///
/// A gated job used to return before recording anything. The scheduler had fired it and it had
/// correctly done nothing — but the health board reads `staff_run` events, so "did nothing" and
/// "never ran" were the same absence, and a deliberately-disabled agent showed in red as
/// `missed — scheduled run never fired`. Every gated job should call this on its opt-out path;
/// `reason` is the one line telling the reader which switch to flip.
pub fn record_off(agent: &str, date: &str, reason: &str) {
    log_run_event(
        agent,
        date,
        RunStatus::Off,
        Some(reason),
        false,
        &RunCost::default(),
    );
}

/// Run the full pipeline for one agent, with the dispatch step injected (the CLI passes the real
/// harness call with its wall-clock budget; tests pass a closure). Every failure path lands
/// nothing in the feed or sinks and logs a `staff_run` event (the health half of the `bn staff`
/// board). A genuine `fail` also raises one warn attention item (`source = staff-<agent>`, keyed
/// per day so a retrying cron doesn't pile up); an `auth`/`rate-limited` dispatch is recorded but
/// does NOT page — an expired login needs a human and a rate-limit just needs a later clock.
pub fn run_pipeline(
    cfg: &Config,
    agent: &ResolvedAgent,
    packet: Result<String, String>,
    dispatch: impl FnOnce(&str) -> Dispatched,
    today: &str,
) -> RunReport {
    let mut cost = RunCost::default();
    let report =
        |status: RunStatus, error: Option<String>, card_path, routed, rerun, cost: &RunCost| {
            RunReport {
                agent: agent.name.clone(),
                ok: status == RunStatus::Ok,
                status,
                error,
                card_path,
                routed,
                rerun,
                tokens: cost.tokens,
                wall_ms: cost.wall_ms,
                note: None,
                silent: false,
            }
        };
    let fail = |status: RunStatus, error: String, cost: &RunCost| {
        // Only a genuine `fail` pages; auth/rate-limited are surfaced through the health board.
        if status == RunStatus::Fail {
            let key = format!("staff-fail:{}:{today}", agent.name);
            crate::util::best_effort(
                crate::attention::add(
                    &job_name(&agent.name),
                    &format!("staff agent '{}' failed: {error}", agent.name),
                    "warn",
                    None,
                    None,
                    None,
                    Some(&key),
                    today,
                ),
                "staff: page attention for agent failure",
            );
        }
        log_run_event(&agent.name, today, status, Some(&error), false, cost);
        report(
            status,
            Some(error),
            None,
            RouteSummary::default(),
            false,
            cost,
        )
    };

    // Backpressure self-throttle (#269): if this agent's most-recent cards are all unread, skip
    // the run rather than pile another card on the ignore stack, and post the one-line pause
    // notice exactly once per unread streak (never two in a row). Reading/rating any of those
    // cards resumes it on the next run. Recorded as a `skipped` run so the health board explains
    // the pause; the announce-once line rides `note`, so the CLI stays silent on later repeats.
    if let Some(throttle) = crate::staff_backpressure::throttle_gate(cfg, &agent.name, today) {
        let reason = throttle.notice(&agent.name);
        log_run_event(
            &agent.name,
            today,
            RunStatus::Skipped,
            Some(&reason),
            false,
            &cost,
        );
        let mut r = report(
            RunStatus::Skipped,
            Some(reason.clone()),
            None,
            RouteSummary::default(),
            false,
            &cost,
        );
        if throttle.announce {
            r.note = Some(reason);
        }
        return r;
    }

    let packet = match packet {
        Ok(p) => p,
        Err(e) => return fail(RunStatus::Fail, format!("gather failed: {e}"), &cost),
    };
    // A gather may decline the run. `{"skip": "<reason>"}` says "there is legitimately nothing to
    // do" — an empty allowlist, a feature switched off — which is neither a failure nor a run
    // worth dispatching a model for. It is the gather that knows: the config lives beside the
    // collector, not in the pipeline, and this is what lets an opt-in gate move out of an
    // agent-shaped native job and into an ordinary `bn` subcommand.
    if let Some(reason) = skip_reason(&packet) {
        record_off(&agent.name, today, &reason);
        let mut r = report(
            RunStatus::Off,
            Some(reason.clone()),
            None,
            RouteSummary::default(),
            false,
            &cost,
        );
        r.note = Some(reason);
        return r;
    }
    // Untrusted-input size limit: a gathered packet is public, untrusted content, so an oversized
    // one fails the run closed rather than ballooning the dispatch prompt (#272).
    if packet.len() > MAX_PACKET_BYTES {
        return fail(
            RunStatus::Fail,
            format!(
                "gather output too large ({} bytes > {MAX_PACKET_BYTES} limit)",
                packet.len()
            ),
            &cost,
        );
    }
    if serde_json::from_str::<serde_json::Value>(&packet).is_err() {
        return fail(
            RunStatus::Fail,
            "gather output is not valid JSON".into(),
            &cost,
        );
    }
    let prompt = build_prompt_with(
        &agent.body,
        &agent.name,
        &agent.sinks,
        &packet,
        today,
        agent.replace,
        agent.max_severity.as_deref(),
    );
    let prompt = match equip_preamble(&agent.skills, &agent.mcps) {
        Some(preamble) => format!("{preamble}\n{prompt}"),
        None => prompt,
    };
    let started = std::time::Instant::now();
    let dispatched = dispatch(&prompt);
    cost.wall_ms = started.elapsed().as_millis() as u64;
    let output = match dispatched {
        Dispatched::Output(o) => o,
        Dispatched::Failed(status, detail) => {
            let status_str = status.as_str();
            let error = if detail.is_empty() {
                format!("dispatch failed ({status_str})")
            } else {
                format!("dispatch failed ({status_str}): {detail}")
            };
            return fail(status, error, &cost);
        }
    };
    // A dispatch that produced output spent tokens whether or not it validated — estimate from
    // the prompt sent plus the reply received and bill it on every exit from here on.
    cost.tokens = estimate_tokens(prompt.len() + output.len());
    let card = match validate_card(
        &output,
        &agent.name,
        &agent.sinks,
        &agent.link_domains,
        today,
        agent.replace,
        agent.max_severity.as_deref(),
    ) {
        Ok(c) => c,
        Err(e) => {
            return fail(
                RunStatus::Fail,
                format!("invalid briefing card: {e}"),
                &cost,
            );
        }
    };
    let rerun = card_path(cfg, &card.date, &card.agent).is_file();
    let path = match write_card(cfg, &card) {
        Ok(p) => p,
        Err(e) => return fail(RunStatus::Fail, format!("feed write failed: {e}"), &cost),
    };
    let routed = if rerun {
        RouteSummary::default()
    } else {
        match route_card(cfg, &card, agent.doc_dir.as_deref(), today) {
            Ok(r) => r,
            Err(e) => return fail(RunStatus::Fail, format!("sink routing failed: {e}"), &cost),
        }
    };
    // Silence (#273): a valid card with no routed items is an `ok-silent` run — the agent ran and
    // decided there was nothing to say. Recorded so a surface never mistakes it for a missed run.
    let silent = card.items.is_empty();
    log_run_event(&agent.name, today, RunStatus::Ok, None, silent, &cost);
    let mut ok = report(RunStatus::Ok, None, Some(path), routed, rerun, &cost);
    ok.silent = silent;
    ok
}

/// The most recent `staff_run` event per agent, for the roster health board. Reads `status` when
/// present and falls back to the `ok` bool for any pre-taxonomy event.
pub fn last_runs() -> std::collections::HashMap<String, LastRun> {
    let mut out: std::collections::HashMap<String, LastRun> = std::collections::HashMap::new();
    for e in crate::events::read_all() {
        if e.kind != "staff_run" {
            continue;
        }
        let Some(agent) = e.fields.get("agent").and_then(|v| v.as_str()) else {
            continue;
        };
        let status = e
            .fields
            .get("status")
            .and_then(|v| v.as_str())
            .and_then(RunStatus::parse)
            .unwrap_or_else(|| match e.fields.get("ok").and_then(|v| v.as_bool()) {
                Some(true) => RunStatus::Ok,
                _ => RunStatus::Fail,
            });
        let error = e
            .fields
            .get("error")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        // Silence bit (#273); absent on pre-#273 events → false (reads as `ok-with-card`).
        let silent = e
            .fields
            .get("silent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            && status == RunStatus::Ok;
        let record = LastRun {
            status,
            ts: e.ts,
            error,
            silent,
        };
        match out.get(agent) {
            Some(existing) if existing.ts > e.ts => {}
            _ => {
                out.insert(agent.to_string(), record);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::skip_reason;

    /// The decline protocol: a gather that knows there is nothing to do says so, and the pipeline
    /// records the agent as off instead of paying a model to look at an empty packet.
    #[test]
    fn a_gather_declines_with_a_top_level_skip() {
        assert_eq!(
            skip_reason(r#"{"skip":"no repos allowlisted"}"#).as_deref(),
            Some("no repos allowlisted")
        );
    }

    #[test]
    fn an_empty_skip_still_declines_with_a_usable_reason() {
        assert_eq!(
            skip_reason(r#"{"skip":"  "}"#).as_deref(),
            Some("gather declined the run")
        );
    }

    /// A packet that merely *mentions* skipping is a real packet. Matching on the word rather than
    /// the top-level field would silently switch an agent off the day it found a skipped test.
    #[test]
    fn a_packet_that_only_mentions_skipping_still_runs() {
        assert_eq!(skip_reason(r#"{"findings":[{"title":"skip me"}]}"#), None);
        assert_eq!(skip_reason(r#"{"nested":{"skip":"not top level"}}"#), None);
        assert_eq!(skip_reason(r#"{"skip":true}"#), None);
        assert_eq!(skip_reason("not json at all"), None);
    }
}
