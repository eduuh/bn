//! The action-centric command center (#522): a pure fold that unifies every actionable signal bn
//! tracks into one ranked list, each carrying a **suggested starting-point prompt** the owner edits
//! before dispatching an agent at it.
//!
//! The dashboard's heart (Epic #488) is a place to *drive development across all repos*: attention
//! items, open todos, plan-reviews awaiting the human, blocked notes, failing crons, stale in-flight
//! PRs, and today's goals all become [`ActionItem`]s. From any item the owner clicks **Start agent**
//! — *urgent* (dispatch now) or *queued* — and a composer opens **pre-filled** with that item's
//! [`ActionItem::suggested_prompt`] (a per-type template), so the dispatched agent gets a real
//! instruction from the owner rather than a blank slate.
//!
//! Everything here is pure over its inputs (the already-read source viewmodels + a `now` clock), so
//! the web layer just gathers the sources and calls [`build_actions`]; the ranking, the
//! prompt-template policy, and the default urgency all live here under unit test.

use crate::action_rank;
use crate::agents::{AgentCatalog, AgentDef, AgentKind};
use crate::attention::AttnItem;
use crate::crons::CronView;
use crate::model::Item;
use crate::orchestrate::{ItemStatus, QueueItem};
use crate::plan_review::{Review, ReviewStatus};
use crate::rollups::SummaryEntry;
use crate::suggestions::{Suggestion, SuggestionStatus};
use crate::util;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;

/// An in-flight PR with no status change for this long is treated as *stale* and surfaced as an
/// action (6h). Tuned to catch a PR that's silently wedged on CI/review without flagging one that's
/// merely mid-agent-turn.
pub const STALE_PR_SECS: i64 = 6 * 3600;

/// A note todo whose creation date is older than this (30 days) with no fresher signal is treated
/// as *stale* backlog: it still ranks below every fresh todo and the UI folds it under a "show all"
/// expander, so a months-old spike never buries a todo on an actively-worked branch — but it stays
/// reachable. Only affects the `todo` kind; blockers/attention/reviews are always pinned on top.
pub const STALE_TODO_SECS: i64 = 30 * 24 * 3600;

/// What a given [`ActionItem`] came from — the item's `type`, driving its icon, prompt template,
/// and default urgency. Serialized kebab-case to match the rest of the web contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionKind {
    /// An open attention item (`bn attn` / `bn doctor`) — a person, cron, or agent raised it.
    Attention,
    /// A plan-review the agent submitted and is paused on, waiting for the human (#349).
    PlanReview,
    /// An open todo on an active branch note.
    Todo,
    /// A blocker recorded on an active branch note.
    Blocker,
    /// A cron job whose most recent run exited non-zero.
    FailingCron,
    /// A dispatched orchestrate item whose PR has sat without a status change past [`STALE_PR_SECS`].
    StalePr,
    /// An open daily goal for today.
    Goal,
    /// A GitHub issue surfaced by the global search palette (#565). `build_actions` never emits
    /// this; it's the shared vocabulary for "work on this issue → start an agent" from a search
    /// result, so the reused composer renders a correct badge + prompt.
    Issue,
    /// An **accepted** suggested task from Today's agent (#577), promoted into the action feed
    /// (#578): the owner approved it on Today, so it becomes actionable here with the shared
    /// Start-agent CTA. Only `Accepted` suggestions become actions — `Suggested`/`Dismissed` never do.
    Suggestion,
}

impl ActionKind {
    /// A short, stable machine token (also what `type` serializes to).
    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::Attention => "attention",
            ActionKind::PlanReview => "plan-review",
            ActionKind::Todo => "todo",
            ActionKind::Blocker => "blocker",
            ActionKind::FailingCron => "failing-cron",
            ActionKind::StalePr => "stale-pr",
            ActionKind::Goal => "goal",
            ActionKind::Issue => "issue",
            ActionKind::Suggestion => "suggestion",
        }
    }
}

/// How urgently the composer defaults to running the item: dispatch *now*, or add to the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Urgency {
    /// Dispatch immediately (`POST /api/dispatch`) — the human's blocking queue.
    Urgent,
    /// Enqueue for the next orchestrate tick (`POST /api/orchestrate/queue`) — background work.
    Queued,
}

/// One unified actionable item. `payload` carries the dispatch coordinates the composer needs
/// (repo/branch/target and any type-specific id), and `suggested_prompt` is the editable
/// starting-point instruction for the agent.
#[derive(Debug, Clone, Serialize)]
pub struct ActionItem {
    /// Stable, type-prefixed id (`attn:a3`, `todo:repo/branch/t1`, …) — unique across kinds so the
    /// list can key on it and de-dupe.
    pub id: String,
    /// The item's origin kind (serialized as `type`).
    #[serde(rename = "type")]
    pub kind: ActionKind,
    /// One-line headline for the row.
    pub title: String,
    /// Human-readable provenance (`attention`, `plan-review`, `cron: <job>`, `note todo`, …).
    pub source: String,
    /// `urgent` | `warn` | `info` — drives the row's tone/sort, mirroring attention severities.
    pub severity: String,
    /// The repo this acts in, when known (required to one-click dispatch; else the composer asks).
    pub repo: Option<String>,
    /// The branch this acts on, when known.
    pub branch: Option<String>,
    /// A deep link (PR url, review link) when the item has one.
    pub link: Option<String>,
    /// Extra multi-line context for the expanded row (a plan body, a cron's last error, …).
    pub detail: Option<String>,
    /// Dispatch hints the composer routes on: `{ repo?, branch?, target?, review_id?, cron?, pr? }`.
    pub payload: Value,
    /// The pre-filled, editable agent prompt — a per-type template the owner tweaks before launch.
    pub suggested_prompt: String,
    /// Whether the composer opens on *Urgent* (dispatch) or *Queue*.
    pub default_urgency: Urgency,
    /// Epoch-seconds the underlying signal was raised — the recency tiebreaker.
    pub created: i64,
    /// True for a low-signal, stale backlog todo (created past [`STALE_TODO_SECS`] ago, or lacking
    /// a parseable date). Always `false` for non-todo kinds. The UI de-emphasizes these and folds
    /// them under the todo section's "show all" expander so urgent work stays visible.
    pub stale: bool,
}

/// The already-read source viewmodels the fold unifies. Every field is borrowed, so the caller owns
/// the reads; `now` is the clock the staleness test uses (injected for deterministic tests).
#[derive(Default)]
pub struct ActionSources<'a> {
    /// Open attention items for today (`attention::open_items`).
    pub attn: &'a [AttnItem],
    /// Every plan-review (`plan_review::list`); only `Pending` ones become actions.
    pub reviews: &'a [Review],
    /// The machine cron registry + last-run health (`crons::status_views`).
    pub crons: &'a [CronView],
    /// Branch-note rollups (`rollups::summary`) — the todo/blocker source.
    pub notes: &'a [SummaryEntry],
    /// Today's daily goals (`daily::read(...).front.goals`).
    pub goals: &'a [Item],
    /// Every orchestrate queue item across live runs (flattened) — the stale-PR source.
    pub queue: &'a [QueueItem],
    /// Today's agent suggestions (`suggestions::read`/`list`) — only the **accepted** ones become
    /// actions (#578); `Suggested`/`Dismissed` are skipped by the fold.
    pub suggestions: &'a [Suggestion],
    /// The current epoch-seconds clock.
    pub now: i64,
}

/// Truncate a possibly-multiline body to a short single line for a row title.
fn one_line(text: &str, max: usize) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    if first.chars().count() > max {
        let mut s: String = first.chars().take(max.saturating_sub(1)).collect();
        s.push('…');
        s
    } else {
        first.to_string()
    }
}

// ---------------------------------------------------------------------------
// Prompt templates — the per-type starting-point instruction policy (#522).
// ---------------------------------------------------------------------------

/// The suggested prompt for an attention item. An `assist`/`auto` item already carries the agent
/// prompt its raiser wrote — honor it verbatim; otherwise template an investigate-and-resolve.
fn prompt_attention(item: &AttnItem) -> String {
    if let Some(p) = item
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return p.to_string();
    }
    let mut s = format!(
        "Investigate and resolve this attention item: {}.",
        item.title.trim()
    );
    if let Some(d) = item
        .detail
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        s.push_str("\n\nContext:\n");
        s.push_str(d);
    }
    s.push_str("\n\nWhen it's genuinely resolved, land the fix and ack the item.");
    s
}

/// The suggested prompt for a pending plan-review: review the plan and decide.
fn prompt_plan_review(r: &Review) -> String {
    format!(
        "Review the plan below for {}/{}. Sanity-check the approach, files-to-touch, and risks, \
then either approve it or leave specific, actionable change requests.\n\nPlan:\n{}",
        r.repo,
        r.branch,
        r.plan.trim()
    )
}

/// The suggested prompt for an open note todo: pick it up and drive it to a landed PR.
fn prompt_todo(entry: &SummaryEntry, text: &str) -> String {
    format!(
        "Pick up this open todo in {}/{} and drive it to a landed PR:\n\n{}\n\nKeep the branch note's \
todos and decisions up to date as you go.",
        entry.repo,
        entry.branch,
        text.trim()
    )
}

/// The suggested prompt for a note blocker: unblock the branch.
fn prompt_blocker(entry: &SummaryEntry, text: &str) -> String {
    format!(
        "Unblock {}/{}. The recorded blocker is:\n\n{}\n\nInvestigate the root cause, clear it, and \
note the resolution as a decision on the branch note.",
        entry.repo,
        entry.branch,
        text.trim()
    )
}

/// The suggested prompt for a failing cron: reproduce, root-cause, fix.
fn prompt_failing_cron(job: &CronView) -> String {
    let exit = job
        .last_exit
        .map(|c| format!(" (last exit {c})"))
        .unwrap_or_default();
    let mut s = format!(
        "Investigate and fix the failing cron job '{}'{exit}. Reproduce the failure by running its \
script, find the root cause, and land a fix so the next scheduled run is green.",
        job.job
    );
    if let Some(path) = job.script_path.as_deref() {
        s.push_str(&format!("\n\nScript: {path}"));
    }
    if let Some(output) = job
        .last_output
        .as_deref()
        .map(str::trim)
        .filter(|output| !output.is_empty())
    {
        s.push_str("\n\nLatest captured output:\n");
        s.push_str(output);
    }
    s
}

/// The suggested prompt for a stale in-flight PR: get it moving again.
fn prompt_stale_pr(item: &QueueItem) -> String {
    let branch = item.branch.as_deref().unwrap_or("the branch");
    let pr = item.pr.as_deref().unwrap_or("its PR");
    format!(
        "The in-flight PR for {}/{} ({}) has had no progress for a while. Check its CI and review \
status, resolve whatever is blocking it (failing checks, review comments, conflicts), and drive it \
to land.",
        item.repo, branch, pr
    )
}

/// The suggested prompt for an open daily goal.
fn prompt_goal(goal: &Item) -> String {
    format!(
        "Make concrete progress on today's goal:\n\n{}\n\nBreak it into the next shippable step and \
take it as far as you can toward done.",
        goal.text.trim()
    )
}

/// The suggested prompt for an accepted suggestion (#578): drive the approved proposal, carrying
/// its rationale and source finding so the agent has the same context the owner reviewed.
fn prompt_suggestion(s: &Suggestion) -> String {
    let mut p = format!(
        "Take on this approved task from Today's agent and drive it to a landed PR:\n\n{}",
        s.title.trim()
    );
    if !s.rationale.trim().is_empty() {
        p.push_str("\n\nWhy it matters:\n");
        p.push_str(s.rationale.trim());
    }
    if !s.url.trim().is_empty() {
        p.push_str(&format!("\n\nSource finding: {}", s.url.trim()));
    }
    p
}

/// Build the unified action list, ranked by urgency and recency.
pub fn build_actions(src: &ActionSources) -> Vec<ActionItem> {
    let mut out: Vec<ActionItem> = Vec::new();

    // Attention items -----------------------------------------------------
    for a in src.attn {
        // A pending plan-review is mirrored as an attention item; skip it here so the richer
        // plan-review action (with the plan body) is the single representation.
        if a.mode.as_deref() == Some("review") || a.source == "plan-review" {
            continue;
        }
        let rank = action_rank::from_severity(&a.severity);
        let urgency = if rank.is_urgent() {
            Urgency::Urgent
        } else {
            Urgency::Queued
        };
        out.push(ActionItem {
            id: format!("attn:{}", a.id),
            kind: ActionKind::Attention,
            title: a.title.clone(),
            source: format!("attention · {}", a.source),
            severity: a.severity.clone(),
            repo: a.repo.clone(),
            branch: a.branch.clone(),
            link: a.link.clone(),
            detail: a.detail.clone(),
            payload: json!({ "repo": a.repo, "branch": a.branch, "attn_id": a.id }),
            suggested_prompt: prompt_attention(a),
            default_urgency: urgency,
            created: a.created,
            stale: false,
        });
    }

    // Pending plan-reviews ------------------------------------------------
    for r in src.reviews {
        if r.status != ReviewStatus::Pending {
            continue;
        }
        out.push(ActionItem {
            id: format!("review:{}", r.id),
            kind: ActionKind::PlanReview,
            title: format!("Plan review · {}/{}", r.repo, r.branch),
            source: "plan-review".to_string(),
            severity: action_rank::URGENT_SEVERITY.to_string(),
            repo: Some(r.repo.clone()),
            branch: Some(r.branch.clone()),
            link: None,
            detail: Some(r.plan.clone()),
            payload: json!({ "repo": r.repo, "branch": r.branch, "review_id": r.id }),
            suggested_prompt: prompt_plan_review(r),
            default_urgency: Urgency::Urgent,
            created: r.created,
            stale: false,
        });
    }

    // Failing crons -------------------------------------------------------
    for c in src.crons {
        if c.ok != Some(false) {
            continue;
        }
        out.push(ActionItem {
            id: format!("cron:{}", c.job),
            kind: ActionKind::FailingCron,
            title: format!("Failing cron · {}", c.job),
            source: format!("cron · {}", c.scope),
            severity: action_rank::URGENT_SEVERITY.to_string(),
            repo: None,
            branch: None,
            link: None,
            detail: c.description.clone(),
            payload: json!({ "cron": c.job, "script": c.script_path }),
            suggested_prompt: prompt_failing_cron(c),
            default_urgency: Urgency::Urgent,
            created: c.last_run.unwrap_or(0),
            stale: false,
        });
    }

    // Note todos + blockers ----------------------------------------------
    for entry in src.notes {
        for b in &entry.blockers {
            out.push(ActionItem {
                id: format!("blocker:{}/{}/{}", entry.repo, entry.branch, b.id),
                kind: ActionKind::Blocker,
                title: format!("Blocked · {}", one_line(&b.text, 80)),
                source: format!("note · {}/{}", entry.repo, entry.branch),
                severity: action_rank::URGENT_SEVERITY.to_string(),
                repo: Some(entry.repo.clone()),
                branch: Some(entry.branch.clone()),
                link: None,
                detail: Some(b.text.clone()),
                payload: json!({
                    "repo": entry.repo,
                    "branch": entry.branch,
                    "target": format!("{}:{}", entry.repo, entry.branch),
                }),
                suggested_prompt: prompt_blocker(entry, &b.text),
                default_urgency: Urgency::Urgent,
                created: util::parse_ts(&b.created).unwrap_or(0),
                stale: false,
            });
        }
        for t in &entry.todos {
            let created = action_created_at(&t.created);
            let stale = src.now.saturating_sub(created) > STALE_TODO_SECS;
            out.push(ActionItem {
                id: format!("todo:{}/{}/{}", entry.repo, entry.branch, t.id),
                kind: ActionKind::Todo,
                title: one_line(&t.text, 90),
                source: format!("note · {}/{}", entry.repo, entry.branch),
                severity: action_rank::INFO_SEVERITY.to_string(),
                repo: Some(entry.repo.clone()),
                branch: Some(entry.branch.clone()),
                link: None,
                detail: if t.text.contains('\n') {
                    Some(t.text.clone())
                } else {
                    None
                },
                payload: json!({
                    "repo": entry.repo,
                    "branch": entry.branch,
                    "target": format!("{}:{}", entry.repo, entry.branch),
                }),
                suggested_prompt: prompt_todo(entry, &t.text),
                default_urgency: Urgency::Queued,
                created,
                stale,
            });
        }
    }

    // Stale in-flight PRs -------------------------------------------------
    for item in src.queue {
        if item.status != ItemStatus::Dispatched {
            continue;
        }
        let Some(pr) = item.pr.as_deref().filter(|s| !s.trim().is_empty()) else {
            continue;
        };
        if src.now - item.updated_at < STALE_PR_SECS {
            continue;
        }
        out.push(ActionItem {
            id: format!("stale-pr:{}/{}", item.repo, item.id),
            kind: ActionKind::StalePr,
            title: format!("Stale PR · {} {}", item.repo, pr),
            source: format!("orchestrate · {}", item.repo),
            severity: action_rank::WARN_SEVERITY.to_string(),
            repo: Some(item.repo.clone()),
            branch: item.branch.clone(),
            link: Some(pr.to_string()),
            detail: Some(format!("Item: {}", item.item)),
            payload: json!({
                "repo": item.repo,
                "branch": item.branch,
                "target": item.branch.as_ref().map(|b| format!("{}:{}", item.repo, b)),
                "pr": pr,
            }),
            suggested_prompt: prompt_stale_pr(item),
            default_urgency: Urgency::Queued,
            created: item.updated_at,
            stale: false,
        });
    }

    // Today's open goals --------------------------------------------------
    for g in src.goals {
        if g.done == Some(true) {
            continue;
        }
        out.push(ActionItem {
            id: format!("goal:{}", g.id),
            kind: ActionKind::Goal,
            title: one_line(&g.text, 90),
            source: "today · goal".to_string(),
            severity: action_rank::INFO_SEVERITY.to_string(),
            repo: None,
            branch: None,
            link: g.link.clone(),
            detail: None,
            payload: json!({ "goal_id": g.id }),
            suggested_prompt: prompt_goal(g),
            default_urgency: Urgency::Queued,
            created: 0,
            stale: false,
        });
    }

    // Accepted suggestions (Today's agent, #578) --------------------------
    // The owner approved these on Today; promote each into an actionable item with the shared
    // Start-agent CTA. Only `Accepted` ones cross over — `Suggested` stay in review, `Dismissed`
    // never resurface.
    for s in src.suggestions {
        if s.status != SuggestionStatus::Accepted {
            continue;
        }
        out.push(ActionItem {
            id: format!("suggestion:{}", s.id),
            kind: ActionKind::Suggestion,
            title: one_line(&s.title, 90),
            source: format!("suggestion · {}", s.source),
            severity: action_rank::INFO_SEVERITY.to_string(),
            repo: None,
            branch: None,
            link: if s.url.trim().is_empty() {
                None
            } else {
                Some(s.url.clone())
            },
            detail: if s.rationale.trim().is_empty() {
                None
            } else {
                Some(s.rationale.clone())
            },
            payload: json!({ "suggestion_id": s.id }),
            suggested_prompt: prompt_suggestion(s),
            default_urgency: Urgency::Queued,
            created: s.created_at,
            stale: false,
        });
    }

    sort_actions(&mut out);
    out
}

fn action_created_at(created: &str) -> i64 {
    util::parse_ts(created).unwrap_or(0)
}

fn sort_actions(actions: &mut [ActionItem]) {
    actions.sort_by(|a, b| {
        action_rank::from_severity(&a.severity)
            .compare(action_rank::from_severity(&b.severity))
            .then(b.created.cmp(&a.created))
    });
}

// ---------------------------------------------------------------------------
// Cron-activity feed (#557) — the observational counterpart to the action list.
// ---------------------------------------------------------------------------

/// One row in the **cron-activity** feed (#557): a job the automation has actually *run*, with its
/// most-recent outcome. This is the observational counterpart to the actionable [`ActionItem`]
/// list — where [`ActionKind::FailingCron`] surfaces only *broken* jobs as "start an agent" work,
/// this shows recent runs regardless of exit so the Command Center can display what the automation
/// is *doing*, not only what broke. Built by [`cron_activity`] from the same [`CronView`] feed,
/// most-recent-run first.
#[derive(Debug, Clone, Serialize)]
pub struct CronActivity {
    /// The job name — ties back to the catalog + the `POST /api/cron/run` re-trigger.
    pub job: String,
    /// Tier label (`branch`/`repo`/`global`).
    pub scope: String,
    /// Whether the most recent run exited 0.
    pub ok: bool,
    /// The most recent run's exit code.
    pub last_exit: Option<i32>,
    /// Epoch-seconds of the most recent run (always present — this feed is run-observed only).
    pub last_run: i64,
    /// Total recorded runs.
    pub runs: usize,
    /// Recorded runs that exited non-zero.
    pub failures: usize,
    /// Whether a `[[cron]]`/staff entry declares this job (vs a bare run-observed orphan).
    pub declared: bool,
    /// The declared `on_calendar` schedule, when the job is declared.
    pub schedule: Option<String>,
    /// The declared description, when set.
    pub description: Option<String>,
    /// The last run's captured output tail, surfaced only when that run *failed* (mirrors
    /// [`CronView::last_output`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output: Option<String>,
    /// `urgent` when the last run failed, else `info` — the row tone, mirroring the action-list
    /// severities.
    pub severity: String,
}

/// Build the cron-activity feed from the machine cron registry ([`crate::crons::status_views`]):
/// every job that has *ever run*, most-recent-run first (ties broken by job name for a stable
/// order), each tagged `urgent` when its last run failed. Declared-but-never-run jobs are omitted
/// here — they belong in the [`build_catalog`] roster, not the activity feed.
pub fn cron_activity(crons: &[CronView]) -> Vec<CronActivity> {
    let mut out: Vec<CronActivity> = crons
        .iter()
        .filter_map(|c| {
            let last_run = c.last_run?;
            let ok = c.ok.unwrap_or(true);
            Some(CronActivity {
                job: c.job.clone(),
                scope: c.scope.clone(),
                ok,
                last_exit: c.last_exit,
                last_run,
                runs: c.runs,
                failures: c.failures,
                declared: c.declared,
                schedule: c.schedule.clone(),
                description: c.description.clone(),
                last_output: c.last_output.clone(),
                severity: if ok { "info" } else { "urgent" }.to_string(),
            })
        })
        .collect();
    out.sort_by(|a, b| b.last_run.cmp(&a.last_run).then_with(|| a.job.cmp(&b.job)));
    out
}

// ---------------------------------------------------------------------------
// Triggerable-agent catalog (#557).
// ---------------------------------------------------------------------------

/// What a [`CatalogItem`] is — one explicit runnable agent kind or a plain cron/native job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CatalogKind {
    Command,
    Staff,
    Hosted,
    Specialist,
    /// A plain cron job / native job (`crons.toml` schedule + `<job>.sh` script + native registry).
    Cron,
}

impl CatalogKind {
    fn from_agent(agent: &AgentDef) -> Self {
        match agent.kind {
            AgentKind::Command => Self::Command,
            AgentKind::Staff => Self::Staff,
            AgentKind::Hosted => Self::Hosted,
            AgentKind::Specialist => Self::Specialist,
        }
    }

    fn is_agent(self) -> bool {
        self != Self::Cron
    }
}

/// One triggerable agent/job in the Command Center catalog (#557): everything the automation can
/// fire, with the metadata a human needs to decide *whether* to re-trigger it — what it does, its
/// cadence, its last-run health, and whether it's declared or on-demand. Agent entries carry their
/// manifest metadata. Every entry's [`CatalogItem::job`] is exactly what `POST /api/cron/run
/// {job}` fires.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogItem {
    /// The triggerable identity accepted by `bn run <job>`.
    pub job: String,
    /// Display name.
    pub name: String,
    /// The explicit agent kind, or `cron` for a non-agent job.
    pub kind: CatalogKind,
    /// Tier label (`branch`/`repo`/`global`).
    pub scope: String,
    /// Whether a `[[cron]]`/staff entry declares this job (in the registry, not just run-observed).
    pub declared: bool,
    /// Whether this entry has neither a calendar nor startup trigger and is manual-only.
    pub on_demand: bool,
    /// The declared `on_calendar` schedule, when declared.
    pub schedule: Option<String>,
    /// The declared description, when set.
    pub description: Option<String>,
    /// Whether a `<job>.sh` script backs this job.
    pub has_script: bool,
    /// The declared entry hits the network.
    pub network: bool,
    /// Also fired on the first bn event of the day (`catch_up`).
    pub catch_up: bool,
    /// Agent only: also fired once when the tmux server starts.
    pub startup: bool,
    /// Agent only: queue capture remains enabled.
    pub queue: bool,
    /// Agent only: failure retry delay for scheduled attempts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<i64>,
    /// Epoch-seconds of the most recent run, when the job has ever run.
    pub last_run: Option<i64>,
    /// The most recent run's exit code, when the job has ever run.
    pub last_exit: Option<i32>,
    /// Whether the most recent run exited 0, when the job has ever run.
    pub ok: Option<bool>,
    /// Total recorded runs.
    pub runs: usize,
    /// Recorded runs that exited non-zero.
    pub failures: usize,
    /// Agent only: the headless harness override, when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// Agent only: reusable bn skills the agent is equipped with (#568). Empty for a
    /// non-agent job or an agent that declares none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// Agent only: MCP servers the agent connects to (#568). Empty for a non-agent job or an
    /// agent that declares none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcps: Vec<String>,
}

/// Build the triggerable catalog from the cron registry plus the strict Markdown agent catalog.
/// Existing run/script rows are enriched in place; never-run agents are appended. Specialists are
/// installable rather than runnable and therefore do not appear.
pub fn build_catalog(crons: &[CronView], agents: &AgentCatalog) -> Vec<CatalogItem> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for cron in crons {
        let agent = agents
            .find(&cron.job)
            .filter(|agent| !agent.is_specialist());
        if let Some(agent) = agent {
            seen.insert(agent.name.clone());
            out.push(agent_catalog_item(agent, Some(cron)));
        } else {
            out.push(cron_catalog_item(cron));
        }
    }
    for agent in agents
        .iter()
        .filter(|agent| !agent.is_specialist() && !seen.contains(&agent.name))
    {
        out.push(agent_catalog_item(agent, None));
    }
    out
}

fn cron_catalog_item(cron: &CronView) -> CatalogItem {
    CatalogItem {
        job: cron.job.clone(),
        name: cron.job.clone(),
        kind: CatalogKind::Cron,
        scope: cron.scope.clone(),
        declared: cron.declared,
        on_demand: cron.schedule.is_none(),
        schedule: cron.schedule.clone(),
        description: cron.description.clone(),
        has_script: cron.has_script,
        network: cron.network,
        catch_up: cron.catch_up,
        startup: false,
        queue: false,
        retry_after_secs: None,
        last_run: cron.last_run,
        last_exit: cron.last_exit,
        ok: cron.ok,
        runs: cron.runs,
        failures: cron.failures,
        harness: None,
        skills: Vec::new(),
        mcps: Vec::new(),
    }
}

fn agent_catalog_item(agent: &AgentDef, cron: Option<&CronView>) -> CatalogItem {
    CatalogItem {
        job: agent.name.clone(),
        name: agent.name.clone(),
        kind: CatalogKind::from_agent(agent),
        scope: agent.scope.label().to_string(),
        declared: true,
        on_demand: agent.on_calendar.is_none() && !agent.is_startup(),
        schedule: agent.on_calendar.clone(),
        description: agent
            .description
            .clone()
            .or_else(|| cron.and_then(|row| row.description.clone())),
        has_script: cron.is_some_and(|row| row.has_script),
        network: agent.network,
        catch_up: false,
        startup: agent.is_startup(),
        queue: agent.queue,
        retry_after_secs: agent.retry_after_secs,
        last_run: cron.and_then(|row| row.last_run),
        last_exit: cron.and_then(|row| row.last_exit),
        ok: cron.and_then(|row| row.ok),
        runs: cron.map(|row| row.runs).unwrap_or(0),
        failures: cron.map(|row| row.failures).unwrap_or(0),
        harness: agent.harness.clone(),
        skills: agent.skills.clone(),
        mcps: agent.mcps.clone(),
    }
}

/// The Today tab's split of a triggerable-agent catalog (#575): runnable agents and the plain
/// crons whose schedule *fires on `date`*, each already carrying its last-run health + the `job`
/// name `POST /api/cron/run` re-fires. `date` is the local day the dashboard is showing.
#[derive(Debug, Clone, Serialize)]
pub struct TodayAutomation {
    /// The plain cron/native jobs scheduled to fire today (undeclared/on-demand jobs are excluded —
    /// only ones with a schedule that fires today).
    pub crons: Vec<CatalogItem>,
    /// The runnable Markdown agents scheduled to fire today.
    pub agents: Vec<CatalogItem>,
}

/// Split a built catalog into today's crons + today's agents (#575): keep the entries whose
/// declared `schedule` fires on `date` ([`crate::staff_schedule::fires_on`]), partitioned by
/// [`CatalogKind`]. On-demand entries (no schedule) are dropped — "today" means *scheduled to run
/// today*, not "every job that exists". Pure over its inputs (the fold, not the catalog build).
pub fn todays_automation(catalog: &[CatalogItem], date: chrono::NaiveDate) -> TodayAutomation {
    let mut crons = Vec::new();
    let mut agents = Vec::new();
    for item in catalog {
        let fires = item
            .schedule
            .as_deref()
            .is_some_and(|s| crate::staff_schedule::fires_on(s, date));
        if !fires {
            continue;
        }
        if item.kind.is_agent() {
            agents.push(item.clone());
        } else {
            crons.push(item.clone());
        }
    }
    TodayAutomation { crons, agents }
}

/// Gather the machine cron registry + strict agent catalog, build the triggerable catalog, and split
/// out today's scheduled crons + agents ([`todays_automation`]) — the whole Today-tab fold (#575)
/// in one call so the web layer needn't reach for `chrono`. Uses the local day
/// ([`crate::util::today`], honouring `$BN_TODAY`); `ctx = None` gives the machine-wide registry.
pub fn build_todays_automation(
    cfg: &crate::resolver::Config,
    ctx: Option<&crate::resolver::Resolved>,
) -> Result<TodayAutomation, crate::agents::AgentCatalogError> {
    let crons = crate::crons::status_views(cfg, ctx);
    let agents = crate::agents::discover(cfg, ctx)?;
    let catalog = build_catalog(&crons, &agents);
    let date = chrono::NaiveDate::parse_from_str(&crate::util::today(), "%Y-%m-%d")
        .unwrap_or_else(|_| chrono::Local::now().date_naive());
    Ok(todays_automation(&catalog, date))
}
