//! `GET /api/repos` — the multi-repo navigation rollup (#495).
//!
//! The Repos view groups the fleet by repo: every repo bn knows (the bare clones it manages on this
//! machine, the managed-repos config, the branch-note store, the live fleet, and the orchestrate
//! runs), each with a per-repo rollup — active agents, open PRs, recent sessions (branches with
//! notes / dispatches), last activity, and cost (`by_repo` from the #381 fold) — plus a drill-in
//! session list. A read; all in-process
//! `bn-core`, reusing the same rollups the Fleet / Notes / Cost surfaces read so the shapes stay
//! one contract. The heavy lifting is a pure [`assemble`] over already-extracted rows, so the
//! merge / rollup / sort is unit-tested without touching disk.

use bn_core::cost::{self, CostFilter, CostView};
use bn_core::resolver::Config;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// tmux window / note-key form of a branch (worktree layout replaces `/` with `-`), so a dispatched
/// `dispatch/web-495` item and its `dispatch-web-495` note collapse onto one session.
fn note_key(branch: &str) -> String {
    branch.replace('/', "-")
}

/// The bn/local repo name from a clone URL (`git@github.com:eduuh/bn.git` / `https://…/bn.git` →
/// `bn`), lowercased to match the resolver's note-canonical repo casing.
fn repo_name(url: &str) -> String {
    let base = url.rsplit(['/', ':']).next().unwrap_or(url);
    base.strip_suffix(".git")
        .unwrap_or(base)
        .to_ascii_lowercase()
}

/// One session (branch) within a repo — a branch note, a live agent, and/or a dispatched orchestrate
/// item, merged by `(repo, note_key(branch))`. Rendered as a card in the drill-in session list.
#[derive(Debug, Clone, Default)]
struct Session {
    repo: String,
    branch: String,
    status: Option<String>,
    pr: Option<String>,
    open_todos: usize,
    blockers: usize,
    worktree: Option<String>,
    path: Option<String>,
    last_activity_ts: i64,
    session_id: Option<String>,
    health: Option<String>,
    target: Option<String>,
    output_tokens: i64,
    /// A live agent or an in-flight dispatch backs this branch (vs. a note-only "recent" session).
    in_flight: bool,
}

impl Session {
    /// Fold `other` into `self`, keeping the first non-empty of each field and the max of the
    /// numeric ones. Seeds are pushed notes → live agents → dispatches, so a note's status / todo
    /// counts survive while a live agent contributes its session id / health / tokens.
    fn merge(&mut self, other: Session) {
        // A live/in-flight seed's state (the orchestrate pane-state) is more current than a note's
        // static status, so it wins when present; otherwise keep the first non-empty.
        self.status = match other.status {
            Some(s) if other.in_flight => Some(s),
            other_status => self.status.take().or(other_status),
        };
        self.pr = self.pr.take().or(other.pr);
        self.open_todos = self.open_todos.max(other.open_todos);
        self.blockers = self.blockers.max(other.blockers);
        self.worktree = self.worktree.take().or(other.worktree);
        self.path = self.path.take().or(other.path);
        self.last_activity_ts = self.last_activity_ts.max(other.last_activity_ts);
        self.session_id = self.session_id.take().or(other.session_id);
        self.health = self.health.take().or(other.health);
        self.target = self.target.take().or(other.target);
        self.output_tokens = self.output_tokens.max(other.output_tokens);
        self.in_flight = self.in_flight || other.in_flight;
    }

    fn to_json(&self) -> Value {
        json!({
            "repo": self.repo,
            "branch": self.branch,
            "status": self.status,
            "pr": self.pr,
            "open_todos": self.open_todos,
            "blockers": self.blockers,
            "worktree": self.worktree,
            "path": self.path,
            "last_activity_ts": self.last_activity_ts,
            "session_id": self.session_id,
            "health": self.health,
            "target": self.target,
            "output_tokens": self.output_tokens,
            "in_flight": self.in_flight,
        })
    }
}

/// `GET /api/repos` viewmodel: read the four sources and fold them into the per-repo rollup. Kept
/// thin — the pure [`assemble`] does the merge / rollup / sort — so this is just the IO.
pub fn repos_view(cfg: &Config) -> Value {
    let mut seeds: Vec<Session> = Vec::new();

    // 1) Branch notes (the "recent sessions" backbone): status, PR, open-todo / blocker counts,
    //    on-disk worktree, and the note's mtime as its last-activity signal.
    for bn in bn_core::rollups::all_notes(cfg) {
        let front = &bn.note.front;
        let wt = cfg.worktree_dir.join(&bn.repo).join(&bn.branch);
        seeds.push(Session {
            repo: bn.repo.to_ascii_lowercase(),
            branch: bn.branch.clone(),
            status: Some(front.status.clone()),
            pr: front.pr.as_ref().map(|s| s.display()),
            open_todos: front.todos.iter().filter(|t| t.is_open_todo()).count(),
            blockers: front.blockers.len(),
            worktree: wt.is_dir().then(|| wt.display().to_string()),
            path: Some(bn.path.display().to_string()),
            last_activity_ts: file_mtime(&bn.path),
            in_flight: false,
            ..Session::default()
        });
    }

    // 2) The live fleet: every running Claude/Copilot session, keyed to its `repo:branch` note.
    let fleet = bn_core::agent::supervise(
        cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    );
    for a in &fleet.agents {
        seeds.push(Session {
            repo: a.repo.to_ascii_lowercase(),
            branch: note_key(&a.branch),
            last_activity_ts: a.last_activity_ts,
            session_id: Some(a.session_id.clone()),
            health: Some(a.health.label().to_string()),
            target: Some(a.target.clone()),
            output_tokens: a.output_tokens,
            in_flight: true,
            ..Session::default()
        });
    }

    // 3) Orchestrate runs: in-flight (dispatched) items — active work even before a note exists.
    let runs = crate::watch::orchestrate_runs_json(None);
    if let Some(arr) = runs.get("runs").and_then(Value::as_array) {
        for run in arr {
            let repo = run.get("repo").and_then(Value::as_str).unwrap_or_default();
            let Some(queue) = run.get("queue").and_then(Value::as_array) else {
                continue;
            };
            for it in queue {
                if it.get("status").and_then(Value::as_str) != Some("dispatched") {
                    continue;
                }
                let branch = it.get("branch").and_then(Value::as_str).unwrap_or_default();
                if branch.is_empty() {
                    continue;
                }
                // The tmux jump target: #372's explicit fields when present, else `repo:note-key`.
                let win = it.get("tmux_window").and_then(Value::as_str);
                let sess = it
                    .get("tmux_session")
                    .and_then(Value::as_str)
                    .unwrap_or(repo);
                let target = match win {
                    Some(w) => format!("{sess}:{w}"),
                    None => format!("{repo}:{}", note_key(branch)),
                };
                seeds.push(Session {
                    repo: repo.to_ascii_lowercase(),
                    branch: note_key(branch),
                    status: it
                        .get("last_state")
                        .and_then(Value::as_str)
                        .map(String::from),
                    pr: it.get("pr").and_then(Value::as_str).map(String::from),
                    last_activity_ts: it.get("updated_at").and_then(Value::as_i64).unwrap_or(0),
                    target: Some(target),
                    in_flight: true,
                    ..Session::default()
                });
            }
        }
    }

    // 4) Per-repo cost (both units) from the trailing-30d #381 fold.
    let cv = cost::rollup(cfg, &CostFilter::window(30));

    let managed: Vec<String> = cfg.managed_repos.iter().map(|u| repo_name(u)).collect();

    // 4b) The repos bn actually manages on this machine: one `<repo>.git` bare clone each. Notes,
    //     agents and runs only ever mention a repo you've *worked* in, so without this a freshly
    //     cloned repo — or one whose notes all closed — had no card at all. Work-only machines keep
    //     personal clones off the board, matching the note walk's own visibility rule.
    let clone_dirs = bn_core::lifecycle::cloned_repo_dirs(cfg);
    let cloned: Vec<String> = clone_dirs
        .keys()
        .filter(|r| cfg.is_visible_repo(r))
        .cloned()
        .collect();

    // 4c) Where each repo lives in a browser, derived from the clone's `origin`. A file read per
    //     repo, not a `git remote get-url` spawn — this whole view is on a 30s poll.
    let links: BTreeMap<String, Value> = clone_dirs
        .iter()
        .filter_map(|(repo, dir)| {
            let l = bn_core::forge::links_for(dir)?;
            Some((repo.clone(), serde_json::to_value(l).ok()?))
        })
        .collect();

    // 5) Per-repo warm-pool state (#497): the live `bn wt pool` shape (size + warm/leased slots) for
    //    every pooled repo, read in-process from the shared `bn_core::pool` status. Keyed lowercased
    //    to match the rollup's note-canonical repo casing.
    let pools: BTreeMap<String, Value> = bn_core::pool::all_pool_status(cfg)
        .iter()
        .map(|st| (st.repo.to_ascii_lowercase(), pool_json(st)))
        .collect();

    assemble(&managed, &cloned, seeds, &cv, &pools, &links)
}

/// The per-repo pool view attached to a repo rollup (#497): pooling-enabled, configured size, the
/// warm / leased / idle split, a best-effort last-warmed timestamp, and each slot's own state. A repo
/// not in the pools config gets [`pool_disabled`] instead, so the field is always present.
///
/// Slots ride along because the counts alone can't answer the question you actually have about a
/// half-leased pool — *which* branch is holding a slot. The list is bounded by the configured size
/// (at most `MAX_POOL_SIZE`) and only pooled repos have one, so this stays a small payload.
fn pool_json(st: &bn_core::pool::PoolStatus) -> Value {
    json!({
        "enabled": st.enabled,
        "total": st.total,
        "warm": st.warm,
        "leased": st.leased,
        "idle": st.idle,
        "last_warmed_ts": st.last_warmed_ts,
        "slots": st.slots.iter().map(|s| json!({
            "n": s.n,
            "leased": s.leased,
            "warm": s.warm,
            "branch": s.branch,
            "holder": s.holder,
            "age_secs": s.age_secs,
            "warmed_ts": s.warmed_ts,
        })).collect::<Vec<_>>(),
    })
}

/// The pool view for a repo with no pool configured — pooling disabled, no slots.
fn pool_disabled() -> Value {
    json!({ "enabled": false, "total": 0, "warm": 0, "leased": 0, "idle": 0, "last_warmed_ts": null, "slots": [] })
}

/// Fold the raw session seeds into `{ repos: [RepoRollup, …] }` — pure, so the whole merge / rollup
/// / sort is testable with hand-built rows. Repos come from the seeds, the managed set, *and* the
/// bare clones on this machine — a repo with no sessions yet still gets a card, so the board lists
/// everything bn can work in rather than only what it has already worked in. Repos sort by
/// last-activity desc (then active-agent count, then name), which sinks the never-touched clones to
/// the bottom; sessions within a repo sort in-flight-first, then most-recent.
fn assemble(
    managed: &[String],
    cloned: &[String],
    seeds: Vec<Session>,
    cost: &CostView,
    pools: &BTreeMap<String, Value>,
    links: &BTreeMap<String, Value>,
) -> Value {
    // Cost lookup keyed by note-canonical (lowercased) repo.
    let cost_by_repo: BTreeMap<String, (Option<f64>, Option<f64>)> = cost
        .by_repo
        .iter()
        .map(|r| (r.key.to_ascii_lowercase(), (r.aic, r.cost_usd)))
        .collect();

    // Merge seeds by (repo, note-key branch).
    let mut merged: BTreeMap<(String, String), Session> = BTreeMap::new();
    for s in seeds {
        let key = (s.repo.clone(), s.branch.clone());
        merged
            .entry(key)
            .and_modify(|acc| acc.merge(s.clone()))
            .or_insert(s);
    }

    // Group sessions by repo.
    let mut by_repo: BTreeMap<String, Vec<Session>> = BTreeMap::new();
    for (_, s) in merged {
        by_repo.entry(s.repo.clone()).or_default().push(s);
    }
    // Every managed repo and every locally cloned repo gets a card even with zero sessions.
    for r in managed.iter().chain(cloned) {
        by_repo.entry(r.clone()).or_default();
    }

    let mut rollups: Vec<Value> = by_repo
        .into_iter()
        .map(|(repo, mut sessions)| {
            // In-flight first, then most-recent, then branch name.
            sessions.sort_by(|a, b| {
                b.in_flight
                    .cmp(&a.in_flight)
                    .then(b.last_activity_ts.cmp(&a.last_activity_ts))
                    .then(a.branch.cmp(&b.branch))
            });
            let active_agents = sessions.iter().filter(|s| s.in_flight).count();
            let open_prs = sessions.iter().filter(|s| s.pr.is_some()).count();
            let last_activity_ts = sessions
                .iter()
                .map(|s| s.last_activity_ts)
                .max()
                .unwrap_or(0);
            let (aic, cost_usd) = cost_by_repo.get(&repo).copied().unwrap_or((None, None));
            json!({
                "repo": repo,
                "managed": managed.iter().any(|m| m == &repo),
                "active_agents": active_agents,
                "open_prs": open_prs,
                "session_count": sessions.len(),
                "last_activity_ts": last_activity_ts,
                "aic": aic,
                "cost_usd": cost_usd,
                "pool": pools.get(&repo).cloned().unwrap_or_else(pool_disabled),
                // `null` for a repo with no clone here or an origin we can't parse — the UI renders
                // no link rather than a guessed one.
                "links": links.get(&repo).cloned().unwrap_or(Value::Null),
                "sessions": sessions.iter().map(Session::to_json).collect::<Vec<_>>(),
            })
        })
        .collect();

    // Most-recently-active repos first; then the busiest; then alphabetical (managed-but-empty sink).
    rollups.sort_by(|a, b| {
        let ts = |v: &Value| {
            v.get("last_activity_ts")
                .and_then(Value::as_i64)
                .unwrap_or(0)
        };
        let active = |v: &Value| v.get("active_agents").and_then(Value::as_u64).unwrap_or(0);
        let name = |v: &Value| {
            v.get("repo")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        ts(b)
            .cmp(&ts(a))
            .then(active(b).cmp(&active(a)))
            .then(name(a).cmp(&name(b)))
    });

    // The flat, alphabetical name list every repo *picker* wants (#1006 follow-up). Derived from
    // the same rollups the cards render, so a repo can never be offered in a dropdown that the
    // board doesn't know about, or vice versa. Sorted by name rather than by activity: a picker is
    // scanned alphabetically, while the cards above are ranked by what's hot.
    //
    // Synthetic repos are excluded: Today's agent is addressed as `today:<date>` so it has a real
    // steering target, which gives it a branch note and therefore a rollup — but you cannot
    // dispatch work *into* `today`, so offering it in a picker would only invite a failed dispatch.
    let mut known: Vec<String> = rollups
        .iter()
        .filter_map(|r| r.get("repo").and_then(Value::as_str))
        .filter(|repo| *repo != bn_core::today_agent::TODAY_REPO)
        .map(str::to_string)
        .collect();
    known.sort();

    json!({ "repos": rollups, "known": known })
}

/// The note file's modified time as epoch-seconds — the best last-activity signal a note carries
/// (the frontmatter has no `updated` field). `0` when the mtime is unreadable.
fn file_mtime(path: &std::path::Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
