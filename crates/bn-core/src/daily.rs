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
    // Goals live in the task store since #1004, so a file-only window would report a quiet week
    // for a week that shipped. Each day folds in the tasks *completed* on it; open work is not
    // reconstructible for a past day (a task open now was not necessarily open then), and
    // guessing would inflate every historical count.
    let finished = completed_by_day(cfg, start, end);
    let mut out = Vec::new();
    let mut cur = start.to_string();
    while cur.as_str() <= end {
        let path = cfg.daily_file(&cur);
        let mut day = match (path.exists(), read(&path)) {
            (true, Ok(d)) => Some(d),
            _ => None,
        };
        if let Some(goals) = finished.get(&cur) {
            let d = day.get_or_insert_with(|| Daily::empty(&cur));
            let mut projected = goals.clone();
            projected.append(&mut d.front.goals);
            d.front.goals = projected;
        }
        if let Some(d) = day {
            out.push(d);
        }
        match util::add_days(&cur, 1) {
            Some(next) => cur = next,
            None => break,
        }
    }
    out
}

/// Tasks finished on each day of `[start..=end]`, as goals. One pass over the store rather than
/// one per day.
fn completed_by_day(
    cfg: &Config,
    start: &str,
    end: &str,
) -> std::collections::HashMap<String, Vec<Item>> {
    let mut out: std::collections::HashMap<String, Vec<Item>> = Default::default();
    let Ok(file) = crate::task_store::task_store(cfg).load() else {
        return out;
    };
    for task in file.tasks() {
        // Machine bookkeeping never lands in the weekly review's "what shipped" either — same
        // rule as the day projection, so the two windows agree about whose work this was.
        if !projection::is_mine(task) {
            continue;
        }
        let Some(done_on) = task.completion.as_deref() else {
            continue;
        };
        if done_on < start || done_on > end {
            continue;
        }
        out.entry(done_on.to_string())
            .or_default()
            .push(projection::task_as_goal(task, done_on));
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

    GoalAggregation { completed, carried }
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

/// Shared implementation for [`add`], `add_est`, and `add_tagged`: lands a goal on today
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
    // Goals written by hand keep living in the day file for now: `connect_note`, the night
    // planner's goals, `rollover` and the DXI ratings all attach `GoalNote`s to a file-backed
    // goal, and the task grammar has no equivalent. #1004 unifies the *read* side — every surface
    // now sees tasks and day-file goals as one list — and leaves that write-side collapse as
    // follow-up rather than half-migrating five subsystems.
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

// --- Goals as a projection over tasks (#1004, EPIC #993) -------------------------------------

mod projection;
pub use projection::task_goals;

/// Today, as the rest of bn should read it: goals projected from tasks, plus the day file's own
/// learnings and reflections.
///
/// Legacy goals written into the day file before #1004 are kept and listed after the projected
/// ones, so nothing anyone recorded disappears and no migration has to run. `bn daily add` writes
/// tasks from here on, so that set only shrinks.
pub fn view(cfg: &Config, today: &str) -> Daily {
    let mut daily = read(&cfg.daily_file(today)).unwrap_or_else(|_| Daily::empty(today));
    let mut goals = task_goals(cfg, today);
    // Legacy file goals keep their place *after* the projection: the store is the source of truth,
    // and these are a shrinking tail.
    goals.append(&mut daily.front.goals);
    daily.front.goals = goals;
    daily
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
    // A task-backed goal is finished with `bn task do <id>`; this path owns the day file's own
    // goals. Both show up in `view`, so `bn daily` lists them as one set either way.
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
