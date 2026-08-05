//! Quarter goals — the layer above daily (`bn daily`) and weekly (`bn weekly`) goals, plus a
//! recurring **impact agent** that scores shipped work against those goals. One file per quarter
//! (`<personal_root>/<machine>/quarters/<YYYY-Qn>.md`), same synced-markdown philosophy as daily:
//! the YAML frontmatter is the serde source of truth (goals with a stable `key`, an optional
//! `metric` hint, and the appended impact reviews); the body is a regenerated, greppable render.
//!
//! Work links *down* to a quarter goal via the optional `q` field on [`crate::model::Item`]
//! (`bn daily add … --q <key>`, `bn add todo … --q <key>`). The impact agent
//! ([`review`]) reads the window's ground truth (merged PRs, completed goals, brag entries),
//! scores each goal (on-track / at-risk / off-track / done), and appends a **validated** review —
//! a malformed run appends nothing and raises a warn attention item instead.

use crate::commands::CmdError;
use crate::resolver::Config;
use crate::runner::CommandRunner;
use crate::store::StoreError;
use crate::util;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// The recognised per-goal statuses, narrowest-to-broadest concern. Kept narrative (no 0.0–1.0
/// grading — that OKR formalism is explicitly out of scope): a status plus one line of evidence.
pub const STATUSES: [&str; 4] = ["on-track", "at-risk", "off-track", "done"];

/// One quarter goal: a checkbox line with a **stable, greppable `key`** (used for `--q` tagging
/// and reviews), an optional `metric` hint, and done state. Distinct from [`crate::model::Item`]
/// because a quarter goal is keyed by `key`, not an ephemeral positional id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuarterGoal {
    /// Stable identifier (`ship-v1`) — what `--q` tags reference and reviews assess.
    pub key: String,
    pub text: String,
    pub created: String,
    #[serde(default)]
    pub done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<String>,
    /// Optional metric hint (`pr_cycle_time<-30%`) — a note to the impact agent, not enforced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<String>,
}

/// One goal's assessment inside an impact review.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalAssessment {
    pub key: String,
    /// One of [`STATUSES`].
    pub status: String,
    pub evidence: String,
}

/// A validated impact review appended by the agent (newest first). Structured so `bn quarter`
/// can surface statuses and so drift becomes attention items — not free prose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImpactReview {
    pub date: String,
    pub window_start: String,
    pub window_end: String,
    pub assessments: Vec<GoalAssessment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranking: Vec<String>,
    pub recommendation: String,
}

/// A quarter file's frontmatter: the goals plus the appended impact reviews (newest first).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuarterFront {
    /// `YYYY-Qn`, e.g. `2026-Q3`.
    pub quarter: String,
    pub created: String,
    #[serde(default)]
    pub goals: Vec<QuarterGoal>,
    /// Impact reviews, newest first. Omitted from YAML while empty so a goals-only quarter stays
    /// byte-identical across writes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviews: Vec<ImpactReview>,
}

/// A fully-read quarter: the typed frontmatter (the body is a regenerated render).
#[derive(Debug, Clone, PartialEq)]
pub struct Quarter {
    pub front: QuarterFront,
}

impl Quarter {
    fn empty(quarter: &str, created: &str) -> Quarter {
        Quarter {
            front: QuarterFront {
                quarter: quarter.to_string(),
                created: created.to_string(),
                goals: vec![],
                reviews: vec![],
            },
        }
    }

    /// Open (not-done) goals, in order.
    pub fn open(&self) -> impl Iterator<Item = &QuarterGoal> {
        self.front.goals.iter().filter(|g| !g.done)
    }

    /// The most recent review, if any (reviews are stored newest-first).
    pub fn latest_review(&self) -> Option<&ImpactReview> {
        self.front.reviews.first()
    }

    /// The status of a goal: `done` when checked off, else the goal's status in the latest
    /// review, else `unscored` (no review has judged it yet).
    pub fn status_of(&self, key: &str) -> &str {
        if self.front.goals.iter().any(|g| g.key == key && g.done) {
            return "done";
        }
        self.latest_review()
            .and_then(|r| r.assessments.iter().find(|a| a.key == key))
            .map(|a| a.status.as_str())
            .unwrap_or("unscored")
    }

    /// Count of goals considered healthy (`on-track` or `done`) — the numerator of the header's
    /// "N/M goals on track".
    pub fn on_track_count(&self) -> usize {
        self.front
            .goals
            .iter()
            .filter(|g| matches!(self.status_of(&g.key), "on-track" | "done"))
            .count()
    }

    /// The top at-risk / off-track goal (off-track first), for the standup one-liner. `None` when
    /// nothing is flagged.
    pub fn top_at_risk(&self) -> Option<&QuarterGoal> {
        let by = |want: &str| {
            self.front
                .goals
                .iter()
                .find(|g| self.status_of(&g.key) == want)
        };
        by("off-track").or_else(|| by("at-risk"))
    }
}

// ---- Quarter arithmetic --------------------------------------------------------------------

/// The quarter id (`YYYY-Qn`) a `YYYY-MM-DD` date falls in. `None` if the date doesn't parse.
pub fn quarter_id(date: &str) -> Option<String> {
    let (year, month, _) = parse_ymd(date)?;
    Some(format!("{year:04}-Q{}", (month - 1) / 3 + 1))
}

/// Parse a quarter id into `(year, quarter)` with `quarter` in `1..=4`. `None` if malformed.
pub fn parse_qid(qid: &str) -> Option<(i64, u32)> {
    let (y, q) = qid.trim().split_once("-Q")?;
    let year: i64 = y.parse().ok()?;
    let quarter: u32 = q.parse().ok()?;
    (1..=4).contains(&quarter).then_some((year, quarter))
}

/// True for a well-formed quarter id (`2026-Q3`).
pub fn is_qid(qid: &str) -> bool {
    parse_qid(qid).is_some()
}

/// The inclusive `[start, end]` `YYYY-MM-DD` bounds of a quarter. Quarter ends are fixed calendar
/// dates (Q1 → Mar 31, Q2 → Jun 30, Q3 → Sep 30, Q4 → Dec 31). `None` for a malformed id.
pub fn quarter_bounds(qid: &str) -> Option<(String, String)> {
    let (year, q) = parse_qid(qid)?;
    let start_month = (q - 1) * 3 + 1;
    let (end_month, end_day) = match q {
        1 => (3, 31),
        2 => (6, 30),
        3 => (9, 30),
        _ => (12, 31),
    };
    Some((
        format!("{year:04}-{start_month:02}-01"),
        format!("{year:04}-{end_month:02}-{end_day:02}"),
    ))
}

/// Fraction (`0.0..=1.0`) of the quarter elapsed as of `today`. Clamped: before the quarter → 0,
/// after → 1. `None` if `qid`/`today` don't parse.
pub fn elapsed_fraction(qid: &str, today: &str) -> Option<f64> {
    let (start, end) = quarter_bounds(qid)?;
    let (s, e, t) = (
        util::epoch_day(&start)?,
        util::epoch_day(&end)?,
        util::epoch_day(today)?,
    );
    if e <= s {
        return Some(0.0);
    }
    Some((((t - s) as f64) / ((e - s) as f64)).clamp(0.0, 1.0))
}

/// Parse a `YYYY-MM-DD` into `(year, month, day)`.
fn parse_ymd(date: &str) -> Option<(i64, u32, u32)> {
    if !util::is_iso_date(date) {
        return None;
    }
    let mut p = date.splitn(3, '-');
    let y: i64 = p.next()?.parse().ok()?;
    let m: u32 = p.next()?.parse().ok()?;
    let d: u32 = p.next()?.parse().ok()?;
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then_some((y, m, d))
}

// ---- IO ------------------------------------------------------------------------------------

/// Read a quarter file, parsing the leading `---` frontmatter into [`QuarterFront`].
pub fn read(path: &Path) -> Result<Quarter, StoreError> {
    let text = std::fs::read_to_string(path)?;
    let front: QuarterFront = crate::store::parse_frontmatter(&text)?;
    Ok(Quarter { front })
}

/// Serialize a quarter and write it (creating the `quarters/` dir as needed).
fn write(quarter: &Quarter, path: &Path) -> Result<(), StoreError> {
    let text = to_text(quarter)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

/// Render a quarter to on-disk text: frontmatter + a **date-stable** body (goals as checkbox
/// lines with their key/metric, then the impact reviews). The body carries nothing time-relative
/// (no "% elapsed") so re-writing an unchanged quarter stays byte-identical and never churns sync.
fn to_text(q: &Quarter) -> Result<String, StoreError> {
    let fm = serde_yaml::to_string(&q.front).map_err(|e| StoreError::Parse(e.to_string()))?;
    Ok(format!(
        "---\n{fm}---\n\n# Quarter — {}\n\n{}",
        q.front.quarter,
        render_body(q)
    ))
}

/// The date-stable markdown body: `## Goals` checkbox lines + `## Impact reviews`.
fn render_body(q: &Quarter) -> String {
    let mut out = String::from("## Goals\n");
    if q.front.goals.is_empty() {
        out.push_str("- _(none yet — `bn quarter add \"<goal>\" --key <key>`)_\n");
    }
    for g in &q.front.goals {
        out.push_str(&goal_line(g));
        out.push('\n');
    }
    if !q.front.reviews.is_empty() {
        out.push_str("\n## Impact reviews\n");
        for r in &q.front.reviews {
            out.push('\n');
            out.push_str(&render_review(r));
        }
    }
    out
}

/// One goal's checkbox line: `- [x] <text> (key: <key>, metric: <metric>)`.
fn goal_line(g: &QuarterGoal) -> String {
    let check = if g.done { "x" } else { " " };
    let mut meta = format!("key: {}", g.key);
    if let Some(m) = &g.metric {
        meta.push_str(&format!(", metric: {m}"));
    }
    format!("- [{check}] {} ({meta})", g.text)
}

/// One review rendered as a dated block for the file body / `bn quarter` view.
fn render_review(r: &ImpactReview) -> String {
    let mut out = format!("### {} ({}..{})\n", r.date, r.window_start, r.window_end);
    for a in &r.assessments {
        out.push_str(&format!("- {}: {} — {}\n", a.key, a.status, a.evidence));
    }
    if !r.ranking.is_empty() {
        out.push_str("\nImpact ranking:\n");
        for (i, item) in r.ranking.iter().enumerate() {
            out.push_str(&format!("{}. {item}\n", i + 1));
        }
    }
    out.push_str(&format!("\nRecommendation: {}\n", r.recommendation));
    out
}

/// Ensure the current quarter's file exists, returning it. Unlike daily there is **no
/// carry-forward** — quarter goals are deliberately set, not inherited — so a fresh quarter opens
/// empty. Idempotent: re-opening an existing quarter re-seeds nothing and writes nothing.
pub fn ensure_current(cfg: &Config, today: &str) -> Result<Quarter, StoreError> {
    let qid = quarter_id(today).ok_or_else(|| StoreError::Parse(format!("bad date {today}")))?;
    let path = cfg.quarter_file(&qid);
    if path.exists() {
        return read(&path);
    }
    let q = Quarter::empty(&qid, today);
    write(&q, &path)?;
    Ok(q)
}

/// Read a specific quarter (`bn quarter <YYYY-Qn>`). `Ok(None)` when the file doesn't exist yet.
pub fn read_quarter(cfg: &Config, qid: &str) -> Result<Option<Quarter>, CmdError> {
    if !is_qid(qid) {
        return Err(CmdError::InvalidDate(qid.to_string()));
    }
    let path = cfg.quarter_file(qid);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(read(&path)?))
}

// ---- Mutations -----------------------------------------------------------------------------

/// Outcome of adding a goal.
#[derive(Debug)]
pub struct AddOutcome {
    pub key: String,
    pub text: String,
    pub quarter: String,
}

/// Outcome of marking a goal done.
#[derive(Debug)]
pub struct DoneOutcome {
    pub key: String,
    pub text: String,
}

/// Add a goal to the current quarter. `key` is used verbatim when given (slugified), else derived
/// from the text; a duplicate key is rejected so tags stay unambiguous. Persists and returns the
/// stable key the goal now lives under.
pub fn add(
    cfg: &Config,
    today: &str,
    text: &str,
    key: Option<&str>,
    metric: Option<&str>,
) -> Result<AddOutcome, CmdError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(CmdError::EmptyText);
    }
    let mut q = ensure_current(cfg, today)?;
    let key = match key.map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) => slugify(k),
        None => unique_key(&q, &slugify(text)),
    };
    if q.front.goals.iter().any(|g| g.key == key) {
        return Err(CmdError::Duplicate(key));
    }
    q.front.goals.push(QuarterGoal {
        key: key.clone(),
        text: text.to_string(),
        created: today.to_string(),
        done: false,
        done_at: None,
        metric: metric
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string),
    });
    write(&q, &cfg.quarter_file(&q.front.quarter))?;
    Ok(AddOutcome {
        key,
        text: text.to_string(),
        quarter: q.front.quarter,
    })
}

/// Mark a goal done by exact key or a case-insensitive substring over open goals' key/text.
/// Errors on zero or multiple matches (mirrors `daily::done`).
pub fn done(cfg: &Config, today: &str, query: &str) -> Result<DoneOutcome, CmdError> {
    let mut q = ensure_current(cfg, today)?;
    let query = query.trim();
    let idx = if let Some(i) = q.front.goals.iter().position(|g| g.key == query) {
        i
    } else {
        let ql = query.to_lowercase();
        let matches: Vec<usize> = q
            .front
            .goals
            .iter()
            .enumerate()
            .filter(|(_, g)| {
                !g.done
                    && (g.key.to_lowercase().contains(&ql) || g.text.to_lowercase().contains(&ql))
            })
            .map(|(i, _)| i)
            .collect();
        match matches.len() {
            0 => return Err(CmdError::NotFound(query.to_string())),
            1 => matches[0],
            _ => {
                let keys = matches
                    .iter()
                    .map(|&i| q.front.goals[i].key.clone())
                    .collect();
                return Err(CmdError::Ambiguous(query.to_string(), keys));
            }
        }
    };
    if q.front.goals[idx].done {
        return Err(CmdError::AlreadyDone(q.front.goals[idx].key.clone()));
    }
    q.front.goals[idx].done = true;
    q.front.goals[idx].done_at = Some(today.to_string());
    let (key, text) = (
        q.front.goals[idx].key.clone(),
        q.front.goals[idx].text.clone(),
    );
    write(&q, &cfg.quarter_file(&q.front.quarter))?;
    Ok(DoneOutcome { key, text })
}

/// Slugify free text into a stable, greppable key: lowercase, non-alphanumerics → `-`, collapsed,
/// trimmed, capped. Empty input (e.g. all punctuation) falls back to `goal`.
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let slug: String = out.trim_matches('-').chars().take(40).collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "goal".to_string()
    } else {
        slug
    }
}

/// A key not already present, disambiguating collisions with a numeric suffix (`ship`, `ship-2`).
fn unique_key(q: &Quarter, base: &str) -> String {
    if !q.front.goals.iter().any(|g| g.key == base) {
        return base.to_string();
    }
    let mut suffix = 2u64;
    loop {
        let key = format!("{base}-{suffix}");
        if !q.front.goals.iter().any(|goal| goal.key == key) {
            return key;
        }
        suffix = suffix.saturating_add(1);
    }
}

// ---- Live view -----------------------------------------------------------------------------

/// The one-line header: `Q3 · 41% elapsed · 2/5 goals on track`.
pub fn header(q: &Quarter, today: &str) -> String {
    let short = q
        .front
        .quarter
        .rsplit_once('-')
        .map(|(_, s)| s.to_string())
        .unwrap_or_else(|| q.front.quarter.clone());
    let pct = (elapsed_fraction(&q.front.quarter, today).unwrap_or(0.0) * 100.0).round() as i64;
    format!(
        "{short} · {pct}% elapsed · {}/{} goals on track",
        q.on_track_count(),
        q.front.goals.len()
    )
}

/// The live human view: header + goals with status + the latest reviews.
pub fn render(q: &Quarter, today: &str) -> String {
    let mut out = format!("{}\n\n", header(q, today));
    if q.front.goals.is_empty() {
        out.push_str("No quarter goals yet — `bn quarter add \"<goal>\" --key <key>`\n");
        return out;
    }
    for g in &q.front.goals {
        let check = if g.done { "x" } else { " " };
        let status = q.status_of(&g.key);
        out.push_str(&format!(
            "- [{check}] {} [{}] (key: {}",
            g.text, status, g.key
        ));
        if let Some(m) = &g.metric {
            out.push_str(&format!(", metric: {m}"));
        }
        out.push_str(")\n");
    }
    if let Some(r) = q.latest_review() {
        out.push_str(&format!("\nLatest impact review — {}\n", r.date));
        out.push_str(&format!("  Recommendation: {}\n", r.recommendation));
    }
    out
}

// ---- The impact agent ----------------------------------------------------------------------

/// A completed daily goal that was tagged to a quarter goal, gathered for the review packet.
#[derive(Debug, Clone, PartialEq)]
pub struct TaggedWork {
    pub text: String,
    pub q: String,
    pub date: String,
}

/// The read-only context handed to the impact agent: the quarter goals, prior reviews (for
/// continuity), the window's ground truth (brag packet: merged PRs, completed goals, achievements),
/// and the explicitly `--q`-tagged work completed in the window.
#[derive(Debug, Clone)]
pub struct ReviewContext {
    pub quarter: String,
    pub window_start: String,
    pub window_end: String,
    pub goals: Vec<QuarterGoal>,
    pub prior_reviews: Vec<ImpactReview>,
    pub ground_truth: String,
    pub tagged_work: Vec<TaggedWork>,
}

/// The window start for a review: the most recent prior review's date (so windows tile without
/// gaps), else `window_days` back from today — clamped to the quarter start (a review never reads
/// before its own quarter began).
fn window_start(q: &Quarter, today: &str, window_days: i64) -> String {
    let default = util::add_days(today, -window_days.max(1)).unwrap_or_else(|| today.to_string());
    let start = q
        .latest_review()
        .map(|r| r.window_end.clone())
        .unwrap_or(default);
    match quarter_bounds(&q.front.quarter) {
        Some((qstart, _)) if start.as_str() < qstart.as_str() => qstart,
        _ => start,
    }
}

/// Gather the impact packet as of `today`. gh/git reads (via brag) go through `runner`, so this is
/// deterministic under `FakeRunner`.
pub fn gather_context(
    cfg: &Config,
    q: &Quarter,
    today: &str,
    window_days: i64,
    runner: &dyn CommandRunner,
) -> ReviewContext {
    let start = window_start(q, today, window_days);
    let ground_truth =
        crate::brag::render_markdown(&crate::brag::brag(cfg, today, Some(&start), runner));
    let mut tagged_work = Vec::new();
    for day in crate::daily::read_window(cfg, &start, today) {
        for g in day.completed() {
            if let Some(tag) = g.q.as_deref().filter(|s| !s.is_empty()) {
                tagged_work.push(TaggedWork {
                    text: g.text.clone(),
                    q: tag.to_string(),
                    date: g.done_at.clone().unwrap_or_else(|| day.front.date.clone()),
                });
            }
        }
    }
    ReviewContext {
        quarter: q.front.quarter.clone(),
        window_start: start,
        window_end: today.to_string(),
        goals: q.front.goals.clone(),
        prior_reviews: q.front.reviews.clone(),
        ground_truth,
        tagged_work,
    }
}

/// The pinned agent prompt (`prompts/quarter-review.md`), baked in at compile time so the review
/// has a single source of truth and no runtime file dependency. Frontmatter is stripped.
pub fn pinned_prompt() -> &'static str {
    const RAW: &str = include_str!("../../../config/prompts/quarter-review.md");
    match crate::store::split_frontmatter(RAW) {
        Some((_, body)) => body.trim_start(),
        None => RAW,
    }
}

/// Build the full prompt: the pinned instruction followed by the read-only packet.
pub fn build_prompt(ctx: &ReviewContext) -> String {
    let mut goals = String::new();
    for g in &ctx.goals {
        goals.push_str(&format!("- {}: {}", g.key, g.text));
        if let Some(m) = &g.metric {
            goals.push_str(&format!(" [metric: {m}]"));
        }
        if g.done {
            goals.push_str(" [already marked done]");
        }
        goals.push('\n');
    }
    if goals.is_empty() {
        goals.push_str("(no goals)\n");
    }

    let prev = if ctx.prior_reviews.is_empty() {
        "(none)\n".to_string()
    } else {
        let mut s = String::new();
        for r in ctx.prior_reviews.iter().take(3) {
            s.push_str(&render_review(r));
            s.push('\n');
        }
        s
    };

    let tagged = if ctx.tagged_work.is_empty() {
        "(no explicitly-tagged work this window)\n".to_string()
    } else {
        let mut s = String::new();
        for w in &ctx.tagged_work {
            s.push_str(&format!("- [{}] {} (→ {})\n", w.date, w.text, w.q));
        }
        s
    };

    format!(
        "{prompt}\n\n--- INPUT ---\n## Quarter {q} (window {ws}..{we})\n\n### Goals\n{goals}\n### \
         Previous reviews (for continuity)\n{prev}\n### Tagged work this window\n{tagged}\n### \
         Ground truth (merged PRs, completed goals, achievements)\n{ground}\n--- END INPUT ---\n",
        prompt = pinned_prompt(),
        q = ctx.quarter,
        ws = ctx.window_start,
        we = ctx.window_end,
        ground = ctx.ground_truth,
    )
}

/// Normalise a status token: lowercase, spaces/underscores → `-`. Returns the canonical status
/// when recognised, else `None`.
fn normalize_status(s: &str) -> Option<String> {
    let norm: String = s
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c == ' ' || c == '_' { '-' } else { c })
        .collect();
    STATUSES.contains(&norm.as_str()).then_some(norm)
}

/// Split an assessment's remainder into `(status, evidence)` on the first status-vs-evidence
/// separator (em dash, `--`, ` - `, or `:`). Tolerant of the exact separator the agent used.
fn split_status_evidence(rest: &str) -> (String, String) {
    for sep in ["—", "--", " - ", ":"] {
        if let Some((a, b)) = rest.split_once(sep) {
            return (a.trim().to_string(), b.trim().to_string());
        }
    }
    // No separator — the whole remainder is the status token (evidence empty).
    (rest.trim().to_string(), String::new())
}

/// Parse and **validate** the agent's output against the pinned format. Enforces: a `PER-GOAL`
/// section with ≥1 assessment; every assessed key exists in `valid_keys` (no hallucinated goals);
/// every status is one of [`STATUSES`]; and a non-empty `RECOMMENDATION`. On any failure returns
/// `Err(reason)` — the caller appends nothing and raises a warn attention item.
pub fn parse_review(
    text: &str,
    valid_keys: &HashSet<String>,
    date: &str,
    window_start: &str,
    window_end: &str,
) -> Result<ImpactReview, String> {
    #[derive(PartialEq)]
    enum Sec {
        None,
        PerGoal,
        Ranking,
        Recommendation,
    }
    let header_of = |line: &str| -> Option<Sec> {
        match line.trim().to_uppercase().replace(' ', "-").as_str() {
            "PER-GOAL" | "PERGOAL" => Some(Sec::PerGoal),
            "RANKING" | "IMPACT-RANKING" => Some(Sec::Ranking),
            "RECOMMENDATION" | "RECOMMENDATIONS" => Some(Sec::Recommendation),
            _ => None,
        }
    };

    let mut sec = Sec::None;
    let mut assessments: Vec<GoalAssessment> = Vec::new();
    let mut ranking: Vec<String> = Vec::new();
    let mut rec_lines: Vec<String> = Vec::new();

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(h) = header_of(line) {
            sec = h;
            continue;
        }
        match sec {
            Sec::PerGoal => {
                let body = line.trim_start_matches(['-', '*', '•']).trim();
                let Some((key, rest)) = body.split_once(':') else {
                    continue; // prose line — tolerate and skip
                };
                let key = key.trim().to_string();
                let (status_raw, evidence) = split_status_evidence(rest);
                let Some(status) = normalize_status(&status_raw) else {
                    continue; // not a status line — skip
                };
                if !valid_keys.contains(&key) {
                    return Err(format!("assessed unknown goal key '{key}'"));
                }
                assessments.push(GoalAssessment {
                    key,
                    status,
                    evidence,
                });
            }
            Sec::Ranking => {
                let item = line
                    .trim_start_matches(|c: char| {
                        c.is_ascii_digit() || c == '.' || c == '-' || c == '*' || c == ' '
                    })
                    .trim();
                if !item.is_empty() {
                    ranking.push(item.to_string());
                }
            }
            Sec::Recommendation => rec_lines.push(line.to_string()),
            Sec::None => {}
        }
    }

    if assessments.is_empty() {
        return Err("no valid PER-GOAL assessments found".to_string());
    }
    let recommendation = rec_lines.join(" ").trim().to_string();
    if recommendation.is_empty() {
        return Err("missing RECOMMENDATION".to_string());
    }
    Ok(ImpactReview {
        date: date.to_string(),
        window_start: window_start.to_string(),
        window_end: window_end.to_string(),
        assessments,
        ranking,
        recommendation,
    })
}

/// Append a validated review to the quarter file (newest first) and persist.
pub fn append_review(cfg: &Config, qid: &str, review: ImpactReview) -> Result<(), StoreError> {
    let path = cfg.quarter_file(qid);
    let mut q = if path.exists() {
        read(&path)?
    } else {
        Quarter::empty(qid, &review.date)
    };
    q.front.reviews.insert(0, review);
    write(&q, &path)
}

/// The attention items an appended review raises: one per at-risk (`warn`) / off-track (`urgent`)
/// goal, keyed by goal key (`attn:quarter:<key>`) so a later review that clears the risk resolves
/// it via ingest's `replace`.
pub fn attn_items(qid: &str, review: &ImpactReview) -> Vec<crate::attention::IngestItem> {
    review
        .assessments
        .iter()
        .filter_map(|a| {
            let severity = match a.status.as_str() {
                "off-track" => "urgent",
                "at-risk" => "warn",
                _ => return None,
            };
            Some(crate::attention::IngestItem {
                title: format!("Quarter {qid}: goal '{}' {}", a.key, a.status),
                severity: Some(severity.to_string()),
                source: Some("quarter".to_string()),
                key: Some(a.key.clone()),
                link: None,
                repo: None,
                branch: None,
                detail: Some(a.evidence.clone()),
            })
        })
        .collect()
}

/// What [`review`] did.
#[derive(Debug)]
pub enum ReviewOutcome {
    /// The agent's output validated: the review was appended and `raised` attention items raised.
    Appended { review: ImpactReview, raised: usize },
    /// The agent produced output that failed validation: nothing was appended and a warn
    /// attention item was raised. Carries the reason.
    Malformed { reason: String },
    /// Nothing to do / agent unavailable — no goals set, or no harness/output. No attn raised.
    Skipped { reason: String },
}

/// Run the impact agent for the current quarter: gather context → prompt the harness → validate →
/// append + raise attn (or raise a warn attn on malformed). `harness` picks the CLI (`copilot` by
/// default, `claude` when set); `run_agent` shells through `runner` so it's testable. `window_days`
/// bounds the ground-truth window when there's no prior review.
pub fn review(
    cfg: &Config,
    runner: &dyn CommandRunner,
    harness: Option<&str>,
    today: &str,
    window_days: i64,
) -> Result<ReviewOutcome, StoreError> {
    let q = ensure_current(cfg, today)?;
    if q.front.goals.is_empty() {
        return Ok(ReviewOutcome::Skipped {
            reason: "no quarter goals set".to_string(),
        });
    }
    let qid = q.front.quarter.clone();
    let ctx = gather_context(cfg, &q, today, window_days, runner);
    let prompt = build_prompt(&ctx);
    let Some(output) = run_agent(runner, harness.unwrap_or("copilot"), &prompt) else {
        return Ok(ReviewOutcome::Skipped {
            reason: "impact agent unavailable or produced no output".to_string(),
        });
    };

    let valid_keys: HashSet<String> = q.front.goals.iter().map(|g| g.key.clone()).collect();
    match parse_review(
        &output,
        &valid_keys,
        today,
        &ctx.window_start,
        &ctx.window_end,
    ) {
        Ok(review) => {
            append_review(cfg, &qid, review.clone())?;
            let items = attn_items(&qid, &review);
            let raised = crate::attention::ingest(&items, "quarter", "warn", true, today)
                .map(|o| o.raised)
                .unwrap_or(0);
            Ok(ReviewOutcome::Appended { review, raised })
        }
        Err(reason) => {
            util::best_effort(
                crate::attention::add_with_detail(
                    "quarter",
                    &format!(
                        "Quarter {qid} review: agent output failed validation — nothing recorded"
                    ),
                    "warn",
                    None,
                    None,
                    None,
                    Some("review-malformed"),
                    today,
                    Some(&reason),
                ),
                "quarter: page attention for malformed review",
            );
            Ok(ReviewOutcome::Malformed { reason })
        }
    }
}

/// Shell the pinned prompt through the chosen headless harness, returning its trimmed stdout.
/// `copilot -p <prompt> --model auto --no-color` (mirrors `jobs/lib/prompt.sh`) or `claude -p
/// <prompt>`. `None` when the binary is absent / fails / emits nothing, or when `BN_PROMPT_AI=0`.
fn run_agent(runner: &dyn CommandRunner, harness: &str, prompt: &str) -> Option<String> {
    if std::env::var("BN_PROMPT_AI").ok().as_deref() == Some("0") {
        return None;
    }
    let out = match harness {
        "claude" | "claudecode" => runner.run("claude", &["-p", prompt]),
        _ => runner.run("copilot", &["-p", prompt, "--model", "auto", "--no-color"]),
    };
    out.ok_stdout()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;
    use crate::test_support::{config, with_state_and_notes_dir};
    use std::collections::HashSet;

    fn keys(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn quarter_id_and_bounds_are_calendar_correct() {
        assert_eq!(quarter_id("2026-01-15").as_deref(), Some("2026-Q1"));
        assert_eq!(quarter_id("2026-07-05").as_deref(), Some("2026-Q3"));
        assert_eq!(quarter_id("2026-12-31").as_deref(), Some("2026-Q4"));
        assert_eq!(quarter_id("not-a-date"), None);
        assert_eq!(
            quarter_bounds("2026-Q3"),
            Some(("2026-07-01".into(), "2026-09-30".into()))
        );
        assert_eq!(
            quarter_bounds("2026-Q1"),
            Some(("2026-01-01".into(), "2026-03-31".into()))
        );
        assert_eq!(parse_qid("2026-Q5"), None, "quarter must be 1..=4");
        assert!(!is_qid("2026-13"));
    }

    #[test]
    fn elapsed_fraction_clamps_to_the_quarter() {
        assert_eq!(elapsed_fraction("2026-Q3", "2026-07-01"), Some(0.0));
        assert_eq!(elapsed_fraction("2026-Q3", "2026-09-30"), Some(1.0));
        assert_eq!(elapsed_fraction("2026-Q3", "2026-06-01"), Some(0.0)); // before
        assert_eq!(elapsed_fraction("2026-Q3", "2026-12-01"), Some(1.0)); // after
        let mid = elapsed_fraction("2026-Q3", "2026-08-15").unwrap();
        assert!((0.4..0.6).contains(&mid), "mid-quarter ≈ half: {mid}");
    }

    #[test]
    fn ensure_current_auto_creates_empty_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let c = config(tmp.path());
        let q = ensure_current(&c, "2026-07-05").unwrap();
        assert_eq!(q.front.quarter, "2026-Q3");
        assert!(q.front.goals.is_empty());
        assert!(c.quarter_file("2026-Q3").exists());
        // Re-open a later day in the same quarter: nothing changes.
        let again = ensure_current(&c, "2026-08-01").unwrap();
        assert_eq!(again.front.quarter, "2026-Q3");
    }

    #[test]
    fn add_and_done_roundtrip_with_stable_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let c = config(tmp.path());
        let a = add(
            &c,
            "2026-07-05",
            "Ship bn v1.0 public release",
            Some("ship-v1"),
            None,
        )
        .unwrap();
        assert_eq!(a.key, "ship-v1");
        // Derived key from text when none given.
        let b = add(
            &c,
            "2026-07-05",
            "Reduce PR cycle time",
            None,
            Some("pr_cycle<-30%"),
        )
        .unwrap();
        assert_eq!(b.key, "reduce-pr-cycle-time");
        // Duplicate key rejected.
        assert!(matches!(
            add(&c, "2026-07-05", "again", Some("ship-v1"), None),
            Err(CmdError::Duplicate(_))
        ));
        // Persisted + greppable key in the file.
        let text = std::fs::read_to_string(c.quarter_file("2026-Q3")).unwrap();
        assert!(text.contains("key: ship-v1"));
        assert!(text.contains("metric: pr_cycle<-30%"));

        let d = done(&c, "2026-07-06", "ship-v1").unwrap();
        assert_eq!(d.key, "ship-v1");
        let q = ensure_current(&c, "2026-07-06").unwrap();
        assert_eq!(q.status_of("ship-v1"), "done");
        assert!(matches!(
            done(&c, "2026-07-06", "ship-v1"),
            Err(CmdError::AlreadyDone(_))
        ));
    }

    #[test]
    fn header_reports_elapsed_and_on_track() {
        let tmp = tempfile::tempdir().unwrap();
        let c = config(tmp.path());
        add(&c, "2026-07-05", "goal a", Some("a"), None).unwrap();
        add(&c, "2026-07-05", "goal b", Some("b"), None).unwrap();
        done(&c, "2026-07-05", "a").unwrap();
        let q = ensure_current(&c, "2026-07-05").unwrap();
        let h = header(&q, "2026-09-30");
        assert!(
            h.starts_with("Q3 · 100% elapsed · 1/2 goals on track"),
            "{h}"
        );
    }

    #[test]
    fn parse_review_accepts_the_pinned_format() {
        let out = "PER-GOAL\nship-v1: on-track — merged the release PR\npr-cycle: at-risk — no cycle-time work landed\n\nRANKING\n1. release PR :: ship-v1\n2. refactor :: none\n\nRECOMMENDATION\nMove one engineer from the refactor onto pr-cycle.";
        let r = parse_review(
            out,
            &keys(&["ship-v1", "pr-cycle"]),
            "2026-07-05",
            "2026-06-28",
            "2026-07-05",
        )
        .unwrap();
        assert_eq!(r.assessments.len(), 2);
        assert_eq!(r.assessments[0].status, "on-track");
        assert_eq!(r.assessments[1].status, "at-risk");
        assert_eq!(r.ranking.len(), 2);
        assert!(r.recommendation.contains("pr-cycle"));
    }

    #[test]
    fn parse_review_rejects_malformed_output() {
        let vk = keys(&["ship-v1"]);
        // No PER-GOAL section at all.
        assert!(parse_review("just some prose about the quarter", &vk, "d", "s", "e").is_err());
        // Hallucinated goal key.
        let bad_key = "PER-GOAL\nnope: on-track — x\n\nRECOMMENDATION\ndo the thing";
        assert!(parse_review(bad_key, &vk, "d", "s", "e").is_err());
        // Missing recommendation.
        let no_rec = "PER-GOAL\nship-v1: on-track — shipped";
        assert!(parse_review(no_rec, &vk, "d", "s", "e").is_err());
        // Unknown status → not counted → no valid assessments → error.
        let bad_status = "PER-GOAL\nship-v1: sideways — meh\n\nRECOMMENDATION\ndo it";
        assert!(parse_review(bad_status, &vk, "d", "s", "e").is_err());
    }

    #[test]
    fn review_appends_and_raises_attn_on_valid_output() {
        let tmp = tempfile::tempdir().unwrap();
        with_state_and_notes_dir(tmp.path(), "testhost", || {
            let c = config(tmp.path());
            add(&c, "2026-07-05", "Ship v1", Some("ship-v1"), None).unwrap();
            add(&c, "2026-07-05", "Cycle time", Some("pr-cycle"), None).unwrap();
            let agent_out = "PER-GOAL\nship-v1: on-track — release PR merged\npr-cycle: off-track — nothing landed\n\nRECOMMENDATION\nReallocate to pr-cycle.";
            let runner = FakeRunner::new().on_program("copilot", agent_out);
            let outcome = review(&c, &runner, Some("copilot"), "2026-07-06", 7).unwrap();
            match outcome {
                ReviewOutcome::Appended { review, raised } => {
                    assert_eq!(review.assessments.len(), 2);
                    assert_eq!(raised, 1, "one off-track goal → one attn item");
                }
                other => panic!("expected Appended, got {other:?}"),
            }
            // Review persisted to the file, newest first.
            let q = ensure_current(&c, "2026-07-06").unwrap();
            assert_eq!(q.front.reviews.len(), 1);
            assert_eq!(q.status_of("pr-cycle"), "off-track");
            // Attention item raised under the quarter source.
            let open = crate::attention::open_items("2026-07-06");
            assert!(open
                .iter()
                .any(|i| i.source == "quarter" && i.title.contains("pr-cycle")));
        });
    }

    #[test]
    fn review_raises_warn_and_appends_nothing_on_malformed_output() {
        let tmp = tempfile::tempdir().unwrap();
        with_state_and_notes_dir(tmp.path(), "testhost", || {
            let c = config(tmp.path());
            add(&c, "2026-07-05", "Ship v1", Some("ship-v1"), None).unwrap();
            let runner =
                FakeRunner::new().on_program("copilot", "the quarter is going fine, trust me");
            let outcome = review(&c, &runner, Some("copilot"), "2026-07-06", 7).unwrap();
            assert!(matches!(outcome, ReviewOutcome::Malformed { .. }));
            // Nothing appended.
            let q = ensure_current(&c, "2026-07-06").unwrap();
            assert!(q.front.reviews.is_empty());
            // A warn attention item was raised.
            let open = crate::attention::open_items("2026-07-06");
            assert!(open
                .iter()
                .any(|i| i.source == "quarter" && i.severity == "warn"));
        });
    }

    #[test]
    fn review_skips_when_no_goals() {
        let tmp = tempfile::tempdir().unwrap();
        with_state_and_notes_dir(tmp.path(), "testhost", || {
            let c = config(tmp.path());
            let runner = FakeRunner::new()
                .on_program("copilot", "PER-GOAL\nx: on-track — y\n\nRECOMMENDATION\nz");
            let outcome = review(&c, &runner, Some("copilot"), "2026-07-06", 7).unwrap();
            assert!(matches!(outcome, ReviewOutcome::Skipped { .. }));
        });
    }

    #[test]
    fn gather_context_collects_tagged_daily_work() {
        let tmp = tempfile::tempdir().unwrap();
        let c = config(tmp.path());
        add(&c, "2026-07-01", "Ship v1", Some("ship-v1"), None).unwrap();
        // A tagged daily goal completed in-window.
        crate::daily::add_tagged(
            &c,
            "2026-07-02",
            "wrote release notes",
            None,
            Some("ship-v1".into()),
            None,
        )
        .unwrap();
        crate::daily::done(&c, "2026-07-02", "release notes", None).unwrap();
        let q = ensure_current(&c, "2026-07-05").unwrap();
        let ctx = gather_context(&c, &q, "2026-07-05", 7, &FakeRunner::new());
        assert!(ctx
            .tagged_work
            .iter()
            .any(|w| w.q == "ship-v1" && w.text.contains("release notes")));
    }

    #[test]
    fn pinned_prompt_strips_frontmatter() {
        let p = pinned_prompt();
        assert!(!p.starts_with("---"), "frontmatter must be stripped");
        assert!(
            p.contains("PER-GOAL"),
            "prompt must carry the output contract"
        );
    }
}
