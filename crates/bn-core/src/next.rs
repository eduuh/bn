//! `bn next` — the "what do I do now?" engine. [`doctor`](crate::doctor) answers *what's wrong*;
//! this answers *what to act on next*, folding every already-captured, offline signal into one
//! ranked action list: failing cron jobs and raised attention items (broken / flagged), PRs
//! awaiting your review (others blocked on you), your own in-flight PRs going stale (chase them),
//! today's open daily goals (the plan), and blocked branch notes (need unblocking). Read-only and
//! network-free — same north star as doctor: read, don't re-fetch. The CLI shows the top few; the
//! full ranked list goes to `--json` / the MCP tool.

use crate::action_rank;
use crate::daily;
use crate::doctor::{self, Finding};
use crate::events;
use crate::metrics;
use crate::resolver::{state_dir, Config};
use crate::reviews;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A PR untouched for at least this many days counts as "going stale" — old enough that it's
/// worth a nudge (ping a reviewer, rebase, address comments) rather than left to drift. Fresher
/// in-flight PRs surface nowhere: there's nothing to *do* about a PR you opened an hour ago.
const STALE_PR_DAYS: i64 = 2;

/// What kind of thing an action is — drives its glyph and how it's phrased.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// The WIP / focus guard — too many branches in flight; finish one. The forward-looking
    /// summary that leads the list when the desk is sprawling.
    Wip,
    /// A failing cron job — something on the machine is broken.
    Cron,
    /// A hand-/script-/agent-raised attention item.
    Attn,
    /// Today's most-important meeting (from the `meetings` job) — be there.
    Meeting,
    /// An email that needs a reply (from the `m365-sweep` job) — someone's waiting.
    Email,
    /// A Teams @-mention or DM awaiting your reply (from `m365-sweep`) — someone's waiting.
    Mention,
    /// A PR awaiting your review — someone is blocked on you.
    Review,
    /// Your own in-flight PR going stale — chase it.
    Pr,
    /// An open daily goal — today's plan.
    Goal,
    /// A branch note with open blockers — needs unblocking.
    Blocked,
}

/// The dashboard section an action sorts into — the grouping layer over [`ActionKind`] that makes
/// a long mixed list scannable. Ordered by how much each section demands of you: what's broken,
/// then who's blocked on you, then today's plan, then your own backlog.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ActionCategory {
    /// Something is on fire right now — a failing cron, an urgent flagged item.
    Breaking,
    /// Other people are blocked on you — reviews, emails, Teams mentions awaiting a reply.
    WaitingOnYou,
    /// Today's plan — meetings and daily goals.
    Today,
    /// Your own backlog — stale PRs, blocked notes, the WIP nudge, misc flagged items.
    YourWork,
}

impl ActionCategory {
    /// Section header for the human view, render order = enum order. Icons are the Nerd Font
    /// glyphs from [`crate::glyph`] (bolt / eye / calendar / tasks), escaped so the codepoint
    /// is visible in source.
    pub fn header(self) -> &'static str {
        match self {
            ActionCategory::Breaking => "\u{f0e7} BREAKING",
            ActionCategory::WaitingOnYou => "\u{f06e} WAITING ON YOU",
            ActionCategory::Today => "\u{f073} TODAY",
            ActionCategory::YourWork => "\u{f0ae} YOUR WORK",
        }
    }

    /// All sections in render order.
    pub fn all() -> [ActionCategory; 4] {
        [
            ActionCategory::Breaking,
            ActionCategory::WaitingOnYou,
            ActionCategory::Today,
            ActionCategory::YourWork,
        ]
    }
}

impl ActionKind {
    /// The leading glyph for the human view.
    fn glyph(self) -> &'static str {
        match self {
            ActionKind::Wip => "▰",
            ActionKind::Cron => "⚠",
            ActionKind::Attn => crate::glyph::BELL,
            ActionKind::Meeting => crate::glyph::CALENDAR,
            ActionKind::Email => crate::glyph::ENVELOPE,
            ActionKind::Mention => crate::glyph::COMMENT,
            ActionKind::Review => crate::glyph::BAN,
            ActionKind::Pr => crate::glyph::PULL_REQUEST,
            ActionKind::Goal => "◷",
            ActionKind::Blocked => "○",
        }
    }
}

/// How close an action is to where you currently are (the open punch) — the momentum tiebreak
/// (#246). Stamped at read time by [`apply_momentum`]; never persisted to the attention cache.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Momentum {
    /// Same repo *and* branch as the open punch — zero context switch.
    Branch,
    /// Same repo, different (or unknown) branch — a cheap switch.
    Repo,
}

/// One ranked next action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub kind: ActionKind,
    /// The imperative one-liner — what to do ("review eduuh/bn#2", "unblock bn/feat-x").
    pub title: String,
    /// Supporting context: age, source, counts.
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// The repo this action targets, when the source knows it — a short name ("bn") for local
    /// sources (notes, cron, attn) and `owner/name` for GitHub-backed ones (reviews, PRs).
    /// Feeds the momentum tiebreak; [`repo_key`] normalizes the two shapes for matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The branch this action targets, when the source knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The momentum boost this action earned against the current open punch, if any. Stamped at
    /// read time (see [`apply_momentum`]); `None` (an explicit JSON `null`) in the cache and
    /// under `--no-momentum`. Always serialized — the #246 contract is a `momentum` field per
    /// action, so badge-rendering surfaces never have to special-case a missing key.
    #[serde(default)]
    pub momentum: Option<Momentum>,
    /// The underlying attention-item id, when this action *is* a raised attn item (so the `bn jump`
    /// picker can start it — open its worktree window + mark it in-flight). `None` for every other
    /// source (reviews, PRs, goals, the WIP guard). Survives the attention.json cache round-trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attn_id: Option<String>,
    /// The `--est` stamp this action carries, when its source knows one — currently daily goals
    /// (the plan band). A human duration (`90m`, `2h`) or a coarse t-shirt size (`S`/`M`/`L`); see
    /// [`crate::calib`]. Backs `bn next --fit`, which sizes it (× the calibration multiplier)
    /// against the gap to the next meeting. `None` for every un-estimated source. Round-trips the
    /// attention cache; omitted from JSON when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub est: Option<String>,
    /// Sort weight, higher first: 4 = the WIP/focus guard (finish something before starting more),
    /// 3 = broken or blocking others, 2 = your stalled work, 1 = your own backlog.
    pub priority: u8,
    /// Tiebreak within a priority, larger first — staleness in days or failure count.
    pub age: i64,
    /// Hours since this action's relevant moment, for the recency cutoff. `Some(h)` only for an
    /// incoming review request's wait — the source that accumulates stale clutter (a years-old
    /// review ask from a dead repo). Everything else is `None` (never ages out): the WIP nudge,
    /// cron breakage, blocked notes, your own PR pings, today's goals, a just-raised attn item,
    /// and an approved PR. Internal — not serialized.
    #[serde(skip)]
    pub age_hours: Option<i64>,
}

impl Action {
    /// `"N. <glyph> <title>"` — the numbered headline line for the human view.
    pub fn headline(&self, n: usize) -> String {
        format!("{n}. {} {}", self.kind.glyph(), self.title)
    }

    /// This action's leading glyph — for the grouped (un-numbered) dashboard rows.
    pub fn kind_glyph(&self) -> &'static str {
        self.kind.glyph()
    }

    /// Whether this action is urgent enough to still interrupt a [`crate::focus`] block —
    /// "broken / urgent severity (failing cron, a true blocker)". A failing cron, a blocked note,
    /// an urgent attention item, or an over-SLA review (priority 4: someone is late-blocked on
    /// you) breaks through; everything else — the WIP nudge, warn/info attn, in-SLA reviews, your
    /// own stale PRs, today's goals — is non-urgent and gets suppressed + queued during deep work.
    pub fn is_urgent(&self) -> bool {
        match self.kind {
            ActionKind::Cron => action_rank::URGENT.is_urgent(),
            ActionKind::Blocked => action_rank::BLOCKED.is_urgent(),
            // Reviews ride the SLA: only an over-SLA review (priority 4) is an interrupt.
            ActionKind::Review => self.priority >= action_rank::GUARD.priority(),
            // attn carries severity in its priority: urgent → 3, warn → 2, info → 1.
            ActionKind::Attn => self.priority >= action_rank::URGENT.priority(),
            // Email / Teams mention: a high-confidence urgent one (severity → priority 3) interrupts.
            ActionKind::Email | ActionKind::Mention => {
                self.priority >= action_rank::URGENT.priority()
            }
            // A meeting interrupts (you have to be there); the WIP guard never does.
            ActionKind::Meeting => true,
            ActionKind::Wip | ActionKind::Pr | ActionKind::Goal => false,
        }
    }

    /// Which dashboard section this action sorts into (see [`ActionCategory`]). Most kinds map
    /// straight from their `kind`; a flagged [`ActionKind::Attn`] is urgent-or-not by its priority
    /// (urgent → Breaking, else parked in Your Work).
    pub fn category(&self) -> ActionCategory {
        match self.kind {
            ActionKind::Cron => ActionCategory::Breaking,
            ActionKind::Attn => {
                if self.priority >= action_rank::URGENT.priority() {
                    ActionCategory::Breaking
                } else {
                    ActionCategory::YourWork
                }
            }
            ActionKind::Review | ActionKind::Email | ActionKind::Mention => {
                ActionCategory::WaitingOnYou
            }
            ActionKind::Meeting | ActionKind::Goal => ActionCategory::Today,
            ActionKind::Wip | ActionKind::Pr | ActionKind::Blocked => ActionCategory::YourWork,
        }
    }
}

/// The richer [`ActionKind`] a flagged attention item earns from the job that raised it (its
/// `source`), so a meeting reads as a meeting and an email as an email — not a generic bell. Anything
/// unrecognised stays a plain flagged item.
fn kind_for_attn_source(source: &str) -> ActionKind {
    match source {
        "meetings" => ActionKind::Meeting,
        "email" => ActionKind::Email,
        "teams" => ActionKind::Mention,
        // A finished autonomous review (`bn review-pr`) is a PR sitting on your decision —
        // route it like an incoming review so it lands in WAITING ON YOU, not the backlog.
        "auto-review" => ActionKind::Review,
        _ => ActionKind::Attn,
    }
}

/// Partition a (priority-sorted) action slice into the dashboard sections, in render order. Each
/// section keeps the incoming order; empty sections are dropped so the caller prints only headers
/// that have items. The grouping is render-only — the ranked vector itself is untouched, so
/// `--brief`, `--json`, and the focus/recency gates all still see the flat priority order.
pub fn group_by_category(actions: &[Action]) -> Vec<(ActionCategory, Vec<&Action>)> {
    ActionCategory::all()
        .into_iter()
        .filter_map(|cat| {
            let items: Vec<&Action> = actions.iter().filter(|a| a.category() == cat).collect();
            (!items.is_empty()).then_some((cat, items))
        })
        .collect()
}

/// The expected wall-clock seconds an estimated action will actually take: its `est` scaled by the
/// calibration `multiplier` for its bucket (`bn calib`), or the raw estimate when nothing's been
/// calibrated yet (or a degenerate ≤0 multiplier). Rounded to the nearest second. The sizing heart
/// of `bn next --fit`.
pub fn expected_secs(est_secs: i64, multiplier: Option<f64>) -> i64 {
    match multiplier {
        Some(m) if m > 0.0 => (est_secs as f64 * m).round() as i64,
        _ => est_secs,
    }
}

/// The outcome of sizing the plan band against the gap to the next meeting (`bn next --fit`, #247).
#[derive(Debug, Clone, Default)]
pub struct FitPlan {
    /// Everything shown: non-plan actions (never filtered) plus the plan-band goals that fit.
    pub kept: Vec<Action>,
    /// Estimated plan-band goals that don't fit the gap — hidden from `kept`, counted for a note.
    pub dropped: usize,
    /// Un-estimated plan-band goals — surfaced under a separate tail rather than silently hidden,
    /// since fit can't size them.
    pub unsized_tail: Vec<Action>,
}

/// Size the plan band (daily goals) against `gap`, per #247: keep every goal whose expected
/// duration (`est` × its calibration multiplier) fits the gap minus the wrap-up buffer
/// ([`crate::meetings::FIT_BUFFER_SECS`]); peel un-estimated goals into [`FitPlan::unsized_tail`] (never
/// hidden); drop the rest. Non-plan actions — a failing cron, a review awaiting you, a meeting
/// anchor — are never filtered: urgency doesn't care how much time is left. With no gap (no fresh
/// upcoming meeting) `--fit` is a no-op and everything is kept, matching plain `bn next`.
pub fn fit_plan(
    actions: Vec<Action>,
    gap: Option<&crate::meetings::Gap>,
    report: &crate::calib::Report,
    sizes: &crate::calib::CoarseSizes,
) -> FitPlan {
    let Some(gap) = gap else {
        return FitPlan {
            kept: actions,
            ..Default::default()
        };
    };
    let budget = gap.seconds - crate::meetings::FIT_BUFFER_SECS;
    let mut plan = FitPlan::default();
    for a in actions {
        if a.kind != ActionKind::Goal {
            plan.kept.push(a);
            continue;
        }
        match a
            .est
            .as_deref()
            .and_then(|e| crate::calib::est_secs(sizes, e))
        {
            None => plan.unsized_tail.push(a),
            Some(est) => {
                let mult = crate::calib::multiplier_for(report, a.repo.as_deref(), &[]);
                if expected_secs(est, mult) <= budget {
                    plan.kept.push(a);
                } else {
                    plan.dropped += 1;
                }
            }
        }
    }
    plan
}

/// The ranked action list, most-pressing first. Folds five offline sources — see the module docs.
/// Returns the full list; callers decide how many to show. A clear desk (no failures, no items,
/// no reviews, no stale PRs, no open goals, no blocked notes) yields an empty list.
pub fn next_actions(cfg: &Config, today: &str, now: i64) -> Vec<Action> {
    let mut out = all_actions(cfg, today, now);
    // Focus mode (#119): inside a declared deep-work block, only urgent actions still surface;
    // the routine ones are suppressed and queued, draining when the block ends (see `drained`).
    if crate::focus::is_active(now) {
        out.retain(Action::is_urgent);
    }
    // Routine-aware schedule (#801): inside a *protected* recurring block (family/recovery/
    // exercise/sleep, or any block explicitly marked `protected`), non-urgent guidance pauses the
    // same way focus mode does — urgent/breaking items always still surface. Work/learning blocks
    // are unprotected by default and keep full guidance. A disabled or unresolved `[routine]`
    // config is a silent no-op (see `current_snapshot`).
    if crate::routine::current_snapshot(cfg, now).is_some_and(|s| s.suppresses_guidance) {
        out.retain(Action::is_urgent);
    }
    out
}

/// The full ranked action list *ignoring* any active focus block — the unfiltered source that
/// both [`next_actions`] (which then applies the focus gate) and [`drained`] (which inverts it)
/// build on. Folds the five offline sources and sorts priority-desc, age-desc.
fn all_actions(cfg: &Config, today: &str, now: i64) -> Vec<Action> {
    let mut out = Vec::new();
    out.extend(from_wip(cfg, now, today));
    out.extend(from_doctor(cfg, today));
    out.extend(from_reviews(cfg, today, now));
    out.extend(from_in_flight(cfg, now));
    out.extend(from_goals(cfg, today));
    retain_recent_actions(&mut out, effective_cutoff_hours(cfg, today));
    sort_actions(&mut out);
    out
}

fn retain_recent_actions(actions: &mut Vec<Action>, cutoff_hours: i64) {
    actions.retain(|action| action.age_hours.is_none_or(|age| age <= cutoff_hours));
}

fn sort_actions(actions: &mut [Action]) {
    actions.sort_by(rank_cmp);
}

/// Rank by priority, demanding category, then age; stable ties preserve source order.
fn rank_cmp(a: &Action, b: &Action) -> std::cmp::Ordering {
    action_rank::compare_priority(a.priority, b.priority)
        .then(a.category().cmp(&b.category()))
        .then(b.age.cmp(&a.age))
}

/// The comparable name of a repo: its last `/`-segment, so a punch's short name ("bn") matches a
/// GitHub `owner/name` ("eduuh/bn") and an ADO `Org/Project/_git/Repo` shape alike.
fn repo_key(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
}

/// Whether two repo identifiers name the same repo, across the short-name / `owner/name` shapes.
fn same_repo(a: &str, b: &str) -> bool {
    repo_key(a).eq_ignore_ascii_case(repo_key(b))
}

/// The momentum a single action earns against the open punch's repo/branch: same repo+branch →
/// [`Momentum::Branch`], same repo → [`Momentum::Repo`], anything else (including an action with
/// no repo) → `None`. Pure — the unit-testable heart of the tiebreak.
fn momentum_for(action: &Action, punch_repo: &str, punch_branch: Option<&str>) -> Option<Momentum> {
    let repo = action.repo.as_deref()?;
    if !same_repo(repo, punch_repo) {
        return None;
    }
    match (action.branch.as_deref(), punch_branch) {
        (Some(a), Some(p)) if a == p => Some(Momentum::Branch),
        _ => Some(Momentum::Repo),
    }
}

/// Sort weight of a momentum stamp, smaller first: current branch, then current repo, then rest.
fn momentum_weight(m: Option<Momentum>) -> u8 {
    match m {
        Some(Momentum::Branch) => 0,
        Some(Momentum::Repo) => 1,
        None => 2,
    }
}

/// [`rank_cmp`] with the momentum tiebreak slotted *inside* a band (#246): priority desc, then
/// category, then momentum (closest to where you are first), then age desc. Priority and category
/// compare first, so momentum can never lift an action across a band — a failing cron still
/// outranks anything cozy.
fn momentum_cmp(a: &Action, b: &Action) -> std::cmp::Ordering {
    action_rank::compare_priority(a.priority, b.priority)
        .then(a.category().cmp(&b.category()))
        .then(momentum_weight(a.momentum).cmp(&momentum_weight(b.momentum)))
        .then(b.age.cmp(&a.age))
}

/// Momentum-aware re-rank of an (already rank-sorted) action list against the current open punch
/// (#246): stamp each action's [`Momentum`], stable-sort with [`momentum_cmp`], then group
/// same-repo actions adjacent within each band so batching is the natural reading order. With no
/// open punch today — or one that carries no repo (e.g. `bn in "lunch"` outside a worktree) — the
/// list is returned untouched, bit-identical to the pure priority/staleness order. Applied at
/// read time by the CLI surfaces, never at cache-write time, so the cache stays punch-free.
pub fn apply_momentum(cfg: &Config, actions: Vec<Action>, today: &str, now: i64) -> Vec<Action> {
    let Some(open) = crate::track::open_interval(cfg, today, now) else {
        return actions;
    };
    let Some(punch_repo) = open.repo.as_deref() else {
        return actions;
    };
    let mut actions = actions;
    for a in &mut actions {
        a.momentum = momentum_for(a, punch_repo, open.branch.as_deref());
    }
    actions.sort_by(momentum_cmp);
    group_adjacent(actions)
}

/// Keep same-repo actions adjacent within each (priority, category, momentum) run: later members
/// of a repo already seen in the run are pulled up behind its first occurrence, so the output
/// reads as batches per repo instead of interleaving them by staleness. Runs are never crossed —
/// bands and momentum tiers keep their order. Repo-less actions stay exactly where they were.
fn group_adjacent(actions: Vec<Action>) -> Vec<Action> {
    let same_run = |a: &Action, b: &Action| {
        a.priority == b.priority
            && a.category() == b.category()
            && momentum_weight(a.momentum) == momentum_weight(b.momentum)
    };
    let mut out: Vec<Action> = Vec::with_capacity(actions.len());
    let mut slots: Vec<Option<Action>> = actions.into_iter().map(Some).collect();
    let mut start = 0;
    while start < slots.len() {
        let Some(anchor) = slots[start].clone() else {
            start += 1;
            continue;
        };
        let mut end = start + 1;
        while end < slots.len() {
            let Some(next) = slots[end].as_ref() else {
                break;
            };
            if !same_run(&anchor, next) {
                break;
            }
            end += 1;
        }
        for i in start..end {
            let Some(a) = slots[i].take() else { continue };
            let repo = a.repo.clone();
            out.push(a);
            if let Some(repo) = repo {
                for slot in &mut slots[i + 1..end] {
                    let matches = slot
                        .as_ref()
                        .is_some_and(|b| b.repo.as_deref().is_some_and(|r| same_repo(r, &repo)));
                    if matches {
                        if let Some(action) = slot.take() {
                            out.push(action);
                        }
                    }
                }
            }
        }
        start = end;
    }
    out
}

/// The recency-cutoff window in hours: [`Config::attention_max_age_hours`] (default 24), widened
/// over the weekend on **Mondays** (+48h) so Friday's items still surface after Sat/Sun. Derived
/// from the local `today` date, so it tracks bn's day boundaries rather than UTC.
fn effective_cutoff_hours(cfg: &Config, today: &str) -> i64 {
    let base = cfg.attention_max_age_hours.max(0.0) as i64;
    // Monday is weekday_index 1 (0=Sun); reach back across the weekend to the prior Friday.
    let weekend = if crate::util::weekday_index(today) == Some(1) {
        48
    } else {
        0
    };
    base + weekend
}

/// The queue a just-ended focus block was holding back: the current **non-urgent** actions —
/// "while you were heads-down: 2 PRs to review, 1 goal". Derived live (not from a stored
/// snapshot) so it always reflects the desk as it stands at drain time. The inverse of the gate
/// [`next_actions`] applies while a block is active.
pub fn drained(cfg: &Config, today: &str, now: i64) -> Vec<Action> {
    let mut out = all_actions(cfg, today, now);
    out.retain(|a| !a.is_urgent());
    out
}

/// The trailing window (days) the WIP guard reads merge cadence over for its clear-by forecast —
/// the same default `bn metrics` uses, so the forecast on `bn next` and the metrics row agree.
const WIP_CADENCE_DAYS: i64 = 30;

/// The forward-looking WIP / focus guard as a single top-priority action — "you have N branches in
/// flight; finish <one> first". Emitted only when WIP is above the healthy band (the guard's own
/// `healthy` flag); a tidy desk surfaces nothing here. Priority 4 puts it above even broken-cron,
/// so it leads `bn next`: the nudge to *land* work before starting more.
fn from_wip(cfg: &Config, now: i64, today: &str) -> Vec<Action> {
    let g = metrics::wip_guard_for(cfg, now, WIP_CADENCE_DAYS, today);
    if g.healthy {
        return Vec::new();
    }
    let title = match &g.recommend {
        Some(rec) => format!("finish {rec} — {} branches in flight", g.in_flight),
        None => format!("{} branches in flight", g.in_flight),
    };
    let mut detail = String::new();
    if g.stale > 0 {
        detail.push_str(&format!("{} stale ≥4d", g.stale));
    }
    if let Some(reason) = &g.recommend_reason {
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        detail.push_str(reason);
    }
    if let Some(clear) = &g.clear_by {
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        detail.push_str(&format!("clear-by {clear}"));
    }
    if detail.is_empty() {
        detail = format!("{} in flight", g.in_flight);
    }
    // The recommendation is "repo/branch"; repos are directory names (no '/'), branches may
    // contain '/' — so the first segment is the repo. Cosmetic here (the guard is alone at
    // priority 4), but keeps the momentum badge honest.
    let (repo, branch) = match g.recommend.as_deref().and_then(|r| r.split_once('/')) {
        Some((r, b)) => (Some(r.to_string()), Some(b.to_string())),
        None => (None, None),
    };
    vec![Action {
        kind: ActionKind::Wip,
        title,
        detail,
        link: None,
        repo,
        branch,
        momentum: None,
        attn_id: None,
        est: None,
        priority: action_rank::GUARD.priority(),
        age: g.in_flight as i64,
        age_hours: None,
    }]
}

/// Cron failures, raised attn items, and *blocked* notes from the attention list. Stale/no-goal
/// note hygiene stays in `bn doctor` — it isn't a "do this next" action, so it's dropped here.
fn from_doctor(cfg: &Config, today: &str) -> Vec<Action> {
    // Repo todos are surfaced by `bn doctor`, not here — `bn next` is the curated do-this-next
    // list and already drops pure note hygiene, so pass no repo (Finding::Todo never arises).
    doctor::doctor(cfg, today, i64::MAX, None)
        .into_iter()
        .filter_map(|f| match f {
            Finding::Cron(c) => Some(Action {
                kind: ActionKind::Cron,
                title: format!("fix cron {}", c.job),
                detail: format!(
                    "{} · {} failure(s) on record",
                    c.repo.as_deref().unwrap_or("global"),
                    c.failures
                ),
                link: None,
                repo: c.repo,
                branch: c.branch,
                momentum: None,
                attn_id: None,
                est: None,
                priority: action_rank::URGENT.priority(),
                age: c.failures as i64,
                age_hours: None,
            }),
            Finding::Attn(a) => {
                let priority = action_rank::from_severity(&a.severity).priority();
                let kind = kind_for_attn_source(&a.source);
                // Rich context (e.g. a review's reasoning) for the detail/second view when the
                // raiser supplied it; otherwise the terse severity·source line.
                let detail = a
                    .detail
                    .clone()
                    .unwrap_or_else(|| format!("{} · {}", a.severity, a.source));
                Some(Action {
                    kind,
                    title: a.title,
                    detail,
                    link: a.link,
                    repo: a.repo,
                    branch: a.branch,
                    momentum: None,
                    attn_id: Some(a.id),
                    est: None,
                    priority,
                    age: 0,
                    age_hours: None,
                })
            }
            // Only blocked notes are actionable here; pure stale/no-goal nags are doctor's job.
            Finding::Note(n) if n.blockers > 0 => Some(Action {
                kind: ActionKind::Blocked,
                title: format!("unblock {}/{}", n.repo, n.branch),
                detail: format!(
                    "{} blocker(s), {} open todo(s) · idle {}d",
                    n.blockers, n.open_todos, n.idle_days
                ),
                link: None,
                repo: Some(n.repo),
                branch: Some(n.branch),
                momentum: None,
                attn_id: None,
                est: None,
                priority: action_rank::BLOCKED.priority(),
                age: n.idle_days,
                // Exempt from the recency cutoff: a blocker is active unfinished work — the longer
                // it sits the *more* it needs unblocking, so it must never age off the list.
                age_hours: None,
            }),
            Finding::Note(_) => None,
            // Not produced here (repo=None above); doctor owns repo-todo surfacing.
            Finding::Todo(_) => None,
            // Lingering captures are hygiene, not a next action — doctor nags, `bn q drain` fixes.
            Finding::Inbox(_) => None,
            // Not produced here (repo=None above); doctor owns MCP-resolvability nags.
            Finding::Mcp(_) => None,
            // Neither staff health signal is a curated "do this next" task: a missed scheduled run
            // (#273) and a stale/silent staff (#270, home machine down) are both `bn doctor` /
            // `bn staff` territory, so they stay off the `bn next` list like note hygiene.
            Finding::Staff(_) | Finding::StaleStaff(_) => None,
            // A cross-machine notes leak is a machine-setup misconfig surfaced by `bn doctor`, not
            // a per-repo task — keep it off the `bn next` action list. Inactive shell wrappers are
            // the same shape of problem: your shell's setup, not the next piece of work.
            Finding::NotesLeak(_) | Finding::ShellInit(_) => None,
            Finding::Backlog(_) => None,
        })
        .collect()
}

/// PRs that genuinely await *your* review (someone is waiting on you). Excludes `learning` PRs,
/// your own PRs (`BN_GITHUB_LOGIN`), and ones you've already approved/requested-changes on — see
/// [`ReviewPr::awaiting_review`]. A PR's priority rises as it nears/breaches the first-review SLA
/// ([`Config::sla_first_review_hours`]): over-SLA → priority 4 (above broken cron — others are now
/// blocked late), near → stays 3 but sorts ahead of its peers via the wait.
fn from_reviews(cfg: &Config, today: &str, now: i64) -> Vec<Action> {
    use crate::reviews::SlaState;
    let sla = cfg.sla_first_review_hours;
    let me = reviews::my_login();
    reviews::read()
        .prs
        .into_iter()
        // Others' PRs still awaiting *my* review: not mine (by author), not already acted on, and
        // still open — a merged/closed/abandoned PR (`is_dead`) is history, dropped here.
        .filter(|p| p.awaiting_review(me.as_deref()) && !p.is_dead())
        .map(|p| {
            let age = days_between(&p.updated_at, today);
            let state = p.sla_state(now, sla);
            // Over-SLA reviews jump above broken cron — someone's late-blocked on you.
            let priority = if state == Some(SlaState::Over) {
                action_rank::GUARD.priority()
            } else {
                action_rank::URGENT.priority()
            };
            // Surface the wait + SLA state in the detail line when we can measure it.
            let detail = match (p.wait_hours(now), state) {
                (Some(w), Some(s)) => {
                    let tag = match s {
                        SlaState::Over => "SLA over",
                        SlaState::Near => "SLA near",
                        SlaState::Ok => "SLA ok",
                    };
                    format!(
                        "{} {:.0}h · {tag} {:.0}h · {}",
                        crate::glyph::CLOCK,
                        w,
                        sla,
                        p.title
                    )
                }
                _ => p.title.clone(),
            };
            // Tiebreak by wait-hours when known (over-SLA, longest-waited first), else by day-age.
            let tie = p.wait_hours(now).map(|w| w as i64).unwrap_or(age);
            // The recency cutoff measures a review by its wait (hours), falling back to day-age
            // when the cache has no timestamp — a long-stale review ask ages off the list.
            let age_hours = Some(p.wait_hours(now).map(|w| w as i64).unwrap_or(age * 24));
            Action {
                kind: ActionKind::Review,
                title: format!("review {}#{} @{}", p.repo, p.number, p.author),
                detail,
                link: Some(p.url),
                repo: Some(p.repo),
                branch: None,
                momentum: None,
                attn_id: None,
                est: None,
                priority,
                age: tie,
                age_hours,
            }
        })
        .collect()
}

/// Your own in-flight PRs that have gone stale (`>= STALE_PR_DAYS` old). The action is normally to
/// *ping your reviewers* (nudge for review, rebase, address comments). When the `pr-queue` job has
/// stamped a PR's review verdict (its `mine`-tagged entry in the reviews cache), that enriches the
/// action: an **approved** PR flips to "merge it" (it's ready — land it), and a merged/closed PR is
/// dropped even if the event log still thinks it open. Absent that data, every stale PR is a plain
/// ping. Exempt from the recency cutoff: a PR is only pingable once it's already sat a couple of
/// days, so an age cap would delete every ping.
fn from_in_flight(cfg: &Config, now: i64) -> Vec<Action> {
    // My own PRs the producer fetched from GitHub, carrying state + review verdict. Indexed below
    // by repo#number to enrich the event-derived list. Empty when the job hasn't run → plain pings.
    let cache = reviews::read();
    let evs = events::read_events_across(&cfg.metrics_logs_all_machines());
    metrics::in_flight_prs(&evs, now)
        .into_iter()
        .filter(|p| p.age_days >= STALE_PR_DAYS)
        .filter_map(|p| {
            let meta = cache
                .prs
                .iter()
                .find(|m| m.tag == "mine" && m.repo == p.repo && m.number == p.number);
            // Merged/closed/abandoned → done; drop it even if the event log lags behind GitHub.
            if meta.is_some_and(|m| m.is_dead()) {
                return None;
            }
            let (title, detail, priority) = if meta.is_some_and(|m| m.is_approved()) {
                (
                    format!("merge your approved PR {}#{}", p.repo, p.number),
                    "approved — ready to land".to_string(),
                    action_rank::URGENT.priority(),
                )
            } else {
                (
                    format!("ping reviewers on your PR {}#{}", p.repo, p.number),
                    format!("open {}d, no review — nudge them", p.age_days),
                    action_rank::WARN.priority(),
                )
            };
            Some(Action {
                kind: ActionKind::Pr,
                title,
                detail,
                link: github_pr_url(&p.repo, p.number),
                repo: Some(p.repo),
                branch: None,
                momentum: None,
                attn_id: None,
                est: None,
                priority,
                age: p.age_days,
                age_hours: None,
            })
        })
        .collect()
}

/// Today's open daily goals — the plan you set for yourself. Ensures today's file (carrying
/// yesterday forward, same as `bn daily`); on any read error it just contributes nothing.
fn from_goals(cfg: &Config, today: &str) -> Vec<Action> {
    let daily = daily::view(cfg, today);
    #[allow(clippy::needless_else)]
    let Some(daily) = Some(daily) else {
        return Vec::new();
    };
    daily
        .open()
        .map(|g| Action {
            kind: ActionKind::Goal,
            title: g.text.clone(),
            detail: "today's goal".to_string(),
            link: g.link.clone(),
            repo: None,
            branch: None,
            momentum: None,
            attn_id: None,
            est: g.est.clone(),
            priority: action_rank::INFO.priority(),
            age: 0,
            age_hours: None,
        })
        .collect()
}

/// Whole days from `earlier` (an ISO-8601 timestamp or `YYYY-MM-DD`) to `today` (`YYYY-MM-DD`),
/// clamped at 0. Best-effort: only the leading `YYYY-MM-DD` of each is compared, so a full RFC-3339
/// `updated_at` works without a date-time parser. Unparseable input → 0.
fn days_between(earlier: &str, today: &str) -> i64 {
    let date = earlier.get(..10).unwrap_or(earlier);
    crate::util::idle_days_since(date, today)
}

/// A GitHub PR URL from an `owner/name` repo. Returns `None` for any other shape (e.g. an ADO
/// identifier) rather than fabricate a wrong link — better no link than a broken one.
fn github_pr_url(repo: &str, number: i64) -> Option<String> {
    (repo.matches('/').count() == 1 && !repo.contains(' '))
        .then(|| format!("https://github.com/{repo}/pull/{number}"))
}

/// Maximum age for the precomputed attention cache. The warmer runs every five minutes; this keeps
/// a missed/stuck warmer from serving a week-old snapshot forever.
const CACHE_MAX_AGE_SECS: i64 = 10 * 60;

/// Path to the precomputed attention cache. `bn next --cached` reads this snapshot so the prefix-J
/// picker opens instantly instead of re-walking every branch note (`next_actions` is I/O-heavy on
/// a busy machine). Kept warm by the `attention-sync` job, and self-healed on a miss/stale cache.
pub fn cache_path() -> PathBuf {
    state_dir().join("attention.json")
}

/// The cached attention list plus when it was taken, so a reader can show staleness if it wants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionCache {
    pub generated_at: String,
    #[serde(default)]
    pub generated_at_epoch: i64,
    pub actions: Vec<Action>,
}

/// Write an already-computed action list to [`cache_path`]. Returns the I/O error rather than
/// swallowing it, so the refresher can report a failed write instead of silently going stale.
pub fn write_cache(actions: &[Action]) -> std::io::Result<()> {
    write_cache_at(actions, crate::util::now_epoch_secs())
}

fn write_cache_at(actions: &[Action], now: i64) -> std::io::Result<()> {
    let cache = AttentionCache {
        generated_at: stamp_at(now),
        generated_at_epoch: now,
        actions: actions.to_vec(),
    };
    let json = serde_json::to_string(&cache)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let path = cache_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)
}

/// Recompute the attention list and rewrite the cache — what `bn next --refresh` (the
/// `attention-sync` cron) calls. Read-only against the world, write-only to the cache.
pub fn refresh_cache(cfg: &Config, today: &str, now: i64) -> std::io::Result<()> {
    write_cache_at(&next_actions(cfg, today, now), now)
}

/// Remove the precomputed attention cache. Returns `Ok(false)` when there was no cache to clear.
pub fn clear_cache() -> std::io::Result<bool> {
    match std::fs::remove_file(cache_path()) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Read the cached attention list. `None` when the cache is absent or unparseable — never an
/// error: a missing cache just means "not refreshed yet", so the caller falls back to a live
/// compute (and warms the cache for next time). This is the raw reader; user-facing fast paths
/// should call [`read_fresh_cache`] so stale snapshots cannot linger indefinitely.
pub fn read_cache() -> Option<Vec<Action>> {
    let text = std::fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str::<AttentionCache>(&text)
        .ok()
        .map(|c| c.actions)
}

/// Read the cached attention list only if it is fresh enough to trust. Older caches are treated as
/// misses so callers recompute live and rewrite the snapshot.
pub fn read_fresh_cache(now: i64) -> Option<Vec<Action>> {
    let path = cache_path();
    let text = std::fs::read_to_string(&path).ok()?;
    let cache = serde_json::from_str::<AttentionCache>(&text).ok()?;
    if !cache_is_fresh(&path, &cache, now) {
        return None;
    }
    Some(cache.actions)
}

/// Return a fresh cached list when available, otherwise compute live and warm the cache.
pub fn cached_or_compute(cfg: &Config, today: &str, now: i64) -> Vec<Action> {
    match read_fresh_cache(now) {
        Some(a) => a,
        None => {
            let a = next_actions(cfg, today, now);
            crate::util::best_effort(write_cache_at(&a, now), "next: warm attention cache");
            a
        }
    }
}

fn cache_is_fresh(path: &Path, cache: &AttentionCache, now: i64) -> bool {
    let generated = if cache.generated_at_epoch > 0 {
        Some(cache.generated_at_epoch)
    } else {
        file_mtime_epoch(path)
    };
    // A future-dated timestamp (clock skew or a corrupt cache) must not read as fresh: otherwise
    // `now - ts` goes negative and stays under the max age, freezing a stale snapshot until the
    // wall clock catches up and hiding newly-surfaced actions. Require `ts <= now` as well.
    generated.is_some_and(|ts| ts <= now && now.saturating_sub(ts) <= CACHE_MAX_AGE_SECS)
}

fn file_mtime_epoch(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
}

fn stamp_at(epoch_secs: i64) -> String {
    chrono::DateTime::from_timestamp(epoch_secs, 0)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(crate::util::now_stamp)
}


