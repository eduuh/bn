//! The global search / command palette fold (#565): one ranked, typed, actionable result set over
//! everything bn already aggregates.
//!
//! "Searching for things should be able to be found. Like work on task." A ⌘K palette in the
//! dashboard needs a single place to find *anything* — an open todo, a blocker, an attention item,
//! a command-center action, a branch note, a learning, a live agent, a repo, a cron, a skill, or a
//! GitHub issue — and to **act** on it. The actionable core reuses the already-ranked
//! [`crate::action_center`] items (so "work on this task" opens the same composer, pre-filled),
//! while the rest fold in as navigational jump targets.
//!
//! Everything here is pure over its inputs (the already-read source viewmodels + the query), so the
//! web layer just gathers the sources — exactly as the `/api/actions` handler already does — and
//! calls [`search`]; the ranking policy lives here under unit test. GitHub issues are fetched
//! best-effort through the injected [`crate::runner::CommandRunner`] seam ([`github_issues`]), kept
//! separate so the ranking fold stays deterministic.

use crate::action_center::{ActionItem, ActionKind, Urgency};
use crate::agent::LiveAgent;
use crate::crons::CronView;
use crate::learnings::LearningsView;
use crate::runner::{gh_json, CommandRunner};
use crate::skills::SkillInfo;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The most results the palette ever returns for one query — enough to be useful, few enough to
/// stay scannable and keep the payload small.
pub const RESULT_CAP: usize = 40;

/// A GitHub issue with no title-term match still ranks (gh matched its body), but below every
/// candidate whose title actually contains a query term.
const ISSUE_BASE_SCORE: i64 = 25;

/// The presentation kind of a palette result — drives the row icon/label/group in the UI.
/// Serialized kebab-case to match the rest of the web contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResultKind {
    // Action-derived (carry a "work on" action).
    Todo,
    Blocker,
    Attention,
    PlanReview,
    FailingCron,
    StalePr,
    Goal,
    Issue,
    Suggestion,
    // Navigational jump targets.
    Note,
    Learning,
    Agent,
    Repo,
    Cron,
    Skill,
}

impl ResultKind {
    /// Map an [`ActionKind`] to its palette presentation kind (they share the same vocabulary).
    fn from_action(kind: ActionKind) -> Self {
        match kind {
            ActionKind::Attention => ResultKind::Attention,
            ActionKind::PlanReview => ResultKind::PlanReview,
            ActionKind::Todo => ResultKind::Todo,
            ActionKind::Blocker => ResultKind::Blocker,
            ActionKind::FailingCron => ResultKind::FailingCron,
            ActionKind::StalePr => ResultKind::StalePr,
            ActionKind::Goal => ResultKind::Goal,
            ActionKind::Issue => ResultKind::Issue,
            ActionKind::Suggestion => ResultKind::Suggestion,
        }
    }

    /// A tiny per-kind priority that only breaks score ties — the human's queue (attention,
    /// blockers, reviews) floats above passive references (notes, repos, skills) at equal text
    /// relevance. Kept far smaller than any field weight so it never overrides a better match.
    fn tiebreak(self) -> i64 {
        match self {
            ResultKind::Attention | ResultKind::Blocker | ResultKind::PlanReview => 6,
            ResultKind::Todo | ResultKind::Goal | ResultKind::Issue | ResultKind::StalePr => 5,
            ResultKind::Suggestion => 5,
            ResultKind::FailingCron => 4,
            ResultKind::Agent | ResultKind::Note => 3,
            ResultKind::Repo | ResultKind::Cron => 2,
            ResultKind::Learning | ResultKind::Skill => 1,
        }
    }
}

/// One typed, actionable search result. `route` is an in-app SPA path to jump to; `link` an
/// external URL to open; `action`, when present, feeds the reused StartAgentComposer for a
/// one-click "work on this".
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    /// Stable, type-prefixed id — unique across kinds so the UI can key on it.
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ResultKind,
    /// One-line headline for the row.
    pub title: String,
    /// Provenance / context line under the title.
    pub subtitle: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// External URL to open in a new tab (issue / PR), when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// In-app route to navigate to when chosen (`/notes`, `/agents/<id>`, `/repos/<repo>`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    /// When present, the result offers "work on this": the embedded action feeds the reused
    /// StartAgentComposer verbatim (dispatch / queue an agent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionItem>,
    /// Relevance score (higher = better); the returned list is already sorted score-desc.
    pub score: i64,
}

/// An active branch note as a jump target — the caller fills these from `rollups::all_notes` (repo,
/// branch, derived goal, status).
#[derive(Debug, Clone)]
pub struct NoteCard {
    pub repo: String,
    pub branch: String,
    pub goal: Option<String>,
    pub status: String,
}

/// A repo bn knows about — the caller fills these from the repos rollup (repo + a short subtitle).
#[derive(Debug, Clone)]
pub struct RepoCard {
    pub repo: String,
    pub subtitle: String,
}

/// A GitHub issue hit decoded from `gh issue list --json number,title,url,state`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IssueHit {
    pub number: i64,
    pub title: String,
    pub url: String,
    pub state: String,
}

/// The already-read source viewmodels the palette folds. Every field is borrowed, so the caller
/// owns the reads; mirrors [`crate::action_center::ActionSources`] in spirit.
#[derive(Default)]
pub struct PaletteSources<'a> {
    /// The unified command-center items (`action_center::build_actions`) — the actionable core:
    /// todos, blockers, attention, plan-reviews, failing crons, stale PRs, goals.
    pub actions: &'a [ActionItem],
    /// Active branch notes, as "jump to a note" targets.
    pub notes: &'a [NoteCard],
    /// The learnings / reflections feed (grouped by theme).
    pub learnings: Option<&'a LearningsView>,
    /// The live agent fleet.
    pub agents: &'a [LiveAgent],
    /// Every repo bn knows.
    pub repos: &'a [RepoCard],
    /// The machine cron registry (healthy ones are jump targets; failing ones are already actions).
    pub crons: &'a [CronView],
    /// Every discovered skill.
    pub skills: &'a [SkillInfo],
    /// Best-effort GitHub issues already fetched for the query (see [`github_issues`]).
    pub issues: &'a [IssueHit],
}

/// Rank a candidate against the query. `terms` are the lowercased whitespace-split query words;
/// every term must appear in *some* field (AND semantics) or the candidate is dropped (`None`).
/// The score sums, per term, the weight of the strongest field it hits — so a title match beats a
/// body match — plus whole-query bonuses on the title and the kind tiebreak.
fn rank(query: &str, terms: &[String], fields: &[(&str, i64)], kind: ResultKind) -> Option<i64> {
    let lowered: Vec<(String, i64)> = fields
        .iter()
        .filter(|(t, _)| !t.is_empty())
        .map(|(t, w)| (t.to_lowercase(), *w))
        .collect();

    let mut score = 0i64;
    for term in terms {
        let best = lowered
            .iter()
            .filter(|(t, _)| t.contains(term))
            .map(|(_, w)| *w)
            .max();
        score += best?; // a term matched nothing — not a result
    }

    // Whole-query bonuses, measured on the title (the first field, by convention).
    if let Some((title, _)) = lowered.first() {
        if title == query {
            score += 200;
        } else if title.starts_with(query) {
            score += 100;
        } else if title.contains(query) {
            score += 40;
        }
    }

    Some(score + kind.tiebreak())
}

/// The global search fold: rank every source against `query` into one typed, actionable list,
/// sorted best-first and capped at [`RESULT_CAP`]. An empty query yields no results.
pub fn search(src: &PaletteSources, query: &str) -> Vec<SearchResult> {
    let q = query.trim().to_lowercase();
    let terms: Vec<String> = q.split_whitespace().map(str::to_string).collect();
    if terms.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<SearchResult> = Vec::new();

    // Actionable core — reuse the already-ranked command-center items verbatim, so "work on this"
    // opens the same composer with the same per-type suggested prompt.
    for a in src.actions {
        let kind = ResultKind::from_action(a.kind);
        let fields = [
            (a.title.as_str(), 100),
            (a.source.as_str(), 40),
            (a.repo.as_deref().unwrap_or(""), 30),
            (a.branch.as_deref().unwrap_or(""), 30),
            (a.detail.as_deref().unwrap_or(""), 20),
        ];
        if let Some(score) = rank(&q, &terms, &fields, kind) {
            out.push(SearchResult {
                id: format!("action:{}", a.id),
                kind,
                title: a.title.clone(),
                subtitle: a.source.clone(),
                repo: a.repo.clone(),
                branch: a.branch.clone(),
                link: a.link.clone(),
                route: None,
                action: Some(a.clone()),
                score,
            });
        }
    }

    // Branch notes — "jump to a note".
    for n in src.notes {
        let title = format!("{}/{}", n.repo, n.branch);
        let goal = n.goal.as_deref().unwrap_or("");
        let fields = [
            (title.as_str(), 100),
            (goal, 60),
            (n.repo.as_str(), 40),
            (n.branch.as_str(), 40),
            (n.status.as_str(), 10),
        ];
        if let Some(score) = rank(&q, &terms, &fields, ResultKind::Note) {
            let subtitle = if goal.is_empty() {
                format!("note · {}", n.status)
            } else {
                format!("note · {}", first_line(goal, 80))
            };
            out.push(SearchResult {
                id: format!("note:{}/{}", n.repo, n.branch),
                kind: ResultKind::Note,
                title,
                subtitle,
                repo: Some(n.repo.clone()),
                branch: Some(n.branch.clone()),
                link: None,
                route: Some("/notes".to_string()),
                action: None,
                score,
            });
        }
    }

    // Learnings / reflections.
    if let Some(view) = src.learnings {
        for group in &view.groups {
            for e in &group.entries {
                let fields = [
                    (e.text.as_str(), 100),
                    (e.repo.as_deref().unwrap_or(""), 40),
                    (e.skill.as_deref().unwrap_or(""), 40),
                    (group.theme.as_str(), 20),
                ];
                if let Some(score) = rank(&q, &terms, &fields, ResultKind::Learning) {
                    out.push(SearchResult {
                        id: format!("learning:{}", e.key),
                        kind: ResultKind::Learning,
                        title: first_line(&e.text, 90),
                        subtitle: format!("learning · {}", group.theme),
                        repo: e.repo.clone(),
                        branch: e.branch.clone(),
                        link: None,
                        route: Some("/notes".to_string()),
                        action: None,
                        score,
                    });
                }
            }
        }
    }

    // Live agents.
    for a in src.agents {
        let title = format!("{} · {}/{}", a.harness, a.repo, a.branch);
        let health = a.health.label();
        let fields = [
            (a.repo.as_str(), 70),
            (a.branch.as_str(), 70),
            (a.harness.as_str(), 40),
            (a.session_id.as_str(), 30),
            (health, 10),
        ];
        if let Some(score) = rank(&q, &terms, &fields, ResultKind::Agent) {
            out.push(SearchResult {
                id: format!("agent:{}", a.session_id),
                kind: ResultKind::Agent,
                title,
                subtitle: format!("agent · {health} · {}", a.harness),
                repo: Some(a.repo.clone()),
                branch: Some(a.branch.clone()),
                link: None,
                route: Some(format!("/agents/{}", a.session_id)),
                action: None,
                score,
            });
        }
    }

    // Repos.
    for r in src.repos {
        let fields = [(r.repo.as_str(), 100), (r.subtitle.as_str(), 30)];
        if let Some(score) = rank(&q, &terms, &fields, ResultKind::Repo) {
            out.push(SearchResult {
                id: format!("repo:{}", r.repo),
                kind: ResultKind::Repo,
                title: r.repo.clone(),
                subtitle: if r.subtitle.is_empty() {
                    "repo".to_string()
                } else {
                    format!("repo · {}", r.subtitle)
                },
                repo: Some(r.repo.clone()),
                branch: None,
                link: None,
                route: Some(format!("/repos/{}", r.repo)),
                action: None,
                score,
            });
        }
    }

    // Crons — healthy ones as jump targets (failing ones are already actionable items above).
    for c in src.crons {
        if c.ok == Some(false) {
            continue;
        }
        let desc = c.description.as_deref().unwrap_or("");
        let fields = [(c.job.as_str(), 100), (desc, 40), (c.scope.as_str(), 20)];
        if let Some(score) = rank(&q, &terms, &fields, ResultKind::Cron) {
            out.push(SearchResult {
                id: format!("cron:{}", c.job),
                kind: ResultKind::Cron,
                title: c.job.clone(),
                subtitle: format!("cron · {}", c.scope),
                repo: None,
                branch: None,
                link: None,
                route: Some("/cron".to_string()),
                action: None,
                score,
            });
        }
    }

    // Skills — one result per effective definition.
    for s in src.skills {
        if !s.effective {
            continue;
        }
        let when = s.when.as_deref().unwrap_or("");
        let kw = s.keywords.join(" ");
        let fields = [
            (s.name.as_str(), 100),
            (s.description.as_str(), 50),
            (kw.as_str(), 60),
            (when, 40),
        ];
        if let Some(score) = rank(&q, &terms, &fields, ResultKind::Skill) {
            out.push(SearchResult {
                id: format!("skill:{}", s.name),
                kind: ResultKind::Skill,
                title: s.name.clone(),
                subtitle: format!("skill · {}", first_line(&s.description, 80)),
                repo: None,
                branch: None,
                link: None,
                route: Some("/skills".to_string()),
                action: None,
                score,
            });
        }
    }

    // GitHub issues — already filtered by gh's own title/body search, so include each (a body-only
    // match scores the base), and offer "work on this issue → start an agent".
    for i in src.issues {
        let title = format!("#{} {}", i.number, i.title);
        let fields = [(i.title.as_str(), 100)];
        let score = rank(&q, &terms, &fields, ResultKind::Issue)
            .unwrap_or_else(|| ISSUE_BASE_SCORE + ResultKind::Issue.tiebreak());
        out.push(SearchResult {
            id: format!("issue:{}", i.url),
            kind: ResultKind::Issue,
            title: title.clone(),
            subtitle: format!("issue · {}", i.state),
            repo: issue_repo(&i.url),
            branch: None,
            link: Some(i.url.clone()),
            route: None,
            action: Some(issue_action(i)),
            score,
        });
    }

    // Best-first, then a stable id tiebreak so equal scores order deterministically.
    out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    out.truncate(RESULT_CAP);
    out
}

/// Best-effort GitHub issue search for the palette query, through the injected runner. Returns up
/// to `limit` open issues (in the runner's cwd repo) matching `query` in title/body; empty on any
/// gh failure — binary absent, not authed, or the cwd isn't a repo. Deterministic under
/// [`crate::runner::FakeRunner`].
pub fn github_issues(runner: &dyn CommandRunner, query: &str, limit: usize) -> Vec<IssueHit> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let limit_s = limit.to_string();
    let args = [
        "issue",
        "list",
        "--search",
        q,
        "--state",
        "open",
        "--limit",
        &limit_s,
        "--json",
        "number,title,url,state",
    ];
    gh_json::<Vec<IssueHit>>(runner, &args).unwrap_or_default()
}

/// Best-effort list of a *specific repo's* open GitHub issues, newest-first, through the injected
/// runner (`gh issue list -R <nwo> --state open`). `nwo` is the `owner/repo` slug; unlike
/// [`github_issues`] this is not a search — it drains the repo's open backlog so the per-repo
/// command center (#556) can pick what a fleet should work on. Returns up to `limit`; empty on any
/// gh failure — binary absent, not authed, or the repo unreachable. Deterministic under
/// [`crate::runner::FakeRunner`].
pub fn github_issues_repo(runner: &dyn CommandRunner, nwo: &str, limit: usize) -> Vec<IssueHit> {
    let nwo = nwo.trim();
    if nwo.is_empty() {
        return Vec::new();
    }
    let limit_s = limit.to_string();
    let args = [
        "issue",
        "list",
        "-R",
        nwo,
        "--state",
        "open",
        "--limit",
        &limit_s,
        "--json",
        "number,title,url,state",
    ];
    gh_json::<Vec<IssueHit>>(runner, &args).unwrap_or_default()
}

/// A synthetic [`ActionItem`] for "work on this GitHub issue" — the reused composer renders its
/// `issue` badge and a fix-and-PR prompt, pre-filled with the issue's repo when the URL yields one.
fn issue_action(i: &IssueHit) -> ActionItem {
    let repo = issue_repo(&i.url);
    ActionItem {
        id: format!("issue:{}", i.url),
        kind: ActionKind::Issue,
        title: format!("#{} {}", i.number, i.title),
        source: "github issue".to_string(),
        severity: "info".to_string(),
        repo: repo.clone(),
        branch: None,
        link: Some(i.url.clone()),
        detail: None,
        payload: json!({ "repo": repo, "issue": i.url }),
        suggested_prompt: format!(
            "Work on GitHub issue #{} and open a PR that fixes it:\n\n{}\n\n{}\n\nFixes #{}.",
            i.number,
            i.title.trim(),
            i.url,
            i.number
        ),
        default_urgency: Urgency::Urgent,
        created: 0,
        stale: false,
    }
}

/// Parse the bare repo name from a GitHub issue URL (`https://github.com/<owner>/<repo>/issues/N`
/// → `repo`), so the composer pre-fills the repo. `None` when the shape doesn't match.
fn issue_repo(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let mut parts = rest.split('/');
    let _owner = parts.next()?;
    let repo = parts.next()?;
    (!repo.is_empty()).then(|| repo.to_string())
}

/// The first line of a possibly-multiline string, truncated to `max` chars with an ellipsis.
fn first_line(text: &str, max: usize) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    if first.chars().count() > max {
        let mut s: String = first.chars().take(max.saturating_sub(1)).collect();
        s.push('…');
        s
    } else {
        first.to_string()
    }
}
