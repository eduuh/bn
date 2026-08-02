//! Daily goals — a day-scoped, cross-branch todo list. Each day is one file
//! (`<personal_root>/<machine>/daily/<YYYY-MM-DD>.md`) holding a list of goal items:
//! activities/actions, each optionally linking to an ADO work item or any URL.
//!
//! Unlike branch notes this axis is keyed by **date**, not `<repo>/<branch>`. The first
//! time a day is opened, the previous day's *unfinished* goals carry forward (links
//! preserved); completed goals stay as history on the day they were done.
//!
//! Goals reuse the note [`Item`] type (with its optional `link`) so id allocation
//! ([`crate::id::next_id`]) and done semantics are shared.

use crate::commands::CmdError;
use crate::id::{next_id, next_id_from_ids};
use crate::model::{GoalNote, Item};
use crate::resolver::Config;
use crate::store::StoreError;
use crate::util;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// Id prefix for daily goals (`g1`, `g2`, …) — distinct from the six note-section prefixes.
const GOAL_PREFIX: char = 'g';

/// Id prefix for scheduled follow-up items in the queue (`s1`, `s2`, …).
const SCHED_PREFIX: char = 's';

/// Id prefix for notes connected to a goal (`n1`, `n2`, …) — allocated per-goal (#564).
const NOTE_PREFIX: char = 'n';

/// One day's goals. `goals` reuses [`Item`]: `done` is always `Some(_)` (every goal is a
/// todo), and `link` carries the optional ADO/URL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DailyFront {
    pub date: String,
    pub created: String,
    #[serde(default)]
    pub goals: Vec<Item>,
    /// Things learned about the codebase/tooling today (the weekly "learn something new" habit).
    /// Omitted from YAML while empty so days without entries stay byte-identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub learnings: Vec<LogEntry>,
    /// Reflections — one thing to improve next time (the weekly "reflect" habit).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reflections: Vec<LogEntry>,
}

/// A dated day-log line — a `bn learned` or `bn reflect` entry. Distinct from [`Item`] (a goal):
/// it has no done-state and no carry-forward, but it *does* carry optional provenance stamped at
/// write time so `bn learnings` can attribute a fleet's learnings back to the branch/issue/harness
/// that produced them. All provenance is `Option` and skipped-when-absent, so a plain-shell
/// `bn learned` on the daily axis (no worktree) round-trips byte-identically to the legacy
/// `{id, text, created}` shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub id: String,
    pub text: String,
    #[serde(alias = "added")]
    pub created: String,
    /// Repo the entry was recorded in (basename, e.g. `bn`), when `bn learned`/`bn reflect` ran
    /// inside a resolvable worktree. Absent for the daily/global axis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Branch the entry was recorded on, paired with `repo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Issue/work-item the branch note points at (its `source:` collab line or `work_item`),
    /// so a learning can be traced back to the dispatch that produced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    /// Harness driving bn when the entry was recorded (`claude`/`copilot`), from
    /// [`crate::util::detect_harness`]. `Some` ⇒ an agent authored it; `None` ⇒ recorded at a
    /// plain shell (mine).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
}

/// Provenance stamped onto a [`LogEntry`] at write time. Gathered by the CLI (which can resolve
/// the cwd and read the branch note) and passed into [`learned`]/[`reflect`]; `Default` (all
/// `None`) is the daily/global axis with no branch attribution.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogProvenance {
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub issue: Option<String>,
    pub harness: Option<String>,
}

/// A fully-read day file: just the typed frontmatter (the body is a regenerated title).
#[derive(Debug, Clone, PartialEq)]
pub struct Daily {
    pub front: DailyFront,
}

/// One open goal that recurred across multiple days in an aggregated window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedGoal {
    pub text: String,
    pub days: usize,
}

/// Canonical daily-goal counts for a window of day files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoalAggregation {
    pub completed: usize,
    pub carried: Vec<CarriedGoal>,
    pub slipped: usize,
}

/// Result of `add`. `scheduled` is `None` for a goal placed straight onto today; `Some(date)`
/// when it was queued as a follow-up for a future day. `landed_today` is true when it ended up
/// on today's list (a plain add, or a follow-up whose date was already due).
#[derive(Debug)]
pub struct AddOutcome {
    pub id: String,
    pub text: String,
    pub link: Option<String>,
    pub scheduled: Option<String>,
    pub landed_today: bool,
}

/// Result of `done`: the completed goal's id and text.
#[derive(Debug)]
pub struct DoneOutcome {
    pub id: String,
    pub text: String,
    /// Recorded partial progress (1–99) when the goal was advanced rather than closed
    /// (`bn daily done --pct N`, #213); `None` means it completed.
    pub pct: Option<u8>,
    /// Actual duration stamped from the punch log at close (calibration), when the goal carried
    /// an `est` and its linked punch intervals summed to more than zero — else `None`.
    pub actual: Option<String>,
}

/// Result of `drop_goal`: the removed goal's id, text, and link — enough for the caller to say
/// what left the list (and for the operator to re-add it if the drop was a mistake).
#[derive(Debug)]
pub struct DropOutcome {
    pub id: String,
    pub text: String,
    pub link: Option<String>,
}

/// A note to connect to a goal (#564): a linked branch note (`repo` + `branch`) and/or attached
/// free-text / uploaded content (`text`), optionally titled. [`connect_note`] normalizes each
/// field (trim + drop-if-empty) and rejects an input that carries neither a complete link nor text.
#[derive(Debug, Clone, Default)]
pub struct NoteInput {
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub text: Option<String>,
    pub title: Option<String>,
}

/// Result of connecting or disconnecting a goal note: the goal it now hangs on (id + text) and the
/// affected [`GoalNote`] (the freshly-connected one, or the just-removed one).
#[derive(Debug)]
pub struct NoteOutcome {
    pub goal_id: String,
    pub goal_text: String,
    pub note: GoalNote,
}

/// A follow-up queued for a future day. Lives in the scheduled queue (not a day file, which
/// would suppress that day's carry-over) until its `scheduled` date arrives, when
/// `ensure_today` drains it into that day's goals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduledItem {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Time estimate (`--est`), preserved so a scheduled goal keeps its estimate when it drains
    /// onto its day. Omitted from YAML when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub est: Option<String>,
    /// Optional quarter-goal key carried through the queue so a tagged follow-up keeps its tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    /// Target date (`YYYY-MM-DD`) the follow-up should surface on.
    pub scheduled: String,
    pub created: String,
}

/// The scheduled-queue file's frontmatter: just the list of pending follow-ups.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct QueueFront {
    #[serde(default)]
    scheduled: Vec<ScheduledItem>,
}

/// Result of `defer`: the goal moved off today and the date it's now scheduled for.
#[derive(Debug)]
pub struct DeferOutcome {
    pub id: String,
    pub text: String,
    pub scheduled: String,
}

impl Daily {
    fn empty(date: &str) -> Daily {
        Daily {
            front: DailyFront {
                date: date.to_string(),
                created: date.to_string(),
                goals: vec![],
                learnings: vec![],
                reflections: vec![],
            },
        }
    }

    /// Open goals (not yet done), in order.
    pub fn open(&self) -> impl Iterator<Item = &Item> {
        self.front.goals.iter().filter(|g| g.done == Some(false))
    }

    /// Completed goals, in order.
    pub fn completed(&self) -> impl Iterator<Item = &Item> {
        self.front.goals.iter().filter(|g| g.done == Some(true))
    }
}

/// Render a day to its on-disk text: `---\n<frontmatter>---\n\n# Daily — <date>\n`.
fn to_text(daily: &Daily) -> Result<String, StoreError> {
    let fm = serde_yaml::to_string(&daily.front).map_err(|e| StoreError::Parse(e.to_string()))?;
    Ok(format!("---\n{fm}---\n\n# Daily — {}\n", daily.front.date))
}

/// Read a day file, parsing the leading `---` frontmatter block into [`DailyFront`].
pub fn read(path: &Path) -> Result<Daily, StoreError> {
    let text = std::fs::read_to_string(path)?;
    let front: DailyFront = crate::store::parse_frontmatter(&text)?;
    Ok(Daily { front })
}

/// Serialize a day and write it (creating the `daily/` dir as needed).
fn write(daily: &Daily, path: &Path) -> Result<(), StoreError> {
    let text = to_text(daily)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

/// Ensure today's day file exists, returning it. On first open of a new day, the previous
/// day's **open** goals are carried forward — renumbered `g1..`, link and original
/// `created` date kept (so "pending N days" stays visible); completed goals are left behind
/// on their own day. Then any scheduled follow-ups now due (`scheduled <= today`) are
/// drained from the queue into today. Idempotent: re-opening the same day re-seeds nothing
/// and only writes if a due follow-up landed.
pub fn ensure_today(cfg: &Config, today: &str) -> Result<Daily, StoreError> {
    let path = cfg.daily_file(today);
    let existed = path.exists();

    let mut daily = if existed {
        read(&path)?
    } else {
        let mut d = Daily::empty(today);
        if let Some(prev_path) = latest_prior(&cfg.daily_dir(), today) {
            if let Ok(prev) = read(&prev_path) {
                for g in prev.open() {
                    let id = next_id(&d.front.goals, GOAL_PREFIX);
                    d.front.goals.push(Item {
                        id,
                        text: g.text.clone(),
                        created: g.created.clone(),
                        done: Some(false),
                        done_at: None,
                        link: g.link.clone(),
                        pct: None,
                        est: g.est.clone(),
                        actual: g.actual.clone(),
                        q: g.q.clone(),
                        notes: g.notes.clone(),
                    });
                }
            }
        }
        d
    };

    let drained = drain_due_into(cfg, &mut daily, today)?;
    if !existed || drained {
        write(&daily, &path)?;
    }
    Ok(daily)
}

/// The most recent day file strictly before `today`, read into a [`Daily`] (the "yesterday"
/// for standup — could be Friday on a Monday). `None` if there's no prior day.
pub fn previous_day(cfg: &Config, today: &str) -> Option<Daily> {
    let path = latest_prior(&cfg.daily_dir(), today)?;
    read(&path).ok()
}

/// Read every existing day file in the inclusive date range `[start..=end]`, oldest first.
/// Missing days are skipped. Used by the weekly review to gather completed goals.
pub fn read_window(cfg: &Config, start: &str, end: &str) -> Vec<Daily> {
    let mut out = Vec::new();
    let mut cur = start.to_string();
    while cur.as_str() <= end {
        let path = cfg.daily_file(&cur);
        if path.exists() {
            if let Ok(d) = read(&path) {
                out.push(d);
            }
        }
        match util::add_days(&cur, 1) {
            Some(next) => cur = next,
            None => break,
        }
    }
    out
}

/// Aggregate completions and recurring open goals across a daily window.
///
/// Open goals are matched by trimmed, lowercased, whitespace-collapsed text and count at most
/// once per day. Carried goals appear on at least two distinct days and are ordered most-carried
/// first, then by display text.
pub fn aggregate_goals(days: &[Daily]) -> GoalAggregation {
    let mut completed = 0;
    let mut per_goal: BTreeMap<String, (usize, String)> = BTreeMap::new();

    for day in days {
        completed += day.completed().count();
        let mut seen = HashSet::new();
        for goal in day.open() {
            let normalized = goal
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            if normalized.is_empty() || !seen.insert(normalized.clone()) {
                continue;
            }
            let entry = per_goal
                .entry(normalized)
                .or_insert_with(|| (0, goal.text.trim().to_string()));
            entry.0 += 1;
        }
    }

    let mut carried: Vec<CarriedGoal> = per_goal
        .into_values()
        .filter(|(days, _)| *days >= 2)
        .map(|(days, text)| CarriedGoal { text, days })
        .collect();
    carried.sort_by(|a, b| b.days.cmp(&a.days).then_with(|| a.text.cmp(&b.text)));
    let slipped = carried.len();

    GoalAggregation {
        completed,
        carried,
        slipped,
    }
}

/// Outcome of `rollover`: whether today's file was freshly created, what carried forward
/// from the prior day, what scheduled follow-ups came due, and the resulting day.
#[derive(Debug)]
pub struct RolloverOutcome {
    pub created_today: bool,
    pub carried: Vec<Item>,
    pub due: Vec<ScheduledItem>,
    /// Inbox captures auto-routed to their hinted branch's todos by the rollover drain.
    pub inbox_routed: Vec<crate::inbox::RoutedOutcome>,
    /// Captures still in the inbox afterwards — they need interactive triage (`bn q drain`).
    pub inbox_left: usize,
    /// Goals retired because the PR they hang off has merged or closed (#966) — they can't be
    /// acted on any more, so they leave the list instead of carrying forever.
    pub retired: PruneOutcome,
    pub today: Daily,
}

/// What a [`prune_dead_pr_goals`] sweep retired: open goals dropped off today's list and
/// follow-ups dropped from the scheduled queue, both because their PR is merged/closed.
#[derive(Debug, Default)]
pub struct PruneOutcome {
    pub goals: Vec<Item>,
    pub scheduled: Vec<ScheduledItem>,
}

impl PruneOutcome {
    /// Nothing was retired — the common case, and what the caller prints "nothing stale" for.
    pub fn is_empty(&self) -> bool {
        self.goals.is_empty() && self.scheduled.is_empty()
    }

    /// Total items retired across both lists.
    pub fn len(&self) -> usize {
        self.goals.len() + self.scheduled.len()
    }
}

/// Force today's roll-forward: ensure today exists (carry prior open goals + drain due
/// follow-ups) and report what moved. Idempotent — running twice in a day carries nothing
/// the second time. Backs the morning `daily-rollover` job so unfinished goals advance even
/// on days bn is never opened interactively.
pub fn rollover(cfg: &Config, today: &str) -> Result<RolloverOutcome, StoreError> {
    let existed = cfg.daily_file(today).exists();
    // Capture provenance BEFORE ensure_today merges everything together.
    let carried: Vec<Item> = if existed {
        vec![]
    } else {
        previous_day(cfg, today)
            .map(|d| {
                d.front
                    .goals
                    .into_iter()
                    .filter(|g| g.done == Some(false))
                    .collect()
            })
            .unwrap_or_default()
    };
    let due: Vec<ScheduledItem> = read_queue(cfg)
        .into_iter()
        .filter(|s| s.scheduled.as_str() <= today)
        .collect();
    ensure_today(cfg, today)?;
    // Drain the capture inbox's unambiguous items (hinted at a still-active branch note) so
    // the inbox empties even on days bn is never opened interactively. Only Store errors can
    // surface here — the drain adds non-empty text to the literal "todo" section — but the
    // CmdError wrapper still has to be unwrapped for this fn's StoreError signature.
    let inbox = crate::inbox::drain_auto(cfg, today).map_err(|e| match e {
        CmdError::Store(s) => s,
        other => StoreError::Parse(other.message()),
    })?;
    // Retire goals whose PR has merged or closed *after* the carry: a stale "ping reviewers on
    // #923" that was carried a moment ago is exactly what this sweep exists to catch, and it has
    // to run on today's whole list (not just the carried slice) because any `bn` command could
    // have created the day before the morning job got here.
    let retired = prune_dead_pr_goals(cfg, today, &dead_prs(cfg))?;
    // Re-read: the prune rewrote today's file and renumbered nothing, but the returned day must
    // reflect the pruned list rather than the pre-sweep one.
    let today_daily = ensure_today(cfg, today)?;
    Ok(RolloverOutcome {
        created_today: !existed,
        carried,
        due,
        inbox_routed: inbox.routed,
        inbox_left: inbox.left,
        retired,
        today: today_daily,
    })
}

/// The PRs your local record says have left the open set — read from the metrics log every
/// machine syncs, so this stays offline (`pr-observe` does the fetching). See
/// [`crate::pr_observe::terminal_prs`].
pub fn dead_prs(cfg: &Config) -> HashSet<(String, i64)> {
    crate::pr_observe::terminal_prs(&crate::events::read_events_across(
        &cfg.metrics_logs_all_machines(),
    ))
}

/// Retire goals hanging off a PR in `dead` — merged or closed, so the goal ("ping reviewers on
/// wira360#923") can never be acted on again and would otherwise carry forward forever (#966).
///
/// Only **open** goals are swept: a completed goal is history and stays on the day it was done.
/// Scheduled follow-ups are swept the same way, so a queued ping for a since-merged PR never
/// lands. Goals with no link, or a link that isn't a PR URL, are always left alone — the sweep
/// acts on positive evidence only. Writes only when something was actually retired.
pub fn prune_dead_pr_goals(
    cfg: &Config,
    today: &str,
    dead: &HashSet<(String, i64)>,
) -> Result<PruneOutcome, StoreError> {
    let mut out = PruneOutcome::default();
    if dead.is_empty() {
        return Ok(out);
    }
    let is_dead =
        |link: Option<&String>| link.is_some_and(|l| crate::pr_observe::link_is_terminal(dead, l));

    let path = cfg.daily_file(today);
    if path.exists() {
        let mut daily = read(&path)?;
        let (retired, kept): (Vec<Item>, Vec<Item>) = daily
            .front
            .goals
            .drain(..)
            .partition(|g| g.done == Some(false) && is_dead(g.link.as_ref()));
        if !retired.is_empty() {
            daily.front.goals = kept;
            write(&daily, &path)?;
            out.goals = retired;
        }
    }

    let queue = read_queue(cfg);
    if !queue.is_empty() {
        let (retired, kept): (Vec<ScheduledItem>, Vec<ScheduledItem>) =
            queue.into_iter().partition(|s| is_dead(s.link.as_ref()));
        if !retired.is_empty() {
            write_queue(cfg, &kept)?;
            out.scheduled = retired;
        }
    }
    Ok(out)
}

/// The most recent prior day file in `dir` (a `YYYY-MM-DD.md` stem strictly before
/// `today`). ISO dates sort lexicographically, so the max qualifying stem is the latest.
fn latest_prior(dir: &Path, today: &str) -> Option<std::path::PathBuf> {
    let mut best: Option<(String, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !util::is_iso_date(stem) || stem >= today {
            continue;
        }
        if best
            .as_ref()
            .map(|(b, _)| stem > b.as_str())
            .unwrap_or(true)
        {
            best = Some((stem.to_string(), path));
        }
    }
    best.map(|(_, p)| p)
}

/// Resolve the link to attach to a goal. An explicit `--link` URL wins; otherwise a
/// `--task <id>` is recorded verbatim. Neither → no link.
pub fn resolve_link(task: Option<&str>, link: Option<&str>) -> Option<String> {
    if let Some(url) = link.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(url.to_string());
    }
    task.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Add a goal with neither an estimate nor a quarter tag — the common path.
pub fn add(
    cfg: &Config,
    today: &str,
    text: &str,
    link: Option<String>,
    on: Option<&str>,
) -> Result<AddOutcome, CmdError> {
    add_full(cfg, today, text, link, None, on, None)
}

/// [`add`] with an optional `--est` estimate (a `bn focus start` duration; validated by the
/// caller). The estimate rides onto the landed goal, or onto the scheduled follow-up so it
/// survives the drain. `est = None` is the plain `add` behaviour.
pub fn add_est(
    cfg: &Config,
    today: &str,
    text: &str,
    link: Option<String>,
    on: Option<&str>,
    est: Option<String>,
) -> Result<AddOutcome, CmdError> {
    add_full(cfg, today, text, link, None, on, est)
}

/// Add a goal, optionally tagged to a quarter goal (`--q <key>`). With `on = None` it lands
/// straight on today's list (creating the day with carry-over if needed) with a fresh `g<N>`
/// id. With `on = Some(date)` it's queued as a follow-up for that day (a `schedule` call); a
/// date already due drains onto today via `ensure_today`. Persists and reports where it landed.
pub fn add_tagged(
    cfg: &Config,
    today: &str,
    text: &str,
    link: Option<String>,
    q: Option<String>,
    on: Option<&str>,
) -> Result<AddOutcome, CmdError> {
    add_full(cfg, today, text, link, q, on, None)
}

/// Shared implementation for [`add`], [`add_est`], and [`add_tagged`]: lands a goal on today
/// (or schedules it when `on = Some(date)`), stamping an optional `est` estimate (#245) and/or
/// `q` quarter-goal key (#252). All three optional inputs default to `None` on the plain path.
#[allow(clippy::too_many_arguments)]
pub fn add_full(
    cfg: &Config,
    today: &str,
    text: &str,
    link: Option<String>,
    q: Option<String>,
    on: Option<&str>,
    est: Option<String>,
) -> Result<AddOutcome, CmdError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(CmdError::EmptyText);
    }
    if let Some(date) = on {
        return schedule(cfg, today, text, link, q, date, est);
    }
    let mut daily = ensure_today(cfg, today)?;
    let id = next_id(&daily.front.goals, GOAL_PREFIX);
    daily.front.goals.push(Item {
        id: id.clone(),
        text: text.to_string(),
        created: today.to_string(),
        done: Some(false),
        done_at: None,
        link: link.clone(),
        pct: None,
        est,
        actual: None,
        q,
        notes: Vec::new(),
    });
    write(&daily, &cfg.daily_file(today))?;
    Ok(AddOutcome {
        id,
        text: text.to_string(),
        link,
        scheduled: None,
        landed_today: true,
    })
}

/// Which day-scoped log a [`log_entry`] call appends to.
#[derive(Debug, Clone, Copy)]
enum LogField {
    Learned,
    Reflection,
}

/// Append a non-checkable note (a learning or a reflection) to today's day file. These aren't
/// goals — no done state, no carry-forward — they're a record `bn weekly` rolls up and
/// `bn learnings` reviews. `prov` stamps the entry with the branch/issue/harness it came from
/// (all `None` on the daily/global axis).
fn log_entry(
    cfg: &Config,
    today: &str,
    text: &str,
    field: LogField,
    prov: &LogProvenance,
) -> Result<AddOutcome, CmdError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(CmdError::EmptyText);
    }
    let mut daily = ensure_today(cfg, today)?;
    let (list, prefix) = match field {
        LogField::Learned => (&mut daily.front.learnings, 'l'),
        LogField::Reflection => (&mut daily.front.reflections, 'r'),
    };
    let id = next_id_from_ids(list.iter().map(|e| e.id.as_str()), prefix);
    list.push(LogEntry {
        id: id.clone(),
        text: text.to_string(),
        created: today.to_string(),
        repo: prov.repo.clone(),
        branch: prov.branch.clone(),
        issue: prov.issue.clone(),
        harness: prov.harness.clone(),
    });
    write(&daily, &cfg.daily_file(today))?;
    Ok(AddOutcome {
        id,
        text: text.to_string(),
        link: None,
        scheduled: None,
        landed_today: true,
    })
}

/// Record something learned today (`bn learned "…"`).
pub fn learned(
    cfg: &Config,
    today: &str,
    text: &str,
    prov: &LogProvenance,
) -> Result<AddOutcome, CmdError> {
    log_entry(cfg, today, text, LogField::Learned, prov)
}

/// Record a reflection — one thing to improve (`bn reflect "…"`).
pub fn reflect(
    cfg: &Config,
    today: &str,
    text: &str,
    prov: &LogProvenance,
) -> Result<AddOutcome, CmdError> {
    log_entry(cfg, today, text, LogField::Reflection, prov)
}

/// Mark a goal done by explicit id (`g3`) or a case-insensitive substring over OPEN goals,
/// erroring on zero or multiple matches (mirrors `commands::done`).
pub fn done(
    cfg: &Config,
    today: &str,
    query: &str,
    pct: Option<u8>,
) -> Result<DoneOutcome, CmdError> {
    let mut daily = ensure_today(cfg, today)?;
    let q = query.trim();

    let idx = if is_goal_id(q) {
        daily
            .front
            .goals
            .iter()
            .position(|g| g.id == q)
            .ok_or_else(|| CmdError::NotFound(q.to_string()))?
    } else {
        let ql = q.to_lowercase();
        let matches: Vec<usize> = daily
            .front
            .goals
            .iter()
            .enumerate()
            .filter(|(_, g)| g.done == Some(false) && g.text.to_lowercase().contains(&ql))
            .map(|(i, _)| i)
            .collect();
        match matches.len() {
            0 => {
                // No *open* goal matches. If a completed goal does, say so explicitly rather
                // than a bare NotFound (mirrors the explicit-id path's AlreadyDone below).
                if let Some(i) = daily
                    .front
                    .goals
                    .iter()
                    .position(|g| g.done == Some(true) && g.text.to_lowercase().contains(&ql))
                {
                    return Err(CmdError::AlreadyDone(daily.front.goals[i].id.clone()));
                }
                return Err(CmdError::NotFound(q.to_string()));
            }
            1 => matches[0],
            _ => {
                let ids = matches
                    .iter()
                    .map(|&i| daily.front.goals[i].id.clone())
                    .collect();
                return Err(CmdError::Ambiguous(q.to_string(), ids));
            }
        }
    };

    if daily.front.goals[idx].done == Some(true) {
        return Err(CmdError::AlreadyDone(daily.front.goals[idx].id.clone()));
    }
    // A sub-100 pct advances the goal instead of closing it (mirrors `commands::done`).
    let partial = pct.filter(|&p| p < 100);
    let actual = match partial {
        Some(p) => {
            daily.front.goals[idx].pct = Some(p.max(1));
            None
        }
        None => {
            daily.front.goals[idx].done = Some(true);
            daily.front.goals[idx].done_at = Some(today.to_string());
            daily.front.goals[idx].pct = None;
            // Calibration: if the goal carried an estimate, stamp the actual summed from the
            // punch log over its open window, scoped to the goal's link. Skip cleanly (stamp
            // nothing) when the goal has no link or the log had no matching time, per #245.
            stamp_goal_actual(cfg, &mut daily.front.goals[idx], today)
        }
    };
    let id = daily.front.goals[idx].id.clone();
    let text = daily.front.goals[idx].text.clone();
    write(&daily, &cfg.daily_file(today))?;
    Ok(DoneOutcome {
        id,
        text,
        pct: partial,
        actual,
    })
}

/// Remove a goal from today's list outright, by explicit id (`g3`) or a case-insensitive
/// substring — the escape hatch for a goal that should never have been raised (a stale agent
/// ping, a duplicate). Distinct from [`done`] on purpose: a dropped goal was not accomplished,
/// so it must not land in the completed fold that standup, `bn brag`, and the metrics read.
/// Matches goals in any state (a wrongly-closed goal can be dropped too) via [`goal_index`].
pub fn drop_goal(cfg: &Config, today: &str, query: &str) -> Result<DropOutcome, CmdError> {
    let mut daily = ensure_today(cfg, today)?;
    let idx = goal_index(&daily, query)?;
    let goal = daily.front.goals.remove(idx);
    write(&daily, &cfg.daily_file(today))?;
    Ok(DropOutcome {
        id: goal.id,
        text: goal.text,
        link: goal.link,
    })
}

/// Stamp `actual:` on a just-closed goal from the punch log, returning the stamped value. A
/// no-op (returns `None`) unless the goal carries an `est`, has a `link` to join the timeline
/// on, and that link's intervals over `[created..=today]` summed to more than zero.
fn stamp_goal_actual(cfg: &Config, goal: &mut Item, today: &str) -> Option<String> {
    let _est = goal.est.as_deref().and_then(crate::focus::parse_duration)?;
    let link = goal.link.as_deref()?;
    let now = util::now_epoch_secs();
    let secs = crate::calib::goal_actual_secs(cfg, link, &goal.created, today, now);
    if secs <= 0 {
        return None;
    }
    let actual = crate::track::fmt_dur(secs);
    goal.actual = Some(actual.clone());
    Some(actual)
}

/// True for an explicit goal id `g<digits>`.
fn is_goal_id(q: &str) -> bool {
    match q.strip_prefix(GOAL_PREFIX) {
        Some(rest) => !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

/// Resolve a goal on today's list by explicit id (`g<N>`) or a case-insensitive text substring —
/// the same lookup [`done`] uses, except a substring matches goals in *any* state (a note can hang
/// on a completed goal too). An id must exist; a substring must match exactly one goal.
fn goal_index(daily: &Daily, query: &str) -> Result<usize, CmdError> {
    let q = query.trim();
    if q.is_empty() {
        return Err(CmdError::NotFound(q.to_string()));
    }
    if is_goal_id(q) {
        return daily
            .front
            .goals
            .iter()
            .position(|g| g.id == q)
            .ok_or_else(|| CmdError::NotFound(q.to_string()));
    }
    let ql = q.to_lowercase();
    let matches: Vec<usize> = daily
        .front
        .goals
        .iter()
        .enumerate()
        .filter(|(_, g)| g.text.to_lowercase().contains(&ql))
        .map(|(i, _)| i)
        .collect();
    match matches.len() {
        0 => Err(CmdError::NotFound(q.to_string())),
        1 => Ok(matches[0]),
        _ => Err(CmdError::Ambiguous(
            q.to_string(),
            matches
                .iter()
                .map(|&i| daily.front.goals[i].id.clone())
                .collect(),
        )),
    }
}

/// Connect a note to a goal (#564): attach a linked branch note (`repo` + `branch`) and/or
/// free-text / uploaded content to the goal found by `query` (id or substring) on today's list.
/// The note is stored inline on the goal `Item` (so it survives daily carry-forward) with a fresh
/// per-goal `n<N>` id. Rejects an input carrying neither a complete `repo`+`branch` link nor any
/// text (`EmptyText`); `NotFound`/`Ambiguous` when the goal query doesn't resolve to exactly one.
pub fn connect_note(
    cfg: &Config,
    today: &str,
    query: &str,
    input: NoteInput,
) -> Result<NoteOutcome, CmdError> {
    let clean = |s: Option<String>| s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let repo = clean(input.repo);
    let branch = clean(input.branch);
    let text = clean(input.text);
    let title = clean(input.title);
    // A link needs BOTH repo and branch; anything less falls back to a text-only note.
    let is_link = repo.is_some() && branch.is_some();
    if !is_link && text.is_none() {
        return Err(CmdError::EmptyText);
    }

    let mut daily = ensure_today(cfg, today)?;
    let idx = goal_index(&daily, query)?;
    let goal = &mut daily.front.goals[idx];
    let id = next_id_from_ids(goal.notes.iter().map(|n| n.id.as_str()), NOTE_PREFIX);
    let note = GoalNote {
        id,
        repo: if is_link { repo } else { None },
        branch: if is_link { branch } else { None },
        text,
        title,
        created: today.to_string(),
    };
    goal.notes.push(note.clone());
    let goal_id = goal.id.clone();
    let goal_text = goal.text.clone();
    write(&daily, &cfg.daily_file(today))?;
    Ok(NoteOutcome {
        goal_id,
        goal_text,
        note,
    })
}

/// Disconnect a note (`note_id`, a `n<N>`) from the goal found by `query` on today's list, the
/// inverse of [`connect_note`]. `NotFound` when the goal doesn't resolve or carries no such note.
pub fn disconnect_note(
    cfg: &Config,
    today: &str,
    query: &str,
    note_id: &str,
) -> Result<NoteOutcome, CmdError> {
    let note_id = note_id.trim();
    let mut daily = ensure_today(cfg, today)?;
    let idx = goal_index(&daily, query)?;
    let goal = &mut daily.front.goals[idx];
    let npos = goal
        .notes
        .iter()
        .position(|n| n.id == note_id)
        .ok_or_else(|| CmdError::NotFound(note_id.to_string()))?;
    let note = goal.notes.remove(npos);
    let goal_id = goal.id.clone();
    let goal_text = goal.text.clone();
    write(&daily, &cfg.daily_file(today))?;
    Ok(NoteOutcome {
        goal_id,
        goal_text,
        note,
    })
}

// ---- Scheduled follow-up queue -------------------------------------------------------

/// Path of the scheduled-queue file (`<daily_dir>/scheduled.md`).
fn scheduled_path(cfg: &Config) -> std::path::PathBuf {
    cfg.daily_dir().join("scheduled.md")
}

/// Read the scheduled queue (empty if the file is missing or unreadable).
fn read_queue(cfg: &Config) -> Vec<ScheduledItem> {
    let path = scheduled_path(cfg);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return vec![];
    };
    crate::store::parse_frontmatter::<QueueFront>(&text)
        .map(|q| q.scheduled)
        .unwrap_or_default()
}

/// Write the scheduled queue, sorted by date then id.
fn write_queue(cfg: &Config, items: &[ScheduledItem]) -> Result<(), StoreError> {
    let mut items = items.to_vec();
    items.sort_by(|a, b| a.scheduled.cmp(&b.scheduled).then(a.id.cmp(&b.id)));
    let front = QueueFront { scheduled: items };
    let fm = serde_yaml::to_string(&front).map_err(|e| StoreError::Parse(e.to_string()))?;
    let text = format!("---\n{fm}---\n\n# Scheduled follow-ups\n");
    let path = scheduled_path(cfg);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, text)?;
    Ok(())
}

/// Next `s<N>` id given the queue's existing items.
fn next_sched_id(items: &[ScheduledItem]) -> String {
    let max = items
        .iter()
        .filter_map(|i| i.id.strip_prefix(SCHED_PREFIX))
        .filter_map(|n| n.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    format!("{SCHED_PREFIX}{}", max + 1)
}

/// Move scheduled items now due (`scheduled <= today`) out of the queue and into `daily`'s
/// goals (renumbered `g..`, original `created` preserved). Rewrites the shrunken queue.
/// Returns true if anything was drained (so the caller persists the day).
fn drain_due_into(cfg: &Config, daily: &mut Daily, today: &str) -> Result<bool, StoreError> {
    let queue = read_queue(cfg);
    if queue.is_empty() {
        return Ok(false);
    }
    let (due, pending): (Vec<_>, Vec<_>) = queue
        .into_iter()
        .partition(|s| s.scheduled.as_str() <= today);
    if due.is_empty() {
        return Ok(false);
    }
    for s in due {
        let id = next_id(&daily.front.goals, GOAL_PREFIX);
        daily.front.goals.push(Item {
            id,
            text: s.text,
            created: s.created,
            done: Some(false),
            done_at: None,
            link: s.link,
            pct: None,
            est: s.est,
            actual: None,
            q: s.q,
            notes: Vec::new(),
        });
    }
    write_queue(cfg, &pending)?;
    Ok(true)
}

/// Validate a `YYYY-MM-DD` date argument.
fn valid_date(date: &str) -> Result<(), CmdError> {
    match util::epoch_day(date) {
        Some(_) => Ok(()),
        None => Err(CmdError::InvalidDate(date.to_string())),
    }
}

/// Queue a follow-up for a future day (`on`), the `Some`-date arm of [`add`]. If `on <= today`
/// it's drained into today immediately (via `ensure_today`). `created` is stamped today so
/// pending-days counts from when you raised it. Text already trimmed/non-empty by `add`.
fn schedule(
    cfg: &Config,
    today: &str,
    text: &str,
    link: Option<String>,
    q: Option<String>,
    on: &str,
    est: Option<String>,
) -> Result<AddOutcome, CmdError> {
    valid_date(on)?;
    // Already due → it belongs on today's list now. Add it directly so the returned id is the
    // real `g<N>` the user can `done`, not an `s<N>` queue id that the drain immediately
    // consumes (leaving the caller holding an id that resolves to nothing).
    if on <= today {
        let mut daily = ensure_today(cfg, today)?;
        let id = next_id(&daily.front.goals, GOAL_PREFIX);
        daily.front.goals.push(Item {
            id: id.clone(),
            text: text.to_string(),
            created: today.to_string(),
            done: Some(false),
            done_at: None,
            link: link.clone(),
            pct: None,
            est,
            actual: None,
            q,
            notes: Vec::new(),
        });
        write(&daily, &cfg.daily_file(today))?;
        return Ok(AddOutcome {
            id,
            text: text.to_string(),
            link,
            scheduled: Some(on.to_string()),
            landed_today: true,
        });
    }
    let mut queue = read_queue(cfg);
    let id = next_sched_id(&queue);
    queue.push(ScheduledItem {
        id: id.clone(),
        text: text.to_string(),
        link: link.clone(),
        est,
        q,
        scheduled: on.to_string(),
        created: today.to_string(),
    });
    write_queue(cfg, &queue)?;
    Ok(AddOutcome {
        id,
        text: text.to_string(),
        link,
        scheduled: Some(on.to_string()),
        landed_today: false,
    })
}

/// Move an OPEN goal off today onto a future day (`on`). Matches by explicit id (`g3`) or a
/// case-insensitive substring over open goals; preserves text, link, and original `created`.
pub fn defer(cfg: &Config, today: &str, query: &str, on: &str) -> Result<DeferOutcome, CmdError> {
    valid_date(on)?;
    let mut daily = ensure_today(cfg, today)?;
    let q = query.trim();

    let idx = if is_goal_id(q) {
        daily
            .front
            .goals
            .iter()
            .position(|g| g.id == q && g.done == Some(false))
            .ok_or_else(|| CmdError::NotFound(q.to_string()))?
    } else {
        let ql = q.to_lowercase();
        let matches: Vec<usize> = daily
            .front
            .goals
            .iter()
            .enumerate()
            .filter(|(_, g)| g.done == Some(false) && g.text.to_lowercase().contains(&ql))
            .map(|(i, _)| i)
            .collect();
        match matches.len() {
            0 => return Err(CmdError::NotFound(q.to_string())),
            1 => matches[0],
            _ => {
                let ids = matches
                    .iter()
                    .map(|&i| daily.front.goals[i].id.clone())
                    .collect();
                return Err(CmdError::Ambiguous(q.to_string(), ids));
            }
        }
    };

    // Read the target goal WITHOUT mutating the source yet. The source day is only rewritten
    // once the destination write has succeeded, so a failing destination write can never lose
    // the goal from today (the data-loss bug this ordering guards against).
    let goal = daily.front.goals[idx].clone();

    // Deferring to today/past just re-lands the goal on today under a fresh `g<N>`; a future
    // date queues it as `s<N>`. Either way the returned id names where the goal now lives — the
    // original `g<N>` was just removed and would resolve to nothing.
    let id = if on <= today {
        // Same file: do the removal AND the re-add in one in-memory day, then write ONCE. A
        // single write is all-or-nothing, so if it fails today's file still holds the original
        // goal — nothing is lost.
        daily.front.goals.remove(idx);
        let id = next_id(&daily.front.goals, GOAL_PREFIX);
        daily.front.goals.push(Item {
            id: id.clone(),
            text: goal.text.clone(),
            created: goal.created.clone(),
            done: Some(false),
            done_at: None,
            link: goal.link.clone(),
            pct: None,
            est: goal.est.clone(),
            actual: goal.actual.clone(),
            q: goal.q.clone(),
            notes: goal.notes.clone(),
        });
        write(&daily, &cfg.daily_file(today))?;
        id
    } else {
        // Different files: commit the destination (queue) FIRST. Only once it is safely on disk
        // do we remove the goal from today — so a failing queue write leaves today untouched and
        // the goal survives.
        let mut queue = read_queue(cfg);
        let sched_id = next_sched_id(&queue);
        queue.push(ScheduledItem {
            id: sched_id.clone(),
            text: goal.text.clone(),
            link: goal.link.clone(),
            est: goal.est.clone(),
            q: goal.q.clone(),
            scheduled: on.to_string(),
            created: goal.created.clone(),
        });
        write_queue(cfg, &queue)?;
        daily.front.goals.remove(idx);
        write(&daily, &cfg.daily_file(today))?;
        sched_id
    };

    Ok(DeferOutcome {
        id,
        text: goal.text,
        scheduled: on.to_string(),
    })
}

// ---- DXI self-rating (#110) ----------------------------------------------------------

/// One day's optional <10s self-rating — the perceptual axis (DX Core 4 effectiveness). Stored
/// privately beside the daily files; `bn weekly` plots the trend next to the objective metrics.
/// `focus` and `friction` are 1–5; `mood` is a free one-word tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxiRating {
    pub date: String,
    /// Focus / flow time, 1 (fragmented) .. 5 (deep focus).
    pub focus: u8,
    /// CI / tooling friction, 1 (smooth) .. 5 (fighting it all day).
    pub friction: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mood: Option<String>,
}

/// The DXI store file's frontmatter: a date-keyed list of ratings (newest naturally last).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct DxiFront {
    #[serde(default)]
    ratings: Vec<DxiRating>,
}

/// Path of the DXI store (`<daily_dir>/dxi.md`) — one file beside the day files, never synced
/// into a day file (it's a private self-report, not a goal).
fn dxi_path(cfg: &Config) -> std::path::PathBuf {
    cfg.daily_dir().join("dxi.md")
}

/// Read every recorded DXI rating, oldest first (empty if the file is missing/unreadable).
pub fn read_ratings(cfg: &Config) -> Vec<DxiRating> {
    let path = dxi_path(cfg);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return vec![];
    };
    crate::store::parse_frontmatter::<DxiFront>(&text)
        .map(|d| d.ratings)
        .unwrap_or_default()
}

/// The rating recorded for `date`, if any.
pub fn rating_for(cfg: &Config, date: &str) -> Option<DxiRating> {
    read_ratings(cfg).into_iter().find(|r| r.date == date)
}

/// Clamp a self-report score to the 1–5 scale (out-of-range input is pinned, never rejected —
/// the prompt must never be a blocker).
fn clamp_score(v: u8) -> u8 {
    v.clamp(1, 5)
}

/// Record (or overwrite) the DXI self-rating for `date`. Idempotent per day: a second rating the
/// same day replaces the first. `mood` is trimmed; blank → none. Scores are clamped to 1–5.
pub fn rate(
    cfg: &Config,
    date: &str,
    focus: u8,
    friction: u8,
    mood: Option<&str>,
) -> Result<DxiRating, StoreError> {
    let rating = DxiRating {
        date: date.to_string(),
        focus: clamp_score(focus),
        friction: clamp_score(friction),
        mood: mood
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    };
    let mut ratings = read_ratings(cfg);
    if let Some(slot) = ratings.iter_mut().find(|r| r.date == date) {
        *slot = rating.clone();
    } else {
        ratings.push(rating.clone());
    }
    ratings.sort_by(|a, b| a.date.cmp(&b.date));
    write_ratings(cfg, &ratings)?;
    Ok(rating)
}

/// Write the DXI store (date-sorted), creating the `daily/` dir as needed.
fn write_ratings(cfg: &Config, ratings: &[DxiRating]) -> Result<(), StoreError> {
    let front = DxiFront {
        ratings: ratings.to_vec(),
    };
    let fm = serde_yaml::to_string(&front).map_err(|e| StoreError::Parse(e.to_string()))?;
    let text = format!("---\n{fm}---\n\n# DXI self-ratings\n");
    let path = dxi_path(cfg);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, text)?;
    Ok(())
}

/// Pending (not-yet-due) follow-ups, sorted by date — the "upcoming" list.
pub fn scheduled_pending(cfg: &Config, today: &str) -> Vec<ScheduledItem> {
    let mut items: Vec<ScheduledItem> = read_queue(cfg)
        .into_iter()
        .filter(|s| s.scheduled.as_str() > today)
        .collect();
    items.sort_by(|a, b| a.scheduled.cmp(&b.scheduled).then(a.id.cmp(&b.id)));
    items
}

/// Human render for `bn daily`: a counts header, then open goals as `- [ ] [g1] text`
/// (with `→ <link>` when present). Completed goals are collapsed to the header's `done` count and
/// intentionally NOT enumerated — they're kept as history in the day file (read a past-day view for
/// the full log), but listing every `- [x]` line only bloats the token budget of the agents that
/// ground on `bn daily`. `upcoming` is the not-yet-due follow-up queue (only today's view passes a
/// non-empty slice); when present it's appended as an "Upcoming follow-ups" tail.
pub fn render(daily: &Daily, upcoming: &[ScheduledItem]) -> String {
    let open: Vec<&Item> = daily.open().collect();
    let done_count = daily.completed().count();
    let mut out = format!(
        "Daily — {} · {} open, {} done\n",
        daily.front.date,
        open.len(),
        done_count
    );
    if open.is_empty() && done_count == 0 {
        out.push_str("  (no goals yet — `bn daily add <text>`)\n");
    }
    for g in open {
        out.push_str(&format!("- [ ] [{}] {}", g.id, g.text));
        if let Some(p) = g.pct {
            out.push_str(&format!(" ({p}%)"));
        }
        if let Some(est) = &g.est {
            out.push_str(&format!("  (est {est})"));
        }
        if let Some(link) = &g.link {
            out.push_str(&format!("  → {link}"));
        }
        out.push('\n');
    }
    if !daily.front.learnings.is_empty() {
        out.push_str("\nLearned\n");
        for l in &daily.front.learnings {
            out.push_str(&format!("  • {}\n", l.text));
        }
    }
    if !daily.front.reflections.is_empty() {
        out.push_str("\nReflections\n");
        for r in &daily.front.reflections {
            out.push_str(&format!("  ↳ {}\n", r.text));
        }
    }
    if !upcoming.is_empty() {
        out.push_str("\nUpcoming follow-ups\n");
        for s in upcoming {
            out.push_str(&format!("  → {} [{}]", s.text, s.scheduled));
            if let Some(link) = &s.link {
                out.push_str(&format!("  → {link}"));
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cfg(root: &Path) -> Config {
        Config {
            personal_root: root.to_path_buf(),
            work_root: root.to_path_buf(),
            worktree_dir: PathBuf::from("/nonexistent"),
            hostname: "testhost".into(),
            ..Default::default()
        }
    }

    #[test]
    fn add_assigns_ids_and_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let a = add(&c, "2026-06-22", "first", None, None).unwrap();
        assert_eq!(a.id, "g1");
        let b = add(&c, "2026-06-22", "second", None, None).unwrap();
        assert_eq!(b.id, "g2");
        let d = read(&c.daily_file("2026-06-22")).unwrap();
        assert_eq!(d.front.goals.len(), 2);
        assert_eq!(d.front.goals[0].done, Some(false));
    }

    #[test]
    fn learned_and_reflect_log_to_their_own_lists() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let l = learned(
            &c,
            "2026-06-22",
            "MCP tools auto-register via #[tool]",
            &LogProvenance::default(),
        )
        .unwrap();
        assert_eq!(l.id, "l1");
        let r = reflect(
            &c,
            "2026-06-22",
            "scope env-isolation before smoke tests",
            &LogProvenance::default(),
        )
        .unwrap();
        assert_eq!(r.id, "r1");
        let d = read(&c.daily_file("2026-06-22")).unwrap();
        assert_eq!(d.front.learnings.len(), 1);
        assert_eq!(d.front.reflections.len(), 1);
        // Log entries are not goals — they never appear as open/closed work.
        assert_eq!(d.open().count(), 0);
        assert_eq!(d.completed().count(), 0);
        // A plain-shell entry carries no provenance (daily/global axis).
        assert_eq!(d.front.learnings[0].harness, None);
        assert_eq!(d.front.learnings[0].repo, None);
    }

    #[test]
    fn log_entry_stamps_provenance_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let prov = LogProvenance {
            repo: Some("bn".into()),
            branch: Some("feat-x".into()),
            issue: Some("https://github.com/eduuh/bn/issues/322".into()),
            harness: Some("copilot".into()),
        };
        learned(&c, "2026-06-22", "sidecars survive wt cleanup", &prov).unwrap();
        let d = read(&c.daily_file("2026-06-22")).unwrap();
        let e = &d.front.learnings[0];
        assert_eq!(e.repo.as_deref(), Some("bn"));
        assert_eq!(e.branch.as_deref(), Some("feat-x"));
        assert_eq!(e.harness.as_deref(), Some("copilot"));
        assert_eq!(
            e.issue.as_deref(),
            Some("https://github.com/eduuh/bn/issues/322")
        );
    }

    #[test]
    fn legacy_learning_yaml_without_provenance_reads() {
        // A day file written by an older bn (learnings as bare {id,text,created}) must still parse.
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let path = c.daily_file("2026-06-20");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "---\ndate: '2026-06-20'\ncreated: '2026-06-20'\ngoals: []\nlearnings:\n- id: l1\n  text: old note\n  created: '2026-06-20'\n---\n\n# Daily — 2026-06-20\n",
        )
        .unwrap();
        let d = read(&path).unwrap();
        assert_eq!(d.front.learnings.len(), 1);
        assert_eq!(d.front.learnings[0].text, "old note");
        assert_eq!(d.front.learnings[0].harness, None);
    }

    #[test]
    fn resolve_link_prefers_url_then_task() {
        // explicit URL wins over a task id
        assert_eq!(
            resolve_link(Some("12345"), Some("https://x/y")).as_deref(),
            Some("https://x/y")
        );
        // task id stored verbatim
        assert_eq!(resolve_link(Some("99"), None).as_deref(), Some("99"));
        // neither → none
        assert_eq!(resolve_link(None, None), None);
    }

    #[test]
    fn add_with_task_link_persists() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let link = resolve_link(Some("12345"), None);
        add(&c, "2026-06-22", "Review work item", link, None).unwrap();
        let d = read(&c.daily_file("2026-06-22")).unwrap();
        assert_eq!(d.front.goals[0].link.as_deref(), Some("12345"));
    }

    #[test]
    fn tagged_goal_persists_q_and_carries_forward() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        // Tag a goal to a quarter goal; it round-trips through the parser.
        add_tagged(
            &c,
            "2026-06-22",
            "wrote release notes",
            None,
            Some("ship-v1".into()),
            None,
        )
        .unwrap();
        let d = read(&c.daily_file("2026-06-22")).unwrap();
        assert_eq!(d.front.goals[0].q.as_deref(), Some("ship-v1"));
        assert!(std::fs::read_to_string(c.daily_file("2026-06-22"))
            .unwrap()
            .contains("q: ship-v1"));
        // Carry-forward to a new day preserves the tag.
        let next = ensure_today(&c, "2026-06-23").unwrap();
        assert_eq!(next.front.goals[0].q.as_deref(), Some("ship-v1"));
    }

    #[test]
    fn done_by_id_and_substring() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "wire the thing", None, None).unwrap();
        add(&c, "2026-06-22", "ship the thing", None, None).unwrap();
        let d = done(&c, "2026-06-22", "wire", None).unwrap();
        assert_eq!(d.id, "g1");
        // already done
        assert_eq!(
            done(&c, "2026-06-22", "g1", None).unwrap_err().code(),
            "already_done"
        );
        // substring ambiguous across the remaining open ones? only g2 open now
        let d2 = done(&c, "2026-06-22", "g2", None).unwrap();
        assert_eq!(d2.id, "g2");
    }

    #[test]
    fn done_substring_on_completed_goal_reports_already_done() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "wire the thing", None, None).unwrap();
        done(&c, "2026-06-22", "wire", None).unwrap();
        // Re-doing by substring (no open match left) must say already_done, not not_found —
        // matching the explicit-id path.
        assert_eq!(
            done(&c, "2026-06-22", "wire", None).unwrap_err().code(),
            "already_done"
        );
    }

    #[test]
    fn done_pct_advances_goal_without_closing() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "finish the S360 tasks", None, None).unwrap();

        let d = done(&c, "2026-06-22", "g1", Some(40)).unwrap();
        assert_eq!(d.pct, Some(40));
        let day = read(&c.daily_file("2026-06-22")).unwrap();
        assert_eq!(day.front.goals[0].done, Some(false), "still open");
        assert_eq!(day.front.goals[0].pct, Some(40));
        // The rendered view surfaces the percentage.
        assert!(render(&day, &[]).contains("(40%)"));

        // Completing clears the pct.
        done(&c, "2026-06-22", "g1", None).unwrap();
        let day = read(&c.daily_file("2026-06-22")).unwrap();
        assert_eq!(day.front.goals[0].done, Some(true));
        assert_eq!(day.front.goals[0].pct, None);
    }

    #[test]
    fn done_ambiguous_and_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "fix the bug", None, None).unwrap();
        add(&c, "2026-06-22", "fix the test", None, None).unwrap();
        assert_eq!(
            done(&c, "2026-06-22", "fix", None).unwrap_err().code(),
            "ambiguous"
        );
        assert_eq!(
            done(&c, "2026-06-22", "nope", None).unwrap_err().code(),
            "not_found"
        );
    }

    #[test]
    fn connect_note_links_a_branch_note_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "ship the feature", None, None).unwrap();
        let out = connect_note(
            &c,
            "2026-06-22",
            "g1",
            NoteInput {
                repo: Some("bn".into()),
                branch: Some("dispatch-goals-connect-notes".into()),
                text: None,
                title: None,
            },
        )
        .unwrap();
        assert_eq!(out.goal_id, "g1");
        assert_eq!(out.note.id, "n1");
        assert!(out.note.is_link());
        // Persisted inline on the goal item.
        let day = read(&c.daily_file("2026-06-22")).unwrap();
        assert_eq!(day.front.goals[0].notes.len(), 1);
        assert_eq!(day.front.goals[0].notes[0].repo.as_deref(), Some("bn"));
        assert_eq!(
            day.front.goals[0].notes[0].branch.as_deref(),
            Some("dispatch-goals-connect-notes")
        );
    }

    #[test]
    fn connect_note_attaches_free_text_and_allocates_ids() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "ship the feature", None, None).unwrap();
        let n1 = connect_note(
            &c,
            "2026-06-22",
            "ship",
            NoteInput {
                text: Some("some supporting context".into()),
                title: Some("context.md".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(n1.note.id, "n1");
        assert!(n1.note.has_text());
        assert!(!n1.note.is_link());
        let n2 = connect_note(
            &c,
            "2026-06-22",
            "g1",
            NoteInput {
                text: Some("more context".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(n2.note.id, "n2", "ids allocated per-goal, monotonic");
        let day = read(&c.daily_file("2026-06-22")).unwrap();
        assert_eq!(day.front.goals[0].notes.len(), 2);
    }

    #[test]
    fn connect_note_rejects_an_empty_input() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "ship the feature", None, None).unwrap();
        // Neither a complete link (needs both repo+branch) nor any text.
        let err = connect_note(
            &c,
            "2026-06-22",
            "g1",
            NoteInput {
                repo: Some("bn".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(err.code(), "empty_text");
    }

    #[test]
    fn connect_note_unknown_goal_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "ship the feature", None, None).unwrap();
        let err = connect_note(
            &c,
            "2026-06-22",
            "g9",
            NoteInput {
                text: Some("x".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[test]
    fn disconnect_note_removes_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "ship the feature", None, None).unwrap();
        connect_note(
            &c,
            "2026-06-22",
            "g1",
            NoteInput {
                text: Some("a".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let out = disconnect_note(&c, "2026-06-22", "g1", "n1").unwrap();
        assert_eq!(out.note.id, "n1");
        let day = read(&c.daily_file("2026-06-22")).unwrap();
        assert!(day.front.goals[0].notes.is_empty());
        // Removing a missing note id is not_found.
        assert_eq!(
            disconnect_note(&c, "2026-06-22", "g1", "n1")
                .unwrap_err()
                .code(),
            "not_found"
        );
    }

    #[test]
    fn connected_notes_carry_forward_to_the_next_day() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "long-running goal", None, None).unwrap();
        connect_note(
            &c,
            "2026-06-22",
            "g1",
            NoteInput {
                repo: Some("bn".into()),
                branch: Some("feat-x".into()),
                text: None,
                title: None,
            },
        )
        .unwrap();
        // Open the next day: the still-open goal carries forward with its connected note.
        let next = ensure_today(&c, "2026-06-23").unwrap();
        assert_eq!(next.front.goals.len(), 1);
        assert_eq!(next.front.goals[0].notes.len(), 1);
        assert_eq!(next.front.goals[0].notes[0].repo.as_deref(), Some("bn"));
    }

    #[test]
    fn carry_over_seeds_open_goals_only_renumbered_with_links() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        // yesterday: g1 open (with link), g2 done
        add(
            &c,
            "2026-06-21",
            "carry me",
            Some("https://x/1".into()),
            None,
        )
        .unwrap();
        add(&c, "2026-06-21", "leave me done", None, None).unwrap();
        done(&c, "2026-06-21", "g2", None).unwrap();

        // first open of the new day carries only the open goal, renumbered g1
        let today = ensure_today(&c, "2026-06-22").unwrap();
        assert_eq!(today.front.goals.len(), 1);
        assert_eq!(today.front.goals[0].id, "g1");
        assert_eq!(today.front.goals[0].text, "carry me");
        assert_eq!(today.front.goals[0].link.as_deref(), Some("https://x/1"));
        // created is PRESERVED (original day), so pending-days counts from when it was raised
        assert_eq!(today.front.goals[0].created, "2026-06-21");
        assert_eq!(today.front.goals[0].done, Some(false));
    }

    #[test]
    fn ensure_today_is_idempotent_no_reseed() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-21", "carry me", None, None).unwrap();
        // create today (seeds g1), then mark it done, then re-open: must NOT re-seed
        ensure_today(&c, "2026-06-22").unwrap();
        done(&c, "2026-06-22", "g1", None).unwrap();
        let again = ensure_today(&c, "2026-06-22").unwrap();
        assert_eq!(again.front.goals.len(), 1);
        assert_eq!(again.front.goals[0].done, Some(true));
    }

    #[test]
    fn no_prior_day_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let today = ensure_today(&c, "2026-06-22").unwrap();
        assert!(today.front.goals.is_empty());
    }

    #[test]
    fn round_trip_file_text() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "a goal", Some("https://x/9".into()), None).unwrap();
        let path = c.daily_file("2026-06-22");
        let back = read(&path).unwrap();
        write(&back, &path).unwrap();
        let reread = read(&path).unwrap();
        assert_eq!(back, reread);
    }

    #[test]
    fn schedule_future_does_not_appear_until_due_then_lands() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let out = add(
            &c,
            "2026-06-22",
            "follow up with Sam",
            None,
            Some("2026-06-24"),
        )
        .unwrap();
        assert!(!out.landed_today);
        // not on today's list yet
        let today = ensure_today(&c, "2026-06-22").unwrap();
        assert!(today.front.goals.is_empty());
        // shows as pending/upcoming
        assert_eq!(scheduled_pending(&c, "2026-06-22").len(), 1);
        // on the target day it drains into that day's goals
        let due_day = ensure_today(&c, "2026-06-24").unwrap();
        assert_eq!(due_day.front.goals.len(), 1);
        assert_eq!(due_day.front.goals[0].text, "follow up with Sam");
        // queue now empty
        assert!(scheduled_pending(&c, "2026-06-24").is_empty());
    }

    #[test]
    fn schedule_for_today_or_past_lands_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let out = add(&c, "2026-06-22", "do now", None, Some("2026-06-22")).unwrap();
        assert!(out.landed_today);
        // The returned id is a real goal id the user can act on — not a consumed queue id.
        assert_eq!(out.id, "g1");
        assert!(done(&c, "2026-06-22", &out.id, None).is_ok());
        let today = ensure_today(&c, "2026-06-22").unwrap();
        assert_eq!(today.front.goals.len(), 1);
        assert_eq!(today.front.goals[0].text, "do now");
    }

    #[test]
    fn scheduled_drain_coexists_with_carryover() {
        // Regression: a scheduled item due on a future day must NOT pre-create that day's
        // file and thereby suppress carry-over. The due day should get BOTH the carried
        // open goal and the drained follow-up.
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "carry me", None, None).unwrap();
        add(&c, "2026-06-22", "due tomorrow", None, Some("2026-06-23")).unwrap();
        let next = ensure_today(&c, "2026-06-23").unwrap();
        let texts: Vec<&str> = next.front.goals.iter().map(|g| g.text.as_str()).collect();
        assert!(
            texts.contains(&"carry me"),
            "carry-over still happened: {texts:?}"
        );
        assert!(
            texts.contains(&"due tomorrow"),
            "scheduled drained: {texts:?}"
        );
    }

    #[test]
    fn defer_moves_goal_off_today_to_future() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "ping the team", None, None).unwrap();
        add(&c, "2026-06-22", "stay here", None, None).unwrap();
        let d = defer(&c, "2026-06-22", "ping", "2026-06-25").unwrap();
        // Returns the scheduled-queue id it now lives under, not the removed goal id "g1".
        assert_eq!(d.id, "s1");
        // gone from today, the other remains
        let today = ensure_today(&c, "2026-06-22").unwrap();
        let texts: Vec<&str> = today.front.goals.iter().map(|g| g.text.as_str()).collect();
        assert_eq!(texts, vec!["stay here"]);
        // resurfaces on the deferred day
        let later = ensure_today(&c, "2026-06-25").unwrap();
        assert!(later.front.goals.iter().any(|g| g.text == "ping the team"));
    }

    #[test]
    fn defer_preserves_goal_when_destination_write_fails() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "must survive", None, None).unwrap();

        // Force the destination (scheduled queue) write to fail by making `scheduled.md` a
        // directory: `std::fs::write` to a directory path errors on every platform.
        let queue_path = c.daily_dir().join("scheduled.md");
        std::fs::create_dir_all(&queue_path).unwrap();

        // Deferring to a future date must fail at the queue write...
        assert!(
            defer(&c, "2026-06-22", "must survive", "2026-06-25").is_err(),
            "expected the queue write to a directory path to fail"
        );

        // ...but the goal must NOT be lost from today. Before the fix, defer removed the goal
        // and wrote today BEFORE the (failing) queue write, dropping it entirely.
        let today = read(&c.daily_file("2026-06-22")).unwrap();
        let texts: Vec<&str> = today.front.goals.iter().map(|g| g.text.as_str()).collect();
        assert_eq!(texts, vec!["must survive"]);
    }

    #[test]
    fn rollover_reports_created_carried_and_due() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "unfinished", None, None).unwrap();
        add(&c, "2026-06-22", "due next day", None, Some("2026-06-23")).unwrap();
        let r = rollover(&c, "2026-06-23").unwrap();
        assert!(r.created_today);
        assert_eq!(r.carried.len(), 1);
        assert_eq!(r.carried[0].text, "unfinished");
        assert_eq!(r.due.len(), 1);
        assert_eq!(r.due[0].text, "due next day");
        // second rollover same day carries nothing
        let r2 = rollover(&c, "2026-06-23").unwrap();
        assert!(!r2.created_today);
        assert!(r2.carried.is_empty());
        assert!(r2.due.is_empty());
    }

    /// Record a `pr_merged`/`pr_closed` metrics event, the way the `pr-observe` job does.
    fn log_pr_terminal(c: &Config, kind: &str, repo: &str, number: i64) {
        let mut fields = serde_json::Map::new();
        fields.insert("repo".into(), serde_json::json!(repo));
        fields.insert("number".into(), serde_json::json!(number));
        crate::events::append_metric_at(&c.metrics_log_path(), kind, 1_000, fields).unwrap();
    }

    fn pr_url(repo: &str, number: i64) -> String {
        format!("https://github.com/{repo}/pull/{number}")
    }

    #[test]
    fn prune_retires_goals_whose_pr_is_merged_or_closed() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let day = "2026-06-23";
        add(
            &c,
            day,
            "ping reviewers on wira360#923",
            Some(pr_url("eduuh/wira360", 923)),
            None,
        )
        .unwrap();
        add(
            &c,
            day,
            "review bn#245",
            Some(pr_url("eduuh/bn", 245)),
            None,
        )
        .unwrap();
        add(&c, day, "land bn#300", Some(pr_url("eduuh/bn", 300)), None).unwrap();
        add(&c, day, "write the ADR", None, None).unwrap();
        // A follow-up queued against the same dead PR must never land either.
        add(
            &c,
            day,
            "ping again",
            Some(pr_url("eduuh/wira360", 923)),
            Some("2026-06-30"),
        )
        .unwrap();
        // A completed goal on the dead PR is history — it stays.
        done(&c, day, "land bn#300", None).unwrap();
        log_pr_terminal(&c, "pr_closed", "eduuh/wira360", 923);
        log_pr_terminal(&c, "pr_merged", "eduuh/bn", 300);

        let out = prune_dead_pr_goals(&c, day, &dead_prs(&c)).unwrap();
        assert_eq!(out.goals.len(), 1);
        assert_eq!(out.goals[0].text, "ping reviewers on wira360#923");
        assert_eq!(out.scheduled.len(), 1);
        let left: Vec<String> = read(&c.daily_file(day))
            .unwrap()
            .front
            .goals
            .into_iter()
            .map(|g| g.text)
            .collect();
        // The still-open PR, the unlinked goal, and the already-completed one survive.
        assert_eq!(left, ["review bn#245", "land bn#300", "write the ADR"]);
        // Idempotent: a second sweep finds nothing left to retire.
        assert!(prune_dead_pr_goals(&c, day, &dead_prs(&c))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn rollover_retires_carried_goals_for_dead_prs() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(
            &c,
            "2026-06-22",
            "ping reviewers on wira360#923",
            Some(pr_url("eduuh/wira360", 923)),
            None,
        )
        .unwrap();
        add(&c, "2026-06-22", "still open work", None, None).unwrap();
        log_pr_terminal(&c, "pr_closed", "eduuh/wira360", 923);

        let r = rollover(&c, "2026-06-23").unwrap();
        // It carried (the day file is built before the sweep), then the sweep retired it.
        assert_eq!(r.carried.len(), 2);
        assert_eq!(r.retired.goals.len(), 1);
        assert_eq!(r.retired.len(), 1);
        let texts: Vec<&str> = r
            .today
            .front
            .goals
            .iter()
            .map(|g| g.text.as_str())
            .collect();
        assert_eq!(texts, ["still open work"]);
    }

    #[test]
    fn drop_removes_a_goal_without_completing_it() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let day = "2026-06-23";
        add(&c, day, "stale agent ping", None, None).unwrap();
        add(&c, day, "real work", None, None).unwrap();

        let d = drop_goal(&c, day, "stale").unwrap();
        assert_eq!(d.id, "g1");
        let daily = read(&c.daily_file(day)).unwrap();
        assert_eq!(daily.front.goals.len(), 1);
        assert_eq!(daily.front.goals[0].text, "real work");
        // Nothing was recorded as completed — a drop is not an accomplishment.
        assert_eq!(daily.completed().count(), 0);
        // Gone means gone.
        assert!(matches!(
            drop_goal(&c, day, "stale"),
            Err(CmdError::NotFound(_))
        ));
    }

    #[test]
    fn drop_refuses_an_ambiguous_substring() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let day = "2026-06-23";
        add(&c, day, "ping reviewers on #1", None, None).unwrap();
        add(&c, day, "ping reviewers on #2", None, None).unwrap();
        assert!(matches!(
            drop_goal(&c, day, "ping reviewers"),
            Err(CmdError::Ambiguous(_, _))
        ));
        assert_eq!(read(&c.daily_file(day)).unwrap().front.goals.len(), 2);
    }

    #[test]
    fn rollover_drains_unambiguous_inbox_captures() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let ctx = crate::resolver::Resolved {
            repo: "bn".into(),
            branch: "feat-x".into(),
        };
        // An active branch note makes its hinted capture unambiguous; the hintless one isn't.
        crate::commands::add(&c, &ctx, "todo", "seed", None, "2026-06-22").unwrap();
        crate::inbox::capture(&c, "routable", Some(&ctx), "2026-06-22", None).unwrap();
        crate::inbox::capture(&c, "needs a human", None, "2026-06-22", None).unwrap();

        let r = rollover(&c, "2026-06-23").unwrap();
        assert_eq!(r.inbox_routed.len(), 1);
        assert_eq!(r.inbox_routed[0].text, "routable");
        assert_eq!(r.inbox_left, 1);
        let note = crate::store::read(&c.note_file(&ctx)).unwrap();
        assert!(note.front.todos.iter().any(|t| t.text == "routable"));
        let left: Vec<_> = crate::inbox::read(&c).into_iter().map(|i| i.text).collect();
        assert_eq!(left, ["needs a human"]);
    }

    #[test]
    fn render_shows_upcoming_tail_for_today_only() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "today goal", None, None).unwrap();
        add(&c, "2026-06-22", "later", None, Some("2026-06-25")).unwrap();
        let today = ensure_today(&c, "2026-06-22").unwrap();
        let upcoming = scheduled_pending(&c, "2026-06-22");
        // today's view renders the upcoming tail
        let with = render(&today, &upcoming);
        assert!(with.contains("Upcoming follow-ups"), "{with}");
        assert!(with.contains("later [2026-06-25]"), "{with}");
        // a historical view (empty slice) omits it
        let without = render(&today, &[]);
        assert!(!without.contains("Upcoming follow-ups"), "{without}");
    }

    #[test]
    fn render_collapses_done_goals_to_the_header_count() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-22", "ship the thing", None, None).unwrap();
        add(&c, "2026-06-22", "finished chore", None, None).unwrap();
        done(&c, "2026-06-22", "finished chore", None).unwrap();
        let day = read(&c.daily_file("2026-06-22")).unwrap();
        let out = render(&day, &[]);
        // The done goal counts in the header but is not enumerated (token budget for agents).
        assert!(out.contains("1 open, 1 done"), "{out}");
        assert!(out.contains("ship the thing"), "{out}");
        assert!(!out.contains("finished chore"), "{out}");
        assert!(!out.contains("[x]"), "{out}");
    }

    #[test]
    fn dxi_rate_records_clamps_and_overwrites_per_day() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        // out-of-range scores are clamped, not rejected (never a blocker)
        let r = rate(&c, "2026-06-25", 9, 0, Some("focused")).unwrap();
        assert_eq!(r.focus, 5);
        assert_eq!(r.friction, 1);
        assert_eq!(r.mood.as_deref(), Some("focused"));
        assert_eq!(rating_for(&c, "2026-06-25").unwrap().focus, 5);
        // same-day re-rate overwrites (idempotent per day), blank mood → none
        rate(&c, "2026-06-25", 3, 4, Some("   ")).unwrap();
        let ratings = read_ratings(&c);
        assert_eq!(ratings.len(), 1);
        assert_eq!(ratings[0].focus, 3);
        assert_eq!(ratings[0].friction, 4);
        assert!(ratings[0].mood.is_none());
    }

    #[test]
    fn dxi_ratings_are_date_sorted_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        rate(&c, "2026-06-25", 4, 2, None).unwrap();
        rate(&c, "2026-06-23", 2, 5, Some("tired")).unwrap();
        let ratings = read_ratings(&c);
        assert_eq!(ratings.len(), 2);
        assert_eq!(ratings[0].date, "2026-06-23"); // oldest first
        assert_eq!(ratings[1].date, "2026-06-25");
        assert_eq!(ratings[0].mood.as_deref(), Some("tired"));
    }

    #[test]
    fn dxi_missing_store_reads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        assert!(read_ratings(&c).is_empty());
        assert!(rating_for(&c, "2026-06-25").is_none());
    }

    #[test]
    fn read_window_collects_existing_days() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add(&c, "2026-06-20", "a", None, None).unwrap();
        add(&c, "2026-06-22", "b", None, None).unwrap(); // 21 has no file
        let days = read_window(&c, "2026-06-20", "2026-06-22");
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].front.date, "2026-06-20");
        assert_eq!(days[1].front.date, "2026-06-22");
    }

    #[test]
    fn est_persists_on_goal_and_survives_carry_forward() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        add_est(
            &c,
            "2026-06-20",
            "ship board",
            None,
            None,
            Some("90m".into()),
        )
        .unwrap();
        // Grammar round-trip: the estimate lands on the goal line.
        let d = ensure_today(&c, "2026-06-20").unwrap();
        assert_eq!(d.front.goals[0].est.as_deref(), Some("90m"));
        // Left open, it carries into the next opened day with the estimate preserved.
        let next = ensure_today(&c, "2026-06-21").unwrap();
        assert_eq!(next.front.goals[0].est.as_deref(), Some("90m"));
        assert!(next.front.goals[0].actual.is_none());
    }

    #[test]
    fn goal_close_stamps_actual_from_linked_punches() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let day = "2026-06-20";
        // 30m tracked against the goal's link; a different link must not count.
        let t0 = crate::track::parse_at(day, "09:00").unwrap();
        crate::track::punch_in(&c, day, t0, "spec", None, None, None, Some("ADO-5")).unwrap();
        crate::track::punch_in(&c, day, t0 + 1800, "other", None, None, None, Some("ADO-9"))
            .unwrap();
        crate::track::punch_out(&c, day, t0 + 1800 + 600).unwrap();

        add_est(
            &c,
            day,
            "write spec",
            Some("ADO-5".into()),
            None,
            Some("1h".into()),
        )
        .unwrap();
        let out = done(&c, day, "write spec", None).unwrap();
        assert_eq!(out.actual.as_deref(), Some("30m"));
        let d = read(&c.daily_file(day)).unwrap();
        assert_eq!(d.front.goals[0].actual.as_deref(), Some("30m"));
    }

    #[test]
    fn goal_close_without_link_stamps_no_actual() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let day = "2026-06-20";
        let t0 = crate::track::parse_at(day, "09:00").unwrap();
        crate::track::punch_in(&c, day, t0, "spec", None, None, None, None).unwrap();
        crate::track::punch_out(&c, day, t0 + 1800).unwrap();
        // Estimated but unlinked → can't join the timeline → skip cleanly.
        add_est(&c, day, "unlinked goal", None, None, Some("1h".into())).unwrap();
        assert!(done(&c, day, "unlinked", None).unwrap().actual.is_none());
    }
}
