//! `GET /api/repos` — the multi-repo navigation rollup (#495).
//!
//! The Repos view groups the fleet by repo: every repo bn knows (the managed-repos config, the
//! branch-note store, the live fleet, and the orchestrate runs), each with a per-repo rollup —
//! active agents, open PRs, recent sessions (branches with notes / dispatches), last activity, and
//! cost (`by_repo` from the #381 fold) — plus a drill-in session list. A read; all in-process
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

    // 5) Per-repo warm-pool state (#497): the live `bn wt pool` shape (size + warm/leased slots) for
    //    every pooled repo, read in-process from the shared `bn_core::pool` status. Keyed lowercased
    //    to match the rollup's note-canonical repo casing.
    let pools: BTreeMap<String, Value> = bn_core::pool::all_pool_status(cfg)
        .iter()
        .map(|st| (st.repo.to_ascii_lowercase(), pool_json(st)))
        .collect();

    assemble(&managed, seeds, &cv, &pools)
}

/// The compact per-repo pool view attached to a repo rollup (#497): pooling-enabled, configured
/// size, and the warm / leased / idle split, plus a best-effort last-warmed timestamp. A repo not in
/// the pools config gets [`pool_disabled`] instead, so the field is always present.
fn pool_json(st: &bn_core::pool::PoolStatus) -> Value {
    json!({
        "enabled": st.enabled,
        "total": st.total,
        "warm": st.warm,
        "leased": st.leased,
        "idle": st.idle,
        "last_warmed_ts": st.last_warmed_ts,
    })
}

/// The pool view for a repo with no pool configured — pooling disabled, no slots.
fn pool_disabled() -> Value {
    json!({ "enabled": false, "total": 0, "warm": 0, "leased": 0, "idle": 0, "last_warmed_ts": null })
}

/// Fold the raw session seeds into `{ repos: [RepoRollup, …] }` — pure, so the whole merge / rollup
/// / sort is testable with hand-built rows. Repos come from the seeds *and* the managed set (a
/// managed repo with no sessions yet still gets a card). Repos sort by last-activity desc (then
/// active-agent count, then name); sessions within a repo sort in-flight-first, then most-recent.
fn assemble(
    managed: &[String],
    seeds: Vec<Session>,
    cost: &CostView,
    pools: &BTreeMap<String, Value>,
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
    // Every managed repo gets a card even with zero sessions.
    for m in managed {
        by_repo.entry(m.clone()).or_default();
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

    json!({ "repos": rollups })
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

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::cost::CostRow;

    fn cost_with_repo(key: &str, aic: f64, usd: f64) -> CostView {
        CostView {
            by_repo: vec![CostRow {
                key: key.to_string(),
                sessions: 1,
                input_tokens: 0,
                cache_read_tokens: 0,
                output_tokens: 0,
                aic: Some(aic),
                cost_usd: Some(usd),
            }],
            ..CostView::default()
        }
    }

    fn note_seed(repo: &str, branch: &str, pr: Option<&str>, todos: usize, ts: i64) -> Session {
        Session {
            repo: repo.into(),
            branch: branch.into(),
            status: Some("open".into()),
            pr: pr.map(String::from),
            open_todos: todos,
            last_activity_ts: ts,
            in_flight: false,
            ..Session::default()
        }
    }

    fn live_seed(repo: &str, branch: &str, sid: &str, tokens: i64, ts: i64) -> Session {
        Session {
            repo: repo.into(),
            branch: branch.into(),
            session_id: Some(sid.into()),
            health: Some("active".into()),
            target: Some(format!("{repo}:{branch}")),
            output_tokens: tokens,
            last_activity_ts: ts,
            in_flight: true,
            ..Session::default()
        }
    }

    fn repos_of(v: &Value) -> &Vec<Value> {
        v.get("repos").and_then(Value::as_array).unwrap()
    }

    #[test]
    fn merges_note_and_live_agent_onto_one_session() {
        // A note and a live agent for the same branch collapse into one session carrying both the
        // note's todo count / PR and the agent's session id / tokens; the repo counts it active.
        let seeds = vec![
            note_seed("bn", "dispatch-web-495", Some("42"), 3, 100),
            live_seed("bn", "dispatch-web-495", "sess-1", 5000, 200),
        ];
        let v = assemble(
            &[],
            seeds,
            &cost_with_repo("bn", 1.5, 0.9),
            &BTreeMap::new(),
        );
        let repos = repos_of(&v);
        assert_eq!(repos.len(), 1);
        let bn = &repos[0];
        assert_eq!(bn["repo"], "bn");
        assert_eq!(bn["active_agents"], 1);
        assert_eq!(bn["open_prs"], 1);
        assert_eq!(bn["session_count"], 1);
        assert_eq!(bn["aic"], 1.5);
        assert_eq!(bn["cost_usd"], 0.9);
        let sessions = bn["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s["session_id"], "sess-1");
        assert_eq!(s["output_tokens"], 5000);
        assert_eq!(s["open_todos"], 3);
        assert_eq!(s["pr"], "42");
        assert_eq!(s["in_flight"], true);
        assert_eq!(s["last_activity_ts"], 200);
    }

    #[test]
    fn dispatched_slash_branch_collapses_onto_its_dash_note() {
        // The orchestrate seed keeps slashes → note_key normalizes so it lands on the note's row.
        let mut disp = Session {
            repo: "bn".into(),
            branch: note_key("dispatch/web-495"),
            in_flight: true,
            last_activity_ts: 300,
            ..Session::default()
        };
        disp.status = Some("working".into());
        let seeds = vec![note_seed("bn", "dispatch-web-495", None, 1, 100), disp];
        let v = assemble(&[], seeds, &CostView::default(), &BTreeMap::new());
        let repos = repos_of(&v);
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0]["session_count"], 1);
        assert_eq!(repos[0]["active_agents"], 1);
        // The in-flight pane-state wins over the note's static status for display.
        assert_eq!(repos[0]["sessions"][0]["status"], "working");
        assert_eq!(repos[0]["sessions"][0]["in_flight"], true);
    }

    #[test]
    fn managed_repo_with_no_sessions_still_gets_a_card() {
        let v = assemble(
            &["forge".into()],
            vec![note_seed("bn", "main", None, 0, 100)],
            &CostView::default(),
            &BTreeMap::new(),
        );
        let repos = repos_of(&v);
        let names: Vec<&str> = repos.iter().map(|r| r["repo"].as_str().unwrap()).collect();
        assert!(names.contains(&"forge"), "managed repo missing: {names:?}");
        let forge = repos.iter().find(|r| r["repo"] == "forge").unwrap();
        assert_eq!(forge["managed"], true);
        assert_eq!(forge["session_count"], 0);
        assert_eq!(forge["active_agents"], 0);
        // A repo with no pool configured still carries a pool field, pooling disabled.
        assert_eq!(forge["pool"]["enabled"], false);
        // bn is not managed here.
        let bn = repos.iter().find(|r| r["repo"] == "bn").unwrap();
        assert_eq!(bn["managed"], false);
    }

    #[test]
    fn repos_sort_most_recent_first() {
        let seeds = vec![
            note_seed("alpha", "b1", None, 0, 100),
            note_seed("zeta", "b2", None, 0, 999),
        ];
        let v = assemble(&[], seeds, &CostView::default(), &BTreeMap::new());
        let repos = repos_of(&v);
        // zeta is more recently active despite sorting after alpha alphabetically.
        assert_eq!(repos[0]["repo"], "zeta");
        assert_eq!(repos[1]["repo"], "alpha");
    }

    #[test]
    fn repo_name_parses_clone_urls() {
        assert_eq!(repo_name("git@github.com:eduuh/bn.git"), "bn");
        assert_eq!(repo_name("https://github.com/eduuh/Forge.git"), "forge");
        assert_eq!(repo_name("git@github.com:eduuh/wira360"), "wira360");
    }

    #[test]
    fn pooled_repo_carries_its_warm_pool_state() {
        // #497: a repo present in the pools map gets its live pool shape on the rollup; an unpooled
        // repo still gets a `pool` object with `enabled:false` so the field is always present.
        let pools: BTreeMap<String, Value> = [(
            "bn".to_string(),
            json!({ "enabled": true, "total": 3, "warm": 2, "leased": 1, "idle": 2, "last_warmed_ts": 123 }),
        )]
        .into_iter()
        .collect();
        let seeds = vec![
            note_seed("bn", "main", None, 0, 100),
            note_seed("forge", "main", None, 0, 90),
        ];
        let v = assemble(&[], seeds, &CostView::default(), &pools);
        let repos = repos_of(&v);
        let bn = repos.iter().find(|r| r["repo"] == "bn").unwrap();
        assert_eq!(bn["pool"]["enabled"], true);
        assert_eq!(bn["pool"]["total"], 3);
        assert_eq!(bn["pool"]["warm"], 2);
        assert_eq!(bn["pool"]["leased"], 1);
        assert_eq!(bn["pool"]["last_warmed_ts"], 123);
        // forge isn't pooled → disabled pool object.
        let forge = repos.iter().find(|r| r["repo"] == "forge").unwrap();
        assert_eq!(forge["pool"]["enabled"], false);
        assert_eq!(forge["pool"]["total"], 0);
    }
}
