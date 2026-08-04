//! Unified token-usage + **$ cost** accounting across *all* agent work (issue #381).
//!
//! Today usage is fragmented: `bn fleet` shows live tokens for currently-running dispatched
//! agents (no $, no history) and `bn staff cost` shows a monthly $ ledger for **only** the #266
//! staff-cron agents. This module is the one accounting surface that unifies them, categorized by
//! **work-class** (who launched the run) and reported in **two clearly-labeled cost units**:
//!
//! * **AIC** (AI Credits) — the *real* Copilot bill. bn already captures `totalNanoAiu` per Copilot
//!   session ([`crate::agent::parse`]); it is surfaced here as `aic`, `None` for raw-Claude / non-
//!   Copilot runs (which carry no AIC).
//! * **$ (API-equiv)** — a *benchmark*: tokens × the [`model_price`] table. Not a bill — what the
//!   same work would cost at public per-model API list prices. The price table is **cache-aware**:
//!   the bulk of prompt tokens are cache reads, which bill ~10× cheaper than fresh input, so
//!   pricing every input token at the fresh rate overcounts by ~10×. [`cost_usd`] therefore prices
//!   the cached subset of the input at a separate cache-read rate.
//!
//! [`WorkClass`] is the category: orchestration | coding | scheduled | research | remote (+
//! `unattributed` for rows recorded before the launch path threaded a class through). Every launch
//! path stamps its class: `bn dispatch` ⇒ coding (or research when scouting), orchestrated waves ⇒
//! coding, the #266 staff / cron / scheduled prompts ⇒ scheduled.
//!
//! [`rollup`] folds two event sources into one [`CostView`]: the rich `agent_session` events (#309
//! real token capture, across every machine's synced metrics log) and the `staff_run` ledger (#267
//! estimated scheduled spend, the machine-local state log). Staff runs dispatch headless (`-p`, no
//! pinned session), so they never also surface as an `agent_session` — the two sources are disjoint
//! and summing them double-counts nothing.
//!
//! The [`CostView`] shape (totals + by_category/by_machine/by_repo/by_model/by_day) matches the
//! bn-web dashboard design (#391/#389) so the web read-API can call [`rollup`] natively rather than
//! re-deriving the fold.

use crate::events::{self, Event};
use crate::resolver::Config;
use crate::util::{local_date_of, now_epoch_secs};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

const DAY: i64 = 86_400;

// --- work-class -----------------------------------------------------------------------------

/// Which launch path produced a run — the category the cost view rolls up by. `Unattributed`
/// covers rows recorded before their launch path threaded a class through (the backfill default),
/// so historical usage still folds in rather than being dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkClass {
    /// The `bn orchestrate` daemon's own dispatch → gate → merge activity and planner agents.
    Orchestration,
    /// Dispatched *ship* agents (`bn dispatch` / orchestrated waves) that produce PRs.
    Coding,
    /// Cron / scheduled work: the #266 staff roster, the night shift (#250), `bn staff run`.
    Scheduled,
    /// Scout / research agents that investigate without shipping.
    Research,
    /// Remote (#278) runs triggered on GitHub Actions rather than a local harness.
    Remote,
    /// Recorded before a launch path stamped a class — the backfill default.
    Unattributed,
}

impl WorkClass {
    /// The stable machine token stored on events and used as the `by_category` key.
    pub fn as_str(self) -> &'static str {
        match self {
            WorkClass::Orchestration => "orchestration",
            WorkClass::Coding => "coding",
            WorkClass::Scheduled => "scheduled",
            WorkClass::Research => "research",
            WorkClass::Remote => "remote",
            WorkClass::Unattributed => "unattributed",
        }
    }

    /// The title-case label for human-facing tables.
    pub fn label(self) -> &'static str {
        match self {
            WorkClass::Orchestration => "Orchestration",
            WorkClass::Coding => "Coding",
            WorkClass::Scheduled => "Scheduled",
            WorkClass::Research => "Research",
            WorkClass::Remote => "Remote",
            WorkClass::Unattributed => "Unattributed",
        }
    }

    /// Parse the machine token back into a class; unknown / empty → `None`.
    pub fn parse(s: &str) -> Option<WorkClass> {
        Some(match s.trim() {
            "orchestration" => WorkClass::Orchestration,
            "coding" => WorkClass::Coding,
            "scheduled" => WorkClass::Scheduled,
            "research" => WorkClass::Research,
            "remote" => WorkClass::Remote,
            "unattributed" => WorkClass::Unattributed,
            _ => return None,
        })
    }

    /// Parse, falling back to [`WorkClass::Unattributed`] for an unknown / absent value.
    pub fn parse_or_unattributed(s: &str) -> WorkClass {
        WorkClass::parse(s).unwrap_or(WorkClass::Unattributed)
    }
}

// --- price table (cache-aware) --------------------------------------------------------------

/// One model's list price in USD per 1M tokens, split three ways. `cache_read_per_1m` is the
/// discounted rate for prompt tokens served from the provider's cache (~10× cheaper than fresh
/// input on both Anthropic and OpenAI); most of our prompt tokens are cache reads, so pricing them
/// at the fresh `input_per_1m` rate overcounts by ~10×. Output tokens dominate cost.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ModelPrice {
    pub input_per_1m: f64,
    pub cache_read_per_1m: f64,
    pub output_per_1m: f64,
}

/// The per-model price table — **the one place** to keep $ rates current. Matched by
/// case-insensitive substring so the many concrete ids of a family (`claude-opus-4.8`,
/// `gpt-5.3-codex`, `gemini-3.1-pro-preview`, …) all resolve to one row. Order matters: the cheap
/// tier needles (`nano`/`mini`/`flash`) come before their base family so `gpt-5-mini` doesn't
/// match the `gpt-5` row. Cache-read rates are the provider's ~10%-of-input cached-token rate. An
/// unrecognized model returns `None` — the caller prices it as unknown (`cost_usd` null), never a
/// panic.
pub fn model_price(model: &str) -> Option<ModelPrice> {
    let m = model.trim().to_ascii_lowercase();
    if m.is_empty() {
        return None;
    }
    // (needle, input $/1M, cache-read $/1M, output $/1M). First substring match wins, so the
    // order guards against substring collisions: `flash`/`gemini` precede `mini` because
    // "ge**mini**" contains "mini"; the cheap `nano`/`mini` tiers precede the `gpt-5` base so
    // `gpt-5-mini` doesn't match `gpt-5`.
    const TABLE: &[(&str, f64, f64, f64)] = &[
        ("nano", 0.05, 0.005, 0.40),
        // Google Gemini (before the generic `mini`, which "gemini" contains).
        ("flash", 0.30, 0.03, 2.50),
        ("gemini", 1.25, 0.125, 10.00),
        // OpenAI GPT-5 mini/base.
        ("mini", 0.25, 0.025, 2.00),
        // Anthropic Claude.
        ("haiku", 0.80, 0.08, 4.00),
        ("sonnet", 3.00, 0.30, 15.00),
        ("opus", 15.00, 1.50, 75.00),
        // OpenAI GPT-5 family base + codex variants.
        ("gpt-5", 1.25, 0.125, 10.00),
    ];
    for (needle, inp, cache, out) in TABLE {
        if m.contains(needle) {
            return Some(ModelPrice {
                input_per_1m: *inp,
                cache_read_per_1m: *cache,
                output_per_1m: *out,
            });
        }
    }
    None
}

/// The $ (API-equiv) cost of one run on `model`, or `None` when the model is not in the
/// [`model_price`] table. **Cache-aware:** `fresh_input_tokens` is the non-cached input (priced at
/// the full rate) and `cache_read_tokens` the cached prompt tokens (priced at the ~10×-cheaper
/// cache rate) — the two are already separated (see [`fresh_input`]), not overlapping. Negative
/// counts clamp to zero.
pub fn cost_usd(
    model: &str,
    fresh_input_tokens: i64,
    cache_read_tokens: i64,
    output_tokens: i64,
) -> Option<f64> {
    let p = model_price(model)?;
    let per_1m = |tokens: i64, rate: f64| (tokens.max(0) as f64) / 1_000_000.0 * rate;
    Some(
        per_1m(fresh_input_tokens, p.input_per_1m)
            + per_1m(cache_read_tokens, p.cache_read_per_1m)
            + per_1m(output_tokens, p.output_per_1m),
    )
}

/// Split a session's raw `input_tokens` into the **non-cached** input to price at the full rate.
/// The harnesses disagree on what `input_tokens` means: a Copilot session reports the *total*
/// prompt there (cache reads are a subset — typically ~95% of it), so the fresh part is
/// `input - cache_read`; a Claude transcript reports only the *fresh* input, with cache reads
/// counted separately (and often far larger). Normalizing to the fresh input is what stops the
/// ~10× overcount from pricing every cached token as fresh.
pub fn fresh_input(harness: &str, input_tokens: i64, cache_read_tokens: i64) -> i64 {
    if harness.eq_ignore_ascii_case("copilot") {
        (input_tokens - cache_read_tokens.max(0)).max(0)
    } else {
        input_tokens.max(0)
    }
}

/// Sum two optional numbers, treating `None` as "absent" rather than zero: `None + None` stays
/// `None` (a group with no priceable model reports null $, and a Claude-only group reports null
/// AIC — never a misleading 0), while any present value makes the sum `Some`.
fn add_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x + y),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

// --- the cost view --------------------------------------------------------------------------

/// One grouped line of a [`CostView`] — a category / machine / repo / model / day and its folded
/// usage in **both** cost units. `aic` is `None` when nothing in the group was a Copilot run (the
/// real bill is Copilot-only); `cost_usd` is `None` when nothing in the group had a priceable model.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostRow {
    pub key: String,
    pub sessions: u64,
    /// Fresh (non-cached) input tokens, priced at the full input rate.
    pub input_tokens: i64,
    /// Cached prompt tokens, priced at the ~10×-cheaper cache-read rate.
    pub cache_read_tokens: i64,
    pub output_tokens: i64,
    /// **AIC** (AI Credits) — the real Copilot bill (Σ `totalNanoAiu` / 1e9). `None` = no Copilot
    /// runs in this group.
    pub aic: Option<f64>,
    /// **$ (API-equiv)** — the cache-aware benchmark from the price table. `None` = no priceable
    /// model in this group.
    pub cost_usd: Option<f64>,
}

/// The whole-window totals across every source, in both cost units.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CostTotals {
    pub sessions: u64,
    /// Fresh (non-cached) input tokens.
    pub input_tokens: i64,
    /// Cached prompt tokens (priced at the cheap cache-read rate).
    pub cache_read_tokens: i64,
    pub output_tokens: i64,
    /// **AIC** (AI Credits) — the real Copilot bill. `None` = no Copilot runs.
    pub aic: Option<f64>,
    /// **$ (API-equiv)** — the cache-aware benchmark. `None` = nothing priceable.
    pub cost_usd: Option<f64>,
}

/// One `(day, category)` cell of the [`CostView::by_day_category`] cross-tab — the per-day,
/// per-work-class usage the dashboard's stacked-by-category bar chart (#391 §4(b)) needs. Kept
/// separate from the 1-D `by_day` (day totals) and `by_category` (window totals) breakdowns because
/// a bar *stacked by category* needs both axes at once. Ordered by day ascending, then category, so
/// the chart can build a stable per-category series without re-sorting.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DayCategory {
    pub day: String,
    /// The [`WorkClass::as_str`] token (`orchestration` | `coding` | … | `unattributed`).
    pub category: String,
    pub sessions: u64,
    pub input_tokens: i64,
    pub cache_read_tokens: i64,
    pub output_tokens: i64,
    /// **AIC** — the real Copilot bill for this cell; `None` = no Copilot runs.
    pub aic: Option<f64>,
    /// **$ (API-equiv)** — the cache-aware benchmark for this cell; `None` = nothing priceable.
    pub cost_usd: Option<f64>,
}

/// Unified token + dual-cost usage over a trailing window, broken down along every dimension the
/// #381 surface (and the bn-web dashboard #391) reports. Each `by_*` vec is sorted cost-desc so the
/// dominant contributor leads; `by_day_category` is the (day × category) cross-tab, ordered by day.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CostView {
    pub window_days: i64,
    pub totals: CostTotals,
    pub by_category: Vec<CostRow>,
    pub by_machine: Vec<CostRow>,
    pub by_repo: Vec<CostRow>,
    pub by_model: Vec<CostRow>,
    pub by_day: Vec<CostRow>,
    /// The (day × category) cross-tab for the stacked-by-category-per-day chart (#391 §4(b)).
    pub by_day_category: Vec<DayCategory>,
}

/// What [`rollup`] should fold. `window_days` is the trailing window (`bn cost` defaults it to the
/// current month-to-date); `now` anchors it (overridable for tests); `repo`, when set, restricts
/// the fold to one note-canonical repo. `window_secs`, when `Some`, overrides `window_days` with a
/// finer trailing window in seconds — the resource-aware scheduler (#377) uses it for a strict
/// trailing-hour token fold, which day granularity can't express.
#[derive(Debug, Clone)]
pub struct CostFilter {
    pub window_days: i64,
    pub now: i64,
    pub repo: Option<String>,
    /// A trailing window in seconds that overrides `window_days` when set (#377). `None` = use
    /// `window_days` as before.
    pub window_secs: Option<i64>,
}

impl CostFilter {
    /// A window of `window_days` ending now, across all repos.
    pub fn window(window_days: i64) -> CostFilter {
        CostFilter {
            window_days,
            now: now_epoch_secs(),
            repo: None,
            window_secs: None,
        }
    }

    /// A trailing `secs`-second window for `repo` ending at `now` (#377) — the fine-grained fold the
    /// scheduler's token-rate budget needs (e.g. the last hour).
    pub fn secs(secs: i64, repo: &str, now: i64) -> CostFilter {
        CostFilter {
            window_days: 0,
            now,
            repo: Some(repo.to_string()),
            window_secs: Some(secs),
        }
    }
}

/// One normalized run drawn from either event source, before folding. `sessions` is always 1 per
/// sample; the fold sums them. Public so the pure [`fold`] is testable without touching disk.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSample {
    pub work_class: WorkClass,
    pub machine: String,
    pub repo: String,
    pub model: String,
    pub ts: i64,
    /// Fresh (non-cached) input tokens (harness-normalized; see [`fresh_input`]).
    pub input_tokens: i64,
    pub cache_read_tokens: i64,
    pub output_tokens: i64,
    /// AIC (real Copilot bill); `None` for Claude / non-Copilot.
    pub aic: Option<f64>,
    /// $ (API-equiv) benchmark; `None` when the model is unpriceable.
    pub cost_usd: Option<f64>,
}

#[derive(Default)]
struct Acc {
    sessions: u64,
    input_tokens: i64,
    cache_read_tokens: i64,
    output_tokens: i64,
    aic: Option<f64>,
    cost_usd: Option<f64>,
}

impl Acc {
    fn add(&mut self, s: &RunSample) {
        self.sessions += 1;
        self.input_tokens += s.input_tokens;
        self.cache_read_tokens += s.cache_read_tokens;
        self.output_tokens += s.output_tokens;
        self.aic = add_opt(self.aic, s.aic);
        self.cost_usd = add_opt(self.cost_usd, s.cost_usd);
    }
    fn into_row(self, key: String) -> CostRow {
        CostRow {
            key,
            sessions: self.sessions,
            input_tokens: self.input_tokens,
            cache_read_tokens: self.cache_read_tokens,
            output_tokens: self.output_tokens,
            aic: self.aic,
            cost_usd: self.cost_usd,
        }
    }
}

/// Canonicalize a model id so `.`/`-` version-separator variants fold into one key: a hyphen
/// *between two digits* becomes a dot, so `claude-opus-4-8` and `claude-opus-4.8` (or `sonnet-4-6`
/// and `sonnet-4.6`) collapse to the single canonical dotted id (`claude-opus-4.8`). Only the
/// digit–`-`–digit case is rewritten, so family separators (`claude-opus`, `gpt-5-mini`,
/// `gpt-5.3-codex`) are left intact. Trimmed; an empty / whitespace id stays empty so the fold can
/// drop it rather than surface a bare `—` row.
pub fn normalize_model_id(model: &str) -> String {
    let trimmed = model.trim();
    let bytes = trimmed.as_bytes();
    let mut out = String::with_capacity(trimmed.len());
    for (i, ch) in trimmed.char_indices() {
        let dot_sep = ch == '-'
            && i > 0
            && bytes[i - 1].is_ascii_digit()
            && bytes.get(i + 1).is_some_and(u8::is_ascii_digit);
        out.push(if dot_sep { '.' } else { ch });
    }
    out
}

/// Sort folded rows by $ cost desc, then output tokens desc, then key asc — a stable, deterministic
/// order that leads with the dominant spender.
fn sort_rows(rows: &mut [CostRow]) {
    rows.sort_by(|a, b| {
        b.cost_usd
            .unwrap_or(0.0)
            .total_cmp(&a.cost_usd.unwrap_or(0.0))
            .then(b.output_tokens.cmp(&a.output_tokens))
            .then(a.key.cmp(&b.key))
    });
}

/// Group `samples` by a key, sorted by $ cost desc, then output tokens desc, then key asc — a
/// stable, deterministic order that leads with the dominant spender. An empty key renders as `—`.
fn group_by(samples: &[RunSample], key: impl Fn(&RunSample) -> String) -> Vec<CostRow> {
    let mut map: BTreeMap<String, Acc> = BTreeMap::new();
    for s in samples {
        let mut k = key(s);
        if k.is_empty() {
            k = "—".to_string();
        }
        map.entry(k).or_default().add(s);
    }
    let mut rows: Vec<CostRow> = map.into_iter().map(|(k, a)| a.into_row(k)).collect();
    sort_rows(&mut rows);
    rows
}

/// Group `samples` by model id for the `by_model` breakdown. Unlike [`group_by`], model ids are
/// canonicalized via [`normalize_model_id`] so `.`/`-` version variants fold together, and rows
/// whose model is empty (mid-session Copilot runs surface no model until shutdown) are dropped
/// rather than collapsed into a misleading bare `—` row. Same sort as [`group_by`].
fn group_by_model(samples: &[RunSample]) -> Vec<CostRow> {
    let mut map: BTreeMap<String, Acc> = BTreeMap::new();
    for s in samples {
        let k = normalize_model_id(&s.model);
        if k.is_empty() {
            continue;
        }
        map.entry(k).or_default().add(s);
    }
    let mut rows: Vec<CostRow> = map.into_iter().map(|(k, a)| a.into_row(k)).collect();
    sort_rows(&mut rows);
    rows
}

/// Fold already-windowed `samples` into a [`CostView`]. Pure over the slice — [`rollup`] reads the
/// event logs and normalizes; this does the grouping and totals.
pub fn fold(samples: &[RunSample], window_days: i64) -> CostView {
    let mut t = Acc::default();
    for s in samples {
        t.add(s);
    }
    CostView {
        window_days,
        totals: CostTotals {
            sessions: t.sessions,
            input_tokens: t.input_tokens,
            cache_read_tokens: t.cache_read_tokens,
            output_tokens: t.output_tokens,
            aic: t.aic,
            cost_usd: t.cost_usd,
        },
        by_category: group_by(samples, |s| s.work_class.as_str().to_string()),
        by_machine: group_by(samples, |s| s.machine.clone()),
        by_repo: group_by(samples, |s| s.repo.clone()),
        by_model: group_by_model(samples),
        by_day: group_by(samples, |s| local_date_of(s.ts)),
        by_day_category: group_by_day_category(samples),
    }
}

/// Fold `samples` into the (day × category) cross-tab powering the stacked-by-category bar chart
/// (#391). Ordered by day ascending then category ascending (the `BTreeMap` key order), a
/// deterministic layout the chart consumes without re-sorting.
fn group_by_day_category(samples: &[RunSample]) -> Vec<DayCategory> {
    let mut map: BTreeMap<(String, String), Acc> = BTreeMap::new();
    for s in samples {
        let key = (local_date_of(s.ts), s.work_class.as_str().to_string());
        map.entry(key).or_default().add(s);
    }
    map.into_iter()
        .map(|((day, category), a)| DayCategory {
            day,
            category,
            sessions: a.sessions,
            input_tokens: a.input_tokens,
            cache_read_tokens: a.cache_read_tokens,
            output_tokens: a.output_tokens,
            aic: a.aic,
            cost_usd: a.cost_usd,
        })
        .collect()
}

// --- attribution store ----------------------------------------------------------------------

const ATTRIBUTION_KIND: &str = "run_attribution";

/// Canonical form of a harness session id for the attribution join. UUIDs are case-insensitive by
/// spec, yet the two sides of the join are produced independently: a launch path pins the id it
/// generates (macOS `uuidgen` is upper-case; `bn dispatch` lower-cases it) while the harness later
/// records the id it *actually* used in its own log — and nothing guarantees the two agree on case
/// or surrounding whitespace. An exact-string join therefore silently drops any such variance into
/// `unattributed` (the same key-shape trap as the #530 model-id `.`↔`-` fold). Canonicalizing both
/// the [`attribution_map`] keys *and* every lookup through here keeps the join matching real
/// sessions regardless of case/whitespace, so attributed spend lands in its real work-class and an
/// `Orchestration` session gets its `orchestrator` role.
pub fn canonical_session_id(session_id: &str) -> String {
    session_id.trim().to_lowercase()
}

/// Record that the harness session `session_id` belongs to `work_class` (with its `repo`/`branch`
/// for context). Appended to the machine-local state log at launch time by every launch path that
/// pins a session id (`bn dispatch` generates the id it passes as `--session`, so it is the
/// harness session id [`crate::agent::session::ingest`] later keys on). [`attribution_map`] reads
/// these back so ingest can stamp `work_class` onto the `agent_session` event. Best-effort: a
/// failed write just leaves the session `unattributed`.
pub fn attribute(session_id: &str, work_class: WorkClass, repo: &str, branch: &str) {
    attribute_with_controller(session_id, work_class, repo, branch, None);
}

/// Record launch attribution plus the controller that owns the worker, when dispatched by one.
pub fn attribute_with_controller(
    session_id: &str,
    work_class: WorkClass,
    repo: &str,
    branch: &str,
    controller_id: Option<&str>,
) {
    attribute_session(SessionLaunch {
        session_id,
        work_class,
        repo,
        branch,
        task_name: None,
        harness: None,
        worktree: None,
        controller_id,
    });
}

/// Optional launch metadata used by the cross-repo session registry. It is written into the same
/// `run_attribution` event as cost ownership, so launch recording remains one durable store.
#[derive(Debug, Clone, Copy)]
pub struct SessionLaunch<'a> {
    pub session_id: &'a str,
    pub work_class: WorkClass,
    pub repo: &'a str,
    pub branch: &'a str,
    pub task_name: Option<&'a str>,
    pub harness: Option<&'a str>,
    pub worktree: Option<&'a str>,
    pub controller_id: Option<&'a str>,
}

/// Record one harness launch in the existing attribution log.
pub fn attribute_session(launch: SessionLaunch<'_>) {
    let mut f = serde_json::Map::new();
    f.insert("session_id".into(), serde_json::json!(launch.session_id));
    f.insert(
        "work_class".into(),
        serde_json::json!(launch.work_class.as_str()),
    );
    f.insert("repo".into(), serde_json::json!(launch.repo));
    f.insert("branch".into(), serde_json::json!(launch.branch));
    if let Some(task_name) = launch.task_name {
        f.insert("task_name".into(), serde_json::json!(task_name));
    }
    if let Some(harness) = launch.harness {
        f.insert("harness".into(), serde_json::json!(harness));
    }
    if let Some(worktree) = launch.worktree {
        f.insert("worktree".into(), serde_json::json!(worktree));
    }
    if let Some(controller_id) = launch.controller_id {
        f.insert("controller_id".into(), serde_json::json!(controller_id));
    }
    crate::util::best_effort(
        events::append(ATTRIBUTION_KIND, f),
        "cost: record run attribution",
    );
}

/// The `session_id → work_class` map from the machine-local attribution log (latest write wins).
/// Keys are [`canonical_session_id`]-normalized so the join tolerates case/whitespace variance
/// between the launch-pinned id and the harness-recorded id. Consulted by ingest (to stamp the
/// event) and by [`rollup`] (to attribute events recorded before the field existed, or attributed
/// after ingest); look up through [`canonical_session_id`] to hit these keys.
pub fn attribution_map() -> BTreeMap<String, WorkClass> {
    let mut map = BTreeMap::new();
    for e in events::read_all() {
        if e.kind != ATTRIBUTION_KIND {
            continue;
        }
        let (Some(sid), Some(wc)) = (
            e.fields.get("session_id").and_then(|v| v.as_str()),
            e.fields.get("work_class").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        map.insert(
            canonical_session_id(sid),
            WorkClass::parse_or_unattributed(wc),
        );
    }
    map
}

/// A launch-path attribution record with the context the live fleet needs to synthesize a
/// pending "starting…" entry for a just-dispatched agent: the pinned harness session id, the
/// `repo`/`branch` it targets, and when the launch was recorded (`launched_at`, epoch secs).
/// [`attribution_map`] collapses these to `session_id → work_class`; this keeps the full context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRecord {
    pub session_id: String,
    pub task_name: String,
    pub harness: String,
    pub repo: String,
    pub branch: String,
    pub worktree: String,
    pub work_class: WorkClass,
    pub launched_at: i64,
    pub controller_id: Option<String>,
}

/// Every launch attribution record from the machine-local log, in file (chronological) order —
/// the raw feed the live fleet folds into pending "starting…" entries (#545) for agents that were
/// dispatched but haven't written their first transcript yet. Best-effort: malformed lines are
/// skipped, a missing log yields an empty vec.
pub fn launch_records() -> Vec<LaunchRecord> {
    events::read_all()
        .into_iter()
        .filter(|e| e.kind == ATTRIBUTION_KIND)
        .filter_map(|e| {
            let session_id = e
                .fields
                .get("session_id")
                .and_then(|v| v.as_str())?
                .to_string();
            let repo = e
                .fields
                .get("repo")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let branch = e
                .fields
                .get("branch")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let task_name = e
                .fields
                .get("task_name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let harness = e
                .fields
                .get("harness")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let worktree = e
                .fields
                .get("worktree")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let work_class = e
                .fields
                .get("work_class")
                .and_then(|v| v.as_str())
                .map(WorkClass::parse_or_unattributed)
                .unwrap_or(WorkClass::Unattributed);
            let controller_id = e
                .fields
                .get("controller_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            Some(LaunchRecord {
                session_id,
                task_name,
                harness,
                repo,
                branch,
                worktree,
                work_class,
                launched_at: e.ts,
                controller_id,
            })
        })
        .collect()
}

// --- rollup (reads the logs, normalizes, folds) ---------------------------------------------

/// This machine segment of a synced metrics-log path (`<personal_root>/<machine>/metrics/
/// events.jsonl`), the `by_machine` attribution. Falls back to `—` if the shape is unexpected.
fn machine_of(path: &Path) -> String {
    path.parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "—".to_string())
}

fn field_i64(e: &Event, k: &str) -> i64 {
    e.fields.get(k).and_then(|v| v.as_i64()).unwrap_or(0)
}

/// Normalize one `agent_session` event into a [`RunSample`]. `work_class` is the event's own field
/// when meaningful, else the launch-path attribution for its session id, else `unattributed`. The
/// priced model is the first of the (comma-joined, first-seen order) `models`; `aic` is the real
/// Copilot bill (`cost_nanoaiu`), absent for Claude.
fn sample_from_agent_session(
    e: &Event,
    machine: &str,
    attribution: &BTreeMap<String, WorkClass>,
) -> RunSample {
    let field_wc = e
        .fields
        .get("work_class")
        .and_then(|v| v.as_str())
        .and_then(WorkClass::parse)
        .filter(|c| *c != WorkClass::Unattributed);
    let work_class = field_wc
        .or_else(|| {
            e.fields
                .get("session_id")
                .and_then(|v| v.as_str())
                .and_then(|sid| attribution.get(&canonical_session_id(sid)).copied())
        })
        .unwrap_or(WorkClass::Unattributed);
    let model = e
        .fields
        .get("models")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .split(", ")
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();
    let input = field_i64(e, "input_tokens");
    let cache_read = field_i64(e, "cache_read_tokens");
    let output = field_i64(e, "output_tokens");
    let harness = e
        .fields
        .get("harness")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // Normalize `input_tokens` to the non-cached input the full rate applies to (harness-aware),
    // so a Copilot session's ~95%-cached prompt isn't priced 10× too high.
    let fresh = fresh_input(harness, input, cache_read);
    let aic = e
        .fields
        .get("cost_nanoaiu")
        .and_then(|v| v.as_i64())
        .map(|n| n as f64 / 1e9);
    RunSample {
        work_class,
        machine: machine.to_string(),
        repo: e
            .fields
            .get("repo")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        model: model.clone(),
        ts: e.ts,
        input_tokens: fresh,
        cache_read_tokens: cache_read,
        output_tokens: output,
        aic,
        cost_usd: cost_usd(&model, fresh, cache_read, output),
    }
}

/// Normalize one `staff_run` ledger event (#267) into a [`RunSample`]. Staff runs are `scheduled`
/// by construction; their token figure is the [`crate::staff::estimate_tokens`] estimate (headless
/// `-p` surfaces no metered count), folded as output tokens — the headline usage signal — and
/// priced from the run's model. Headless runs carry no AIC.
fn sample_from_staff_run(e: &Event, machine: &str) -> RunSample {
    let work_class = e
        .fields
        .get("work_class")
        .and_then(|v| v.as_str())
        .map(WorkClass::parse_or_unattributed)
        .unwrap_or(WorkClass::Scheduled);
    let model = e
        .fields
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let tokens = field_i64(e, "tokens");
    RunSample {
        work_class,
        machine: machine.to_string(),
        repo: String::new(),
        model: model.clone(),
        ts: e.ts,
        input_tokens: 0,
        cache_read_tokens: 0,
        output_tokens: tokens,
        aic: None,
        cost_usd: cost_usd(&model, 0, 0, tokens),
    }
}

/// Fold the unified cost view for `filter`: `agent_session` events across every machine's synced
/// metrics log (real #309 token capture, machine = source-log segment) plus the machine-local
/// `staff_run` ledger (#267 scheduled estimate). Both are windowed to `filter.now -
/// filter.window_days` and, when `filter.repo` is set, restricted to that repo. This is the
/// reusable core the `bn cost` CLI and the bn-web read-API (#391/#389) both call.
pub fn rollup(cfg: &Config, filter: &CostFilter) -> CostView {
    let cutoff = match filter.window_secs {
        Some(secs) => filter.now - secs.max(0),
        None => filter.now - filter.window_days.max(0) * DAY,
    };
    let attribution = attribution_map();
    let mut samples: Vec<RunSample> = Vec::new();

    // Latest agent_session snapshot per id. A resumed harness session appends a newer aggregate
    // snapshot under the same id; selecting latest prevents usage/cost double-counting.
    let mut latest_sessions: BTreeMap<String, (String, Event)> = BTreeMap::new();
    for path in cfg.metrics_logs_all_machines() {
        let machine = machine_of(&path);
        for e in events::read_events_at(&path) {
            if e.kind != "agent_session" {
                continue;
            }
            let Some(id) = e.fields.get("session_id").and_then(|value| value.as_str()) else {
                continue;
            };
            let key = canonical_session_id(id);
            if latest_sessions
                .get(&key)
                .is_none_or(|(_, current)| e.ts >= current.ts)
            {
                latest_sessions.insert(key, (machine.clone(), e));
            }
        }
    }
    for (_, (machine, event)) in latest_sessions {
        if event.ts < cutoff {
            continue;
        }
        let sample = sample_from_agent_session(&event, &machine, &attribution);
        if repo_matches(filter, &sample.repo) {
            samples.push(sample);
        }
    }

    // staff_run ledger (machine-local). Skipped entirely under a repo filter — staff runs are
    // roster-scoped, not repo-scoped, so they carry no repo to match.
    if filter.repo.is_none() {
        let machine = cfg.machine().to_string();
        for e in events::read_all() {
            if e.kind != "staff_run" || e.ts < cutoff {
                continue;
            }
            samples.push(sample_from_staff_run(&e, &machine));
        }
    }

    fold(&samples, filter.window_days)
}

fn repo_matches(filter: &CostFilter, repo: &str) -> bool {
    match &filter.repo {
        Some(want) => want == repo,
        None => true,
    }
}

// --- live (in-flight run so far) ------------------------------------------------------------

/// A snapshot of currently-running agents' usage so far, categorized by work-class — what
/// `bn cost --live` shows. Live Copilot logs surface only `output_tokens` mid-session and no model
/// (the model / AIC land in the final `session.shutdown`, folded by ingest), so `aic` and
/// `cost_usd` here are `None` until the run finishes.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct LiveCost {
    pub agents: usize,
    pub output_tokens: i64,
    pub by_category: Vec<CostRow>,
}

/// Roll the live fleet ([`crate::agent::supervise`]) up by work-class, attributing each
/// running session via the launch-path [`attribution_map`]. In-flight usage, no history.
pub fn live_rollup(cfg: &Config) -> LiveCost {
    use crate::agent::{supervise, DEFAULT_IDLE_MINS, DEFAULT_STUCK_MINS};
    let fleet = supervise(cfg, DEFAULT_IDLE_MINS, DEFAULT_STUCK_MINS);
    let attribution = attribution_map();
    let samples: Vec<RunSample> = fleet
        .agents
        .iter()
        .map(|a| RunSample {
            work_class: attribution
                .get(&canonical_session_id(&a.session_id))
                .copied()
                .unwrap_or(WorkClass::Unattributed),
            machine: cfg.machine().to_string(),
            repo: a.repo.clone(),
            model: String::new(),
            ts: a.last_activity_ts,
            input_tokens: fresh_input(&a.harness, a.input_tokens, a.cache_read_tokens),
            cache_read_tokens: a.cache_read_tokens,
            output_tokens: a.output_tokens,
            aic: None,
            cost_usd: None,
        })
        .collect();
    LiveCost {
        agents: fleet.agents.len(),
        output_tokens: fleet.total_tokens,
        by_category: group_by(&samples, |s| s.work_class.as_str().to_string()),
    }
}


