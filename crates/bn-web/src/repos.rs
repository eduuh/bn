//! `GET /api/repos` — the multi-repo navigation rollup (#495).
//!
//! The Repos view groups local repo state: repos with a bare clone on this machine, a checked-out
//! worktree, or a live/in-flight agent. Branch notes, managed-repos config, and cost data decorate
//! those cards but do not create repo cards by themselves — `/repos` is an inventory of local repos,
//! not every note namespace bn has ever seen. The heavy lifting is a pure [`assemble`] over
//! already-extracted rows, so the merge / rollup / sort is unit-tested without touching disk.

use bn_core::cost::{self, CostFilter, CostView};
use bn_core::resolver::{Config, JobScope, Resolved};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const POOL_MIN_READY: usize = 1;

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

/// Synthetic targets are steerable agent addresses, not work repos. They may have notes/fleet rows
/// so their owning page can render state, but the Repos board should list only repos bn can work in.
fn synthetic_repos(cfg: &Config) -> BTreeSet<String> {
    bn_core::agents::discover(cfg, None)
        .map(|catalog| {
            catalog
                .iter()
                .filter_map(|agent| agent.host_repo.clone())
                .collect()
        })
        .unwrap_or_default()
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

    // 4b) The repos bn actually has cloned on this machine: one `<repo>.git` bare clone each. Notes,
    //     agents and runs only ever mention a repo you've *worked* in, so without this a freshly
    //     cloned repo — or one whose notes all closed — had no card at all. Managed config alone is
    //     not local state: it can mark a cloned repo as managed, but it cannot mint a card.
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

    // 5) Per-repo warm-pool state (#497): the live `bn wt pool` shape (size + warm/leased slots),
    //    read in-process from the shared `bn_core::pool` status. Asked per **cloned** repo, which
    //    is now the whole answer — pooling has no config and no opt-out, so every clone has a pool
    //    and none of them can be "disabled". Keyed lowercased to match the rollup's note-canonical
    //    repo casing.
    let pools: BTreeMap<String, Value> = cloned
        .iter()
        .map(|repo| {
            let st = bn_core::pool::pool_status(cfg, repo);
            (repo.to_ascii_lowercase(), pool_json(&st))
        })
        .collect();

    let setup: BTreeMap<String, Value> = cloned
        .iter()
        .map(|repo| (repo.clone(), setup_json(cfg, repo)))
        .collect();
    let synthetic = synthetic_repos(cfg);

    assemble(
        &managed,
        &cloned,
        &synthetic,
        seeds,
        &cv,
        RepoLookups {
            pools: &pools,
            links: &links,
            setup: &setup,
        },
    )
}

/// The per-repo pool view attached to a repo rollup (#497): pooling-enabled, configured size, the
/// warm / leased / idle split, a best-effort last-warmed timestamp, and each slot's own state. A
/// repo explicitly opted out of pooling gets [`pool_disabled`] instead, so the field is always
/// present.
///
/// Slots ride along because the counts alone can't answer the question you actually have about a
/// half-leased pool — *which* branch is holding a slot. The list is bounded by the configured size
/// (at most `MAX_POOL_SIZE`) and only pooled repos have one, so this stays a small payload.
fn pool_json(st: &bn_core::pool::PoolStatus) -> Value {
    // Slot 1 is the reserved main slot, never a worker — it is excluded from every count that
    // answers "can this pool take more work?".
    let controller = bn_core::agent::MAIN_POOL_SLOT;
    let ready = st
        .slots
        .iter()
        .filter(|s| s.n != controller && !s.leased && s.warm)
        .count();
    let worker_idle = st
        .slots
        .iter()
        .filter(|s| s.n != controller && !s.leased)
        .count();
    let full = worker_idle == 0;
    let needs_top_up = ready < POOL_MIN_READY && worker_idle > ready;
    json!({
        "enabled": st.enabled,
        "total": st.total,
        "warm": st.warm,
        "leased": st.leased,
        "idle": st.idle,
        "ready": ready,
        "worker_idle": worker_idle,
        "needs_top_up": needs_top_up,
        "full": full,
        "last_warmed_ts": st.last_warmed_ts,
        "policy": pool_policy_json(),
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
    json!({
        "enabled": false,
        "total": 0,
        "warm": 0,
        "leased": 0,
        "idle": 0,
        "ready": 0,
        "worker_idle": 0,
        "needs_top_up": false,
        "full": false,
        "last_warmed_ts": null,
        "policy": pool_policy_json(),
        "slots": []
    })
}

/// The pool policy each repo card still needs.
///
/// This used to carry the whole policy as data — eight descriptive keys plus six sentences of
/// prose rules — so the board could render a "Warm-pool policy" explainer card above the repos.
/// That card is gone, and with it the only reader of the prose. What remains is the one field the
/// UI *computes* with: `min_ready`, which the optimistic pool update needs to recompute
/// `needs_top_up` locally after a resize. The rest was documentation shipped down the wire on
/// every 30s poll, once per repo.
fn pool_policy_json() -> Value {
    json!({
        "min_ready": POOL_MIN_READY,
        "max_freshness_hours": 24,
    })
}

struct RepoLookups<'a> {
    pools: &'a BTreeMap<String, Value>,
    links: &'a BTreeMap<String, Value>,
    setup: &'a BTreeMap<String, Value>,
}

/// Fold the raw session seeds into `{ repos: [RepoRollup, …] }` — pure, so the whole merge / rollup
/// / sort is testable with hand-built rows. Repos come from local evidence: bare clones, checked-out
/// worktrees, and live/in-flight agents. Notes and managed config attach to those repos but cannot
/// create cards alone. Repos sort by last-activity desc (then active-agent count, then name), which
/// sinks the never-touched clones to the bottom; sessions within a repo sort in-flight-first, then
/// most-recent.
fn assemble(
    managed: &[String],
    cloned: &[String],
    synthetic: &BTreeSet<String>,
    seeds: Vec<Session>,
    cost: &CostView,
    lookups: RepoLookups<'_>,
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
        if synthetic.contains(&s.repo) {
            continue;
        }
        let key = (s.repo.clone(), s.branch.clone());
        merged
            .entry(key)
            .and_modify(|acc| acc.merge(s.clone()))
            .or_insert(s);
    }

    // Group sessions by repo.
    let mut by_repo: BTreeMap<String, Vec<Session>> = BTreeMap::new();
    let real_repos: std::collections::BTreeSet<String> = cloned
        .iter()
        .filter(|r| !synthetic.contains(*r))
        .cloned()
        .chain(
            merged
                .values()
                .filter(|s| s.in_flight || s.worktree.is_some())
                .map(|s| s.repo.clone()),
        )
        .collect();
    for (_, s) in merged {
        if !real_repos.contains(&s.repo) {
            continue;
        }
        by_repo.entry(s.repo.clone()).or_default().push(s);
    }
    // Every managed repo and every locally cloned repo gets a card even with zero sessions.
    for r in &real_repos {
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
                "pool": lookups
                    .pools
                    .get(&repo)
                    .cloned()
                    .unwrap_or_else(pool_disabled),
                "setup": lookups
                    .setup
                    .get(&repo)
                    .cloned()
                    .unwrap_or_else(|| setup_unknown(&repo)),
                // `null` for a repo with no clone here or an origin we can't parse — the UI renders
                // no link rather than a guessed one.
                "links": lookups.links.get(&repo).cloned().unwrap_or(Value::Null),
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
    // Synthetic repos are excluded from rollups above, but keep the picker defensive in case a new
    // synthetic target is added before the rollup filter learns about it.
    let mut known: Vec<String> = rollups
        .iter()
        .filter_map(|r| r.get("repo").and_then(Value::as_str))
        .filter(|repo| !synthetic.contains(*repo))
        .map(str::to_string)
        .collect();
    known.sort();

    json!({ "repos": rollups, "known": known })
}

fn scope_label(scope: JobScope) -> &'static str {
    match scope {
        JobScope::Branch => "branch",
        JobScope::Project => "project",
        JobScope::Repo => "repo",
        JobScope::Global => "global",
    }
}

fn repo_main(repo: &str) -> Resolved {
    Resolved {
        repo: repo.to_ascii_lowercase(),
        branch: "main".to_string(),
    }
}

/// Where a repo-owned (`JobScope::Repo`) script lives now: `<notes_dir_for(repo)>/jobs/<repo>/`
/// — machine-independent, synced through the notes repo like everything else in `notes_root`.
/// This is the path the dashboard shows/writes to for a "repo" scope save, matching
/// [`bn_core::resolver::job_dirs`]'s `JobScope::Repo` tier exactly.
fn repo_script_path(cfg: &Config, repo: &str, name: &str) -> PathBuf {
    cfg.notes_dir_for(repo)
        .join("jobs")
        .join(repo)
        .join(format!("{name}.sh"))
}

fn is_scaffold_build(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.contains("Fill in your repo's install/build")
        && text.contains("bn wt pool warm")
        && text.lines().any(|line| line.trim() == "true")
}

fn conventional_script_json(cfg: &Config, ctx: &Resolved, name: &str) -> Value {
    let repo_path = repo_script_path(cfg, &ctx.repo, name);
    let resolved = bn_core::resolver::job_script(cfg, Some(ctx), name);
    let (scope, path) = match resolved {
        Some((scope, path)) => (Some(scope), Some(path)),
        None => (None, None),
    };
    let inherited = scope.is_some_and(|s| s != JobScope::Repo);
    let missing = scope.is_none();
    json!({
        "name": name,
        "exists": path.is_some(),
        "scope": scope.map(scope_label),
        "path": path.as_ref().map(|p| p.display().to_string()),
        "repo_path": repo_path.display().to_string(),
        "repo_owned": scope == Some(JobScope::Repo),
        "inherited": inherited,
        "editable": true,
        "needs_setup": missing || (name == "build" && path.as_deref().is_some_and(is_scaffold_build)),
        "scaffold": name == "build" && path.as_deref().is_some_and(is_scaffold_build),
    })
}

pub(crate) fn setup_json(cfg: &Config, repo: &str) -> Value {
    let ctx = repo_main(repo);
    let conventional = ["build", "test", "lint"];
    let scripts: Vec<Value> = conventional
        .iter()
        .map(|name| conventional_script_json(cfg, &ctx, name))
        .collect();
    let mut reasons = Vec::new();
    for script in &scripts {
        let name = script.get("name").and_then(Value::as_str).unwrap_or("");
        if script
            .get("scaffold")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            reasons.push(format!("{name} script is still the scaffold"));
        } else if !script
            .get("exists")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && name == "build"
        {
            reasons.push("missing build script".to_string());
        }
    }
    let verify_command = cfg.repo_verify(repo).map(str::to_string);
    let verify_jobs = scripts
        .iter()
        .filter(|s| s.get("exists").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|s| s.get("name").and_then(Value::as_str).map(str::to_string))
        .collect::<Vec<_>>();
    let verify_mode = if verify_command.is_some() {
        "command"
    } else if verify_jobs.is_empty() {
        reasons
            .push("no verify gate (no repo verify command or build/test/lint scripts)".to_string());
        "missing"
    } else {
        "scripts"
    };
    json!({
        "status": if reasons.is_empty() { "ready" } else { "needs-setup" },
        "needs_setup": !reasons.is_empty(),
        "reasons": reasons,
        "scripts": scripts,
        "verify": {
            "mode": verify_mode,
            "command": verify_command,
            "jobs": verify_jobs,
        },
    })
}

fn setup_unknown(repo: &str) -> Value {
    json!({
        "status": "unknown",
        "needs_setup": true,
        "reasons": [format!("repo {repo} has no local setup scan")],
        "scripts": [],
        "verify": { "mode": "missing", "command": null, "jobs": [] },
    })
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

    fn cost_row(key: &str, aic: f64, usd: f64) -> bn_core::cost::CostRow {
        bn_core::cost::CostRow {
            key: key.to_string(),
            sessions: 1,
            input_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            output_tokens: 0,
            aic: Some(aic),
            cost_usd: Some(usd),
        }
    }

    fn session(repo: &str, branch: &str) -> Session {
        Session {
            repo: repo.to_string(),
            branch: branch.to_string(),
            ..Session::default()
        }
    }

    fn lookups<'a>(
        pools: &'a BTreeMap<String, Value>,
        links: &'a BTreeMap<String, Value>,
        setup: &'a BTreeMap<String, Value>,
    ) -> RepoLookups<'a> {
        RepoLookups {
            pools,
            links,
            setup,
        }
    }

    #[test]
    fn repo_name_strips_host_and_git_suffix() {
        assert_eq!(repo_name("git@github.com:eduuh/bn.git"), "bn");
        assert_eq!(repo_name("https://github.com/eduuh/BN.git"), "bn");
        assert_eq!(repo_name("bn"), "bn");
    }

    #[test]
    fn note_key_flattens_slashes() {
        assert_eq!(note_key("dispatch/web-495"), "dispatch-web-495");
    }

    #[test]
    fn assemble_gives_local_clones_cards_and_uses_managed_as_metadata() {
        let cost = CostView::default();
        let pools = BTreeMap::new();
        let links = BTreeMap::new();
        let v = assemble(
            &["managed".to_string(), "cloned".to_string()],
            &["cloned".to_string()],
            &BTreeSet::new(),
            vec![],
            &cost,
            lookups(&pools, &links, &BTreeMap::new()),
        );
        let repos = v.get("repos").and_then(Value::as_array).unwrap();
        let names: Vec<&str> = repos
            .iter()
            .filter_map(|r| r.get("repo").and_then(Value::as_str))
            .collect();
        assert!(names.contains(&"cloned"));
        assert!(!names.contains(&"managed"));
        // The `managed` flag is metadata on a local repo, not a separate repo source.
        let cloned_card = repos.iter().find(|r| r["repo"] == "cloned").unwrap();
        assert_eq!(cloned_card["managed"], serde_json::json!(true));
    }

    #[test]
    fn assemble_merges_seeds_by_repo_branch_and_attaches_cost() {
        let cost = CostView {
            by_repo: vec![cost_row("bn", 1.5, 2.0)],
            ..CostView::default()
        };
        let seeds = vec![session("bn", "feat"), session("bn", "feat")];
        let v = assemble(
            &[],
            &["bn".to_string()],
            &BTreeSet::new(),
            seeds,
            &cost,
            lookups(&BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new()),
        );
        let card = v["repos"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["repo"] == "bn")
            .unwrap();
        // Two seeds on the same (repo, branch) collapse to one session.
        assert_eq!(card["session_count"], serde_json::json!(1));
        assert_eq!(card["aic"], serde_json::json!(1.5));
        assert_eq!(card["cost_usd"], serde_json::json!(2.0));
    }

    #[test]
    fn assemble_attaches_pool_json_or_disabled() {
        let mut pools = BTreeMap::new();
        pools.insert(
            "bn".to_string(),
            serde_json::json!({
                "enabled": true,
                "total": 4,
                "warm": 1,
                "leased": 1,
                "idle": 3,
                "ready": 1,
                "worker_idle": 2,
                "needs_top_up": false,
                "full": false,
                "last_warmed_ts": null,
                "policy": pool_policy_json(),
                "slots": [],
            }),
        );
        let v = assemble(
            &["bn".to_string()],
            &["bn".to_string(), "app".to_string()],
            &BTreeSet::new(),
            vec![],
            &CostView::default(),
            lookups(&pools, &BTreeMap::new(), &BTreeMap::new()),
        );
        let repos = v["repos"].as_array().unwrap();
        let bn = repos.iter().find(|r| r["repo"] == "bn").unwrap();
        assert_eq!(bn["pool"]["enabled"], serde_json::json!(true));
        assert_eq!(bn["pool"]["total"], serde_json::json!(4));
        assert_eq!(bn["pool"]["policy"]["min_ready"], serde_json::json!(1));
        // A repo with no pool entry gets the disabled shape, never a missing field.
        let app = repos.iter().find(|r| r["repo"] == "app").unwrap();
        assert_eq!(app["pool"]["enabled"], serde_json::json!(false));
        assert_eq!(app["pool"]["needs_top_up"], serde_json::json!(false));
        assert_eq!(app["pool"]["policy"]["min_ready"], serde_json::json!(1));
        // The board-level policy block and its prose rules are gone — nothing renders them.
        assert!(v.get("pool_policy").is_none());
        assert!(bn["pool"]["policy"].get("rules").is_none());
    }

    #[test]
    fn pool_json_reports_worker_policy_state() {
        let st = bn_core::pool::PoolStatus {
            repo: "bn".to_string(),
            enabled: true,
            total: 3,
            leased: 1,
            warm: 1,
            idle: 2,
            last_warmed_ts: None,
            slots: vec![
                bn_core::pool::PoolSlot {
                    n: bn_core::agent::MAIN_POOL_SLOT,
                    leased: false,
                    warm: true,
                    branch: None,
                    holder: None,
                    age_secs: None,
                    warmed_ts: None,
                },
                bn_core::pool::PoolSlot {
                    n: 2,
                    leased: true,
                    warm: true,
                    branch: Some("feat".to_string()),
                    holder: Some("agent".to_string()),
                    age_secs: Some(5),
                    warmed_ts: None,
                },
                bn_core::pool::PoolSlot {
                    n: 3,
                    leased: false,
                    warm: false,
                    branch: None,
                    holder: None,
                    age_secs: None,
                    warmed_ts: None,
                },
            ],
        };
        let v = pool_json(&st);
        // The controller slot is warm, but it does not count as a ready worker.
        assert_eq!(v["ready"], serde_json::json!(0));
        assert_eq!(v["worker_idle"], serde_json::json!(1));
        assert_eq!(v["needs_top_up"], serde_json::json!(true));
        assert_eq!(v["full"], serde_json::json!(false));
    }

    #[test]
    fn assemble_attaches_setup_json_or_unknown() {
        let mut setup = BTreeMap::new();
        setup.insert(
            "bn".to_string(),
            serde_json::json!({ "status": "ready", "needs_setup": false }),
        );
        let v = assemble(
            &[],
            &["bn".to_string(), "app".to_string()],
            &BTreeSet::new(),
            vec![],
            &CostView::default(),
            lookups(&BTreeMap::new(), &BTreeMap::new(), &setup),
        );
        let repos = v["repos"].as_array().unwrap();
        let bn = repos.iter().find(|r| r["repo"] == "bn").unwrap();
        assert_eq!(bn["setup"]["status"], serde_json::json!("ready"));
        let app = repos.iter().find(|r| r["repo"] == "app").unwrap();
        assert_eq!(app["setup"]["status"], serde_json::json!("unknown"));
    }

    #[test]
    fn assemble_known_excludes_today() {
        let synthetic = "virtual".to_string();
        let synthetic_repos = BTreeSet::from([synthetic.clone()]);
        let seeds = vec![session(&synthetic, "2026-08-06")];
        let v = assemble(
            &["bn".to_string(), synthetic.clone()],
            &["bn".to_string(), synthetic.clone()],
            &synthetic_repos,
            seeds,
            &CostView::default(),
            lookups(&BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new()),
        );
        let repos: Vec<&str> = v["repos"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r.get("repo").and_then(Value::as_str))
            .collect();
        let known: Vec<&str> = v["known"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(repos.contains(&"bn"));
        assert!(!repos.contains(&synthetic.as_str()));
        assert!(known.contains(&"bn"));
        assert!(!known.contains(&synthetic.as_str()));
    }

    #[test]
    fn assemble_excludes_note_only_pseudo_repos() {
        let seeds = vec![session("repo", "master"), session("bn", "main")];
        let v = assemble(
            &[],
            &["bn".to_string()],
            &BTreeSet::new(),
            seeds,
            &CostView::default(),
            lookups(&BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new()),
        );
        let repos: Vec<&str> = v["repos"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r.get("repo").and_then(Value::as_str))
            .collect();
        assert!(repos.contains(&"bn"));
        assert!(!repos.contains(&"repo"));
    }

    #[test]
    fn assemble_sorts_in_flight_repo_ahead_of_idle() {
        let idle = session("aaa", "x"); // alphabetically first, but no activity
        let mut hot = session("zzz", "y");
        hot.in_flight = true;
        hot.last_activity_ts = 9_999;
        let v = assemble(
            &[],
            &[],
            &BTreeSet::new(),
            vec![idle, hot],
            &CostView::default(),
            lookups(&BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new()),
        );
        let repos = v["repos"].as_array().unwrap();
        // Most-recently-active repo leads despite the alphabetical tiebreak.
        assert_eq!(repos[0]["repo"], serde_json::json!("zzz"));
    }

    #[test]
    fn session_merge_prefers_in_flight_status_and_max_counts() {
        let mut base = session("bn", "feat");
        base.status = Some("planning".to_string());
        base.open_todos = 3;
        let mut live = session("bn", "feat");
        live.status = Some("running".to_string());
        live.in_flight = true;
        live.open_todos = 1;
        base.merge(live);
        // A live seed's state wins; numeric fields take the max.
        assert_eq!(base.status.as_deref(), Some("running"));
        assert_eq!(base.open_todos, 3);
        assert!(base.in_flight);
    }
}
