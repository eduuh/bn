//! `bn forge sweep`: the Forge agent loop's two steps, run once, only when `[tasks] provider =
//! "forge"`. Nothing runs it automatically: you run it by hand, or from your own cron.
//!
//! **Claim** ([`sweep_forge_claims`]): an activity assigned to the family's agent member that nothing
//! has started yet is work waiting for a machine. This machine claims it when the activity's
//! repository is one it has cloned and is allowed to work on, and while it holds fewer than `--max`
//! unfinished claims. `claim_activity` is Forge's conditional update, so two machines racing for one
//! task cannot both win: the loser hears `already_claimed` and moves on without a word. The winner
//! stamps the local repo name as `repo:` (so dispatch and the SMART gate see the name bn knows the
//! clone by) and comments `claimed by bn@<node>`. Then it starts the task the way every producer
//! starts one: `bn dispatch --task <id>` ([`start_claimed`]).
//!
//! **Complete on merge** ([`poll_forge_merges`]): `bn agent done` leaves a task with a PR open
//! ([`crate::cmd::close_out`]). This asks `gh pr view <pr> --json state` about each one, at most
//! once per [`PR_POLL_SECS`] per task, so a cron that runs the sweep often does not hammer GitHub.
//! `MERGED` completes the task in Forge; `CLOSED` says so once and leaves the task for a person.
//!
//! Both are best-effort. A Forge that cannot be reached costs this run its claims and completions.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use crate::*;
use bn_core::forge_app::client::{BnTaskRow, ClaimOutcome, ForgeApi};
use bn_core::forge_app::store::ForgeTaskStore;
use bn_core::runner::CommandRunner;
use bn_core::task_store::{self, TaskProviderKind};

/// The most often either step does any work: once a minute, however often the sweep is run. Each
/// step is a Forge list.
pub(crate) const SWEEP_EVERY_SECS: i64 = 60;

/// How long a task's PR is left alone after it was last asked about.
pub(crate) const PR_POLL_SECS: i64 = 600;

/// Where the two steps remember when they last ran and what each PR was last seen as.
const STATE_FILE: &str = "forge-sweep.json";

/// The deadline for one `gh pr view`.
const GH_TIMEOUT: Duration = Duration::from_secs(30);

/// Who this machine claims as: `bn@<node_id>`.
pub(crate) fn claimer() -> String {
    format!("bn@{}", bn_core::worker::node_id())
}

/// `bn forge sweep`: the claim step, then the merge poll, once.
pub(crate) fn run(cfg: &Config, max: usize, json: bool) -> i32 {
    if cfg.tasks_provider != TaskProviderKind::ForgeApp {
        let message = "[tasks] provider is not \"forge\"; there is nothing to sweep";
        if json {
            println!("{}", Envelope::failure("forge sweep", "not_forge", message).to_json());
        } else {
            eprintln!("bn forge sweep: {message}");
        }
        return 1;
    }
    let claims = sweep_forge_claims(cfg, max);
    let merges = poll_forge_merges(cfg, &bn_core::runner::RealRunner, bn_core::util::now_epoch_secs());
    if json {
        let data = serde_json::json!({ "claims": claims, "merges": merges });
        println!("{}", Envelope::success("forge sweep", data).to_json());
        return 0;
    }
    for a in &claims {
        let started = a.started.as_deref().map(|id| format!(" -> started {id}")).unwrap_or_default();
        let detail = a.detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default();
        println!("claim {} [{}]: {}{started}{detail}", a.activity, a.repo, a.outcome);
    }
    for p in &merges {
        println!("pr {} for {}: {}", p.pr, p.task, p.outcome);
    }
    if claims.is_empty() && merges.is_empty() {
        println!("Nothing to claim or complete.");
    }
    0
}

// ---------------------------------------------------------------------------------------------
// Claim.
// ---------------------------------------------------------------------------------------------

/// What the claim step did with one candidate.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ClaimAttempt {
    pub(crate) activity: String,
    pub(crate) repo: String,
    /// `claimed`, `already-claimed`, `refused` (any other reason Forge gives) or `error`.
    pub(crate) outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
    /// The bn task id `bn dispatch --task` was started for, once a claim landed and started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) started: Option<String>,
}

/// One activity this machine may claim, with the local repo name it resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) activity: String,
    pub(crate) repo: String,
}

fn open_row(row: &BnTaskRow) -> bool {
    !row.is_completed
        && !row.status.eq_ignore_ascii_case("abandoned")
        && !row.status.eq_ignore_ascii_case("blocked")
}

fn priority_rank(word: &str) -> u8 {
    match word.trim().to_ascii_lowercase().as_str() {
        "urgent" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        _ => 4,
    }
}

/// How many claims this machine has room for: `max`, less the open tasks it already holds that are
/// still being worked. A task waiting on its PR has finished its agent work, so it no longer counts.
pub(crate) fn free_lanes(rows: &[BnTaskRow], claimer: &str, max: usize) -> usize {
    let held = rows
        .iter()
        .filter(|r| open_row(r))
        .filter(|r| r.agent_meta.get("claimer").map(String::as_str) == Some(claimer))
        .filter(|r| !r.agent_meta.contains_key("pr"))
        .count();
    max.saturating_sub(held)
}

/// The activities this machine may claim, best first (priority, then oldest): open, assigned to an
/// agent member, not started, and in a repository that resolves to a local clone this machine is
/// allowed to work in.
pub(crate) fn candidates(
    rows: &[BnTaskRow],
    mut resolve: impl FnMut(&str) -> Option<String>,
    allowed: impl Fn(&str) -> bool,
) -> Vec<Candidate> {
    let mut picked: Vec<(u8, Candidate)> = rows
        .iter()
        .filter(|r| open_row(r) && r.started_at.is_none())
        .filter(|r| r.assignees.iter().any(|a| a.is_agent))
        .filter_map(|r| {
            let slug = r
                .agent_meta
                .get("repo")
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    r.goal_repo_url
                        .as_deref()
                        .and_then(bn_core::forge_app::repo_slug)
                })?;
            let local = resolve(&slug)?;
            (allowed(&slug) || allowed(&local)).then(|| {
                (
                    priority_rank(&r.priority),
                    Candidate {
                        activity: r.id.clone(),
                        repo: local,
                    },
                )
            })
        })
        .collect();
    // Stable, so rows Forge returned oldest first stay in that order within a priority.
    picked.sort_by_key(|(rank, _)| *rank);
    picked.into_iter().map(|(_, c)| c).collect()
}

/// Claim `candidates` in order until `free` claims have landed. A lost race is skipped silently.
pub(crate) fn claim(
    api: &dyn ForgeApi,
    rows: &[BnTaskRow],
    candidates: Vec<Candidate>,
    claimer: &str,
    free: usize,
) -> Vec<ClaimAttempt> {
    let mut attempts = Vec::new();
    let mut claimed = 0;
    for c in candidates {
        if claimed >= free {
            break;
        }
        let attempt = |outcome, detail: Option<String>| ClaimAttempt {
            activity: c.activity.clone(),
            repo: c.repo.clone(),
            outcome,
            detail,
            started: None,
        };
        match api.claim_activity(&c.activity, claimer) {
            Ok(ClaimOutcome::Claimed(_)) => {
                claimed += 1;
                let meta_repo = rows
                    .iter()
                    .find(|r| r.id == c.activity)
                    .and_then(|r| r.agent_meta.get("repo"));
                if meta_repo.map(String::as_str) != Some(c.repo.as_str()) {
                    let set = BTreeMap::from([("repo".to_string(), c.repo.clone())]);
                    if let Err(e) = api.set_activity_meta(&c.activity, &set, &[], None) {
                        eprintln!("bn forge: claimed {} but could not stamp repo:{} — {e}", c.activity, c.repo);
                    }
                }
                if let Err(e) = api.create_comment(&c.activity, &format!("claimed by {claimer}")) {
                    eprintln!("bn forge: claimed {} but could not comment — {e}", c.activity);
                }
                attempts.push(attempt("claimed", None));
            }
            Ok(ClaimOutcome::Refused(reason)) if reason == "already_claimed" => {
                attempts.push(attempt("already-claimed", None));
            }
            Ok(ClaimOutcome::Refused(reason)) => attempts.push(attempt("refused", Some(reason))),
            Err(e) => {
                eprintln!("bn forge: could not claim {} — {e}", c.activity);
                attempts.push(attempt("error", Some(e.to_string())));
                // A transport failure will fail the next claim the same way.
                break;
            }
        }
    }
    attempts
}

/// The claim step: claim up to `max` unfinished tasks for this machine, then start each new claim.
pub(crate) fn sweep_forge_claims(cfg: &Config, max: usize) -> Vec<ClaimAttempt> {
    if cfg.tasks_provider != TaskProviderKind::ForgeApp {
        return Vec::new();
    }
    let now = bn_core::util::now_epoch_secs();
    let path = state_path(cfg);
    let mut state = SweepState::read(&path);
    if now - state.claims_at < SWEEP_EVERY_SECS {
        return Vec::new();
    }
    state.claims_at = now;
    let _ = state.write(&path);

    let store = ForgeTaskStore::from_config(cfg);
    // No key: the task list itself says so on every read.
    let Ok(api) = store.api() else {
        return Vec::new();
    };
    let rows = match api.list_bn_tasks(Some(0)) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("bn forge: could not list tasks to claim — {e}");
            return Vec::new();
        }
    };
    let claimer = claimer();
    let free = free_lanes(&rows, &claimer, max);
    if free == 0 {
        return Vec::new();
    }
    let mut resolved: HashMap<String, Option<String>> = HashMap::new();
    let resolve = |slug: &str| {
        resolved
            .entry(slug.to_string())
            .or_insert_with(|| {
                crate::wt::resolve_bare_by_name(cfg, Some(slug))
                    .ok()
                    .flatten()
                    .map(|(local, _)| local)
            })
            .clone()
    };
    let picked = candidates(&rows, resolve, |repo| cfg.repo_allowed_here(repo));
    let mut attempts = claim(api, &rows, picked, &claimer, free);
    start_claimed(cfg, &mut attempts);
    attempts
}

/// Start every task a claim just landed on, through `bn dispatch --task <id>`: the one way the
/// machine starts work on its own behalf. The bn id is read back from the task list, where a claimed
/// activity is an ordinary task (`src:forge:<activity>`). A task that cannot be found or started
/// keeps its claim and says why in `detail`; running the sweep again does not retry it, because it
/// is no longer unstarted.
pub(crate) fn start_claimed(cfg: &Config, attempts: &mut [ClaimAttempt]) {
    if !attempts.iter().any(|a| a.outcome == "claimed") {
        return;
    }
    let file = match task_store::task_store(cfg).load() {
        Ok(file) => file,
        Err(e) => {
            for a in attempts.iter_mut().filter(|a| a.outcome == "claimed") {
                a.detail = Some(format!("claimed, but the task list could not be read: {e}"));
            }
            return;
        }
    };
    for a in attempts.iter_mut().filter(|a| a.outcome == "claimed") {
        // The mapping's link back to the activity, as docs/config.md describes it.
        let src = format!("forge:{}", a.activity);
        let Some((id, subject)) = file
            .tasks()
            .find(|t| t.meta("src") == Some(src.as_str()))
            .and_then(|t| Some((t.id()?.to_string(), t.summary())))
        else {
            a.detail = Some("claimed, but no bn task carries it yet".to_string());
            continue;
        };
        match crate::cmd::dispatch::DispatchInvocation::new(&subject, &a.repo, "auto")
            .task(Some(&id))
            .launch()
        {
            Ok(()) => a.started = Some(id),
            Err(e) => {
                eprintln!("bn forge: claimed {} but could not start {id} — {e}", a.activity);
                a.detail = Some(format!("claimed, but could not start {id}: {e}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Complete on merge.
// ---------------------------------------------------------------------------------------------

/// What one PR poll found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct PrPoll {
    pub(crate) task: String,
    pub(crate) pr: String,
    /// `merged` (task completed), `closed`, `open`, `unknown` (gh gave no answer) or `waiting`
    /// (asked too recently).
    pub(crate) outcome: &'static str,
}

/// What the sweep remembers between passes.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct SweepState {
    #[serde(default)]
    claims_at: i64,
    #[serde(default)]
    merges_at: i64,
    /// Task identity -> its PR as last polled.
    #[serde(default)]
    prs: BTreeMap<String, PrSeen>,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct PrSeen {
    pr: String,
    polled: i64,
    #[serde(default)]
    closed_noted: bool,
}

impl SweepState {
    fn read(path: &std::path::Path) -> SweepState {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn write(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let body = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, path)
    }
}

fn state_path(cfg: &Config) -> PathBuf {
    std::env::var("BN_STATE_DIR")
        .ok()
        .filter(|d| !d.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| cfg.state_dir())
        .join(STATE_FILE)
}

/// `MERGED`, `CLOSED` or `OPEN`, or `None` when gh cannot say.
fn pr_state(runner: &dyn CommandRunner, pr: &str) -> Option<String> {
    let out = runner.run_with_timeout("gh", &["pr", "view", pr, "--json", "state"], GH_TIMEOUT);
    let body = out.ok_stdout()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v.get("state")
        .and_then(serde_json::Value::as_str)
        .map(str::to_ascii_uppercase)
}

/// The merge poll, as `bn forge sweep` runs it: [`poll_merges`] as this machine, at most once a
/// minute.
pub(crate) fn poll_forge_merges(cfg: &Config, runner: &dyn CommandRunner, now: i64) -> Vec<PrPoll> {
    if cfg.tasks_provider != TaskProviderKind::ForgeApp {
        return Vec::new();
    }
    let path = state_path(cfg);
    if now - SweepState::read(&path).merges_at < SWEEP_EVERY_SECS {
        return Vec::new();
    }
    poll_merges(cfg, runner, &claimer(), now)
}

/// Poll the PR of every open task that has one and is this machine's to finish (claimed by
/// `claimer`, or claimed by nobody), completing the merged ones.
pub(crate) fn poll_merges(
    cfg: &Config,
    runner: &dyn CommandRunner,
    claimer: &str,
    now: i64,
) -> Vec<PrPoll> {
    let path = state_path(cfg);
    let mut state = SweepState::read(&path);
    state.merges_at = now;
    let store = task_store::task_store(cfg);
    let _lock = store.lock();
    let Ok(mut file) = store.load() else {
        let _ = state.write(&path);
        return Vec::new();
    };
    let today = today();
    let due: Vec<(String, String, String)> = file
        .tasks()
        .filter(|t| t.is_open(&today))
        .filter(|t| t.meta("claimer").is_none_or(|c| c == claimer))
        .filter_map(|t| {
            let pr = t.meta("pr").filter(|p| p.starts_with("http"))?;
            let id = t.id()?;
            let key = t.uid().unwrap_or(id);
            Some((id.to_string(), key.to_string(), pr.to_string()))
        })
        .collect();

    let mut polls = Vec::new();
    let mut merged: Vec<(String, String)> = Vec::new();
    for (id, key, pr) in due {
        let seen = state.prs.entry(key.clone()).or_default();
        if seen.pr != pr {
            *seen = PrSeen {
                pr: pr.clone(),
                ..PrSeen::default()
            };
        }
        let mut poll = PrPoll {
            task: id.clone(),
            pr: pr.clone(),
            outcome: "waiting",
        };
        if seen.polled > 0 && now - seen.polled < PR_POLL_SECS {
            polls.push(poll);
            continue;
        }
        seen.polled = now;
        poll.outcome = match pr_state(runner, &pr).as_deref() {
            Some("MERGED") => {
                file.mark_done(&id, &today);
                merged.push((id.clone(), key));
                "merged"
            }
            Some("CLOSED") => {
                if !seen.closed_noted {
                    seen.closed_noted = true;
                    task_store::note_best_effort(
                        store.as_ref(),
                        &id,
                        &format!("PR closed without merge; task left open ({pr})"),
                    );
                }
                "closed"
            }
            Some(_) => "open",
            None => "unknown",
        };
        polls.push(poll);
    }

    if !merged.is_empty() {
        match store.save(&file) {
            Ok(()) => {
                for (id, key) in &merged {
                    task_store::note_best_effort(store.as_ref(), id, "PR merged, completing");
                    state.prs.remove(key);
                }
            }
            Err(e) => {
                eprintln!("bn forge: could not complete merged tasks — {e}");
                // Ask again next pass rather than in ten minutes.
                for (_, key) in &merged {
                    state.prs.remove(key);
                }
                for p in polls.iter_mut().filter(|p| p.outcome == "merged") {
                    p.outcome = "unknown";
                }
            }
        }
    }
    let _ = state.write(&path);
    polls
}

#[cfg(test)]
mod tests;
