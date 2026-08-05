//! Routine-aware personal schedule coach (issue #801, amended for a deterministic lifetime): the
//! user-level recurring weekly routine bn keeps its guidance aligned with — the current block,
//! upcoming transitions, and gentle drift correction — via `bn routine now/next/status/check-in/
//! snooze/skip/override/tick/watch/service`.
//!
//! Parsed from `[routine]` + `[[routine.blocks]]` in `config.toml` (see
//! [`crate::resolver::Config::routine`]) into a [`RoutineConfig`] evaluated against an
//! **authoritative, explicitly-resolved IANA timezone** ([`chrono_tz::Tz`]) — never
//! `chrono::Local` — so a configured zone that intentionally differs from the host still governs
//! evaluation, and a bogus/undiscoverable zone fails clearly at config-resolve time rather than
//! silently drifting onto the host's local clock.
//!
//! Every block carries a **stable id** — explicit, or a deterministic content-derived
//! compatibility id — that every piece of durable state is keyed on; a versioned
//! [`IdentityRegistry`] folds any pre-existing position-based (`bN`) state through a frozen
//! mapping so upgrading, or later reordering `blocks`, never retargets it (see
//! [`load_or_create_identity_registry`]). Every routine operation — [`tick`], check-in, snooze,
//! skip, override, clear, delivery, migration, and health — runs inside one [`RoutineTx`]
//! transaction: a single process-shared exclusive lock, bounded wait, complete ordering
//! (load/validate config -> registry -> state fold -> decide -> durable event append -> health
//! replace -> unlock). Delivery is explicitly **at-least-once** and bounded by a configurable
//! catch-up horizon (see the "Shared at-least-once tick engine" section below); an invalid live
//! config fails closed rather than falling back to a stale schedule or the host's local time.
//!
//! Runtime state (check-ins, snoozes, skips, temporary overrides, delivered-notification keys)
//! is durable but **non-destructive**: none of it ever rewrites the recurring schedule. It rides
//! the same append-only `events.jsonl` substrate under [`crate::resolver::state_dir`] that
//! [`crate::focus`] uses, folded read-side with clear precedence (an active override beats the
//! recurring schedule; a skip nulls out one specific occurrence; a snooze only silences
//! notifications) and expiry (nothing here needs a TTL sweep — an expired override/snooze/skip
//! simply stops matching once `now` moves past it).

use crate::events;
use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, LocalResult, NaiveDate, NaiveDateTime,
    NaiveTime, TimeZone, Timelike, Utc, Weekday,
};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::PathBuf;

/// The config-file sentinel that resolves to the host's discovered IANA zone rather than an
/// explicit name (`timezone = "local"`, or the field simply absent).
pub const LOCAL_SENTINEL: &str = "local";

/// Kinds the issue calls out as needing drift protection even when a block doesn't explicitly
/// set `protected = true` — see [`RoutineBlock::is_effectively_protected`].
const IMPLICITLY_PROTECTED_KINDS: [&str; 4] = ["family", "recovery", "exercise", "sleep"];

/// Default at-least-once retry/catch-up window (minutes) — see
/// [`RoutineConfig::catchup_horizon_minutes`].
const DEFAULT_CATCHUP_HORIZON_MINUTES: i64 = 30;

// ---------------------------------------------------------------------------------------------
// Raw config-file shape (deserialized by `resolver::config_file::ConfigFile`).
// ---------------------------------------------------------------------------------------------

/// The raw `[routine]` table, as TOML deserializes it. Every field optional so an absent
/// `[routine]` section — the common case — parses to a fully-disabled default (see
/// [`RoutineConfig::disabled`]) without requiring a single `enabled = false` line.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct RoutineFileConfig {
    pub enabled: Option<bool>,
    pub timezone: Option<String>,
    pub reminder_minutes_before: Option<i64>,
    pub notify_on_start: Option<bool>,
    /// See [`RoutineConfig::catchup_horizon_minutes`]. Defaults to 30 when unset.
    #[serde(default)]
    pub catchup_horizon_minutes: Option<i64>,
    #[serde(default)]
    pub blocks: Vec<RoutineBlockFile>,
}

/// One raw `[[routine.blocks]]` entry.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct RoutineBlockFile {
    pub name: String,
    /// Explicit stable identity (#801 amendment) — durable state (skips, delivery keys, health's
    /// active block) is always keyed on this, never on config position. Docs/examples must set
    /// it; a block without one falls back to a deterministic content-derived id (see
    /// [`content_derived_id`]) and reports a legacy-identity warning.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub days: Vec<String>,
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub protected: bool,
}

// ---------------------------------------------------------------------------------------------
// Resolved config.
// ---------------------------------------------------------------------------------------------

/// One validated recurring time block.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutineBlock {
    /// Stable id (#801 amendment) — either the explicit `id` from config, or a deterministic
    /// content-derived compatibility id (see [`content_derived_id`]) when none was given. Used
    /// to key every piece of durable state (skips, overrides, delivery/dedupe keys, health's
    /// active block, JSON) so config-position reshuffles never retarget existing state.
    pub id: String,
    /// Whether `id` came from an explicit `id = "..."` in config, vs. a content-derived
    /// compatibility id — surfaced so callers can warn that editing a content-derived block's
    /// name/kind/days/times intentionally creates a new identity.
    pub explicit_id: bool,
    pub name: String,
    /// Free-form, lowercased (`work`, `learning`, `family`, `exercise`, `sleep`, `recovery`,
    /// `transition`, `personal`, …); unset/blank defaults to `"other"`.
    pub kind: String,
    pub protected: bool,
    pub days: Vec<Weekday>,
    pub start: NaiveTime,
    pub end: NaiveTime,
}

impl RoutineBlock {
    /// Whether this block's `[start, end)` wraps past midnight (`end <= start`).
    pub fn wraps_midnight(&self) -> bool {
        self.end <= self.start
    }

    /// Suppression-worthy even without an explicit `protected = true` (#801): family, recovery,
    /// exercise, and sleep blocks protect against work-prompt drift by default.
    pub fn is_effectively_protected(&self) -> bool {
        self.protected || IMPLICITLY_PROTECTED_KINDS.contains(&self.kind.as_str())
    }

    fn parse(idx: usize, raw: RoutineBlockFile) -> Result<RoutineBlock, String> {
        let name = raw.name.trim().to_string();
        if name.is_empty() {
            return Err(format!("routine: block #{idx} is missing a name"));
        }
        if raw.days.is_empty() {
            return Err(format!("routine: block '{name}' has no days"));
        }
        let mut days = Vec::with_capacity(raw.days.len());
        for token in &raw.days {
            let day = parse_weekday(token).map_err(|e| format!("routine: block '{name}': {e}"))?;
            if !days.contains(&day) {
                days.push(day);
            }
        }
        let start = parse_hhmm(&raw.start).map_err(|e| format!("routine: block '{name}': {e}"))?;
        let end = parse_hhmm(&raw.end).map_err(|e| format!("routine: block '{name}': {e}"))?;
        if start == end {
            return Err(format!(
                "routine: block '{name}' is zero-length (start == end) — split a full-day block \
                 into two, or pick a distinct end time"
            ));
        }
        let kind = raw
            .kind
            .as_deref()
            .map(str::trim)
            .map(str::to_lowercase)
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| "other".to_string());
        let explicit_id = raw.id.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let (id, explicit_id) = match explicit_id {
            Some(explicit) => (explicit.to_string(), true),
            None => (content_derived_id(&name, &kind, &days, start, end), false),
        };
        Ok(RoutineBlock {
            id,
            explicit_id,
            name,
            kind,
            protected: raw.protected,
            days,
            start,
            end,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Stable content-derived compatibility ids (#801 amendment) — for a block with no explicit `id`.
// ---------------------------------------------------------------------------------------------

/// FNV-1a 64-bit — deterministic and stable across processes, platforms, and compiler versions,
/// unlike `std`'s `DefaultHasher` (SipHash), which carries **no** cross-version stability
/// guarantee and must never back anything persisted between runs. Dependency-free by design: this
/// hash only needs to be a stable fingerprint, not cryptographically strong.
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// The deterministic compatibility id for a block with no explicit `id` (#801 amendment): a short
/// hash of every identity-defining field (name, kind, days, start, end). Changing *any* of
/// these — including just renaming the block — intentionally mints a new identity, because
/// without an explicit id there is no other way to recognize "the same block" across edits; this
/// is exactly why docs/examples require one. Prefixed `c` (vs. user-chosen explicit ids) so
/// output can tell the two apart at a glance.
fn content_derived_id(
    name: &str,
    kind: &str,
    days: &[Weekday],
    start: NaiveTime,
    end: NaiveTime,
) -> String {
    let mut day_nums: Vec<u8> = days
        .iter()
        .map(|d| d.num_days_from_monday() as u8)
        .collect();
    day_nums.sort_unstable();
    let days_key = day_nums
        .iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let key = format!(
        "{name}|{kind}|{days_key}|{}|{}",
        start.format("%H:%M"),
        end.format("%H:%M")
    );
    format!("c{:016x}", fnv1a64(key.as_bytes()))
}

/// The resolved, validated routine config — the authoritative timezone-aware source [`snapshot`]
/// and every CLI command evaluate against.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutineConfig {
    pub enabled: bool,
    pub tz: Tz,
    /// The resolved IANA zone name (e.g. `"Africa/Nairobi"`) — the literal configured value, or
    /// the host's discovered zone when `timezone` was `"local"`/absent.
    pub timezone: String,
    pub reminder_minutes_before: i64,
    pub notify_on_start: bool,
    /// How far into the past (minutes) a due-but-undelivered reminder/start event is still
    /// eligible for at-least-once retry after a crash/missed tick (#801 amendment) — beyond this,
    /// `tick` reports it `expired` instead of sending, so a long-down watcher/timer can never
    /// resend an arbitrarily stale notification. Overridable via
    /// `$BN_ROUTINE_CATCHUP_HORIZON_MINUTES`; defaults to 30.
    pub catchup_horizon_minutes: i64,
    pub blocks: Vec<RoutineBlock>,
    /// One warning per block that fell back to a content-derived compatibility id because it had
    /// no explicit `id` (#801 amendment) — surfaced by `bn routine status`/`doctor` so a user
    /// knows editing that block's name/kind/days/times will mint a new identity.
    pub legacy_id_warnings: Vec<String>,
}

impl RoutineConfig {
    /// The zero-config default: disabled, no blocks, UTC placeholder timezone. Never fails —
    /// this is what an absent `[routine]` section resolves to, so `bn` never nags a user who
    /// hasn't opted in.
    pub fn disabled() -> RoutineConfig {
        RoutineConfig {
            enabled: false,
            tz: chrono_tz::UTC,
            timezone: "UTC".to_string(),
            reminder_minutes_before: 10,
            notify_on_start: true,
            catchup_horizon_minutes: DEFAULT_CATCHUP_HORIZON_MINUTES,
            blocks: Vec::new(),
            legacy_id_warnings: Vec::new(),
        }
    }

    /// Resolve `[routine]` + `[[routine.blocks]]` from the parsed config file, layering
    /// `$BN_ROUTINE_ENABLED` / `$BN_ROUTINE_TIMEZONE` env overrides (parity with every other
    /// opt-in job flag). Fails clearly — a bad IANA name, an undiscoverable host zone, an invalid
    /// day/time, a zero-length block, a duplicate block id, or an overlap (including
    /// cross-midnight) — rather than silently degrading, so a broken `[routine]` section is loud
    /// precisely where it's read (`bn routine …`), not everywhere `bn` runs.
    pub fn resolve(raw: Option<RoutineFileConfig>) -> Result<RoutineConfig, String> {
        let raw = match raw {
            Some(raw) => raw,
            None => return Ok(RoutineConfig::disabled()),
        };
        let enabled = env_bool("BN_ROUTINE_ENABLED")
            .or(raw.enabled)
            .unwrap_or(false);
        let timezone_input = non_empty_env("BN_ROUTINE_TIMEZONE").or(raw.timezone);
        let (tz, timezone) = resolve_timezone(timezone_input.as_deref())?;
        let reminder_minutes_before = raw.reminder_minutes_before.unwrap_or(10).max(0);
        let notify_on_start = raw.notify_on_start.unwrap_or(true);
        let catchup_horizon_minutes = non_empty_env("BN_ROUTINE_CATCHUP_HORIZON_MINUTES")
            .and_then(|v| v.parse::<i64>().ok())
            .or(raw.catchup_horizon_minutes)
            .unwrap_or(DEFAULT_CATCHUP_HORIZON_MINUTES)
            .max(1);

        let mut blocks = Vec::with_capacity(raw.blocks.len());
        for (idx, block) in raw.blocks.into_iter().enumerate() {
            blocks.push(RoutineBlock::parse(idx, block)?);
        }
        check_overlaps(&blocks)?;
        check_duplicate_ids(&blocks)?;
        let legacy_id_warnings = blocks
            .iter()
            .filter(|b| !b.explicit_id)
            .map(|b| {
                format!(
                    "routine: block '{}' has no explicit id — using content-derived id '{}'; \
                     add `id = \"...\"` so editing its name/kind/days/times doesn't mint a new \
                     identity",
                    b.name, b.id
                )
            })
            .collect();

        Ok(RoutineConfig {
            enabled,
            tz,
            timezone,
            reminder_minutes_before,
            notify_on_start,
            catchup_horizon_minutes,
            blocks,
            legacy_id_warnings,
        })
    }
}

fn env_bool(key: &str) -> Option<bool> {
    non_empty_env(key).map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// Resolve the configured timezone: an explicit IANA name is parsed verbatim (so a config that
/// intentionally targets a zone other than the host's is respected exactly); [`LOCAL_SENTINEL`]
/// or an absent value discovers the host's IANA zone via `iana-time-zone` — never
/// `chrono::Local`, which carries no zone identity to report or reason about.
fn resolve_timezone(raw: Option<&str>) -> Result<(Tz, String), String> {
    let requested = raw.map(str::trim).filter(|s| !s.is_empty());
    match requested {
        None => discover_local_timezone(),
        Some(v) if v.eq_ignore_ascii_case(LOCAL_SENTINEL) => discover_local_timezone(),
        Some(v) => v.parse::<Tz>().map(|tz| (tz, v.to_string())).map_err(|_| {
            format!(
                "routine: unknown IANA timezone '{v}' — use a name like 'Africa/Nairobi' or \
                 '{LOCAL_SENTINEL}'"
            )
        }),
    }
}

/// Discover the host's IANA zone via `iana-time-zone` (reads `/etc/localtime`/`/etc/timezone`,
/// never `TZ`) — except when `$BN_ROUTINE_LOCAL_TIMEZONE_OVERRIDE` is set, which short-circuits
/// straight to a parsed override. This mirrors every other `$BN_ROUTINE_*` knob already in this
/// module (`BN_ROUTINE_ENABLED`, `BN_ROUTINE_TIMEZONE`, …): it exists so a test can deterministically
/// simulate the host's discovered zone changing between two reconciliation passes (e.g. `watch`
/// re-resolving `timezone = "local"` after travel or a system reconfiguration) without depending
/// on the real host clock/zone — a real deployment never sets it.
fn discover_local_timezone() -> Result<(Tz, String), String> {
    if let Some(name) = non_empty_env("BN_ROUTINE_LOCAL_TIMEZONE_OVERRIDE") {
        return name
            .parse::<Tz>()
            .map(|tz| (tz, name.clone()))
            .map_err(|_| {
                format!(
                "routine: $BN_ROUTINE_LOCAL_TIMEZONE_OVERRIDE '{name}' is not a recognized IANA \
                 zone"
            )
            });
    }
    let name = iana_time_zone::get_timezone().map_err(|e| {
        format!(
            "routine: could not discover the system IANA timezone ({e}); set [routine].timezone \
             explicitly"
        )
    })?;
    let tz = name.parse::<Tz>().map_err(|_| {
        format!(
            "routine: system timezone '{name}' is not a recognized IANA zone; set \
             [routine].timezone explicitly"
        )
    })?;
    Ok((tz, name))
}

fn parse_weekday(token: &str) -> Result<Weekday, String> {
    match token.trim().to_lowercase().as_str() {
        "mon" | "monday" => Ok(Weekday::Mon),
        "tue" | "tues" | "tuesday" => Ok(Weekday::Tue),
        "wed" | "weds" | "wednesday" => Ok(Weekday::Wed),
        "thu" | "thur" | "thurs" | "thursday" => Ok(Weekday::Thu),
        "fri" | "friday" => Ok(Weekday::Fri),
        "sat" | "saturday" => Ok(Weekday::Sat),
        "sun" | "sunday" => Ok(Weekday::Sun),
        other => Err(format!(
            "unknown weekday '{other}' (want mon/tue/wed/thu/fri/sat/sun or full names)"
        )),
    }
}

fn parse_hhmm(s: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M")
        .map_err(|_| format!("invalid time '{s}' (want 24h HH:MM)"))
}

// ---------------------------------------------------------------------------------------------
// Overlap validation (including cross-midnight).
// ---------------------------------------------------------------------------------------------

const DAY_MINUTES: i64 = 24 * 60;

/// This block's occupied minute-ranges on the circular Mon..Sun weekly timeline (`0..7*1440`), one
/// range per scheduled day (two when it wraps past midnight — the tail of the start day plus the
/// head of the next).
fn block_intervals(block: &RoutineBlock) -> Vec<(i64, i64)> {
    let start_min = block.start.num_seconds_from_midnight() as i64 / 60;
    let end_min = block.end.num_seconds_from_midnight() as i64 / 60;
    let mut out = Vec::new();
    for &day in &block.days {
        let d = day.num_days_from_monday() as i64;
        if end_min > start_min {
            out.push((d * DAY_MINUTES + start_min, d * DAY_MINUTES + end_min));
        } else {
            // Wraps past midnight: the rest of `day`, then the head of the next day.
            out.push((d * DAY_MINUTES + start_min, d * DAY_MINUTES + DAY_MINUTES));
            let next_d = (d + 1) % 7;
            out.push((next_d * DAY_MINUTES, next_d * DAY_MINUTES + end_min));
        }
    }
    out
}

/// Reject any pair of *different* blocks whose occupied minute-ranges intersect, including a
/// wrap-past-midnight block colliding with an early block on the following day. A single block's
/// own multi-day ranges never self-collide (each scheduled day gets its own 1440-minute lane), so
/// only cross-block pairs are checked.
fn check_overlaps(blocks: &[RoutineBlock]) -> Result<(), String> {
    let expanded: Vec<Vec<(i64, i64)>> = blocks.iter().map(block_intervals).collect();
    for i in 0..blocks.len() {
        for j in (i + 1)..blocks.len() {
            for &(s1, e1) in &expanded[i] {
                for &(s2, e2) in &expanded[j] {
                    if s1 < e2 && s2 < e1 {
                        return Err(format!(
                            "routine: blocks '{}' and '{}' overlap",
                            blocks[i].name, blocks[j].name
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Reject any two blocks that resolved to the same stable id (#801 amendment) — an explicit
/// duplicate, an explicit id that collides with another block's content-derived id, or two
/// content-derived collisions (extremely unlikely, but never silently guessed). Applying a
/// skip/delivery key to the wrong block is worse than failing loudly at config-resolve time.
fn check_duplicate_ids(blocks: &[RoutineBlock]) -> Result<(), String> {
    for i in 0..blocks.len() {
        for j in (i + 1)..blocks.len() {
            if blocks[i].id == blocks[j].id {
                return Err(format!(
                    "routine: blocks '{}' and '{}' resolve to the same id '{}' — ids must be \
                     unique (add/rename an explicit `id`)",
                    blocks[i].name, blocks[j].name, blocks[i].id
                ));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// DST-aware boundary resolution + occurrence matching.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boundary {
    Start,
    End,
}

/// Resolve a block boundary's local wall-clock reading (`date` at `time`, in `tz`) to a concrete
/// UTC instant, honoring the DST contract (#801):
/// - **Ambiguous** (fall-back's repeated hour): a start boundary takes the *earlier* of the two
///   instants (the block opens as soon as either reading would), an end boundary takes the
///   *later* one (so it never closes early) — either way `dst_adjusted` flips true.
/// - **Nonexistent** (spring-forward's skipped hour): advance the wall clock minute-by-minute
///   until it lands on a valid instant, marking `dst_adjusted` true.
fn resolve_boundary(
    tz: Tz,
    date: NaiveDate,
    time: NaiveTime,
    boundary: Boundary,
) -> (DateTime<Utc>, bool) {
    let naive = NaiveDateTime::new(date, time);
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => (dt.with_timezone(&Utc), false),
        LocalResult::Ambiguous(earlier, later) => {
            let picked = match boundary {
                Boundary::Start => earlier,
                Boundary::End => later,
            };
            (picked.with_timezone(&Utc), true)
        }
        LocalResult::None => {
            // No real DST gap exceeds a couple of hours; 4h is a generous, still-bounded probe.
            let mut probe = naive;
            for _ in 0..240 {
                probe += ChronoDuration::minutes(1);
                if let LocalResult::Single(dt) = tz.from_local_datetime(&probe) {
                    return (dt.with_timezone(&Utc), true);
                }
            }
            // Unreachable for any real IANA zone; never panic on a config we already validated.
            (Utc.from_utc_datetime(&naive), true)
        }
    }
}

/// One concrete occurrence of a block: the calendar date its scheduled weekday matched, and the
/// resolved `[start, end)` UTC instants.
#[derive(Debug, Clone)]
pub struct Occurrence<'a> {
    pub block: &'a RoutineBlock,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// The local calendar date (`YYYY-MM-DD`-formattable) this occurrence's weekday matched —
    /// the key skip/notification-dedupe state matches occurrences against.
    pub start_date: NaiveDate,
    pub dst_adjusted: bool,
}

fn occurrence_for(block: &RoutineBlock, tz: Tz, start_date: NaiveDate) -> Option<Occurrence<'_>> {
    if !block.days.contains(&start_date.weekday()) {
        return None;
    }
    let (start, start_dst) = resolve_boundary(tz, start_date, block.start, Boundary::Start);
    let end_date = if block.wraps_midnight() {
        start_date.succ_opt()?
    } else {
        start_date
    };
    let (mut end, end_dst) = resolve_boundary(tz, end_date, block.end, Boundary::End);
    if end <= start {
        // Defensive: guarantee interval ordering even under a pathological DST combination.
        end = start + ChronoDuration::minutes(1);
    }
    Some(Occurrence {
        block,
        start,
        end,
        start_date,
        dst_adjusted: start_dst || end_dst,
    })
}

/// The block occurrence active at `now`, if any — checked against today's and yesterday's local
/// calendar date (the only two start-dates a <24h block could still be spanning `now` from).
fn find_current(cfg: &RoutineConfig, now: DateTime<Utc>) -> Option<Occurrence<'_>> {
    let local_now = now.with_timezone(&cfg.tz);
    let today = local_now.date_naive();
    let candidates = [Some(today), today.pred_opt()];
    for date in candidates.into_iter().flatten() {
        for block in &cfg.blocks {
            if let Some(occ) = occurrence_for(block, cfg.tz, date) {
                if occ.start <= now && now < occ.end {
                    return Some(occ);
                }
            }
        }
    }
    None
}

/// The earliest block occurrence starting strictly after `now`, scanning a full week + one day
/// ahead so every configured block gets at least one candidate start.
fn find_next(cfg: &RoutineConfig, now: DateTime<Utc>) -> Option<Occurrence<'_>> {
    let local_now = now.with_timezone(&cfg.tz);
    let today = local_now.date_naive();
    let mut best: Option<Occurrence<'_>> = None;
    for offset in 0..8 {
        let Some(date) = today.checked_add_signed(ChronoDuration::days(offset)) else {
            continue;
        };
        for block in &cfg.blocks {
            let Some(occ) = occurrence_for(block, cfg.tz, date) else {
                continue;
            };
            if occ.start <= now {
                continue;
            }
            if best.as_ref().is_none_or(|b| occ.start < b.start) {
                best = Some(occ);
            }
        }
    }
    best
}

// ---------------------------------------------------------------------------------------------
// Identity registry (#801 amendment): a frozen legacy `bN` positional-alias -> stable-id map,
// captured once ("the first upgraded transaction") from the config's ordering at that moment, so
// historic skip/delivery events written under the pre-explicit-id `b<index>` convention keep
// folding to the same logical block forever — even after the block is reordered, or the registry
// itself is reloaded on a later run. Reordering AFTER the registry exists cannot retarget state:
// the map is written once and never recomputed from a later config snapshot.
// ---------------------------------------------------------------------------------------------

/// One frozen `bN` legacy alias -> current stable id mapping, captured at registry creation time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IdentityAlias {
    /// The pre-migration positional id (`"b0"`, `"b1"`, …) a config without explicit ids
    /// implicitly had.
    pub alias: String,
    /// The stable id (explicit or content-derived) that block resolved to when the registry was
    /// created.
    pub stable_id: String,
}

/// The versioned, atomically-persisted identity registry — created once, then read-only forever
/// (short of a future explicit re-migration this crate doesn't implement).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IdentityRegistry {
    pub version: u32,
    pub created_at: i64,
    pub aliases: Vec<IdentityAlias>,
}

const IDENTITY_REGISTRY_VERSION: u32 = 1;

fn routine_state_dir() -> PathBuf {
    crate::resolver::state_dir().join("routine")
}

fn identity_registry_path() -> PathBuf {
    routine_state_dir().join("identity.json")
}

/// Load the persisted identity registry, if any, without acquiring any lock — safe because the
/// registry is write-once-then-atomically-replaced (see [`load_or_create_identity_registry`]), so
/// a concurrent reader can never observe a torn write, only "not yet created" or "fully written".
pub fn load_identity_registry() -> Option<IdentityRegistry> {
    let text = std::fs::read_to_string(identity_registry_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// An existing registry is ambiguous against `cfg` when a frozen legacy alias's literal string
/// (e.g. `"b1"`) collides with a *current* block whose own explicit stable id happens to be that
/// same literal, but the registry froze `"b1"` to a *different* block. Folding an old `bN`-keyed
/// event through the registry could then no longer tell whether `"b1"` meant "the block that used
/// to be at position 1" or "the block explicitly named id `b1`" — rather than guess, migration
/// rejects so the operator can rename the colliding explicit id.
fn detect_ambiguous_aliases(
    registry: &IdentityRegistry,
    cfg: &RoutineConfig,
) -> Result<(), String> {
    for alias in &registry.aliases {
        if let Some(block) = cfg.blocks.iter().find(|b| b.id == alias.alias) {
            if block.id != alias.stable_id {
                return Err(format!(
                    "routine: ambiguous identity migration — legacy alias '{}' was frozen to \
                     block id '{}', but block '{}' now has that literal string as its own \
                     explicit id; rename one of them to disambiguate",
                    alias.alias, alias.stable_id, block.name
                ));
            }
        }
    }
    Ok(())
}

/// Load the frozen identity registry, creating it from `cfg`'s current block order the first
/// time this runs after upgrade (#801 amendment invariant 1). Creation captures the *current*
/// ordering once: `b<index>` for each block at its position right now, mapped to that block's
/// resolved stable id — frozen forever after, so a later reorder can't retarget old `bN` state.
/// `Err` when an existing registry is ambiguous against `cfg` (see [`detect_ambiguous_aliases`])
/// — migration refuses to guess rather than silently mis-target a skip/delivery key.
fn load_or_create_identity_registry(
    cfg: &RoutineConfig,
    now: i64,
) -> Result<IdentityRegistry, String> {
    if let Some(existing) = load_identity_registry() {
        detect_ambiguous_aliases(&existing, cfg)?;
        return Ok(existing);
    }
    let aliases = cfg
        .blocks
        .iter()
        .enumerate()
        .map(|(idx, b)| IdentityAlias {
            alias: format!("b{idx}"),
            stable_id: b.id.clone(),
        })
        .collect();
    let registry = IdentityRegistry {
        version: IDENTITY_REGISTRY_VERSION,
        created_at: now,
        aliases,
    };
    crate::store::save_json_atomic(&identity_registry_path(), &registry)
        .map_err(|e| format!("routine: failed to persist identity registry: {e}"))?;
    Ok(registry)
}

/// Fold a stored event's `block_id`/key-prefix field to the stable id it should be compared
/// against: a legacy `bN` alias translates through the frozen registry; any other string (an
/// explicit or content-derived stable id) passes through unchanged. `registry: None` (no registry
/// created yet, or a lock-free best-effort read that found none on disk) means no legacy events
/// exist to fold — every string passes through unchanged.
fn resolve_legacy_block_id(registry: Option<&IdentityRegistry>, stored: &str) -> String {
    registry
        .and_then(|r| r.aliases.iter().find(|a| a.alias == stored))
        .map(|a| a.stable_id.clone())
        .unwrap_or_else(|| stored.to_string())
}

/// Fold a stored `routine_notified` key's `<block_id>@<epoch>:<kind>` prefix the same way
/// [`resolve_legacy_block_id`] folds a bare block id.
fn resolve_legacy_notify_key(registry: Option<&IdentityRegistry>, stored: &str) -> String {
    match stored.split_once('@') {
        Some((block_id, rest)) => {
            format!("{}@{rest}", resolve_legacy_block_id(registry, block_id))
        }
        None => stored.to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// Durable, non-destructive runtime state (check-ins / snoozes / skips / overrides / delivered
// notification keys) — folded from the same append-only `events.jsonl` substrate as `focus`.
// ---------------------------------------------------------------------------------------------

/// Record one date-scoped check-in acknowledgment. Never touches the recurring schedule.
pub fn record_checkin(now: i64, note: Option<&str>) -> std::io::Result<()> {
    let mut fields = Map::new();
    if let Some(note) = note {
        fields.insert("note".into(), json!(note));
    }
    events::append_at_synced("routine_checkin", now, fields)
}

/// The most recent check-in at or before `now` (its timestamp + optional note), if any.
pub fn last_checkin(now: i64) -> Option<(i64, Option<String>)> {
    events::read_all()
        .into_iter()
        .rev()
        .find(|e| e.kind == "routine_checkin" && e.ts <= now)
        .map(|e| {
            let note = e
                .fields
                .get("note")
                .and_then(Value::as_str)
                .map(str::to_string);
            (e.ts, note)
        })
}

/// Silence routine notifications for `minutes` from `now`. A later snooze restarts the window
/// rather than stacking; returns the resolved deadline.
pub fn record_snooze(now: i64, minutes: i64) -> std::io::Result<i64> {
    let until = now + minutes.max(1) * 60;
    let mut fields = Map::new();
    fields.insert("until".into(), json!(until));
    events::append_at_synced("routine_snooze", now, fields)?;
    Ok(until)
}

/// The active snooze deadline at `now`, if any — the latest `routine_snooze` on record whose
/// `until` hasn't yet passed. Expires on its own; no separate clear is needed.
pub fn snoozed_until(now: i64) -> Option<i64> {
    events::read_all()
        .into_iter()
        .rev()
        .filter(|e| e.kind == "routine_snooze" && e.ts <= now)
        .find_map(|e| e.fields.get("until").and_then(Value::as_i64))
        .filter(|&until| until > now)
}

pub fn is_snoozed(now: i64) -> bool {
    snoozed_until(now).is_some()
}

/// Skip one specific block occurrence (`block_id` starting on local `date`, `YYYY-MM-DD`) without
/// touching the recurring schedule — every other occurrence of that block is unaffected.
/// `block_id` is always the block's current stable id — new skips never write a legacy `bN` alias.
pub fn record_skip(now: i64, block_id: &str, date: &str) -> std::io::Result<()> {
    let mut fields = Map::new();
    fields.insert("block_id".into(), json!(block_id));
    fields.insert("date".into(), json!(date));
    events::append_at_synced("routine_skip", now, fields)
}

/// Whether `block_id`'s (a current stable id) occurrence starting on local calendar `date` has
/// been skipped — folding any legacy `bN`-keyed skip through `registry` (see
/// [`resolve_legacy_block_id`]).
pub fn is_skipped(registry: Option<&IdentityRegistry>, block_id: &str, date: &str) -> bool {
    events::read_all().iter().any(|e| {
        e.kind == "routine_skip"
            && e.fields
                .get("block_id")
                .and_then(Value::as_str)
                .map(|stored| resolve_legacy_block_id(registry, stored))
                .as_deref()
                == Some(block_id)
            && e.fields.get("date").and_then(Value::as_str) == Some(date)
    })
}

/// One active temporary override — a manually-declared status that supersedes the recurring
/// schedule until `until`, without ever rewriting it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ActiveOverride {
    pub name: String,
    pub kind: String,
    pub since: i64,
    pub until: i64,
}

/// Declare a temporary override in force until `until` (epoch seconds).
pub fn record_override(now: i64, name: &str, kind: &str, until: i64) -> std::io::Result<()> {
    let mut fields = Map::new();
    fields.insert("name".into(), json!(name));
    // Namespaced as `override_kind` (not `kind`) so it never collides with `Event::kind`, the
    // event-log's own discriminant (`"routine_override"`) — `#[serde(flatten)]` would otherwise
    // emit a duplicate top-level `kind` key that `serde_json` refuses to deserialize, silently
    // dropping the whole event (a bug this exact naming collision caused during development).
    fields.insert("override_kind".into(), json!(kind));
    fields.insert("until".into(), json!(until));
    events::append_at_synced("routine_override", now, fields)
}

/// End the active override early (before its `until`), reverting to the recurring schedule.
pub fn clear_override(now: i64) -> std::io::Result<()> {
    events::append_at_synced("routine_override_clear", now, Map::new())
}

/// The active override at `now`, if any: the latest `routine_override` not yet expired
/// (`until > now`) and not superseded by a later `routine_override_clear`. Overrides always win
/// over the recurring schedule (#801's "temporary override without rewriting the recurring
/// schedule").
pub fn active_override(now: i64) -> Option<ActiveOverride> {
    let events = events::read_all();
    let last_clear_ts = events
        .iter()
        .rev()
        .find(|e| e.kind == "routine_override_clear" && e.ts <= now)
        .map(|e| e.ts);
    events
        .iter()
        .rev()
        .filter(|e| e.kind == "routine_override" && e.ts <= now)
        .find_map(|e| {
            if let Some(clear_ts) = last_clear_ts {
                if e.ts <= clear_ts {
                    return None;
                }
            }
            let until = e.fields.get("until").and_then(Value::as_i64)?;
            if until <= now {
                return None;
            }
            let name = e.fields.get("name").and_then(Value::as_str)?.to_string();
            let kind = e
                .fields
                .get("override_kind")
                .and_then(Value::as_str)?
                .to_string();
            Some(ActiveOverride {
                name,
                kind,
                since: e.ts,
                until,
            })
        })
}

/// Record that the notification keyed `key` (`"<block_id>@<transition_epoch>:<kind>"`, always the
/// current stable id) was delivered — the idempotence guard against a repeated tick or a
/// daemon/timer restart mid-window double-toasting the same transition. Durable: the caller
/// (`tick`) only calls this **after** the notifier itself succeeded, and only trusts the delivery
/// as committed once this synced write returns `Ok` (#801 amendment invariant 3).
pub fn record_notified(now: i64, key: &str) -> std::io::Result<()> {
    let mut fields = Map::new();
    fields.insert("key".into(), json!(key));
    events::append_at_synced("routine_notified", now, fields)
}

/// Whether `key` (built from the current stable id) was already delivered — folding any legacy
/// `bN`-keyed delivery record through `registry`.
pub fn already_notified(registry: Option<&IdentityRegistry>, key: &str) -> bool {
    events::read_all().iter().any(|e| {
        e.kind == "routine_notified"
            && e.fields
                .get("key")
                .and_then(Value::as_str)
                .map(|stored| resolve_legacy_notify_key(registry, stored))
                .as_deref()
                == Some(key)
    })
}

// ---------------------------------------------------------------------------------------------
// Status snapshot (typed current/next/status JSON fields — remaining, alignment, timezone,
// offset, DST adjustment) — the shared read model for `bn routine now/next/status` and the
// `bn next`/`bn daily` integration.
// ---------------------------------------------------------------------------------------------

/// One block view, resolved to concrete local-timezone instants.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BlockView {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub protected: bool,
    /// RFC3339 in the routine's resolved timezone.
    pub start: String,
    /// RFC3339 in the routine's resolved timezone.
    pub end: String,
    /// `"schedule"` (a recurring `[[routine.blocks]]` entry) or `"override"` (a temporary
    /// override currently in force).
    pub source: &'static str,
}

/// A skipped occurrence, surfaced so `status`/`now` stay transparent about why nothing's current.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SkippedView {
    pub id: String,
    pub name: String,
}

/// The full typed status snapshot (#801): current + next blocks, remaining/until-next minutes,
/// schedule alignment, resolved timezone + UTC offset, and whether DST adjusted a boundary.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Snapshot {
    pub enabled: bool,
    pub timezone: String,
    /// e.g. `"+03:00"`.
    pub utc_offset: String,
    /// RFC3339 "as of" instant, in the routine's timezone.
    pub as_of: String,
    pub current: Option<BlockView>,
    pub remaining_minutes: Option<i64>,
    pub next: Option<BlockView>,
    pub until_next_minutes: Option<i64>,
    /// Whether `now` sits inside *some* defined block (or there are no blocks configured at all,
    /// which is vacuously aligned) — a scheduling gap surfaces as `false`, the drift signal.
    pub aligned: bool,
    /// Whether non-urgent `bn next`/daily guidance should be suppressed right now (a protected
    /// block is current and not overridden away).
    pub suppresses_guidance: bool,
    pub dst_adjusted: bool,
    pub skipped: Option<SkippedView>,
    /// RFC3339 snooze deadline, in the routine's timezone, if a snooze is currently active.
    pub snoozed_until: Option<String>,
}

impl Snapshot {
    /// The transparent, human-readable reason non-urgent guidance is paused right now, or `None`
    /// when nothing is being suppressed. `bn next`/`bn daily` print this *instead of* silently
    /// dropping items, so a protected block never looks like a bug — urgent/breaking items still
    /// surface alongside it (#801: "preserves urgent/breaking with transparent explanation").
    pub fn explanation(&self) -> Option<String> {
        self.suppresses_guidance.then(|| {
            let name = self
                .current
                .as_ref()
                .map(|b| b.name.as_str())
                .unwrap_or("this block");
            format!("🛡 {name} is protected — non-urgent guidance paused (urgent items still shown)")
        })
    }
}

/// The routine snapshot `bn next`/`bn daily` fold into their output, or `None` when routine is
/// disabled or its config failed to resolve (a bad `[routine]` section never breaks unrelated
/// commands — see [`crate::resolver::Config::routine`]).
pub fn current_snapshot(cfg: &crate::resolver::Config, now: i64) -> Option<Snapshot> {
    let rc = cfg.routine.as_ref().ok()?;
    if !rc.enabled {
        return None;
    }
    Some(snapshot(rc, now))
}

fn offset_label(dt: DateTime<Tz>) -> String {
    dt.format("%:z").to_string()
}

fn format_instant(epoch: i64, tz: Tz) -> String {
    DateTime::<Utc>::from_timestamp(epoch, 0)
        .map(|dt| dt.with_timezone(&tz).to_rfc3339())
        .unwrap_or_default()
}

fn minutes_until(now: DateTime<Utc>, target: DateTime<Utc>) -> i64 {
    ((target - now).num_seconds().max(0) + 59) / 60
}

fn block_view(occ: &Occurrence<'_>, tz: Tz, source: &'static str) -> BlockView {
    BlockView {
        id: occ.block.id.clone(),
        name: occ.block.name.clone(),
        kind: occ.block.kind.clone(),
        protected: occ.block.is_effectively_protected(),
        start: occ.start.with_timezone(&tz).to_rfc3339(),
        end: occ.end.with_timezone(&tz).to_rfc3339(),
        source,
    }
}

/// The full status snapshot at `now` (epoch seconds). Applies override precedence, skip
/// suppression of the current occurrence only, and reports the snooze deadline for transparency
/// (a snooze never hides `status`/`now` output — only outbound notifications).
pub fn snapshot(cfg: &RoutineConfig, now: i64) -> Snapshot {
    let now_dt = DateTime::<Utc>::from_timestamp(now, 0).unwrap_or_else(Utc::now);
    let local_now = now_dt.with_timezone(&cfg.tz);
    let snoozed = snoozed_until(now).map(|until| format_instant(until, cfg.tz));

    if !cfg.enabled {
        return Snapshot {
            enabled: false,
            timezone: cfg.timezone.clone(),
            utc_offset: offset_label(local_now),
            as_of: local_now.to_rfc3339(),
            remaining_minutes: None,
            current: None,
            next: None,
            until_next_minutes: None,
            aligned: true,
            suppresses_guidance: false,
            dst_adjusted: false,
            skipped: None,
            snoozed_until: snoozed,
        };
    }

    if let Some(ov) = active_override(now) {
        let current = BlockView {
            id: "override".to_string(),
            name: ov.name,
            kind: ov.kind,
            protected: false,
            start: format_instant(ov.since, cfg.tz),
            end: format_instant(ov.until, cfg.tz),
            source: "override",
        };
        let next_occ = find_next(cfg, now_dt);
        return Snapshot {
            enabled: cfg.enabled,
            timezone: cfg.timezone.clone(),
            utc_offset: offset_label(local_now),
            as_of: local_now.to_rfc3339(),
            remaining_minutes: Some(minutes_until(
                now_dt,
                DateTime::<Utc>::from_timestamp(ov.until, 0).unwrap_or(now_dt),
            )),
            current: Some(current),
            next: next_occ.as_ref().map(|o| block_view(o, cfg.tz, "schedule")),
            until_next_minutes: next_occ.as_ref().map(|o| minutes_until(now_dt, o.start)),
            aligned: true,
            suppresses_guidance: false,
            dst_adjusted: next_occ.as_ref().is_some_and(|o| o.dst_adjusted),
            skipped: None,
            snoozed_until: snoozed,
        };
    }

    let current_occ = find_current(cfg, now_dt);
    let registry = load_identity_registry();
    let skipped_view = current_occ.as_ref().and_then(|occ| {
        let date_key = occ.start_date.format("%Y-%m-%d").to_string();
        is_skipped(registry.as_ref(), &occ.block.id, &date_key).then(|| SkippedView {
            id: occ.block.id.clone(),
            name: occ.block.name.clone(),
        })
    });
    let effective_current = if skipped_view.is_some() {
        None
    } else {
        current_occ.as_ref()
    };
    let next_occ = find_next(cfg, now_dt);

    Snapshot {
        enabled: cfg.enabled,
        timezone: cfg.timezone.clone(),
        utc_offset: offset_label(local_now),
        as_of: local_now.to_rfc3339(),
        current: effective_current.map(|o| block_view(o, cfg.tz, "schedule")),
        remaining_minutes: effective_current.map(|o| minutes_until(now_dt, o.end)),
        next: next_occ.as_ref().map(|o| block_view(o, cfg.tz, "schedule")),
        until_next_minutes: next_occ.as_ref().map(|o| minutes_until(now_dt, o.start)),
        aligned: cfg.blocks.is_empty() || effective_current.is_some(),
        suppresses_guidance: effective_current.is_some_and(|o| o.block.is_effectively_protected()),
        dst_adjusted: effective_current.is_some_and(|o| o.dst_adjusted)
            || next_occ.as_ref().is_some_and(|o| o.dst_adjusted),
        skipped: skipped_view,
        snoozed_until: snoozed,
    }
}

/// Whether `bn next`/daily guidance should suppress non-urgent items right now — the cheap gate
/// [`crate::next::next_actions`] consults, mirroring [`crate::focus::is_active`]. `false` when
/// the config failed to resolve or is disabled (callers should treat an `Err` config as a no-op).
pub fn suppresses_guidance(cfg: &RoutineConfig, now: i64) -> bool {
    cfg.enabled && snapshot(cfg, now).suppresses_guidance
}

// ---------------------------------------------------------------------------------------------
// Health (#801 amendment invariant 7): a versioned, atomically-persisted record every locked
// routine transaction updates as its last step ("append event -> atomically replace registry/
// health -> unlock"). Observability only — never load-bearing for a decision.
// ---------------------------------------------------------------------------------------------

const HEALTH_VERSION: u32 = 1;

/// The persisted health record `bn routine status`/a future `bn routine health` renders.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutineHealth {
    pub version: u32,
    pub last_tick_start: Option<i64>,
    pub last_tick_success: Option<i64>,
    /// Set the first tick a live config fails to resolve/validate; cleared the moment it resolves
    /// cleanly again (#801 amendment invariant 4's fail-closed/recovery contract).
    pub invalid_since: Option<i64>,
    pub config_error: Option<String>,
    pub watcher_start: Option<i64>,
    pub watcher_heartbeat: Option<i64>,
    pub watcher_pid: Option<u32>,
    pub next_wake: Option<i64>,
    pub active_block_id: Option<String>,
    pub timezone: Option<String>,
    pub utc_offset: Option<String>,
    pub backend: Option<String>,
    pub last_notify_attempt: Option<i64>,
    pub last_notify_success: Option<i64>,
    pub expired_count: u64,
    pub last_error: Option<String>,
}

impl Default for RoutineHealth {
    fn default() -> Self {
        RoutineHealth {
            version: HEALTH_VERSION,
            last_tick_start: None,
            last_tick_success: None,
            invalid_since: None,
            config_error: None,
            watcher_start: None,
            watcher_heartbeat: None,
            watcher_pid: None,
            next_wake: None,
            active_block_id: None,
            timezone: None,
            utc_offset: None,
            backend: None,
            last_notify_attempt: None,
            last_notify_success: None,
            expired_count: 0,
            last_error: None,
        }
    }
}

fn health_path() -> PathBuf {
    routine_state_dir().join("health.json")
}

/// Load persisted health, resetting to a fresh default on any parse failure or version mismatch
/// (the "health schema migration" case) instead of propagating a hard error.
pub fn load_health() -> RoutineHealth {
    std::fs::read_to_string(health_path())
        .ok()
        .and_then(|t| serde_json::from_str::<RoutineHealth>(&t).ok())
        .filter(|h| h.version == HEALTH_VERSION)
        .unwrap_or_default()
}

fn save_health(health: &RoutineHealth) -> std::io::Result<()> {
    crate::store::save_json_atomic(&health_path(), health)
}

/// How stale a watcher heartbeat/last successful tick may be before it no longer counts as "this
/// is actively covering the schedule right now".
const HEARTBEAT_STALE_SECS: i64 = 20 * 60;
const TICK_STALE_SECS: i64 = 20 * 60;

/// `bn routine status`'s coverage verdict (#801 amendment invariant 7): whether *something* is
/// actively keeping the schedule reconciled right now, degraded (installed/registered but stale,
/// or the config itself is invalid), or uncovered (nothing installed/registered at all).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Coverage {
    Covered,
    Degraded,
    Uncovered,
}

impl Coverage {
    pub fn as_str(self) -> &'static str {
        match self {
            Coverage::Covered => "covered",
            Coverage::Degraded => "degraded",
            Coverage::Uncovered => "uncovered",
        }
    }
}

/// Classify coverage from persisted health plus whether a managed service/timer is installed —
/// pure so it's directly unit-testable without a real launchd/systemd/cron on the test host.
/// Stale heartbeat/pid is never "live"; an invalid config is always at best `degraded` even with
/// a service installed, since fail-closed means it isn't actually doing anything useful right now.
pub fn classify_coverage(
    health: &RoutineHealth,
    service_installed: bool,
    timer_registered: bool,
    now: i64,
) -> Coverage {
    if !service_installed && !timer_registered {
        return Coverage::Uncovered;
    }
    if health.invalid_since.is_some() {
        return Coverage::Degraded;
    }
    let heartbeat_fresh = health.watcher_pid.is_some()
        && health
            .watcher_heartbeat
            .is_some_and(|t| now.saturating_sub(t) <= HEARTBEAT_STALE_SECS);
    let tick_fresh = health
        .last_tick_success
        .is_some_and(|t| now.saturating_sub(t) <= TICK_STALE_SECS);
    if (service_installed && heartbeat_fresh) || (timer_registered && tick_fresh) {
        Coverage::Covered
    } else {
        Coverage::Degraded
    }
}

// ---------------------------------------------------------------------------------------------
// One transaction lock (#801 amendment invariant 2): a single process-shared exclusive routine
// lock with a bounded wait, covering the complete ordered transaction — load/validate config ->
// registry -> state fold -> decide -> append event + flush/sync -> atomically replace registry/
// health -> unlock. Every routine mutation (tick, check-in, snooze, skip, override, clear,
// delivery, migration, health) and the explicit read-only `bn routine status` go through this.
// ---------------------------------------------------------------------------------------------

fn transaction_lock_target() -> PathBuf {
    routine_state_dir().join("tx")
}

/// One locked routine transaction. Holds the lock for its entire lifetime (RAII via the embedded
/// [`crate::store::FileLock`]) — construct it, do everything under it, then let it drop (or hold
/// it across a whole CLI command) so the lock covers the full ordered transaction.
pub struct RoutineTx {
    pub now: i64,
    /// `Err` when the live config failed to resolve/validate this transaction — every
    /// schedule-dependent decision must fail closed on this rather than fall back to a stale
    /// schedule or the host's local time.
    pub config: Result<RoutineConfig, String>,
    /// `Some` only when `config` resolved and identity migration succeeded unambiguously.
    pub registry: Option<IdentityRegistry>,
    registry_error: Option<String>,
    _lock: crate::store::FileLock,
}

impl RoutineTx {
    /// Begin a transaction reloading the *live* config via [`crate::resolver::Config::load`] —
    /// the production entry point every CLI command / the tick engine uses.
    pub fn begin(now: i64) -> Result<RoutineTx, String> {
        Self::begin_with(now, &|| crate::resolver::Config::load().routine)
    }

    /// Begin a transaction sourcing its config from `config_loader` instead of the real
    /// `config.toml` — the deterministic test seam invariant 8 calls for, so tests can pin an
    /// exact [`RoutineConfig`] (or a deliberately-invalid one) without touching the filesystem's
    /// real config resolution.
    pub fn begin_with(
        now: i64,
        config_loader: &dyn Fn() -> Result<RoutineConfig, String>,
    ) -> Result<RoutineTx, String> {
        let lock = crate::store::FileLock::acquire_required(&transaction_lock_target())
            .map_err(|e| format!("routine: could not acquire the routine lock — {e}"))?;
        let config = config_loader();
        let (registry, registry_error) = match &config {
            Ok(rc) => match load_or_create_identity_registry(rc, now) {
                Ok(r) => (Some(r), None),
                Err(e) => (None, Some(e)),
            },
            Err(_) => (None, None),
        };
        Ok(RoutineTx {
            now,
            config,
            registry,
            registry_error,
            _lock: lock,
        })
    }

    /// The combined "can't trust the schedule right now" error: an invalid live config, or a
    /// failed/ambiguous identity migration — both must fail closed identically (#801 amendment
    /// invariant 4).
    pub fn blocking_error(&self) -> Option<String> {
        match &self.config {
            Err(e) => Some(e.clone()),
            Ok(_) => self.registry_error.clone(),
        }
    }

    pub fn is_skipped(&self, block_id: &str, date: &str) -> bool {
        is_skipped(self.registry.as_ref(), block_id, date)
    }

    pub fn already_notified(&self, key: &str) -> bool {
        already_notified(self.registry.as_ref(), key)
    }

    pub fn record_checkin(&self, note: Option<&str>) -> std::io::Result<()> {
        record_checkin(self.now, note)
    }

    pub fn record_snooze(&self, minutes: i64) -> std::io::Result<i64> {
        record_snooze(self.now, minutes)
    }

    pub fn record_skip(&self, block_id: &str, date: &str) -> std::io::Result<()> {
        record_skip(self.now, block_id, date)
    }

    pub fn record_override(&self, name: &str, kind: &str, until: i64) -> std::io::Result<()> {
        record_override(self.now, name, kind, until)
    }

    pub fn clear_override(&self) -> std::io::Result<()> {
        clear_override(self.now)
    }

    pub fn record_notified(&self, key: &str) -> std::io::Result<()> {
        record_notified(self.now, key)
    }

    /// The last ordered step of every transaction: atomically replace the persisted health
    /// record while the lock is still held.
    pub fn save_health(&self, health: &RoutineHealth) -> std::io::Result<()> {
        save_health(health)
    }

    pub fn load_health(&self) -> RoutineHealth {
        load_health()
    }
}

/// Record `bn routine watch`'s startup under the lock: `watcher_start`/`watcher_heartbeat`/
/// `watcher_pid` all set to `now`/`pid`. Called once when the loop starts.
pub fn record_watcher_start(now: i64, pid: u32) -> Result<(), String> {
    let tx = RoutineTx::begin(now)?;
    let mut health = tx.load_health();
    health.watcher_start = Some(now);
    health.watcher_heartbeat = Some(now);
    health.watcher_pid = Some(pid);
    tx.save_health(&health)
        .map_err(|e| format!("routine: failed to persist watcher health: {e}"))
}

/// Record one `bn routine watch` heartbeat under the lock — called after every reconciliation
/// pass so `bn routine service status`/`classify_coverage` can tell a live watcher from a stalled
/// or crashed one (stale heartbeat/pid never reads as live).
pub fn record_watcher_heartbeat(now: i64, pid: u32) -> Result<(), String> {
    let tx = RoutineTx::begin(now)?;
    let mut health = tx.load_health();
    health.watcher_heartbeat = Some(now);
    health.watcher_pid = Some(pid);
    tx.save_health(&health)
        .map_err(|e| format!("routine: failed to persist watcher health: {e}"))
}

/// Clear the watcher pid under the lock on a clean SIGTERM/Ctrl-C exit, so a stopped watcher
/// reads as such immediately rather than waiting out the heartbeat staleness window.
pub fn record_watcher_stop(now: i64) -> Result<(), String> {
    let tx = RoutineTx::begin(now)?;
    let mut health = tx.load_health();
    health.watcher_pid = None;
    tx.save_health(&health)
        .map_err(|e| format!("routine: failed to persist watcher health: {e}"))
}

// ---------------------------------------------------------------------------------------------
// Shared at-least-once tick engine (#801 amendment invariants 3/4): the one evaluator behind
// manual `bn routine tick`, `bn routine watch`, the native `routine-tick` cron/timer job, and the
// `notify` compatibility alias — configurable reminders before transitions + at block start,
// deduped by stable block id + resolved transition instant + event kind, bounded by a
// configurable catch-up horizon so a crash/missed tick can never resend an arbitrarily stale
// notification.
// ---------------------------------------------------------------------------------------------

/// Explicit at-least-once duplicate-risk disclosure (#801 amendment invariant 3): a crash between
/// a successful notification and its durable delivery-record flush may resend the same event on
/// the next tick. Surfaced verbatim in [`TickOutcome`]/docs/output rather than left implicit.
pub const DUPLICATE_RISK: &str = "at-least-once: a crash between a successful notification and \
     its durable delivery record may resend the same event on the next tick; delivery keys \
     (stable block id + resolved instant + event kind) dedupe once the record is flushed";

/// How long (seconds) an invalid live config's bounded retry waits before the next reconciliation
/// attempt — `watch`'s sleep-until computation when a tick reports `error`.
pub const INVALID_CONFIG_RETRY_SECS: i64 = 60;

/// The notification seam ticks call through — re-exported from [`crate::notify::policy`], which
/// now owns it: the policy gate, rather than the routine tick, is the thing worth faking. Existing
/// callers and test fakes are unaffected by the move.
///
/// Note that two dedup layers now cover a routine reminder, and both are deliberate: this module's
/// schedule-shaped key (stable block id + resolved instant + event kind, see [`DUPLICATE_RISK`])
/// decides whether the *reminder* is due, while the gate's interrupt-shaped key decides whether
/// it is worth interrupting for. A reminder the gate holds back is still recorded as notified
/// here, so the tick does not retry it every five minutes — `bn notify status` is where a
/// suppressed one shows up, and `bn routine status` still shows the block itself.
pub use crate::notify::policy::{Notifier, RealNotifier};

/// One due reminder/start-of-block candidate before dedupe/horizon classification.
struct DueEvent {
    key: String,
    message: String,
    kind: &'static str,
    /// The instant this notification *became* due (not the transition itself): a reminder is due
    /// at `occurrence.start - reminder_minutes_before`; a start toast is due at
    /// `occurrence.start`. `now - due_instant` is the staleness the catch-up horizon bounds.
    due_instant: i64,
}

/// Candidate due events at `now`. Pure — no I/O, no dedupe/horizon filtering — so it's directly
/// unit-testable; [`tick_with`] applies dedupe + the catch-up horizon.
fn due_events(cfg: &RoutineConfig, now: i64) -> Vec<DueEvent> {
    let now_dt = DateTime::<Utc>::from_timestamp(now, 0).unwrap_or_else(Utc::now);
    let mut due = Vec::new();

    if let Some(occ) = find_next(cfg, now_dt) {
        let due_instant = occ.start.timestamp() - cfg.reminder_minutes_before * 60;
        if due_instant <= now {
            let key = format!("{}@{}:reminder", occ.block.id, occ.start.timestamp());
            let message = format!(
                "Upcoming: {} starts in {}m",
                occ.block.name,
                minutes_until(now_dt, occ.start)
            );
            due.push(DueEvent {
                key,
                message,
                kind: "reminder",
                due_instant,
            });
        }
    }

    if cfg.notify_on_start {
        if let Some(occ) = find_current(cfg, now_dt) {
            let due_instant = occ.start.timestamp();
            if due_instant <= now {
                let key = format!("{}@{}:start", occ.block.id, occ.start.timestamp());
                let message = format!("Now: {}", occ.block.name);
                due.push(DueEvent {
                    key,
                    message,
                    kind: "start",
                    due_instant,
                });
            }
        }
    }

    due
}

/// One classified tick event outcome (sent/deduped/expired) the CLI/JSON renders.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TickEvent {
    pub key: String,
    pub message: String,
    pub kind: &'static str,
}

/// `bn routine tick`'s full outcome (#801 amendment invariant 4) — the shared shape every caller
/// (manual tick, watch, the native `routine-tick` job, the `notify` alias) renders.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TickOutcome {
    /// RFC3339 UTC instant this tick evaluated at.
    pub evaluated_at: String,
    pub enabled: bool,
    pub snoozed: bool,
    pub active_id: Option<String>,
    pub active: Option<String>,
    pub due: usize,
    pub sent: Vec<TickEvent>,
    pub deduped: Vec<TickEvent>,
    pub expired: Vec<TickEvent>,
    pub backend_available: bool,
    pub duplicate_risk: &'static str,
    /// Set when the live config is invalid, or identity migration failed/was ambiguous — no
    /// notification is ever sent while this is set (#801 amendment invariant 4's fail-closed
    /// rule).
    pub error: Option<String>,
    /// RFC3339 UTC instant of the next deadline worth reconciling at (`watch`'s sleep target).
    pub next_wake: Option<String>,
    pub next_wake_epoch: Option<i64>,
}

fn rfc3339_utc(epoch: i64) -> String {
    DateTime::<Utc>::from_timestamp(epoch, 0)
        .unwrap_or_else(Utc::now)
        .to_rfc3339()
}

/// A bounded reconciliation cap (#801 amendment invariant 5) — `watch` never sleeps longer than
/// this even with nothing else scheduled, so it never busy-polls but also never sleeps "forever".
/// Overridable via `$BN_ROUTINE_RECONCILE_CAP_SECS` for tests.
fn reconcile_cap_secs() -> i64 {
    non_empty_env("BN_ROUTINE_RECONCILE_CAP_SECS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(900)
        .max(1)
}

/// The next instant `watch` should wake and reconcile at (#801 amendment invariant 5): the
/// earliest of the next occurrence's reminder-due instant, its start instant, the current
/// occurrence's end, an active snooze/override expiry, or the bounded reconciliation cap.
fn next_wake_epoch(cfg: &RoutineConfig, now: i64) -> i64 {
    let now_dt = DateTime::<Utc>::from_timestamp(now, 0).unwrap_or_else(Utc::now);
    let cap = now + reconcile_cap_secs();
    let mut candidates = vec![cap];
    if let Some(occ) = find_current(cfg, now_dt) {
        candidates.push(occ.end.timestamp());
    }
    if let Some(occ) = find_next(cfg, now_dt) {
        let reminder_due = occ.start.timestamp() - cfg.reminder_minutes_before * 60;
        candidates.push(reminder_due);
        candidates.push(occ.start.timestamp());
    }
    if let Some(until) = snoozed_until(now) {
        candidates.push(until);
    }
    if let Some(ov) = active_override(now) {
        candidates.push(ov.until);
    }
    candidates
        .into_iter()
        .filter(|&t| t > now)
        .min()
        .unwrap_or(cap)
}

fn invalid_tick_outcome(evaluated_at: String, error: String, now: i64) -> TickOutcome {
    TickOutcome {
        evaluated_at,
        enabled: false,
        snoozed: false,
        active_id: None,
        active: None,
        due: 0,
        sent: Vec::new(),
        deduped: Vec::new(),
        expired: Vec::new(),
        backend_available: true,
        duplicate_risk: DUPLICATE_RISK,
        error: Some(error),
        next_wake: Some(rfc3339_utc(now + INVALID_CONFIG_RETRY_SECS)),
        next_wake_epoch: Some(now + INVALID_CONFIG_RETRY_SECS),
    }
}

/// Run one shared tick (#801 amendment invariants 2/3/4): begin the locked transaction (load/
/// validate config -> registry), evaluate due reminder/start events, deliver each not-yet-
/// delivered one within the catch-up horizon via `notifier`, persist health, then let the
/// transaction's lock drop. `Err` only when the *lock itself* couldn't be acquired (busy/
/// timeout) — an invalid live config is reported via `TickOutcome.error`, never a hard `Err`,
/// because the caller (watch/cron) must keep running to reconcile once it's fixed.
pub fn tick_with(
    now: i64,
    notifier: &dyn Notifier,
    config_loader: &dyn Fn() -> Result<RoutineConfig, String>,
) -> Result<TickOutcome, String> {
    let tx = RoutineTx::begin_with(now, config_loader)?;
    let evaluated_at = rfc3339_utc(now);

    if let Some(err) = tx.blocking_error() {
        let mut health = tx.load_health();
        if health.invalid_since.is_none() {
            health.invalid_since = Some(now);
        }
        health.config_error = Some(err.clone());
        health.last_error = Some(err.clone());
        let _ = tx.save_health(&health);
        return Ok(invalid_tick_outcome(evaluated_at, err, now));
    }

    let cfg = match tx.config.as_ref() {
        Ok(cfg) => cfg,
        Err(err) => {
            return Ok(invalid_tick_outcome(evaluated_at, err.clone(), now));
        }
    };
    let mut health = tx.load_health();
    // Recovery: a previously-invalid config just resolved cleanly again.
    health.invalid_since = None;
    health.config_error = None;
    health.last_tick_start = Some(now);
    health.timezone = Some(cfg.timezone.clone());

    if !cfg.enabled {
        health.last_tick_success = Some(now);
        let _ = tx.save_health(&health);
        return Ok(TickOutcome {
            evaluated_at,
            enabled: false,
            snoozed: false,
            active_id: None,
            active: None,
            due: 0,
            sent: Vec::new(),
            deduped: Vec::new(),
            expired: Vec::new(),
            backend_available: true,
            duplicate_risk: DUPLICATE_RISK,
            error: None,
            next_wake: None,
            next_wake_epoch: None,
        });
    }

    let now_dt = DateTime::<Utc>::from_timestamp(now, 0).unwrap_or_else(Utc::now);
    let current = find_current(cfg, now_dt);
    health.active_block_id = current.as_ref().map(|o| o.block.id.clone());
    health.utc_offset = Some(offset_label(now_dt.with_timezone(&cfg.tz)));

    let snoozed = is_snoozed(now);
    let horizon_secs = cfg.catchup_horizon_minutes * 60;
    let candidates = due_events(cfg, now);
    let due_count = candidates.len();

    let mut sent = Vec::new();
    let mut deduped = Vec::new();
    let mut expired = Vec::new();
    let mut backend_available = true;

    if !snoozed {
        for ev in candidates {
            let outcome = TickEvent {
                key: ev.key.clone(),
                message: ev.message.clone(),
                kind: ev.kind,
            };
            if tx.already_notified(&ev.key) {
                deduped.push(outcome);
                continue;
            }
            let age = now - ev.due_instant;
            if age > horizon_secs {
                expired.push(outcome);
                health.expired_count += 1;
                continue;
            }
            health.last_notify_attempt = Some(now);
            health.backend = Some("desktop-toast".to_string());
            if notifier.notify("bn routine", &ev.message) {
                if tx.record_notified(&ev.key).is_ok() {
                    health.last_notify_success = Some(now);
                    sent.push(outcome);
                }
                // A durable-write failure here is silently retried next tick (still within the
                // horizon) rather than reported as sent — the notifier already fired, but without
                // a flushed delivery record this event isn't "delivered" per invariant 3.
            } else {
                backend_available = false;
            }
        }
    }

    health.last_tick_success = Some(now);
    let next_wake = next_wake_epoch(cfg, now);
    health.next_wake = Some(next_wake);
    let _ = tx.save_health(&health);

    Ok(TickOutcome {
        evaluated_at,
        enabled: true,
        snoozed,
        active_id: current.as_ref().map(|o| o.block.id.clone()),
        active: current.as_ref().map(|o| o.block.name.clone()),
        due: due_count,
        sent,
        deduped,
        expired,
        backend_available,
        duplicate_risk: DUPLICATE_RISK,
        error: None,
        next_wake: Some(rfc3339_utc(next_wake)),
        next_wake_epoch: Some(next_wake),
    })
}

/// Production entry point: [`tick_with`] using [`RealNotifier`] and the live `config.toml`
/// (reloaded fresh, under the lock, every call). The one shared evaluator behind manual
/// `bn routine tick`, `bn routine watch`, the native `routine-tick` cron/timer job, and the
/// `notify` compatibility alias (#801 amendment invariant 3).
pub fn tick(now: i64) -> Result<TickOutcome, String> {
    tick_with(now, &RealNotifier, &|| {
        crate::resolver::Config::load().routine
    })
}

/// One `bn routine watch` reconciliation pass (#801 amendment invariant 5): run the shared tick,
/// then report how long to sleep before the next pass. Pure aside from the tick itself, so the
/// watch loop's sleep-chunking/signal-handling stays in `bn-cli` while the reconciliation math
/// stays here, deterministically testable.
pub struct WatchPass {
    pub tick: TickOutcome,
    pub sleep_secs: i64,
}

/// Run one watch pass at `now`: the shared tick, plus the sleep-until duration derived from its
/// `next_wake_epoch` (already fail-closed-aware: an invalid config's `next_wake` is the bounded
/// retry, not a stale schedule deadline).
pub fn watch_pass(now: i64) -> Result<WatchPass, String> {
    let outcome = tick(now)?;
    let sleep_secs = outcome
        .next_wake_epoch
        .map(|t| (t - now).max(1))
        .unwrap_or_else(reconcile_cap_secs);
    Ok(WatchPass {
        tick: outcome,
        sleep_secs,
    })
}

/// Backward-compatible outcome shape for the pre-amendment `notify()` alias/tests.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NotifyOutcome {
    pub enabled: bool,
    pub snoozed: bool,
    pub sent: Vec<String>,
    pub backend_available: bool,
}

/// Compatibility alias (#801 amendment invariant 4): delivers through the exact same
/// [`tick_with`] engine `bn routine tick`/`watch`/the native job use, reshaped into the
/// pre-amendment `NotifyOutcome` the CLI's `bn routine notify` used to render. Unlike the real
/// `tick`/`watch` entry points, this evaluates against the *given* `cfg` rather than reloading
/// `config.toml` itself — every caller already resolved it once in the same process tick, so
/// there is no staleness to guard against here the way there is across a long-lived watcher.
pub fn notify(cfg: &RoutineConfig, now: i64) -> NotifyOutcome {
    let cfg = cfg.clone();
    match tick_with(now, &RealNotifier, &move || Ok(cfg.clone())) {
        Ok(outcome) => NotifyOutcome {
            enabled: outcome.enabled,
            snoozed: outcome.snoozed,
            sent: outcome.sent.into_iter().map(|e| e.message).collect(),
            backend_available: outcome.backend_available,
        },
        Err(_) => NotifyOutcome {
            enabled: false,
            snoozed: false,
            sent: Vec::new(),
            backend_available: false,
        },
    }
}
