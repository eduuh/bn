//! Staff store retention (#275): the staff's synced stores append forever; this module gives them
//! a policy. Two operations, both idempotent and dry-run-able, exposed as one `bn staff archive`:
//!
//! * **Feed-card compaction** — briefing cards ([`crate::staff`]) live at
//!   `<feed>/<YYYY-MM-DD>/<agent>.{json,md}` and accrue one dir per day forever. Once a quarter has
//!   fully elapsed (we are in a later quarter), its cards are compacted: every card's headline is
//!   folded into a single per-quarter index (`<feed>/archive/<YYYY-Qn>.md`) and the per-day
//!   `.json` + `.md` twins are deleted. The durable narrative already lives in the Archivist's
//!   chronicle (#263), so the card *bodies* are disposable — only the headline ledger is kept.
//! * **Ledger rollup** — the `staff_run` cost ledger (#267) is a stream of per-run events in the
//!   machine-local state log ([`crate::events`]). Runs in months older than a year are rolled up
//!   into one monthly aggregate line per `(agent, model, status)`: tokens and wall-clock summed,
//!   run count kept. Recent months keep per-run fidelity.
//!
//! **Explicitly exempt** (the acceptance calls this out): the Archivist's chronicle
//! (`<notes>/<machine>/chronicle/`) and the weekly retro transcripts (`<notes>/<machine>/insights/`)
//! — they are the point, small, and prime `bn recall` (#249) corpus. This module never reads or
//! writes either tree.
//!
//! **Recall-safe** (#249): the recall index only ever indexes `note.md` files (plus their sibling
//! design docs), never feed cards or the ledger, and it rebuilds from a fresh filesystem scan on
//! every `--index` — so a deleted card simply drops out of the next index rather than lingering as
//! a stale hit. Nothing here fights the incremental index.
//!
//! **Idempotent + measured**: [`archive`] first *measures* the stores (the growth report the
//! acceptance wants — sizes, oldest entries), then plans and (unless `dry_run`) applies. Re-running
//! is a no-op: a compacted quarter has no per-day cards left to fold, and a rolled month re-rolls to
//! byte-identical aggregates. Every write is atomic; the ledger rewrite takes the same advisory lock
//! a concurrent `staff_run` append would.

use crate::events::{self, Event};
use crate::resolver::Config;
use crate::{quarter, staff, store, util};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Calendar months of per-run ledger fidelity to keep. Runs in months older than this fold to
/// monthly aggregates — the "older than a year" the acceptance asks for.
pub const LEDGER_RETAINED_MONTHS: i64 = 12;

// --- report shapes ------------------------------------------------------------------------------

/// Current size of the feed-card store — the growth signal to size the policy against before (and
/// after) compaction. `cards`/`bytes` count the whole feed tree (including any prior archive
/// indexes); `pending_quarters` is how many fully-elapsed quarters still hold un-compacted cards.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct FeedGrowth {
    /// Per-day briefing-card `.json` files on disk.
    pub cards: usize,
    /// Total bytes of the whole feed tree (cards + rendered twins + archive indexes).
    pub bytes: u64,
    /// Oldest card date (`YYYY-MM-DD`), if any.
    pub oldest: Option<String>,
    /// Newest card date (`YYYY-MM-DD`), if any.
    pub newest: Option<String>,
    /// Elapsed quarters that still have per-day cards to compact.
    pub pending_quarters: usize,
}

/// Current size of the `staff_run` ledger — the other growth signal.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct LedgerGrowth {
    /// `staff_run` events in the state log (per-run rows + any existing monthly aggregates).
    pub events: usize,
    /// Oldest `staff_run` date (`YYYY-MM-DD`), if any.
    pub oldest: Option<String>,
    /// `staff_run` events in months older than a year — the rollup-eligible set.
    pub rolluppable: usize,
}

/// One elapsed quarter's compaction result (or, under `--dry-run`, what it would do).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuarterCompaction {
    /// Quarter id (`YYYY-Qn`).
    pub quarter: String,
    /// Cards folded into the index this run.
    pub cards: usize,
    /// The per-quarter index file (`<feed>/archive/<YYYY-Qn>.md`).
    pub index: PathBuf,
}

/// The ledger-rollup result (or, under `--dry-run`, what it would do).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct LedgerRollup {
    /// Per-run rows folded into aggregates this run (0 when nothing changed / already rolled).
    pub runs_folded: usize,
    /// Monthly aggregate lines the rolled region resolves to.
    pub aggregates: usize,
    /// Months rolled, sorted (`YYYY-MM`).
    pub months: Vec<String>,
    /// Whether applying actually rewrote the log (false = already compacted, a true no-op).
    pub changed: bool,
}

/// The whole `bn staff archive` outcome: the growth measurement plus what compaction / rollup did
/// (or, under `dry_run`, would do). Serializable for `--json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RetentionReport {
    pub dry_run: bool,
    pub today: String,
    pub current_quarter: Option<String>,
    pub feed: FeedGrowth,
    pub ledger: LedgerGrowth,
    pub compactions: Vec<QuarterCompaction>,
    pub rollup: LedgerRollup,
}

impl RetentionReport {
    /// Total cards folded across all quarters this run.
    pub fn cards_compacted(&self) -> usize {
        self.compactions.iter().map(|c| c.cards).sum()
    }
}

// --- feed-card compaction -----------------------------------------------------------------------

/// `<feed>/archive` — where per-quarter compacted indexes live. Not a `YYYY-MM-DD` dir, so
/// [`crate::staff::list_cards`] / `unread_count` skip it entirely.
pub fn feed_archive_dir(cfg: &Config) -> PathBuf {
    cfg.staff_feed_dir().join("archive")
}

/// One briefing card as found on disk, reduced to what compaction needs.
struct CardOnDisk {
    date: String,
    agent: String,
    headline: String,
    json: PathBuf,
    md: PathBuf,
}

/// Walk the feed store and collect every per-day card (`<feed>/<YYYY-MM-DD>/<agent>.json`). Only
/// well-formed date dirs are considered, so the `archive/` index dir and any stray files are
/// ignored. Unparseable cards are skipped (a half-synced card just shows up next run).
fn card_files(cfg: &Config) -> Vec<CardOnDisk> {
    let mut out = Vec::new();
    let Ok(dates) = std::fs::read_dir(cfg.staff_feed_dir()) else {
        return out;
    };
    for d in dates.flatten() {
        let date = d.file_name().to_string_lossy().into_owned();
        if !util::is_iso_date(&date) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(d.path()) else {
            continue;
        };
        for f in files.flatten() {
            let json = f.path();
            if json.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Some(card) = staff::read_card(&json) else {
                continue;
            };
            out.push(CardOnDisk {
                md: json.with_extension("md"),
                date: card.date,
                agent: card.agent,
                headline: card.headline,
                json,
            });
        }
    }
    out
}

/// Total bytes of the feed tree, walked recursively. The growth signal; best-effort (unreadable
/// entries contribute 0).
fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for e in entries.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => total += dir_bytes(&p),
            Ok(t) if t.is_file() => total += std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0),
            _ => {}
        }
    }
    total
}

/// True iff quarter `a` is strictly before quarter `b` (calendar order, not string order).
fn quarter_before(a: &str, b: &str) -> bool {
    match (quarter::parse_qid(a), quarter::parse_qid(b)) {
        (Some(x), Some(y)) => x < y,
        _ => false,
    }
}

/// One compacted-index line: `- <date> · <agent> · <headline>`. Stable and self-parseable so a
/// re-run (late-arriving cards for an already-archived quarter) can dedup against what's there.
fn index_line(date: &str, agent: &str, headline: &str) -> String {
    // Headlines are single-line by card contract, but guard against a stray newline wedging a line.
    let clean = headline.replace(['\n', '\r'], " ");
    format!("- {date} · {agent} · {clean}")
}

/// The `(date, agent)` identity of an existing index line, for idempotent merge. `None` for any
/// line that isn't one of ours (preserved verbatim, never deduped — we never drop data).
fn parse_index_key(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("- ")?;
    let (date, tail) = rest.split_once(" · ")?;
    if !util::is_iso_date(date) {
        return None;
    }
    let (agent, _) = tail.split_once(" · ")?;
    Some((date.to_string(), agent.to_string()))
}

/// Render (or merge into) a quarter's archive index. Existing entries are preserved and the new
/// cards unioned in by `(date, agent)`, so a re-run with late-synced cards appends rather than
/// clobbers. Body lines are sorted by date then agent for a deterministic, diffable file.
fn render_quarter_index(quarter: &str, existing: &str, cards: &[&CardOnDisk]) -> String {
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut lines: Vec<String> = Vec::new();

    // Keep every existing body line (they're already-compacted headlines) and remember their keys.
    for line in existing.lines() {
        if let Some(key) = parse_index_key(line) {
            seen.insert(key);
            lines.push(line.to_string());
        }
    }
    for c in cards {
        if seen.insert((c.date.clone(), c.agent.clone())) {
            lines.push(index_line(&c.date, &c.agent, &c.headline));
        }
    }
    lines.sort();

    let noun = if lines.len() == 1 { "card" } else { "cards" };
    let mut out = format!(
        "# Feed archive — {quarter}\n\n_Compacted briefing-card headlines ({} {noun}). Full card bodies were retired under the #275 retention policy; the durable narrative lives in the Archivist's chronicle (#263)._\n\n",
        lines.len()
    );
    for l in &lines {
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// Compact every fully-elapsed quarter's cards. For each such quarter: fold its card headlines into
/// `<feed>/archive/<YYYY-Qn>.md` (merging with any existing index), then delete the per-day
/// `.json` + `.md` twins and prune the now-empty date dirs. Under `dry_run` nothing is written or
/// deleted — the returned plan just reports what would happen. Best-effort: a write/delete hiccup on
/// one card is logged and skipped, never aborting the run.
fn compact_feed(cfg: &Config, today: &str, dry_run: bool) -> Vec<QuarterCompaction> {
    let Some(current) = quarter::quarter_id(today) else {
        return Vec::new();
    };
    let cards = card_files(cfg);

    // Group elapsed-quarter cards by quarter id.
    let mut by_quarter: BTreeMap<String, Vec<&CardOnDisk>> = BTreeMap::new();
    for c in &cards {
        let Some(q) = quarter::quarter_id(&c.date) else {
            continue;
        };
        if quarter_before(&q, &current) {
            by_quarter.entry(q).or_default().push(c);
        }
    }

    let mut out = Vec::new();
    for (q, mut group) in by_quarter {
        group.sort_by(|a, b| a.date.cmp(&b.date).then(a.agent.cmp(&b.agent)));
        let index = feed_archive_dir(cfg).join(format!("{q}.md"));

        if !dry_run {
            let existing = std::fs::read_to_string(&index).unwrap_or_default();
            let body = render_quarter_index(&q, &existing, &group);
            if let Some(dir) = index.parent() {
                util::best_effort(
                    std::fs::create_dir_all(dir),
                    "retention: create archive dir",
                );
            }
            if store::write_atomic(&index, body.as_bytes()).is_ok() {
                delete_cards(&group);
            }
        }
        out.push(QuarterCompaction {
            quarter: q,
            cards: group.len(),
            index,
        });
    }
    out
}

/// Delete each card's twins and prune the date dir once it's empty. Best-effort throughout.
fn delete_cards(cards: &[&CardOnDisk]) {
    let mut dirs: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    for c in cards {
        util::best_effort(remove_if_exists(&c.json), "retention: remove card json");
        util::best_effort(remove_if_exists(&c.md), "retention: remove card md");
        if let Some(d) = c.json.parent() {
            dirs.insert(d.to_path_buf());
        }
    }
    // Remove now-empty date dirs (ignore "not empty" — a card we couldn't parse may still live there).
    for d in dirs {
        let _ = std::fs::remove_dir(&d);
    }
}

fn remove_if_exists(p: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn measure_feed(cfg: &Config, today: &str) -> FeedGrowth {
    let cards = card_files(cfg);
    let current = quarter::quarter_id(today);
    let mut dates: Vec<&str> = cards.iter().map(|c| c.date.as_str()).collect();
    dates.sort_unstable();
    let pending: std::collections::BTreeSet<String> = cards
        .iter()
        .filter_map(|c| quarter::quarter_id(&c.date))
        .filter(|q| current.as_deref().is_some_and(|cur| quarter_before(q, cur)))
        .collect();
    FeedGrowth {
        cards: cards.len(),
        bytes: dir_bytes(&cfg.staff_feed_dir()),
        oldest: dates.first().map(|s| s.to_string()),
        newest: dates.last().map(|s| s.to_string()),
        pending_quarters: pending.len(),
    }
}

// --- ledger rollup ------------------------------------------------------------------------------

fn event_str(e: &Event, k: &str) -> String {
    e.fields
        .get(k)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn event_i64(e: &Event, k: &str) -> i64 {
    e.fields.get(k).and_then(Value::as_i64).unwrap_or(0)
}

/// The `YYYY-MM` bucket of a `staff_run` event: its logical `date` field when present (the ledger
/// records one so month bucketing never depends on the machine-clock ts), else the ts's local date.
fn event_month(e: &Event) -> String {
    let date = e
        .fields
        .get("date")
        .and_then(Value::as_str)
        .map(str::to_string);
    let date = date.unwrap_or_else(|| util::local_date_of(e.ts));
    staff::month_of(&date).to_string()
}

/// The cutoff month: rows in months strictly before this are rolled up. `now`'s month minus
/// [`LEDGER_RETAINED_MONTHS`] — so a full year of recent months keeps per-run fidelity.
fn cutoff_month(now: i64) -> String {
    let today = util::local_date_of(now);
    let year: i64 = today.get(0..4).and_then(|s| s.parse().ok()).unwrap_or(0);
    let month: i64 = today.get(5..7).and_then(|s| s.parse().ok()).unwrap_or(1);
    // Month arithmetic over a 0-based month index so a subtraction wraps years correctly.
    let idx = year * 12 + (month - 1) - LEDGER_RETAINED_MONTHS;
    let (y, m) = (idx.div_euclid(12), idx.rem_euclid(12) + 1);
    format!("{y:04}-{m:02}")
}

/// Aggregate key: one monthly line per `(month, agent, model, status)`.
type RollKey = (String, String, String, String);

fn roll_key(e: &Event) -> RollKey {
    (
        event_month(e),
        event_str(e, "agent"),
        event_str(e, "model"),
        event_str(e, "status"),
    )
}

/// Build the aggregate `staff_run` event for one group. `date`/`ts` anchor at the first of the
/// month; `tokens`/`wall_ms` sum; `runs` sums the members' own counts (a per-run row counts as one,
/// an already-rolled line contributes its stored count) so re-rolling is exactly idempotent.
fn aggregate_event(key: &RollKey, members: &[&Event]) -> Event {
    let (month, agent, model, status) = key;
    let tokens: i64 = members.iter().map(|e| event_i64(e, "tokens")).sum();
    let wall_ms: i64 = members.iter().map(|e| event_i64(e, "wall_ms")).sum();
    let runs: i64 = members
        .iter()
        .map(|e| {
            let n = event_i64(e, "runs");
            if n > 0 {
                n
            } else {
                1
            }
        })
        .sum();
    let date = format!("{month}-01");
    let ts = util::parse_ts(&date).unwrap_or(0);

    let mut fields = Map::new();
    fields.insert("agent".into(), json!(agent));
    fields.insert("date".into(), json!(date));
    fields.insert("status".into(), json!(status));
    fields.insert("ok".into(), json!(status == "ok"));
    fields.insert("model".into(), json!(model));
    fields.insert("tokens".into(), json!(tokens));
    fields.insert("wall_ms".into(), json!(wall_ms));
    fields.insert("runs".into(), json!(runs));
    fields.insert("rollup".into(), json!(true));
    fields.insert(
        "work_class".into(),
        json!(crate::cost::WorkClass::Scheduled.as_str()),
    );
    Event {
        kind: "staff_run".into(),
        ts,
        fields,
    }
}

/// The outcome of planning a rollup over a set of events, ready to apply or report.
struct RollupPlan {
    /// Non-`staff_run` events plus `staff_run` rows newer than the cutoff — kept verbatim.
    kept: Vec<Event>,
    /// Monthly aggregate lines for the rolled region.
    aggregates: Vec<Event>,
    /// Per-run rows that got folded (excludes rows that were already aggregates).
    runs_folded: usize,
    /// Whether the log content would actually change (false = already fully rolled).
    changed: bool,
    /// Months rolled, sorted.
    months: Vec<String>,
}

/// Plan the rollup over `events`: split off `staff_run` rows in months older than a year, fold them
/// into `(month, agent, model, status)` aggregates, and decide whether the log content changes.
/// Pure — reads nothing, writes nothing.
fn plan_rollup(events: Vec<Event>, now: i64) -> RollupPlan {
    let cutoff = cutoff_month(now);
    let mut kept = Vec::new();
    let mut rollable: Vec<Event> = Vec::new();
    for e in events {
        if e.kind == "staff_run" && event_month(&e) < cutoff {
            rollable.push(e);
        } else {
            kept.push(e);
        }
    }

    let mut groups: BTreeMap<RollKey, Vec<&Event>> = BTreeMap::new();
    for e in &rollable {
        groups.entry(roll_key(e)).or_default().push(e);
    }

    let runs_folded = rollable
        .iter()
        .filter(|e| event_i64(e, "runs") <= 0)
        .count();
    // Already-compacted iff every rollable row is itself a singleton aggregate — then re-rolling
    // reproduces the identical set, so no write is needed.
    let all_aggregates = rollable
        .iter()
        .all(|e| e.fields.get("rollup").and_then(Value::as_bool) == Some(true));
    let all_singletons = groups.values().all(|g| g.len() == 1);
    let has_rollable = !rollable.is_empty();
    let already_rolled = all_aggregates && all_singletons;
    let changed = has_rollable && !already_rolled;

    let months: Vec<String> = groups
        .keys()
        .map(|(m, ..)| m.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let aggregates: Vec<Event> = groups.iter().map(|(k, m)| aggregate_event(k, m)).collect();

    RollupPlan {
        kept,
        aggregates,
        runs_folded,
        changed,
        months,
    }
}

/// The state log's events, sorted for a deterministic, stable rewrite: by ts (stable, so equal-ts
/// rows keep their relative order).
fn sorted_for_write(mut kept: Vec<Event>, aggregates: Vec<Event>) -> Vec<Event> {
    kept.extend(aggregates);
    kept.sort_by_key(|e| e.ts);
    kept
}

/// Rewrite the state event log atomically, under the same advisory lock a concurrent `staff_run`
/// append would take, so the rollup can't lose a run that fires mid-rewrite.
fn rewrite_ledger(all: &[Event]) -> std::io::Result<()> {
    let path = events::log_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _lock = store::FileLock::acquire(&path);
    let mut body = String::new();
    for e in all {
        body.push_str(&crate::store::to_json_line(e)?);
        body.push('\n');
    }
    store::write_atomic(&path, body.as_bytes())
}

fn measure_ledger(now: i64) -> LedgerGrowth {
    let cutoff = cutoff_month(now);
    let runs: Vec<Event> = events::read_all()
        .into_iter()
        .filter(|e| e.kind == "staff_run")
        .collect();
    let mut dates: Vec<String> = runs
        .iter()
        .map(|e| {
            e.fields
                .get("date")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| util::local_date_of(e.ts))
        })
        .collect();
    dates.sort();
    let rolluppable = runs.iter().filter(|e| event_month(e) < cutoff).count();
    LedgerGrowth {
        events: runs.len(),
        oldest: dates.first().cloned(),
        rolluppable,
    }
}

/// Roll up the `staff_run` ledger. Returns what happened (or, under `dry_run`, would happen).
fn rollup_ledger(now: i64, dry_run: bool) -> LedgerRollup {
    let plan = plan_rollup(events::read_all(), now);
    let result = LedgerRollup {
        runs_folded: plan.runs_folded,
        aggregates: plan.aggregates.len(),
        months: plan.months.clone(),
        changed: plan.changed,
    };
    if !dry_run && plan.changed {
        let all = sorted_for_write(plan.kept, plan.aggregates);
        util::best_effort(rewrite_ledger(&all), "retention: rewrite staff_run ledger");
    }
    result
}

// --- entry point --------------------------------------------------------------------------------

/// Measure the staff stores, then compact elapsed-quarter feed cards and roll up the ageing
/// `staff_run` ledger. `today` is the `YYYY-MM-DD` logical date (quarter boundary); `now` is epoch
/// seconds (ledger cutoff). Under `dry_run` nothing is written — the report is purely the plan.
/// Idempotent: a second run over the same stores changes nothing.
pub fn archive(cfg: &Config, today: &str, now: i64, dry_run: bool) -> RetentionReport {
    let feed = measure_feed(cfg, today);
    let ledger = measure_ledger(now);
    let compactions = compact_feed(cfg, today, dry_run);
    let rollup = rollup_ledger(now, dry_run);
    RetentionReport {
        dry_run,
        today: today.to_string(),
        current_quarter: quarter::quarter_id(today),
        feed,
        ledger,
        compactions,
        rollup,
    }
}

/// Convenience for callers (CLI, the weekly cron) that just want "now": derives `today`/`now` from
/// the shared clock helpers ([`util::today`] / [`util::now_epoch_secs`], both `$BN_*`-overridable).
pub fn archive_now(cfg: &Config, dry_run: bool) -> RetentionReport {
    archive(cfg, &util::today(), util::now_epoch_secs(), dry_run)
}
