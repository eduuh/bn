//! Morning-planning findings gatherers (#897) — wire the existing gather surfaces into the
//! append-only [`crate::findings`] store so `bn today` / the later planner (#898–#899) have a
//! durable, queryable morning-planning packet.
//!
//! # Why an adapter layer
//!
//! `bn findings` (#576) is a real store but was unwired: nothing recorded into it, so
//! `bn findings sources` was empty. Rather than change the existing write paths (feed / attention
//! / daily) or edit the meetings/m365 shell gatherers, this module adds a thin **Rust adapter
//! layer** that *snapshots already-local caches* into the findings store. Each adapter is a pure,
//! offline read of a store some other job already populated — so wiring findings here is:
//!
//! * **additive** — it touches no existing feed/attention/daily side effect;
//! * **DRY** — it reuses the caches the gatherers already fill (zero duplicate fetch, no network);
//! * **Rust-only** — no new shell.
//!
//! It maps cleanly onto the design doc's future `gather-*` cron consolidation (#902) without doing
//! that consolidation now.
//!
//! # Source taxonomy, stable ids, and item-key rules
//!
//! [`sources`] is the single source of truth for the stable findings source ids the later
//! planner/today slices depend on. Each source has a planning [`Tier`] and an adapter that emits
//! feed-shaped `{key,text,url}` items plus an optional structured `raw` snapshot (so the planner
//! can rank without re-fetching). The **item key** is stable within its source so a run-over-run
//! diff can tell "same item as yesterday" apart from a new one:
//!
//! | source id        | tier        | reads (existing local store)                       | item key            |
//! |------------------|-------------|----------------------------------------------------|---------------------|
//! | `pr-review-queue`| inbound     | [`crate::reviews`] cache — PRs awaiting *your* review | `<repo>#<number>` |
//! | `my-prs`         | in-flight   | [`crate::reviews`] cache — your own open PRs        | `<repo>#<number>`   |
//! | `meetings`       | inbound     | [`crate::feed`] source `meetings` (passthrough)     | upstream feed key   |
//! | `email`          | inbound     | [`crate::feed`] source `email` (passthrough)        | upstream feed key   |
//! | `teams`          | inbound     | [`crate::feed`] source `teams` (passthrough)        | upstream feed key   |
//! | `owe`            | inbound     | [`crate::feed`] source `owe` (passthrough)          | upstream feed key   |
//! | `night-shift`    | in-flight   | [`crate::night::report_since`] — last night's PR-ready dispatches | dispatch id |
//! | `cron-health`    | health      | [`crate::crons::alerting_crons`] over cron status   | cron job name       |
//! | `sentinel`       | health      | [`crate::attention`] items raised by `staff-sentinel` | attn key / title slug |
//! | `stale-branches` | in-flight   | [`crate::doctor::note_findings`] (stale/no_goal/blockers) | `<repo>/<branch>` |
//! | `quarter`        | alignment   | [`crate::quarter::read_quarter`] — current quarter's open goals | goal key       |
//! | `dojo`           | growth      | [`crate::dojo::DojoStore::load_cards`] (capped)     | card id             |
//!
//! An adapter that finds nothing records nothing (no empty run rows), so off the work laptop the
//! meetings/email/teams/owe passthroughs — and the quarter/dojo/night-shift sources on a fresh box
//! — are clean no-ops. [`run`] executes every adapter and appends one findings run per non-empty
//! source.

use crate::attention::AttnItem;
use crate::crons;
use crate::doctor::{self, Finding, NoteFinding};
use crate::dojo::{Card, DojoStore};
use crate::events::CronJobStatus;
use crate::feed::{self, FeedItem};
use crate::night::{self, NightOutcome, NightReportItem};
use crate::quarter::{self, QuarterGoal};
use crate::resolver::Config;
use crate::reviews::{self, ReviewPr};
use serde::Serialize;

/// How far back the `night-shift` gatherer looks for reviewable dispatches — one shift, matching
/// the AM chief card's 24h window so the two surfaces agree on "last night".
const NIGHT_LOOKBACK_HOURS: i64 = 24;

/// Cap on the `dojo` (growth) snapshot: the most-recently-mined cards only, so the append-only
/// findings store can't grow unbounded as cards accumulate over daily cron runs.
const DOJO_SNAPSHOT_CAP: usize = 15;

/// The morning-planning taxonomy tier a findings source belongs to. Stored nowhere on the run
/// record itself (the source id is identity); the planner maps id → tier via [`sources`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tier {
    /// Needs your response: review queue, meetings, mail/Teams.
    Inbound,
    /// Your work in progress: your open PRs, stale/blocked branches.
    InFlight,
    /// Estate health: failing crons, sentinel patrol output.
    Health,
    /// Goal alignment inputs: the current quarter's open goals (`quarter`).
    Alignment,
    /// Growth inputs: mined learning cards (`dojo`).
    Growth,
}

impl Tier {
    /// The stable kebab-case label used in reports and JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Inbound => "inbound",
            Tier::InFlight => "in-flight",
            Tier::Health => "health",
            Tier::Alignment => "alignment",
            Tier::Growth => "growth",
        }
    }
}

/// What one adapter gathered: the feed-shaped items to record, plus an optional untransformed
/// snapshot of the source structs so the planner can re-parse without a re-fetch.
#[derive(Debug, Default, Clone)]
pub struct GatherOut {
    pub items: Vec<FeedItem>,
    pub raw: Option<String>,
}

impl GatherOut {
    fn new(items: Vec<FeedItem>, raw: Option<String>) -> Self {
        Self { items, raw }
    }
}

/// A registered morning-planning findings source: a stable id, its planning tier, and the adapter
/// that snapshots its already-local state into feed items (pure, offline, no re-fetch).
pub struct GatherSource {
    pub id: &'static str,
    pub tier: Tier,
    pub gather: fn(&Config) -> GatherOut,
}

/// The explicit registry — the single source of truth for the stable findings source ids the
/// planner/today slices depend on. Ordered by tier (inbound → in-flight → health) for a stable,
/// readable gather report.
pub fn sources() -> Vec<GatherSource> {
    vec![
        GatherSource {
            id: "pr-review-queue",
            tier: Tier::Inbound,
            gather: gather_pr_review_queue,
        },
        GatherSource {
            id: "meetings",
            tier: Tier::Inbound,
            gather: gather_meetings,
        },
        GatherSource {
            id: "email",
            tier: Tier::Inbound,
            gather: gather_email,
        },
        GatherSource {
            id: "teams",
            tier: Tier::Inbound,
            gather: gather_teams,
        },
        GatherSource {
            id: "owe",
            tier: Tier::Inbound,
            gather: gather_owe,
        },
        GatherSource {
            id: "my-prs",
            tier: Tier::InFlight,
            gather: gather_my_prs,
        },
        GatherSource {
            id: "stale-branches",
            tier: Tier::InFlight,
            gather: gather_stale_branches,
        },
        GatherSource {
            id: "night-shift",
            tier: Tier::InFlight,
            gather: gather_night_shift,
        },
        GatherSource {
            id: "cron-health",
            tier: Tier::Health,
            gather: gather_cron_health,
        },
        GatherSource {
            id: "sentinel",
            tier: Tier::Health,
            gather: gather_sentinel,
        },
        GatherSource {
            id: "quarter",
            tier: Tier::Alignment,
            gather: gather_quarter,
        },
        GatherSource {
            id: "dojo",
            tier: Tier::Growth,
            gather: gather_dojo,
        },
    ]
}

/// One source's outcome in a gather run.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SourceReport {
    pub source: String,
    pub tier: String,
    pub items: usize,
    /// Whether a findings run was appended (false when the adapter found nothing).
    pub recorded: bool,
    /// The run id of the appended record, when one was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// What one [`run`] did across every registered source.
#[derive(Debug, Clone, Serialize, Default)]
pub struct GatherReport {
    pub sources: Vec<SourceReport>,
}

impl GatherReport {
    /// Sources that appended a findings run this pass.
    pub fn recorded(&self) -> usize {
        self.sources.iter().filter(|s| s.recorded).count()
    }

    /// Total finding items recorded across all sources this pass.
    pub fn total_items(&self) -> usize {
        self.sources
            .iter()
            .filter(|s| s.recorded)
            .map(|s| s.items)
            .sum()
    }
}

/// Run every registered adapter and append one findings run per **non-empty** source. Empty
/// adapters are reported (so a run window shows what was checked) but record nothing, keeping the
/// store free of empty rows. A store write error propagates so a cron surfaces it.
pub fn run(cfg: &Config) -> std::io::Result<GatherReport> {
    let mut report = GatherReport::default();
    for src in sources() {
        let out = (src.gather)(cfg);
        let items = out.items.len();
        let run_id = if items == 0 {
            None
        } else {
            Some(crate::findings::record(src.id, None, out.items, out.raw)?.run_id)
        };
        report.sources.push(SourceReport {
            source: src.id.to_string(),
            tier: src.tier.as_str().to_string(),
            items,
            recorded: run_id.is_some(),
            run_id,
        });
    }
    Ok(report)
}

// ── Adapters ─────────────────────────────────────────────────────────────────────────────────
//
// Each adapter is `read a local store` + `pure map to items`. The pure mappers below are unit-
// tested directly; the readers are thin calls to already-tested store functions.

/// `pr-review-queue` — PRs awaiting *your* review, from the review-queue cache the `pr-queue` job
/// writes. Learning PRs, your own PRs, and ones you've already acted on are excluded.
fn gather_pr_review_queue(_cfg: &Config) -> GatherOut {
    let cache = reviews::read();
    let me = reviews::my_login();
    let prs: Vec<&ReviewPr> = cache
        .prs
        .iter()
        .filter(|p| p.awaiting_review(me.as_deref()))
        .collect();
    let items = prs.iter().map(|p| review_item(p)).collect();
    GatherOut::new(items, raw_of(&prs))
}

/// `my-prs` — your own still-open PRs (the `mine`-tagged, not-dead cache entries): stuck/open PR
/// state you may need to nudge, rebase, or land.
fn gather_my_prs(_cfg: &Config) -> GatherOut {
    let cache = reviews::read();
    let prs: Vec<&ReviewPr> = cache
        .prs
        .iter()
        .filter(|p| p.tag == "mine" && !p.is_dead())
        .collect();
    let items = prs.iter().map(|p| my_pr_item(p)).collect();
    GatherOut::new(items, raw_of(&prs))
}

/// `cron-health` — crons in a chronic-failure state, from the same watchdog signal `cron-health`
/// raises to attention. Reads the rolled-up cron status; records nothing when all crons are green.
fn gather_cron_health(_cfg: &Config) -> GatherOut {
    let runs = crate::events::cron_status();
    let alerting = crons::alerting_crons(&runs, crons::ALERT_MIN_FAILURES);
    let items = alerting.iter().map(|s| cron_item(s)).collect();
    GatherOut::new(items, raw_of(&alerting))
}

/// `sentinel` — last night's Debt Sentinel patrol, snapshotted from the attention items it raised
/// (source [`crate::staff::sentinel::SOURCE`]) rather than re-running the expensive patrol.
fn gather_sentinel(_cfg: &Config) -> GatherOut {
    let today = crate::util::today();
    let items: Vec<AttnItem> = crate::attention::open_items(&today)
        .into_iter()
        .filter(|i| i.source == crate::staff::sentinel::SOURCE)
        .collect();
    let feed_items = items.iter().map(attn_item).collect();
    GatherOut::new(feed_items, raw_of(&items))
}

/// `stale-branches` — local branch notes with unfinished work that is stale, goal-less, or
/// blocked, reusing `bn doctor`'s exact note-finding logic (offline filesystem read).
fn gather_stale_branches(cfg: &Config) -> GatherOut {
    let today = crate::util::today();
    let findings = doctor::note_findings(cfg, &today, doctor::DEFAULT_STALE_DAYS);
    let notes: Vec<&NoteFinding> = findings
        .iter()
        .filter_map(|f| match f {
            Finding::Note(n) => Some(n),
            _ => None,
        })
        .collect();
    let items = notes.iter().map(|n| note_item(n)).collect();
    GatherOut::new(items, raw_of(&notes))
}

/// `night-shift` — last night's autonomous dispatches that left a draft PR waiting on review
/// (outcome [`NightOutcome::PrReady`]), from the shared [`night::report_since`] read the AM chief
/// card uses — so the morning card and this findings source agree on "last night's work". In-flight
/// work you need to review; keyed by the dispatch id, url = the PR link.
fn gather_night_shift(_cfg: &Config) -> GatherOut {
    let cutoff = crate::util::now_epoch_secs() - NIGHT_LOOKBACK_HOURS * 3600;
    let reviewable: Vec<NightReportItem> = night::report_since(cutoff)
        .into_iter()
        .filter(|it| it.outcome == NightOutcome::PrReady && it.link.is_some())
        .collect();
    let items = reviewable.iter().map(night_item).collect();
    GatherOut::new(items, raw_of(&reviewable))
}

/// `quarter` — the current quarter's still-open goals (goal-alignment input), reusing the shared
/// quarter store read; the single top at-risk goal is flagged. Records nothing when there's no
/// current quarter file or it has no goals yet.
fn gather_quarter(cfg: &Config) -> GatherOut {
    let today = crate::util::today();
    let Some(qid) = quarter::quarter_id(&today) else {
        return GatherOut::default();
    };
    let Ok(Some(q)) = quarter::read_quarter(cfg, &qid) else {
        return GatherOut::default();
    };
    let at_risk = q.top_at_risk().map(|g| g.key.clone());
    let goals: Vec<&QuarterGoal> = q.open().collect();
    let items = goals
        .iter()
        .map(|g| quarter_item(g, at_risk.as_deref() == Some(g.key.as_str())))
        .collect();
    GatherOut::new(items, raw_of(&goals))
}

/// `dojo` — mined learning cards (growth input), reusing the shared dojo store read, capped to the
/// most-recently-mined [`DOJO_SNAPSHOT_CAP`] so the append-only findings store can't bloat as cards
/// accumulate. Records nothing when no cards are mined.
fn gather_dojo(cfg: &Config) -> GatherOut {
    let mut cards = DojoStore::from_config(cfg).load_cards().unwrap_or_default();
    cards.sort_by_key(|c| std::cmp::Reverse(c.created));
    cards.truncate(DOJO_SNAPSHOT_CAP);
    let items = cards.iter().map(dojo_item).collect();
    GatherOut::new(items, raw_of(&cards))
}

fn gather_meetings(_cfg: &Config) -> GatherOut {
    feed_passthrough("meetings")
}
fn gather_email(_cfg: &Config) -> GatherOut {
    feed_passthrough("email")
}
fn gather_teams(_cfg: &Config) -> GatherOut {
    feed_passthrough("teams")
}
fn gather_owe(_cfg: &Config) -> GatherOut {
    feed_passthrough("owe")
}

/// Snapshot one feed source's current items into findings unchanged — the feed already holds the
/// gathered `{key,text,url}` set the shell gatherer produced; findings just gives it history. The
/// whole [`crate::feed::FeedSource`] (with its `generated_at`) is kept as `raw`.
fn feed_passthrough(source: &str) -> GatherOut {
    match feed::read_all().into_iter().find(|s| s.source == source) {
        Some(src) if !src.items.is_empty() => {
            let raw = serde_json::to_string(&src).ok();
            GatherOut::new(src.items, raw)
        }
        _ => GatherOut::default(),
    }
}

// ── Pure item mappers ──────────────────────────────────────────────────────────────────────────

/// A PR awaiting your review → a feed item keyed `<repo>#<number>`.
fn review_item(p: &ReviewPr) -> FeedItem {
    FeedItem {
        key: pr_key(p),
        text: format!("{}#{} {} — @{}", p.repo, p.number, p.title.trim(), p.author),
        url: p.url.clone(),
    }
}

/// One of your own open PRs → a feed item keyed `<repo>#<number>`, flagged when it's approved and
/// just waiting on a merge (the cue to land it).
fn my_pr_item(p: &ReviewPr) -> FeedItem {
    let ready = if p.is_approved() {
        " (approved — ready to land)"
    } else {
        ""
    };
    FeedItem {
        key: pr_key(p),
        text: format!("{}#{} {}{}", p.repo, p.number, p.title.trim(), ready),
        url: p.url.clone(),
    }
}

fn pr_key(p: &ReviewPr) -> String {
    format!("{}#{}", p.repo, p.number)
}

/// A failing cron → a feed item keyed by the job name (matches the `cron-health` attention key).
fn cron_item(s: &CronJobStatus) -> FeedItem {
    FeedItem {
        key: s.job.clone(),
        text: format!(
            "cron '{}' failing — {}/{} runs failed (last exit {})",
            s.job, s.failures, s.runs, s.last_exit
        ),
        url: String::new(),
    }
}

/// A stale/blocked branch note → a feed item keyed `<repo>/<branch>`, text = the doctor flags.
fn note_item(n: &NoteFinding) -> FeedItem {
    FeedItem {
        key: format!("{}/{}", n.repo, n.branch),
        text: format!("{}/{} — {}", n.repo, n.branch, n.flags.join(", ")),
        url: String::new(),
    }
}

/// A sentinel-raised attention item → a feed item keyed by the attention item's own dedupe key
/// (stable across patrols), falling back to a sanitized slug of the title when unkeyed.
fn attn_item(a: &AttnItem) -> FeedItem {
    let key = a
        .key
        .clone()
        .unwrap_or_else(|| feed::sanitize_source(&a.title));
    FeedItem {
        key,
        text: a.title.clone(),
        url: a.link.clone().unwrap_or_default(),
    }
}

/// A night-shift reviewable dispatch → a feed item keyed by the dispatch id; the draft PR link is
/// the url, and the outcome label (`PR ready`) is appended so the planner sees why it's here.
fn night_item(it: &NightReportItem) -> FeedItem {
    FeedItem {
        key: it.id.clone(),
        text: format!(
            "{}/{} {} — {}",
            it.repo,
            it.branch,
            it.title.trim(),
            it.outcome.label()
        ),
        url: it.link.clone().unwrap_or_default(),
    }
}

/// A still-open quarter goal → a feed item keyed by the goal `key` (stable across quarters); the
/// single top at-risk goal is flagged so goal alignment surfaces the drift. No url.
fn quarter_item(g: &QuarterGoal, at_risk: bool) -> FeedItem {
    let flag = if at_risk { " ⚠ at-risk" } else { "" };
    FeedItem {
        key: g.key.clone(),
        text: format!("{}{}", g.text.trim(), flag),
        url: String::new(),
    }
}

/// A mined dojo card → a feed item keyed by the stable card id; text = `<kind> · <repo> — <prompt
/// snippet>` (the answer is never included). No url.
fn dojo_item(c: &Card) -> FeedItem {
    let prompt = c.prompt.trim();
    let snippet: String = prompt.chars().take(80).collect();
    let ellipsis = if prompt.chars().count() > 80 {
        "…"
    } else {
        ""
    };
    FeedItem {
        key: c.id.clone(),
        text: format!("{} · {} — {}{}", c.kind.label(), c.repo, snippet, ellipsis),
        url: String::new(),
    }
}

/// Serialize a borrowed slice as the `raw` snapshot, or `None` when empty (so an empty adapter
/// records nothing).
fn raw_of<T: Serialize>(items: &[T]) -> Option<String> {
    if items.is_empty() {
        None
    } else {
        serde_json::to_string(items).ok()
    }
}
