//! The Chief of Staff (#257) — the glue agent's twice-daily briefing card.
//!
//! Seven agents producing cards is noise unless one voice synthesizes them. Deterministic and
//! offline: a cron fires, this folds what the system already knows — the night shift, `bn next`,
//! the calendar cache, the quarter, the `q` inbox, yesterday's daily — into the *one card you
//! read*. Morning (05:30) starts the day with the decision made; evening (16:45) shuts down
//! cleanly with tomorrow's first task pre-chosen.
//!
//! Split of concerns: this module is the pure `Packet → Card → markdown` core — no IO, fully
//! unit-testable. The CLI (`bn chief`, the `staff-chief-{am,pm}` native jobs) gathers the packet
//! from the live stores, applies the card's side-effects (idempotent review-night-PR daily goals,
//! tomorrow's first task), and mirrors the card to the feed.
//!
//! Severity discipline (an acceptance criterion of #257, enforced by the type — not convention): a
//! [`CardItem`] can only be [`Severity::Info`] or [`Severity::Warn`]. There is no urgent variant,
//! so the Chief's card can never be urgent — it summarizes; urgency belongs to the underlying
//! sources, so `bn next` ranking stays honest.

use serde::Serialize;

use crate::resolver::Config;

/// Which of the two daily runs a card is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    /// The 05:30 morning brief.
    Am,
    /// The 16:45 evening shutdown.
    Pm,
}

impl Slot {
    /// The agent name a card is filed under (also its feed source): `chief-am` / `chief-pm`.
    pub fn agent(self) -> &'static str {
        match self {
            Slot::Am => "chief-am",
            Slot::Pm => "chief-pm",
        }
    }

    /// The human title of the run.
    pub fn label(self) -> &'static str {
        match self {
            Slot::Am => "morning brief",
            Slot::Pm => "evening shutdown",
        }
    }
}

// ---- the gathered packet -------------------------------------------------------------------

/// A draft PR the night shift left waiting for review — the AM card's "review the night's work".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NightPr {
    pub repo: String,
    /// Compact slug (`bn#245`) for display.
    pub slug: String,
    pub link: String,
}

/// One of `bn next`'s top ranked actions, flattened to what the card needs. `source` is the
/// traceable origin label ("review", "goal", "cron", …) so each headline item points back home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TopAction {
    pub title: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// Today's calendar shape from the meetings feed cache — best-effort, so every field past the
/// count is optional (a source that can't be parsed degrades, it never errors).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CalendarShape {
    pub meetings: usize,
    /// The day's first meeting time (`HH:MM`), when at least one could be parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_meeting: Option<String>,
    /// The biggest gap between consecutive meeting start times (`HH:MM–HH:MM`), when at least two
    /// timed meetings could be parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_gap: Option<String>,
}

/// Quarter progress at a glance (elapsed vs. on-track, plus the top at-risk goal).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuarterStatus {
    pub qid: String,
    pub elapsed_pct: u8,
    pub on_track: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_risk: Option<String>,
}

/// The `q` inbox at a glance (count + oldest item's age in days).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InboxStatus {
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_age_days: Option<i64>,
}

/// A daily rollup — yesterday's for the AM card, today's for the PM card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DailySummary {
    pub done: usize,
    pub open: usize,
    pub done_titles: Vec<String>,
    /// Still-open goals, in order — PM uses the head as the drop/defer candidate and, failing a
    /// better signal, as tomorrow's first task.
    pub open_titles: Vec<String>,
}

/// The compact end-of-day wrap folded into the PM shutdown from the retired `wrap-digest` surface
/// (#274). Only the actionable tails the shutdown card didn't already carry: what shipped (merged
/// PRs → `bn close`) and loose commits the note didn't capture (→ `bn sync`). The noisier
/// branches-touched list the standalone digest printed is deliberately dropped — one voice, not a
/// wall — since the merged PRs already say what moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WrapSummary {
    /// Count of today's commits the current note didn't capture (→ `bn sync`).
    pub unnoted_commits: usize,
    /// PRs merged today (→ `bn close`).
    pub merged_prs: Vec<WrapPr>,
}

/// A PR merged today, flattened to what the PM card's "what shipped" line needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WrapPr {
    /// Compact display label, e.g. `#245 add the widget`.
    pub label: String,
    pub url: String,
}

impl WrapSummary {
    /// True when the day produced nothing worth wrapping — the gather drops it to `None` so the
    /// PM card omits the section entirely.
    pub fn is_empty(&self) -> bool {
        self.unnoted_commits == 0 && self.merged_prs.is_empty()
    }
}

/// The deterministic packet gathered for a run. Every source past `date`/`slot` is optional (or an
/// empty vec) so a missing source omits its card section rather than failing the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Packet {
    pub date: String,
    pub slot: Slot,
    /// Draft PRs from the night shift waiting on review (AM).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub night: Vec<NightPr>,
    /// The top `bn next` actions (already ranked; the card shows the first three).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<TopAction>,
    /// Momentum context — the open-punch repo/branch, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub momentum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calendar: Option<CalendarShape>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quarter: Option<QuarterStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inbox: Option<InboxStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daily: Option<DailySummary>,
    /// The paste-ready standup line, folded into the AM brief from the retired `standup-push`
    /// surface (#274). AM only; `None` unless `[standup].enabled` and the day has signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standup: Option<String>,
    /// The end-of-day wrap, folded into the PM shutdown from the retired `wrap-digest` surface
    /// (#274). PM only; `None` unless `[wrap].enabled` and the day produced work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrap: Option<WrapSummary>,
}

impl Packet {
    /// An empty packet for `date`/`slot` — the gather fills in what it finds.
    pub fn new(date: impl Into<String>, slot: Slot) -> Self {
        Packet {
            date: date.into(),
            slot,
            night: Vec::new(),
            actions: Vec::new(),
            momentum: None,
            calendar: None,
            quarter: None,
            inbox: None,
            daily: None,
            standup: None,
            wrap: None,
        }
    }
}

// ---- the briefing card ---------------------------------------------------------------------

/// Card-item severity — deliberately only `Info`/`Warn` (see the module note on severity
/// discipline). Serializes to `"info"`/`"warn"`; there is no `"urgent"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
}

/// What a card item represents — mirrors the (still-unbuilt #256) briefing-card `kind`, so a later
/// migration onto the staff framework is a lift, not a rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemKind {
    /// A `bn daily` goal the card drives (review-night-PR goals; tomorrow's first task).
    Goal,
    /// A traceable next action.
    Action,
    /// An informational risk/context line (at-risk quarter goal, aging inbox).
    Risk,
}

/// One structured line of a card — the machine-readable counterpart to the markdown body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CardItem {
    pub kind: ItemKind,
    pub severity: Severity,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// The one card — a headline, a rendered markdown body, and structured items. Shaped to the #256
/// briefing card so `bn feed` / the nvim + tmux surfaces can consume it unchanged when the
/// framework lands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Card {
    pub agent: String,
    pub date: String,
    pub slot: Slot,
    pub headline: String,
    pub body_md: String,
    pub items: Vec<CardItem>,
}

/// The one item the PM card pre-decides as tomorrow's first task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TomorrowTask {
    pub text: String,
    pub link: Option<String>,
}

// ---- card builders (pure) ------------------------------------------------------------------

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// A one-line summary of the calendar shape for a headline clause.
fn calendar_phrase(c: &CalendarShape) -> String {
    match (c.meetings, &c.free_gap) {
        (0, _) => "no meetings".to_string(),
        (n, Some(gap)) => format!("{n} meeting{} · free {gap}", plural(n)),
        (n, None) => format!("{n} meeting{}", plural(n)),
    }
}

/// Append `## <title>` + `- <line>` rows to `body`, but only when there's something to show — an
/// empty section is dropped so a card only carries the sources that were present.
fn push_section(body: &mut String, title: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    if !body.is_empty() {
        body.push('\n');
    }
    body.push_str(&format!("## {title}\n"));
    for l in lines {
        body.push_str(&format!("- {l}\n"));
    }
}

/// The morning brief: the day in one sentence, the top three (each traceable), the night's review
/// queue, and risks. `build_am_card` is pure — the CLI applies the side-effects it implies.
pub fn build_am_card(p: &Packet) -> Card {
    let headline = am_headline(p);
    let mut items = Vec::new();
    let mut body = String::new();

    // Top 3 — each traceable to its source.
    let mut top_lines = Vec::new();
    for a in p.actions.iter().take(3) {
        top_lines.push(format!("{} ({})", a.title, a.source));
        items.push(CardItem {
            kind: ItemKind::Action,
            severity: Severity::Info,
            title: a.title.clone(),
            link: a.link.clone(),
        });
    }
    push_section(&mut body, "Top 3", &top_lines);

    // Review the night's work — one line (and one daily goal) per draft PR.
    let mut night_lines = Vec::new();
    for pr in &p.night {
        let text = format!("review night PR {}", pr.slug);
        night_lines.push(format!("{text} → {}", pr.link));
        items.push(CardItem {
            kind: ItemKind::Goal,
            severity: Severity::Info,
            title: text,
            link: Some(pr.link.clone()),
        });
    }
    push_section(&mut body, "Review the night's work", &night_lines);

    // Risks — never urgent; the underlying source owns the urgency.
    let mut risk_lines = Vec::new();
    if let Some(q) = &p.quarter {
        if let Some(at_risk) = &q.at_risk {
            let title = format!("quarter goal at risk: {at_risk}");
            risk_lines.push(title.clone());
            items.push(CardItem {
                kind: ItemKind::Risk,
                severity: Severity::Warn,
                title,
                link: None,
            });
        }
    }
    if let Some(inb) = &p.inbox {
        if let Some(line) = inbox_risk_line(inb) {
            let sev = inbox_severity(inb);
            risk_lines.push(line.clone());
            items.push(CardItem {
                kind: ItemKind::Risk,
                severity: sev,
                title: line,
                link: None,
            });
        }
    }
    push_section(&mut body, "Risks", &risk_lines);

    // The day — context that colours the plan but isn't itself an item.
    let mut day_lines = Vec::new();
    if let Some(c) = &p.calendar {
        day_lines.push(format!("Calendar — {}", calendar_detail(c)));
    }
    if let Some(q) = &p.quarter {
        day_lines.push(quarter_detail(q));
    }
    if let Some(d) = &p.daily {
        day_lines.push(format!("Yesterday — {} done, {} carried", d.done, d.open));
    }
    push_section(&mut body, "The day", &day_lines);

    // Standup — the paste-ready one-liner, folded in from the retired 09:00 standup-push (#274).
    if let Some(line) = &p.standup {
        push_section(&mut body, "Standup", std::slice::from_ref(line));
    }

    Card {
        agent: p.slot.agent().to_string(),
        date: p.date.clone(),
        slot: p.slot,
        headline,
        body_md: body,
        items,
    }
}

/// The evening shutdown: what moved, what to drop or defer, and tomorrow's pre-decided first task.
pub fn build_pm_card(p: &Packet) -> Card {
    let tomorrow = pm_tomorrow_task(p);
    let headline = pm_headline(p, tomorrow.as_ref());
    let mut items = Vec::new();
    let mut body = String::new();

    // What moved today — closed goals.
    if let Some(d) = &p.daily {
        push_section(&mut body, "What moved today", &d.done_titles);
    }

    // What shipped today — folded in from the retired 17:00 wrap-digest (#274): merged PRs to
    // close (capped so a busy day stays a card, not a wall) and loose commits to sync.
    if let Some(w) = &p.wrap {
        const WRAP_PR_CAP: usize = 5;
        let mut lines = Vec::new();
        for pr in w.merged_prs.iter().take(WRAP_PR_CAP) {
            lines.push(format!("{} merged → `bn close`  {}", pr.label, pr.url));
            items.push(CardItem {
                kind: ItemKind::Action,
                severity: Severity::Info,
                title: format!("close {}", pr.label),
                link: Some(pr.url.clone()),
            });
        }
        if w.merged_prs.len() > WRAP_PR_CAP {
            let more = w.merged_prs.len() - WRAP_PR_CAP;
            lines.push(format!("…and {more} more merged today"));
        }
        if w.unnoted_commits > 0 {
            lines.push(format!(
                "{} un-noted commit{} → `bn sync`",
                w.unnoted_commits,
                plural(w.unnoted_commits)
            ));
        }
        push_section(&mut body, "What shipped today", &lines);
    }

    // What to drop or defer — still-open goals, with a defer suggestion.
    if let Some(d) = &p.daily {
        let lines: Vec<String> = d
            .open_titles
            .iter()
            .map(|t| format!("{t} — `bn daily defer \"{}\" --on <date>`", defer_query(t)))
            .collect();
        push_section(&mut body, "Consider dropping or deferring", &lines);
    }

    // Tomorrow's first task — one item, pre-decided.
    if let Some(task) = &tomorrow {
        push_section(
            &mut body,
            "Tomorrow starts with",
            std::slice::from_ref(&task.text),
        );
        items.push(CardItem {
            kind: ItemKind::Goal,
            severity: Severity::Info,
            title: task.text.clone(),
            link: task.link.clone(),
        });
    }

    Card {
        agent: p.slot.agent().to_string(),
        date: p.date.clone(),
        slot: p.slot,
        headline,
        body_md: body,
        items,
    }
}

/// Dispatch to the slot's builder.
pub fn build_card(p: &Packet) -> Card {
    match p.slot {
        Slot::Am => build_am_card(p),
        Slot::Pm => build_pm_card(p),
    }
}

/// The AM headline — the day in one sentence, most-salient clause first.
fn am_headline(p: &Packet) -> String {
    let mut parts = Vec::new();
    if !p.night.is_empty() {
        parts.push(format!(
            "{} night PR{} to review",
            p.night.len(),
            plural(p.night.len())
        ));
    } else if let Some(a) = p.actions.first() {
        parts.push(format!("start: {}", a.title));
    }
    if let Some(c) = &p.calendar {
        parts.push(calendar_phrase(c));
    }
    if let Some(q) = &p.quarter {
        if let Some(at_risk) = &q.at_risk {
            parts.push(format!("quarter at risk: {at_risk}"));
        }
    }
    if parts.is_empty() {
        "A clear slate — pick the top action.".to_string()
    } else {
        parts.join(" · ")
    }
}

/// The PM headline — what got done, and tomorrow's pre-chosen opener.
fn pm_headline(p: &Packet, tomorrow: Option<&TomorrowTask>) -> String {
    let done = p.daily.as_ref().map(|d| d.done).unwrap_or(0);
    match tomorrow {
        Some(t) => format!(
            "{done} goal{} done today · tomorrow starts with: {}",
            plural(done),
            t.text
        ),
        None => format!(
            "{done} goal{} done today · clean slate tomorrow",
            plural(done)
        ),
    }
}

/// Pick tomorrow's first task: the top still-open daily goal (the thing you didn't finish), else
/// the top `bn next` action. `None` when there's nothing to carry.
pub fn pm_tomorrow_task(p: &Packet) -> Option<TomorrowTask> {
    if let Some(d) = &p.daily {
        if let Some(t) = d.open_titles.first() {
            return Some(TomorrowTask {
                text: t.clone(),
                link: None,
            });
        }
    }
    p.actions.first().map(|a| TomorrowTask {
        text: a.title.clone(),
        link: a.link.clone(),
    })
}

/// A short defer query (the leading words) so the suggested command isn't the whole goal text.
fn defer_query(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().take(3).collect();
    words.join(" ")
}

/// The inbox risk line, or `None` when the inbox is empty (nothing to flag).
fn inbox_risk_line(inb: &InboxStatus) -> Option<String> {
    if inb.count == 0 {
        return None;
    }
    Some(match inb.oldest_age_days {
        Some(d) if d > 0 => format!(
            "q inbox: {} item{}, oldest {}d",
            inb.count,
            plural(inb.count),
            d
        ),
        _ => format!("q inbox: {} item{}", inb.count, plural(inb.count)),
    })
}

/// Aging inbox is a `warn`; a fresh backlog is just `info`.
fn inbox_severity(inb: &InboxStatus) -> Severity {
    match inb.oldest_age_days {
        Some(d) if d >= INBOX_STALE_DAYS => Severity::Warn,
        _ => Severity::Info,
    }
}

/// An inbox item older than this (days) makes the inbox risk a `warn`.
const INBOX_STALE_DAYS: i64 = 3;

/// The full calendar detail line for "The day" section.
fn calendar_detail(c: &CalendarShape) -> String {
    let mut s = format!("{} meeting{}", c.meetings, plural(c.meetings));
    if let Some(next) = &c.next_meeting {
        s.push_str(&format!(" · first {next}"));
    }
    if let Some(gap) = &c.free_gap {
        s.push_str(&format!(" · biggest gap {gap}"));
    }
    s
}

/// The quarter detail line for "The day" section.
fn quarter_detail(q: &QuarterStatus) -> String {
    format!(
        "Quarter {} — {}% elapsed · {}/{} on track",
        q.qid, q.elapsed_pct, q.on_track, q.total
    )
}

/// Render a card to terminal / feed markdown: an H1 with the slot + date, the headline, then the
/// body (which already carries only its present sections).
pub fn render(card: &Card) -> String {
    let mut out = format!(
        "# Chief of Staff — {} ({})\n\n{}\n",
        card.slot.label(),
        card.date,
        card.headline
    );
    if !card.body_md.is_empty() {
        out.push('\n');
        out.push_str(&card.body_md);
    }
    out
}

// ---- side-effects (store IO) ---------------------------------------------------------------

/// AM side-effect: add a `bn daily` review goal per night PR, idempotent by PR link — the same
/// dedup key `bn night report` uses (`daily::add` with the PR link), so the two jobs never
/// double-add. Returns the number of goals newly added.
pub fn add_night_goals(cfg: &Config, today: &str, night: &[NightPr]) -> usize {
    if night.is_empty() {
        return 0;
    }
    // Seed the dedup set with links already on today's list, then treat it as an insert-set so a
    // duplicate PR within this run is caught too.
    let mut seen: std::collections::HashSet<String> = Some(crate::daily::view(cfg, today))
        .map(|d| {
            d.front
                .goals
                .iter()
                .filter_map(|g| g.link.clone())
                .collect()
        })
        .unwrap_or_default();
    let mut added = 0;
    for pr in night {
        if !seen.insert(pr.link.clone()) {
            continue;
        }
        let text = crate::night::goal_text(&pr.repo, &pr.link);
        if crate::daily::add(cfg, today, &text, Some(pr.link.clone()), None).is_ok() {
            added += 1;
        }
    }
    added
}

/// The outcome of scheduling the PM card's tomorrow-first-task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TomorrowOutcome {
    /// Newly scheduled `text` for `date`.
    Scheduled { text: String, date: String },
    /// A matching follow-up was already queued for tomorrow (idempotent re-run).
    AlreadySet { text: String },
    /// Nothing to carry — clean slate tomorrow.
    Empty,
    /// Couldn't compute tomorrow's date, or the write failed.
    Failed(String),
}

/// PM side-effect: write tomorrow's first task as a scheduled daily goal (`--on` tomorrow),
/// idempotent — a re-run that finds a matching follow-up already queued for tomorrow adds nothing.
pub fn schedule_tomorrow(cfg: &Config, today: &str, packet: &Packet) -> TomorrowOutcome {
    let Some(task) = pm_tomorrow_task(packet) else {
        return TomorrowOutcome::Empty;
    };
    let Some(tomorrow) = crate::util::add_days(today, 1) else {
        return TomorrowOutcome::Failed("couldn't compute tomorrow's date".to_string());
    };
    let already = crate::daily::scheduled_pending(cfg, today)
        .into_iter()
        .any(|s| {
            s.scheduled == tomorrow
                && (s.text == task.text
                    || (task.link.is_some() && s.link.is_some() && s.link == task.link))
        });
    if already {
        return TomorrowOutcome::AlreadySet { text: task.text };
    }
    match crate::daily::add(cfg, today, &task.text, task.link.clone(), Some(&tomorrow)) {
        Ok(_) => TomorrowOutcome::Scheduled {
            text: task.text,
            date: tomorrow,
        },
        Err(e) => TomorrowOutcome::Failed(format!("{e:?}")),
    }
}

// ---- calendar parsing (pure, best-effort) --------------------------------------------------
/// Build a [`CalendarShape`] from the meetings feed items' texts. `None` when there are no
/// meetings at all; a shape with `next_meeting`/`free_gap` left `None` when times can't be parsed
/// (best-effort — the source degrades, it never errors).
pub fn calendar_shape(meeting_texts: &[String]) -> Option<CalendarShape> {
    if meeting_texts.is_empty() {
        return None;
    }
    let mut times: Vec<u32> = meeting_texts
        .iter()
        .filter_map(|t| parse_first_time(t))
        .collect();
    times.sort_unstable();
    times.dedup();
    let next_meeting = times.first().map(|m| hhmm(*m));
    let free_gap = biggest_gap(&times).map(|(a, b)| format!("{}–{}", hhmm(a), hhmm(b)));
    Some(CalendarShape {
        meetings: meeting_texts.len(),
        next_meeting,
        free_gap,
    })
}

/// The first `HH:MM` (24h) time in a prose line, as minutes since midnight. Best-effort: scans for
/// a `:` flanked by 1–2 digits before and 2 digits after, validating the ranges.
pub fn parse_first_time(text: &str) -> Option<u32> {
    let chars: Vec<char> = text.chars().collect();
    for i in 0..chars.len() {
        if chars[i] != ':' {
            continue;
        }
        // Up to two digits before the colon.
        let mut hh = String::new();
        let mut j = i;
        while j > 0 && chars[j - 1].is_ascii_digit() && hh.len() < 2 {
            j -= 1;
            hh.insert(0, chars[j]);
        }
        // Exactly two digits after the colon (and not a third, to avoid 09:000).
        if hh.is_empty() || i + 2 >= chars.len() {
            continue;
        }
        if !chars[i + 1].is_ascii_digit() || !chars[i + 2].is_ascii_digit() {
            continue;
        }
        if chars.get(i + 3).is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        let mm: u32 = format!("{}{}", chars[i + 1], chars[i + 2]).parse().ok()?;
        let hh: u32 = hh.parse().ok()?;
        if hh < 24 && mm < 60 {
            return Some(hh * 60 + mm);
        }
    }
    None
}

/// The biggest gap between consecutive (sorted) meeting start times, as `(start, end)` minutes.
/// `None` with fewer than two distinct times.
fn biggest_gap(sorted_times: &[u32]) -> Option<(u32, u32)> {
    if sorted_times.len() < 2 {
        return None;
    }
    let mut best = (sorted_times[0], sorted_times[1]);
    let mut best_span = best.1 - best.0;
    for w in sorted_times.windows(2) {
        let span = w[1] - w[0];
        if span > best_span {
            best_span = span;
            best = (w[0], w[1]);
        }
    }
    (best_span > 0).then_some(best)
}

fn hhmm(min: u32) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(title: &str, source: &str, link: Option<&str>) -> TopAction {
        TopAction {
            title: title.to_string(),
            source: source.to_string(),
            link: link.map(str::to_string),
        }
    }

    fn night_pr(repo: &str, slug: &str, link: &str) -> NightPr {
        NightPr {
            repo: repo.to_string(),
            slug: slug.to_string(),
            link: link.to_string(),
        }
    }

    /// A full AM packet with every source present.
    fn full_am_packet() -> Packet {
        let mut p = Packet::new("2026-07-06", Slot::Am);
        p.night = vec![
            night_pr("bn", "bn#245", "https://github.com/you/bn/pull/245"),
            night_pr("bn", "bn#246", "https://github.com/you/bn/pull/246"),
        ];
        p.actions = vec![
            action(
                "review you/bn#12",
                "review",
                Some("https://github.com/you/bn/pull/12"),
            ),
            action("unblock bn/feat-x", "blocked", None),
            action("finish g1", "goal", None),
            action("chase stale PR", "pr", None),
        ];
        p.momentum = Some("bn/feat-x".to_string());
        p.calendar = Some(CalendarShape {
            meetings: 3,
            next_meeting: Some("09:00".to_string()),
            free_gap: Some("10:00–14:00".to_string()),
        });
        p.quarter = Some(QuarterStatus {
            qid: "2026-Q3".to_string(),
            elapsed_pct: 45,
            on_track: 2,
            total: 4,
            at_risk: Some("ship v1".to_string()),
        });
        p.inbox = Some(InboxStatus {
            count: 5,
            oldest_age_days: Some(9),
        });
        p.daily = Some(DailySummary {
            done: 3,
            open: 2,
            done_titles: vec!["shipped x".to_string()],
            open_titles: vec!["carry y".to_string(), "carry z".to_string()],
        });
        p
    }

    #[test]
    fn am_card_renders_every_present_section() {
        let card = build_am_card(&full_am_packet());
        assert_eq!(card.agent, "chief-am");
        assert_eq!(card.slot, Slot::Am);
        // headline leads with the night review count, then calendar, then quarter risk.
        assert!(
            card.headline.contains("2 night PRs to review"),
            "{}",
            card.headline
        );
        assert!(
            card.headline.contains("quarter at risk: ship v1"),
            "{}",
            card.headline
        );
        let b = &card.body_md;
        assert!(b.contains("## Top 3"));
        assert!(b.contains("review you/bn#12 (review)"));
        assert!(b.contains("## Review the night's work"));
        assert!(b.contains("review night PR bn#245"));
        assert!(b.contains("## Risks"));
        assert!(b.contains("quarter goal at risk: ship v1"));
        assert!(b.contains("q inbox: 5 items, oldest 9d"));
        assert!(b.contains("## The day"));
        assert!(b.contains("Quarter 2026-Q3 — 45% elapsed · 2/4 on track"));
    }

    #[test]
    fn am_card_top_3_caps_at_three_actions() {
        let card = build_am_card(&full_am_packet());
        let action_items = card
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::Action)
            .count();
        assert_eq!(action_items, 3, "only the top three actions become items");
    }

    #[test]
    fn am_card_one_goal_item_per_night_pr() {
        let card = build_am_card(&full_am_packet());
        let goals: Vec<_> = card
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::Goal)
            .collect();
        assert_eq!(goals.len(), 2);
        assert_eq!(goals[0].title, "review night PR bn#245");
        assert_eq!(
            goals[0].link.as_deref(),
            Some("https://github.com/you/bn/pull/245")
        );
    }

    #[test]
    fn am_card_degrades_when_sources_missing() {
        // Only next actions present — every other source omitted.
        let mut p = Packet::new("2026-07-06", Slot::Am);
        p.actions = vec![action("finish g1", "goal", None)];
        let card = build_am_card(&p);
        assert!(card.body_md.contains("## Top 3"));
        assert!(!card.body_md.contains("Review the night's work"));
        assert!(!card.body_md.contains("## Risks"));
        assert!(!card.body_md.contains("## The day"));
        assert!(card.headline.starts_with("start: finish g1"));
    }

    #[test]
    fn am_card_empty_packet_never_errors() {
        let p = Packet::new("2026-07-06", Slot::Am);
        let card = build_am_card(&p);
        assert_eq!(card.headline, "A clear slate — pick the top action.");
        assert!(card.body_md.is_empty());
        assert!(card.items.is_empty());
    }

    #[test]
    fn pm_card_picks_top_open_goal_as_tomorrows_first_task() {
        let mut p = Packet::new("2026-07-06", Slot::Pm);
        p.daily = Some(DailySummary {
            done: 2,
            open: 2,
            done_titles: vec!["merged the parser".to_string()],
            open_titles: vec!["write the docs".to_string(), "second thing".to_string()],
        });
        p.actions = vec![action("review you/bn#12", "review", None)];
        let card = build_pm_card(&p);
        assert_eq!(card.agent, "chief-pm");
        let task = pm_tomorrow_task(&p).unwrap();
        assert_eq!(task.text, "write the docs");
        assert!(card
            .headline
            .contains("tomorrow starts with: write the docs"));
        assert!(card.body_md.contains("## What moved today"));
        assert!(card.body_md.contains("merged the parser"));
        assert!(card.body_md.contains("## Consider dropping or deferring"));
        assert!(card.body_md.contains("## Tomorrow starts with"));
        // exactly one goal item (tomorrow's task)
        assert_eq!(
            card.items
                .iter()
                .filter(|i| i.kind == ItemKind::Goal)
                .count(),
            1
        );
    }

    #[test]
    fn pm_tomorrow_task_falls_back_to_top_action() {
        let mut p = Packet::new("2026-07-06", Slot::Pm);
        p.daily = Some(DailySummary {
            done: 4,
            open: 0,
            done_titles: vec!["all done".to_string()],
            open_titles: vec![],
        });
        p.actions = vec![action(
            "review you/bn#12",
            "review",
            Some("https://x/pull/12"),
        )];
        let task = pm_tomorrow_task(&p).unwrap();
        assert_eq!(task.text, "review you/bn#12");
        assert_eq!(task.link.as_deref(), Some("https://x/pull/12"));
    }

    #[test]
    fn pm_card_clean_slate_when_nothing_to_carry() {
        let mut p = Packet::new("2026-07-06", Slot::Pm);
        p.daily = Some(DailySummary {
            done: 4,
            open: 0,
            done_titles: vec!["all done".to_string()],
            open_titles: vec![],
        });
        let card = build_pm_card(&p);
        assert!(pm_tomorrow_task(&p).is_none());
        assert!(card.headline.contains("clean slate tomorrow"));
        assert!(card.items.is_empty());
    }

    #[test]
    fn am_card_folds_in_standup_line() {
        // #274: the AM brief absorbs the standup line as its own section.
        let mut p = Packet::new("2026-07-06", Slot::Am);
        p.actions = vec![action("finish g1", "goal", None)];
        p.standup = Some("Standup: yesterday shipped 2, today 3 planned; blocked on X".to_string());
        let card = build_am_card(&p);
        assert!(card.body_md.contains("## Standup"), "{}", card.body_md);
        assert!(card
            .body_md
            .contains("Standup: yesterday shipped 2, today 3 planned; blocked on X"));
    }

    #[test]
    fn am_card_omits_standup_section_when_absent() {
        let mut p = Packet::new("2026-07-06", Slot::Am);
        p.actions = vec![action("finish g1", "goal", None)];
        let card = build_am_card(&p);
        assert!(!card.body_md.contains("## Standup"));
    }

    #[test]
    fn pm_card_folds_in_wrap_summary() {
        // #274: the PM shutdown absorbs the wrap digest — merged PRs (→ bn close) and loose
        // commits (→ bn sync), plus one Action item per merged PR.
        let mut p = Packet::new("2026-07-06", Slot::Pm);
        p.daily = Some(DailySummary {
            done: 1,
            open: 0,
            done_titles: vec!["shipped it".to_string()],
            open_titles: vec![],
        });
        p.wrap = Some(WrapSummary {
            unnoted_commits: 2,
            merged_prs: vec![WrapPr {
                label: "#245 add the widget".to_string(),
                url: "https://github.com/you/bn/pull/245".to_string(),
            }],
        });
        let card = build_pm_card(&p);
        let b = &card.body_md;
        assert!(b.contains("## What shipped today"), "{b}");
        assert!(b.contains("#245 add the widget merged → `bn close`"), "{b}");
        assert!(b.contains("2 un-noted commits → `bn sync`"), "{b}");
        // one Action item points back to the merged PR.
        let actions: Vec<_> = card
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::Action)
            .collect();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "close #245 add the widget");
        assert_eq!(
            actions[0].link.as_deref(),
            Some("https://github.com/you/bn/pull/245")
        );
    }

    #[test]
    fn pm_card_wrap_caps_merged_prs_at_five() {
        // A busy day stays a card, not a wall: only five PRs render (with items), the rest fold
        // into an "…and N more" line that carries no item.
        let mut p = Packet::new("2026-07-06", Slot::Pm);
        p.wrap = Some(WrapSummary {
            unnoted_commits: 0,
            merged_prs: (1..=8)
                .map(|n| WrapPr {
                    label: format!("#{n} pr {n}"),
                    url: format!("https://x/pull/{n}"),
                })
                .collect(),
        });
        let card = build_pm_card(&p);
        assert!(
            card.body_md.contains("…and 3 more merged today"),
            "{}",
            card.body_md
        );
        assert_eq!(
            card.items
                .iter()
                .filter(|i| i.kind == ItemKind::Action)
                .count(),
            5,
            "only the shown PRs become items"
        );
    }

    #[test]
    fn pm_card_wrap_singular_commit_only() {
        let mut p = Packet::new("2026-07-06", Slot::Pm);
        p.wrap = Some(WrapSummary {
            unnoted_commits: 1,
            merged_prs: vec![],
        });
        let card = build_pm_card(&p);
        assert!(card.body_md.contains("1 un-noted commit → `bn sync`"));
        assert!(card.items.is_empty());
    }

    #[test]
    fn pm_card_omits_wrap_section_when_absent() {
        let mut p = Packet::new("2026-07-06", Slot::Pm);
        p.daily = Some(DailySummary {
            done: 1,
            open: 0,
            done_titles: vec!["shipped it".to_string()],
            open_titles: vec![],
        });
        let card = build_pm_card(&p);
        assert!(!card.body_md.contains("## What shipped today"));
    }

    #[test]
    fn wrap_summary_is_empty_when_all_sources_blank() {
        let w = WrapSummary {
            unnoted_commits: 0,
            merged_prs: vec![],
        };
        assert!(w.is_empty());
        assert!(!WrapSummary {
            unnoted_commits: 1,
            merged_prs: vec![],
        }
        .is_empty());
    }

    #[test]
    fn cards_are_never_urgent() {
        // Enforced by the type (no Severity::Urgent), asserted over the serialized card so a future
        // contract change can't smuggle urgency in.
        for card in [
            build_am_card(&full_am_packet()),
            build_pm_card(&full_am_packet()),
        ] {
            for it in &card.items {
                assert!(matches!(it.severity, Severity::Info | Severity::Warn));
            }
            let json = serde_json::to_string(&card).unwrap();
            assert!(
                !json.contains("urgent"),
                "card must never carry urgent severity: {json}"
            );
        }
    }

    #[test]
    fn inbox_severity_warns_only_when_stale() {
        assert_eq!(
            inbox_severity(&InboxStatus {
                count: 2,
                oldest_age_days: Some(1)
            }),
            Severity::Info
        );
        assert_eq!(
            inbox_severity(&InboxStatus {
                count: 2,
                oldest_age_days: Some(3)
            }),
            Severity::Warn
        );
        assert_eq!(
            inbox_severity(&InboxStatus {
                count: 2,
                oldest_age_days: None
            }),
            Severity::Info
        );
    }

    #[test]
    fn parse_first_time_extracts_from_prose() {
        assert_eq!(parse_first_time("09:00 Standup"), Some(540));
        assert_eq!(parse_first_time("Sync at 14:30 in room 2"), Some(870));
        assert_eq!(parse_first_time("9:05 kickoff"), Some(545));
        assert_eq!(parse_first_time("no time here"), None);
        assert_eq!(parse_first_time("bad 25:00 time"), None);
        assert_eq!(parse_first_time("bad 12:75 time"), None);
    }

    #[test]
    fn calendar_shape_counts_and_finds_biggest_gap() {
        let texts = vec![
            "09:00 Standup".to_string(),
            "10:00 1:1".to_string(),
            "15:00 Review".to_string(),
        ];
        let c = calendar_shape(&texts).unwrap();
        assert_eq!(c.meetings, 3);
        assert_eq!(c.next_meeting.as_deref(), Some("09:00"));
        // biggest gap is 10:00 → 15:00
        assert_eq!(c.free_gap.as_deref(), Some("10:00–15:00"));
    }

    #[test]
    fn calendar_shape_degrades_without_parseable_times() {
        let texts = vec!["all-hands (time TBD)".to_string()];
        let c = calendar_shape(&texts).unwrap();
        assert_eq!(c.meetings, 1);
        assert_eq!(c.next_meeting, None);
        assert_eq!(c.free_gap, None);
    }

    #[test]
    fn calendar_shape_none_without_meetings() {
        assert!(calendar_shape(&[]).is_none());
    }

    #[test]
    fn render_includes_title_headline_and_body() {
        let card = build_am_card(&full_am_packet());
        let out = render(&card);
        assert!(out.starts_with("# Chief of Staff — morning brief (2026-07-06)"));
        assert!(out.contains(&card.headline));
        assert!(out.contains("## Top 3"));
    }

    #[test]
    fn add_night_goals_is_idempotent_by_link() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::test_support::config(dir.path());
        let today = "2026-07-06";
        let night = vec![
            night_pr("bn", "bn#245", "https://github.com/you/bn/pull/245"),
            night_pr("bn", "bn#246", "https://github.com/you/bn/pull/246"),
            // a duplicate link within the same batch must not double-add
            night_pr("bn", "bn#245", "https://github.com/you/bn/pull/245"),
        ];
        assert_eq!(
            add_night_goals(&cfg, today, &night),
            2,
            "first run adds 2 distinct"
        );
        // Re-run: everything already queued, nothing added.
        assert_eq!(
            add_night_goals(&cfg, today, &night),
            0,
            "re-run adds nothing"
        );
        let d = crate::daily::read(&cfg.daily_file(today)).unwrap();
        let review: Vec<_> = d
            .front
            .goals
            .iter()
            .filter(|g| g.text.starts_with("review night PR"))
            .collect();
        assert_eq!(review.len(), 2, "exactly one goal per distinct PR");
        assert!(review.iter().any(|g| g.text == "review night PR bn#245"));
    }

    #[test]
    fn add_night_goals_empty_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::test_support::config(dir.path());
        assert_eq!(add_night_goals(&cfg, "2026-07-06", &[]), 0);
        // No day file is forced into existence for an empty night.
        assert!(!cfg.daily_file("2026-07-06").exists());
    }

    #[test]
    fn schedule_tomorrow_writes_once_then_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::test_support::config(dir.path());
        let today = "2026-07-06";
        let mut p = Packet::new(today, Slot::Pm);
        p.daily = Some(DailySummary {
            done: 1,
            open: 1,
            done_titles: vec!["shipped".to_string()],
            open_titles: vec!["write the docs".to_string()],
        });
        match schedule_tomorrow(&cfg, today, &p) {
            TomorrowOutcome::Scheduled { text, date } => {
                assert_eq!(text, "write the docs");
                assert_eq!(date, "2026-07-07");
            }
            other => panic!("expected Scheduled, got {other:?}"),
        }
        // Re-run: the follow-up is already queued for tomorrow.
        assert_eq!(
            schedule_tomorrow(&cfg, today, &p),
            TomorrowOutcome::AlreadySet {
                text: "write the docs".to_string()
            }
        );
        // Exactly one follow-up queued for tomorrow.
        let pending = crate::daily::scheduled_pending(&cfg, today);
        assert_eq!(
            pending
                .iter()
                .filter(|s| s.scheduled == "2026-07-07")
                .count(),
            1
        );
    }

    #[test]
    fn schedule_tomorrow_empty_when_nothing_to_carry() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::test_support::config(dir.path());
        let p = Packet::new("2026-07-06", Slot::Pm); // no daily, no actions
        assert_eq!(
            schedule_tomorrow(&cfg, "2026-07-06", &p),
            TomorrowOutcome::Empty
        );
    }
}
