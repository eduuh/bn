//! `bn orchestrate` — the native, event-driven fleet orchestrator (#338 / #340).
//!
//! The daemon that replaces the hand-authored, scheduled-prompt supervision wave. It owns a
//! persisted [`OrchestrateState`] queue and drives each item **dispatch → self-gate → merge-queue →
//! next** until the queue drains, recovering wedged agents (api-error / dns-outage / idle-with-WIP /
//! dead) with exponential backoff. The *policy* — pane-state → action, scheduling, backoff — lives
//! in the pure, unit-tested [`bn_core::orchestrate`] core; this module is the thin executor that
//! captures the live signals (tmux pane, git WIP, PR/worktree state) and realizes each action by
//! shelling out to the proven `bn` subcommands (`bn dispatch`, `bn wt merge --all`, `bn agent
//! recover`, `bn agent send`). Shelling out (rather than in-process calls) isolates each side effect:
//! a `fail()`/exit or a pane-respawn inside a child never takes down the daemon.
//!
//! Event-driven, not a fixed clock: actionable fleet transitions land in bn-core's durable typed
//! wake queue, whose filesystem signal wakes resident supervisors immediately. The active/idle
//! intervals remain bounded fallbacks for time-based deadlines and unavailable filesystem watchers,
//! rather than the normal supervision clock.

use crate::*;
use bn_core::backlog::{self, AddSpec, BacklogKind, BacklogStatus, Substrate};
use bn_core::events::EventLevel;
use bn_core::orchestrate::{
    self, Backoff, ExternalState, HistoryRecord, ItemStatus, Liveness, OrchestrateState,
    PaneObservation, PaneState, QueueEntrySpec, RelaunchDecision, RelaunchKind, SupervisorAction,
    WorkClass,
};
use bn_core::runner::CommandRunner;
use std::path::{Path, PathBuf};

/// Poll cadence while work is in flight — react fast to completions/outages.
const POLL_ACTIVE_SECS: u64 = 15;
/// Poll cadence while only waiting (all items queued behind the cap, or backoff pending).
const POLL_IDLE_SECS: u64 = 45;
/// A launcher can outlive a crashed orchestrator briefly before its worktree, GitHub run, or worker
/// row becomes observable. Never replay a fresh claim inside this window.
const CLAIM_ABSENCE_GRACE_SECS: i64 = 120;
/// The backlog priority band `bn orchestrate ask` stamps on its scout entry (#378) — the ad-hoc
/// priority lane. Well above the default-`0` ship band, so an ask leases ahead of the whole ship
/// backlog and, paired with the scheduler's reserved lane-0 slot, dispatches on the next active
/// poll even with a full backlog running — without preempting a mid-flight ship.
const ASK_PRIORITY: i64 = 1000;
/// Concurrency for the empty run `bn orchestrate ask` seeds when no daemon is draining the repo yet
/// (a live/resident run is reused untouched). Small — the ask itself rides the reserved lane.
const ASK_SEED_CONCURRENCY: usize = 2;
/// The resume nudge delivered to an alive-but-wedged agent (api-error / dns-outage).
const RESUME_MSG: &str = "Resume where you left off. If you hit an API or network error, wait a few seconds and retry — don't stop. Run `bn verify` after any rebase, then `bn ingest`, and only then `bn agent done`.";
/// How long an agent may sit at an **idle-clean** prompt — worked (AIC > 0) but with no PR and no
/// fresh activity — before the supervisor treats it as *stalled* and hard-restarts it (#374). Long
/// enough that a brief between-turns pause or an agent about to open its PR / run `bn agent done` is
/// never cut off; short enough that a dead-idle agent stops starving its concurrency slot for hours.
const IDLE_CLEAN_STALE_SECS: i64 = 600;
/// A queue item or retry with no state change for this long needs an operator nudge.
const DOCTOR_STUCK_SECS: i64 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum OrchestrateDoctorSeverity {
    Info,
    Warning,
    Error,
}

impl OrchestrateDoctorSeverity {
    fn label(self) -> &'static str {
        match self {
            OrchestrateDoctorSeverity::Info => "info",
            OrchestrateDoctorSeverity::Warning => "warning",
            OrchestrateDoctorSeverity::Error => "error",
        }
    }

    fn unhealthy(self) -> bool {
        !matches!(self, OrchestrateDoctorSeverity::Info)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct OrchestrateDoctorFinding {
    severity: OrchestrateDoctorSeverity,
    code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
    detail: String,
    fix: String,
}

impl OrchestrateDoctorFinding {
    fn new(
        severity: OrchestrateDoctorSeverity,
        code: &'static str,
        repo: Option<&str>,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            code,
            repo: repo.map(str::to_string),
            detail: detail.into(),
            fix: fix.into(),
        }
    }
}

fn anchor_finding(name: &'static str, path: &Path) -> Option<OrchestrateDoctorFinding> {
    let fix = format!(
        "re-run install.sh to recreate the {name} anchor at {}",
        path.display()
    );
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(_) => {
            return Some(OrchestrateDoctorFinding::new(
                OrchestrateDoctorSeverity::Error,
                "anchor-missing",
                None,
                format!("{name} anchor is missing: {}", path.display()),
                fix,
            ));
        }
    };
    if !meta.file_type().is_symlink() {
        return Some(OrchestrateDoctorFinding::new(
            OrchestrateDoctorSeverity::Warning,
            "anchor-not-symlink",
            None,
            format!("{name} anchor is not a symlink: {}", path.display()),
            fix,
        ));
    }
    match std::fs::canonicalize(path) {
        Ok(target) if target.is_dir() => None,
        Ok(target) => Some(OrchestrateDoctorFinding::new(
            OrchestrateDoctorSeverity::Error,
            "anchor-not-directory",
            None,
            format!(
                "{name} anchor resolves to a non-directory: {} -> {}",
                path.display(),
                target.display()
            ),
            fix,
        )),
        Err(_) => Some(OrchestrateDoctorFinding::new(
            OrchestrateDoctorSeverity::Error,
            "anchor-dangling",
            None,
            format!("{name} anchor is dangling: {}", path.display()),
            fix,
        )),
    }
}

fn orchestrate_lock_findings(state_dir: &Path) -> Vec<OrchestrateDoctorFinding> {
    orchestrate_lock_findings_with(state_dir, bn_core::store::lock_is_stale)
}

fn orchestrate_lock_findings_with(
    state_dir: &Path,
    is_stale: impl Fn(&Path) -> bool,
) -> Vec<OrchestrateDoctorFinding> {
    let Ok(entries) = std::fs::read_dir(state_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with("orchestrate-") && n.ends_with(".json.lock")
                    || n == "orchestrate.json.lock"
            })
        })
        .filter(|path| is_stale(path))
        .map(|path| {
            let target = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".lock"))
                .map(|n| path.with_file_name(n));
            let orphaned = target.as_ref().is_some_and(|target| !target.exists());
            OrchestrateDoctorFinding::new(
                OrchestrateDoctorSeverity::Error,
                if orphaned {
                    "lock-stale-orphaned"
                } else {
                    "lock-stale"
                },
                None,
                format!(
                    "stale orchestrator lock{}: {}",
                    if orphaned { " with no state file" } else { "" },
                    path.display()
                ),
                format!(
                    "remove {} after confirming no orchestrate writer is active",
                    path.display()
                ),
            )
        })
        .collect()
}

fn pane_needs_attention(state: PaneState) -> bool {
    matches!(
        state,
        PaneState::IdleWithWip
            | PaneState::TrustMenu
            | PaneState::CommandApproval
            | PaneState::QuestionMenu
            | PaneState::ApiError
            | PaneState::DnsOutage
            | PaneState::Limited
            | PaneState::Dead
    )
}

fn run_doctor_findings(
    cfg: &Config,
    st: &OrchestrateState,
    now: i64,
) -> Vec<OrchestrateDoctorFinding> {
    let mut findings = Vec::new();
    let counts = st.counts();

    if st.stopped {
        findings.push(OrchestrateDoctorFinding::new(
            OrchestrateDoctorSeverity::Warning,
            "run-stopped-resident",
            Some(&st.repo),
            format!(
                "stopped run is still resident in state with {} queued and {} in flight",
                counts.queued, counts.in_flight
            ),
            format!(
                "inspect `bn orchestrate status --repo {}` and remove or restart the run",
                st.repo
            ),
        ));
    } else if st.is_drained() && !st.is_resident() {
        findings.push(OrchestrateDoctorFinding::new(
            OrchestrateDoctorSeverity::Warning,
            "run-drained-resident",
            Some(&st.repo),
            "drained one-shot run is still resident in state",
            format!(
                "start a fresh run or archive {}",
                orchestrate::state_path_for(&st.repo).display()
            ),
        ));
    }

    let abandoned = counts.queued > 0
        && counts.in_flight == 0
        && (st.stopped || now.saturating_sub(st.updated_at) >= DOCTOR_STUCK_SECS);
    if abandoned {
        findings.push(OrchestrateDoctorFinding::new(
            OrchestrateDoctorSeverity::Error,
            "queue-abandoned",
            Some(&st.repo),
            format!(
                "{} queued item(s) have no active lane; last run update was {}s ago",
                counts.queued,
                now.saturating_sub(st.updated_at)
            ),
            format!(
                "restart supervision with `bn orchestrate supervise --repo {}`",
                st.repo
            ),
        ));
    }

    if counts.queued > 0
        && (st.local_in_flight() >= st.total_local_cap()
            || st.dispatch_bind.as_deref() == Some("local-cap"))
    {
        findings.push(OrchestrateDoctorFinding::new(
            OrchestrateDoctorSeverity::Info,
            "lanes-saturated",
            Some(&st.repo),
            format!(
                "{} local lane(s) fill cap {} while {} item(s) wait",
                st.local_in_flight(),
                st.total_local_cap(),
                counts.queued
            ),
            format!(
                "inspect `bn orchestrate status --repo {}` or scale the run",
                st.repo
            ),
        ));
    }

    let backoff = Backoff::default();
    for item in &st.queue {
        if item.status == ItemStatus::Dispatched
            && item.substrate != Substrate::Remote
            && item.target.is_local()
        {
            match item.branch.as_deref() {
                Some(branch) => {
                    let path = worktree_path(cfg, &st.repo, branch);
                    if !path.join(".git").exists() {
                        findings.push(OrchestrateDoctorFinding::new(
                            OrchestrateDoctorSeverity::Error,
                            "worktree-missing",
                            Some(&st.repo),
                            format!(
                                "in-flight item {} references missing worktree {}",
                                item.id,
                                path.display()
                            ),
                            format!(
                                "reconcile or recover item {} before dispatching replacement work",
                                item.id
                            ),
                        ));
                    }
                }
                None => findings.push(OrchestrateDoctorFinding::new(
                    OrchestrateDoctorSeverity::Error,
                    "worktree-unaddressable",
                    Some(&st.repo),
                    format!(
                        "in-flight item {} has no branch/worktree reference",
                        item.id
                    ),
                    format!(
                        "reconcile item {} or mark the run failed explicitly",
                        item.id
                    ),
                )),
            }
        }

        if item.status == ItemStatus::Queued {
            if let Some(holder) = &item.held_by {
                let holder_live = st
                    .queue
                    .iter()
                    .any(|candidate| candidate.id == *holder && candidate.status.in_flight());
                findings.push(OrchestrateDoctorFinding::new(
                    if holder_live {
                        OrchestrateDoctorSeverity::Info
                    } else {
                        OrchestrateDoctorSeverity::Error
                    },
                    if holder_live {
                        "queue-held"
                    } else {
                        "queue-held-orphaned"
                    },
                    Some(&st.repo),
                    format!(
                        "queued item {} is held by {}{}",
                        item.id,
                        holder,
                        if holder_live {
                            ""
                        } else {
                            ", which is not in flight"
                        }
                    ),
                    format!(
                        "inspect conflict holds with `bn orchestrate status --repo {}`",
                        st.repo
                    ),
                ));
            }
        }

        let Some(state) = item.last_state.filter(|state| pane_needs_attention(*state)) else {
            continue;
        };
        let stale_for = now.saturating_sub(item.updated_at);
        let exhausted = item.attempts >= backoff.max_attempts;
        findings.push(OrchestrateDoctorFinding::new(
            if exhausted || stale_for >= DOCTOR_STUCK_SECS {
                OrchestrateDoctorSeverity::Error
            } else {
                OrchestrateDoctorSeverity::Warning
            },
            if item.attempts > 0 {
                "pane-retrying"
            } else {
                "pane-wedged"
            },
            Some(&st.repo),
            format!(
                "item {} pane is {} with {} relaunch attempt(s); unchanged for {}s",
                item.id,
                state.label(),
                item.attempts,
                stale_for
            ),
            format!(
                "jump to or recover item {} from `bn orchestrate status --repo {}`",
                item.id, st.repo
            ),
        ));
    }
    findings
}

fn orchestrate_doctor_healthy(findings: &[OrchestrateDoctorFinding]) -> bool {
    !findings.iter().any(|finding| finding.severity.unhealthy())
}

fn repo_anchor_paths(config_dir: &Path, home: Option<&Path>) -> Vec<(&'static str, PathBuf)> {
    let resolved = config_dir.join("repo");
    let mut paths = vec![("repo", resolved.clone())];
    if let Some(home) = home {
        let workflow = home.join(".config/bn/repo");
        if workflow != resolved {
            paths.push(("workflow repo", workflow));
        }
    }
    paths
}

fn load_doctor_runs(
    paths: &[PathBuf],
    repo: Option<&str>,
) -> (Vec<OrchestrateState>, Vec<OrchestrateDoctorFinding>) {
    let mut runs = Vec::new();
    let mut findings = Vec::new();
    for path in paths {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                findings.push(OrchestrateDoctorFinding::new(
                    OrchestrateDoctorSeverity::Error,
                    "state-unreadable",
                    repo,
                    format!(
                        "orchestrator state cannot be read: {} ({error})",
                        path.display()
                    ),
                    format!(
                        "restore permissions or remove the corrupt state file {}",
                        path.display()
                    ),
                ));
                continue;
            }
        };
        let st: OrchestrateState = match serde_json::from_str(&text) {
            Ok(st) => st,
            Err(error) => {
                findings.push(OrchestrateDoctorFinding::new(
                    OrchestrateDoctorSeverity::Error,
                    "state-corrupt",
                    repo,
                    format!(
                        "orchestrator state is malformed: {} ({error})",
                        path.display()
                    ),
                    format!(
                        "restore or remove the corrupt state file {}",
                        path.display()
                    ),
                ));
                continue;
            }
        };
        if let Some(want) = repo {
            if st.repo != want {
                if path.file_name().and_then(|name| name.to_str())
                    == Some(orchestrate::state_file_name(want).as_str())
                {
                    findings.push(OrchestrateDoctorFinding::new(
                        OrchestrateDoctorSeverity::Error,
                        "state-repo-mismatch",
                        Some(want),
                        format!(
                            "state file {} contains repo {} instead of {want}",
                            path.display(),
                            st.repo
                        ),
                        format!(
                            "move or repair the mismatched state file {}",
                            path.display()
                        ),
                    ));
                }
                continue;
            }
        }
        runs.push(st);
    }
    (runs, findings)
}

fn collect_orchestrate_doctor(
    cfg: &Config,
    repo: Option<&str>,
    now: i64,
) -> Vec<OrchestrateDoctorFinding> {
    let mut findings = Vec::new();
    let config_dir = resolver::config_dir();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    for (name, path) in repo_anchor_paths(&config_dir, home.as_deref()) {
        if let Some(finding) = anchor_finding(name, &path) {
            findings.push(finding);
        }
    }
    if let Some(finding) = anchor_finding("jobs", &resolver::jobs_dir()) {
        findings.push(finding);
    }
    findings.extend(orchestrate_lock_findings(&resolver::state_dir()));
    let mut paths = match repo {
        Some(repo) => {
            let path = orchestrate::state_path_for(repo);
            path.exists().then_some(path).into_iter().collect()
        }
        None => orchestrate::run_paths(),
    };
    let legacy = orchestrate::legacy_state_path();
    if legacy.exists() {
        paths.push(legacy);
    }
    let (runs, state_findings) = load_doctor_runs(&paths, repo);
    findings.extend(state_findings);
    for st in runs {
        findings.extend(run_doctor_findings(cfg, &st, now));
    }
    findings
}

// ===========================================================================
// `bn orchestrate --queue …` / `start` — seed the run
// ===========================================================================

/// Seed (or extend) an orchestration run from a declared queue and start driving it. Returns false
/// on a fatal setup error (bad repo). With `foreground`, runs the loop inline; otherwise it hands
/// off to a daemon ([`install_repo_daemon`]: launchd on macOS, a systemd `--user` service on Linux)
/// — falling back to a detached child when no service manager exists, so the loop still survives
/// this shell exiting.
#[allow(clippy::too_many_arguments)]
pub(crate) fn orchestrate_start(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo_override: Option<&str>,
    queue: &[String],
    concurrency: usize,
    coding: Option<usize>,
    research: Option<usize>,
    solo: bool,
    agent: Option<&str>,
    foreground: bool,
    json: bool,
) -> bool {
    // #343: a `repo:subject` prefix on a queue entry targets *that* repo, so one
    // `bn orchestrate --queue …` can seed work across several repos, keyed by repo. Unprefixed
    // entries fall to the default repo (`--repo`, else cwd). The prefix is only honored when the
    // token resolves to a known bare repo, so URLs and `owner/repo#123` slugs stay whole.
    let default = resolve_target_repo(cfg, ctx, repo_override);
    let default_repo = default.as_ref().ok().cloned();
    let resolve = |name: &str| {
        wt::resolve_bare_by_name(cfg, Some(name))
            .ok()
            .flatten()
            .map(|(r, _)| r)
    };
    let groups = match partition_queue_by_repo(queue, default_repo.as_deref(), resolve) {
        Ok(g) => g,
        Err(()) => {
            // An unprefixed entry needs a default repo, but none resolved — surface the real reason.
            let e = default
                .err()
                .unwrap_or_else(|| "not in a repo — pass --repo <name> to target one".to_string());
            let msg = format!("bn orchestrate: {e}");
            if json {
                println!(
                    "{}",
                    Envelope::failure("orchestrate", "no-repo", msg).to_json()
                );
            } else {
                eprintln!("{msg}");
            }
            return false;
        }
    };

    // First contact with the post-#395 layout: move a legacy machine-wide run (and its daemon)
    // to its own per-repo file/label before touching any state — even when it's another repo's.
    ensure_migrated(true);
    let now = bn_core::util::now_epoch_secs();

    // Foreground drives one loop inline, so it only makes sense for a single repo; a multi-repo
    // queue always daemonizes each repo's run instead.
    let foreground = foreground && groups.len() == 1;

    let mut datas: Vec<serde_json::Value> = Vec::new();
    for (repo, items) in &groups {
        let data = seed_repo_state(
            cfg,
            repo,
            items,
            concurrency,
            coding,
            research,
            solo,
            agent,
            foreground,
            now,
            json,
        );
        datas.push(data);

        if foreground {
            emit_start_json(json, &datas);
            orchestrate_run(cfg, Some(repo), false, false, json);
            return true;
        }

        // Daemonize (launchd on macOS, a systemd --user service on Linux #608), else fall back to
        // a detached child so the loop outlives this shell. #395: one daemon per repo
        // (`com.bn.orchestrate.<repo>` / `bn-orchestrate-<repo>.service`), so a second run never
        // hijacks — or is hijacked by — another repo's daemon.
        let kind = install_repo_daemon(repo);
        if !json {
            println!("orchestrate: {}", kind.note());
            println!("  bn orchestrate status   ·   bn orchestrate stop --repo {repo}");
        }
    }
    emit_start_json(json, &datas);
    true
}

/// Seed (or extend) a single repo's per-repo run from `items` and persist it — the per-repo core of
/// [`orchestrate_start`], shared by every group of a multi-repo `--queue`. Prints the human seed
/// line (unless `json`) and returns the per-repo result payload for the caller to render.
#[allow(clippy::too_many_arguments)]
fn seed_repo_state(
    cfg: &Config,
    repo: &str,
    items: &[String],
    concurrency: usize,
    coding: Option<usize>,
    research: Option<usize>,
    solo: bool,
    agent: Option<&str>,
    foreground: bool,
    now: i64,
    json: bool,
) -> serde_json::Value {
    // #395: state is keyed per repo, so this load/save can only ever see *this* repo's run —
    // a second orchestration (different repo) writes its own file and never overwrites a live one.
    let path = orchestrate::state_path_for(repo);
    // Extend an existing live run (same repo), else start fresh.
    let mut st = match OrchestrateState::load(&path) {
        // Reuse a live run — or a **standing supervisor** (#426), which is `is_drained()` while it
        // idly waits yet must not be clobbered back to a plain, self-exiting run. Reusing it also
        // preserves its `supervise`/`self_feed` flags, so declaring a queue on a resident repo extends it.
        Some(existing)
            if (!existing.is_drained() || existing.is_resident())
                && !existing.stopped
                && existing.repo == repo =>
        {
            existing
        }
        // #380: a drained/stopped prior run is about to be overwritten by this fresh queue — snapshot
        // its terminal outcomes to run-history.jsonl first, so merged/failed results (and any not yet
        // aged past the eviction window) stay durably queryable across the restart.
        prev => {
            if let Some(old) = prev {
                archive_all_terminal(&old, repo, now);
            }
            OrchestrateState::new(repo, &[], concurrency, solo, now)
        }
    };
    st.concurrency = concurrency.max(1);
    st.solo = solo;
    // #385: typed concurrency lanes — an explicit `--coding` overrides the coding-lane cap and
    // `--research` opens/sets the research lane; omitting both keeps the untyped single-pool run.
    st.scale(coding, research);
    st.stopped = false;
    // #382: pin the harness the daemon dispatches under. An explicit `--agent` (validated) always
    // wins; otherwise a fresh/legacy run inherits the `[orchestrate].agent` config default, and an
    // existing run keeps its harness. `dispatch_harness()` supplies `copilot` when none is set.
    match agent {
        Some(a) => match crate::cmd::dispatch::normalize_agent(a) {
            Some(h) => st.harness = Some(h.to_string()),
            None => fail(&format!(
                "bn orchestrate: unknown --agent '{a}' (use: copilot | claude)"
            )),
        },
        None => ensure_default_harness(cfg, &mut st),
    }
    let added = st.extend(items, now);
    st.updated_at = now;
    if let Err(e) = st.save(&path) {
        fail(&format!("bn orchestrate: could not persist run state: {e}"));
    }
    log_event(
        "orchestrate_start",
        &[("repo", repo), ("added", &added.to_string())],
    );

    if !json {
        println!(
            "orchestrate: {repo} — {} item(s) queued (cap {}{}) · agent {}",
            st.counts().queued,
            caps_label(&st),
            if st.solo { ", solo" } else { "" },
            st.dispatch_harness()
        );
    }

    serde_json::json!({
        "repo": repo,
        "added": added,
        "queued": st.counts().queued,
        "concurrency": st.concurrency,
        "research": st.research,
        "solo": st.solo,
        "agent": st.dispatch_harness(),
        "foreground": foreground,
    })
}

/// Emit the `--json` result for a `--queue` seed: a single-repo run keeps the historical flat
/// envelope (backward-compatible), while a multi-repo seed wraps the per-repo payloads in `repos`.
fn emit_start_json(json: bool, datas: &[serde_json::Value]) {
    if !json {
        return;
    }
    let env = if datas.len() == 1 {
        Envelope::success("orchestrate", datas[0].clone())
    } else {
        Envelope::success("orchestrate", serde_json::json!({ "repos": datas }))
    };
    println!("{}", env.to_json());
}

/// Split a `--queue` entry into `(repo, subject)` when it carries a `repo:` prefix that `resolve`
/// maps to a known bare repo. Returns `None` (leave the entry whole) when there is no colon, when
/// either side is empty, or when the prefix doesn't resolve — so URLs (`https://…`) and
/// `owner/repo#123` slugs, whose leading token is not a repo, are never mis-split.
fn split_repo_prefix(
    item: &str,
    resolve: &impl Fn(&str) -> Option<String>,
) -> Option<(String, String)> {
    let (pre, rest) = item.split_once(':')?;
    let (pre, rest) = (pre.trim(), rest.trim());
    if pre.is_empty() || rest.is_empty() {
        return None;
    }
    let canon = resolve(pre)?;
    Some((canon, rest.to_string()))
}

/// Partition a declared queue into per-repo groups (#343). Each entry may carry a `repo:subject`
/// prefix (see [`split_repo_prefix`]); unprefixed entries go to `default_repo`. Group order follows
/// first appearance and subjects are de-duplicated within a group (matching [`OrchestrateState::extend`]).
/// Returns `Err(())` when an unprefixed entry needs a default repo but `default_repo` is `None`, so
/// the caller can surface the underlying repo-resolution error.
fn partition_queue_by_repo(
    queue: &[String],
    default_repo: Option<&str>,
    resolve: impl Fn(&str) -> Option<String>,
) -> Result<Vec<(String, Vec<String>)>, ()> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for item in queue {
        let (repo, subject) = match split_repo_prefix(item, &resolve) {
            Some(pair) => pair,
            None => match default_repo {
                Some(d) => (d.to_string(), item.clone()),
                None => return Err(()),
            },
        };
        if !groups.contains_key(&repo) {
            order.push(repo.clone());
        }
        let bucket = groups.entry(repo).or_default();
        if !bucket.contains(&subject) {
            bucket.push(subject);
        }
    }
    Ok(order
        .into_iter()
        .map(|r| {
            let items = groups.remove(&r).unwrap_or_default();
            (r, items)
        })
        .collect())
}

// ===========================================================================
// `bn orchestrate supervise` — the standing, session-independent supervisor (#426)
// ===========================================================================

/// Start (or upgrade to) a **standing supervisor** for `repo` — the session-independent fix for the
/// stalled unattended drain (#426). Unlike a `--queue` run, a supervisor keeps standing after its
/// ledger drains and, each tick, additionally adopts the repo's live standalone `bn dispatch` lanes
/// so their green PRs land and dead/stalled lanes recover with nobody at the keyboard. It reuses the
/// same per-repo launchd daemon (`RunAtLoad` + `KeepAlive`) as a run — so it survives the launching
/// shell exiting, a crash, and a reboot — falling back to a detached child when launchd is absent.
///
/// Idempotent: an existing run for the repo is upgraded in place (its `supervise` flag flipped on and
/// its declared queue kept draining); otherwise an empty supervisor run is seeded. Returns false only
/// on a fatal setup error (unknown repo).
///
/// With `all_repos` (#455) it instead supervises **every configured repo** in one invocation —
/// [`orchestrate_supervise_all`] — so agents dispatched into any repo's tmux session get landed +
/// recovered, not just the current repo's.
pub(crate) struct SuperviseOptions {
    pub all_repos: bool,
    pub concurrency: usize,
    pub keep_warm: bool,
    pub low_water: Option<usize>,
    pub owned_repos: Vec<String>,
    pub exclude_titles: Vec<String>,
    pub exclude_labels: Vec<String>,
    pub brainstorm_count: Option<usize>,
    pub foreground: bool,
    pub json: bool,
}

pub(crate) fn orchestrate_supervise(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo_override: Option<&str>,
    options: SuperviseOptions,
) -> bool {
    let keep_warm = resolve_keep_warm_policy(
        cfg,
        options.keep_warm,
        options.low_water,
        &options.exclude_titles,
        &options.exclude_labels,
        options.brainstorm_count,
    );
    let owned_repos = if options.owned_repos.is_empty() {
        cfg.orchestrate_owned_repos.clone()
    } else {
        options.owned_repos
    };
    if keep_warm.is_some() && owned_repos.is_empty() {
        eprintln!(
            "bn orchestrate supervise: keep-warm requires --own <repos> or [orchestrate].own"
        );
        return false;
    }
    if options.all_repos {
        return orchestrate_supervise_all(
            cfg,
            options.concurrency,
            keep_warm.as_ref(),
            &owned_repos,
            options.foreground,
            options.json,
        );
    }
    let repo = match resolve_target_repo(cfg, ctx, repo_override) {
        Ok(r) => r,
        Err(e) => {
            if options.json {
                println!(
                    "{}",
                    Envelope::failure(
                        "orchestrate",
                        "no-repo",
                        format!("bn orchestrate supervise: {e}")
                    )
                    .to_json()
                );
            } else {
                eprintln!("bn orchestrate supervise: {e}");
            }
            return false;
        }
    };

    // Move a legacy machine-wide run to the per-repo layout before touching any state (#395).
    ensure_migrated(true);

    let now = bn_core::util::now_epoch_secs();
    if keep_warm.is_some()
        && !owned_repos.is_empty()
        && !owned_repos.iter().any(|owned| owned == &repo)
    {
        eprintln!("bn orchestrate supervise: repo '{repo}' is not owned by this box");
        return false;
    }
    let st = seed_supervisor(cfg, &repo, options.concurrency, keep_warm.as_ref(), now);

    if options.json {
        let data = serde_json::json!({
            "repo": repo,
            "supervise": true,
            "keep_warm": st.keep_warm,
            "concurrency": st.concurrency,
            "queued": st.counts().queued,
            "in_flight": st.in_flight(),
            "foreground": options.foreground,
        });
        println!("{}", Envelope::success("orchestrate", data).to_json());
    } else {
        println!(
            "orchestrate: supervising {repo} — lands ready PRs + recovers dead dispatch lanes unattended (cap {}).",
            caps_label(&st)
        );
    }

    if options.foreground {
        orchestrate_run(cfg, Some(&repo), false, false, options.json);
        return true;
    }

    // Daemonize (launchd on macOS, a systemd --user service on Linux #608), else a detached child —
    // the same session-independent plumbing a run uses.
    let kind = install_repo_daemon(&repo);
    if !options.json {
        println!("orchestrate: {}", kind.note());
        println!("  bn orchestrate status --repo {repo}   ·   bn orchestrate stop --repo {repo}");
    }
    true
}

/// Seed (or upgrade in place) a resident supervisor run for `repo` and persist it (#426/#455). A
/// live run for the repo keeps its queue/recovery history — only its `supervise` flag is flipped on
/// and `stopped` cleared; otherwise an empty supervisor run is seeded. Shared by the single-repo and
/// the cross-repo (`--all-repos`) supervise paths so both agree on what a supervisor state is.
fn resolve_keep_warm_policy(
    cfg: &Config,
    cli_enabled: bool,
    low_water: Option<usize>,
    exclude_titles: &[String],
    exclude_labels: &[String],
    brainstorm_count: Option<usize>,
) -> Option<orchestrate::KeepWarmPolicy> {
    if !cli_enabled && !cfg.orchestrate_keep_warm {
        return None;
    }
    Some(orchestrate::KeepWarmPolicy {
        low_water: low_water.unwrap_or(cfg.orchestrate_low_water).max(1),
        brainstorm_count: brainstorm_count.unwrap_or(cfg.orchestrate_brainstorm_count),
        brainstorm_interval_secs: cfg.orchestrate_brainstorm_interval_secs,
        exclude_titles: if exclude_titles.is_empty() {
            cfg.orchestrate_exclude_titles.clone()
        } else {
            exclude_titles.to_vec()
        },
        exclude_labels: if exclude_labels.is_empty() {
            cfg.orchestrate_exclude_labels.clone()
        } else {
            exclude_labels.to_vec()
        },
    })
}

fn keep_warm_policy_for_repo<'a>(
    policy: Option<&'a orchestrate::KeepWarmPolicy>,
    owned_repos: &[String],
    repo: &str,
) -> Option<&'a orchestrate::KeepWarmPolicy> {
    policy.filter(|_| owned_repos.iter().any(|owned| owned == repo))
}

/// One scheduled keep-warm pass for the repos this machine owns. Unlike `supervise`, this does not
/// start a daemon or alter a run's stopped/resident lifecycle; it only refreshes the configured
/// policy and invokes the same refill planner used by a live supervisor.
pub(crate) fn job_keep_warm(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let Some(policy) = resolve_keep_warm_policy(cfg, false, None, &[], &[], None) else {
        println!("keep-warm: disabled");
        return 0;
    };
    if cfg.orchestrate_owned_repos.is_empty() {
        eprintln!("keep-warm: [orchestrate].own is empty");
        return 1;
    }

    let now = bn_core::util::now_epoch_secs();
    let repos = keep_warm_job_repos(&cfg.orchestrate_owned_repos);
    let mut failed = 0;

    for repo in &repos {
        if a_repo_worktree(cfg, repo).is_none() {
            println!("keep-warm: {repo} is not present on this box; skipped");
            continue;
        }
        let path = orchestrate::state_path_for(repo);
        if let Err(error) = persist_keep_warm_policy(&path, repo, &policy, now) {
            eprintln!("keep-warm: could not persist {repo} state: {error}");
            failed += 1;
            continue;
        }
        if keep_warm_tick(cfg, &path, repo, now) {
            println!("keep-warm: checked {repo}");
        } else {
            failed += 1;
        }
    }

    if failed == 0 {
        0
    } else {
        1
    }
}

fn persist_keep_warm_policy(
    path: &Path,
    repo: &str,
    policy: &orchestrate::KeepWarmPolicy,
    now: i64,
) -> std::io::Result<()> {
    let _lock = bn_core::store::FileLock::acquire_required(path)?;
    let mut state = OrchestrateState::load(path)
        .unwrap_or_else(|| OrchestrateState::new(repo, &[], 1, false, now));
    state.keep_warm = Some(policy.clone());
    state.updated_at = now;
    state.save(path)
}

fn keep_warm_job_repos(owned_repos: &[String]) -> Vec<String> {
    let mut repos = owned_repos.to_vec();
    repos.sort();
    repos.dedup();
    repos
}

fn seed_supervisor(
    cfg: &Config,
    repo: &str,
    concurrency: usize,
    keep_warm: Option<&orchestrate::KeepWarmPolicy>,
    now: i64,
) -> OrchestrateState {
    let path = orchestrate::state_path_for(repo);
    let mut st = match OrchestrateState::load(&path) {
        Some(existing) if !existing.stopped && existing.repo == repo => existing,
        _ => OrchestrateState::new(repo, &[], concurrency, false, now),
    };
    st.concurrency = concurrency.max(1);
    st.supervise = true;
    st.keep_warm = keep_warm.cloned();
    st.stopped = false;
    ensure_default_harness(cfg, &mut st);
    st.updated_at = now;
    if let Err(e) = st.save(&path) {
        fail(&format!(
            "bn orchestrate supervise: could not persist run state: {e}"
        ));
    }
    log_event("orchestrate_supervise", &[("repo", repo)]);
    st
}

/// #455: supervise **every configured repo** in one invocation — the cross-repo fix for
/// session-per-repo supervision. Seeds a resident supervisor for each configured bare repo (so a
/// repo with a live agent session is already covered), then drives the cross-repo sweep loop
/// ([`orchestrate_run_all`]), which each tick lands + recovers agents across *all* repo tmux
/// sessions — not just the current one. Foreground blocks; otherwise it daemonizes via the
/// all-repos daemon (launchd on macOS, a systemd `--user` service on Linux), falling back to a
/// detached child. Returns false only when no repos are configured.
fn orchestrate_supervise_all(
    cfg: &Config,
    concurrency: usize,
    keep_warm: Option<&orchestrate::KeepWarmPolicy>,
    owned_repos: &[String],
    foreground: bool,
    json: bool,
) -> bool {
    ensure_migrated(true);
    let repos = configured_repos(cfg);
    if repos.is_empty() {
        let msg = "bn orchestrate supervise --all-repos: no bare repos configured — clone one first (bn wt clone <url>)";
        if json {
            println!(
                "{}",
                Envelope::failure("orchestrate", "no-repo", msg).to_json()
            );
        } else {
            eprintln!("{msg}");
        }
        return false;
    }
    let now = bn_core::util::now_epoch_secs();
    for repo in &repos {
        seed_supervisor(
            cfg,
            repo,
            concurrency,
            keep_warm_policy_for_repo(keep_warm, owned_repos, repo),
            now,
        );
    }
    log_event(
        "orchestrate_supervise_all",
        &[("repos", &repos.len().to_string())],
    );

    if json {
        let data = serde_json::json!({
            "all_repos": true,
            "supervise": true,
            "repos": repos,
            "concurrency": concurrency.max(1),
            "foreground": foreground,
        });
        println!("{}", Envelope::success("orchestrate", data).to_json());
    } else {
        println!(
            "orchestrate: supervising {} repos ({}) — lands ready PRs + recovers dead dispatch lanes across every repo session (cap {}).",
            repos.len(),
            repos.join(", "),
            concurrency.max(1)
        );
    }

    if foreground {
        orchestrate_run_all(cfg, false, json);
        return true;
    }

    let kind = install_all_daemon();
    if !json {
        println!("orchestrate: {}", kind.note());
        println!("  bn orchestrate status   ·   bn orchestrate stop --all-repos");
    }
    true
}

/// Every configured bare repo's local name, sorted+deduped (the #455 land/sweep universe). Reads
/// `<bare_dir>/*.git` via the same layout `wt` uses, so it agrees with `bn wt`/`bn dispatch` on what
/// repos exist.
fn configured_repos(cfg: &Config) -> Vec<String> {
    let (bare_dir, _) = wt::layout(cfg);
    let names: Vec<String> = wt::bare_repos(&bare_dir)
        .iter()
        .map(|b| wt::repo_name(b))
        .collect();
    bn_core::crossrepo::cross_repo_land_order(&names)
}

/// Resolve the single target repo for a run/backlog op: an explicit `--repo` name, else the current
/// worktree's repo, else an error the caller renders. Shared by `start` and `add`.
fn resolve_target_repo(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo_override: Option<&str>,
) -> Result<String, String> {
    match wt::resolve_bare_by_name(cfg, repo_override) {
        Ok(Some((r, _bare))) => Ok(r),
        Ok(None) => match ctx {
            Some(c) => Ok(c.repo.clone()),
            None => Err("not in a repo — pass --repo <name> to target one".to_string()),
        },
        Err(e) => Err(e),
    }
}

/// Whether `repo` has a live run — one that is draining, or a *resident* run (a #426 standing
/// supervisor or a #368 self-feeder) still watching/feeding after its queue drained. Either means a
/// daemon is already covering the repo, so `add` need not seed a fresh one.
fn has_live_run(repo: &str) -> bool {
    OrchestrateState::load(&orchestrate::state_path_for(repo))
        .is_some_and(|st| !st.stopped && (!st.is_drained() || st.is_resident()))
}

// ===========================================================================
// `bn orchestrate add` — append to the durable backlog (#375)
// ===========================================================================

/// Append `items` to the durable, prioritized backlog for `repo` (default: the current repo),
/// deduped by `(repo, id)`. The backlog is split from the live run state so it survives run
/// start/stop and reboots; the daemon pulls the highest-priority pending entries into the run as
/// concurrency slots free, so `add` works whether or not a run is live and mid-run adds are picked
/// up within one poll. With `start`, ensure a daemon is running for the repo afterward (seeding an
/// empty run when none is live) so a fresh backlog begins draining. Returns false only on a fatal
/// setup error (unknown repo).
#[allow(clippy::too_many_arguments)]
pub(crate) fn orchestrate_add(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo_override: Option<&str>,
    items: &[String],
    spec: AddSpec,
    start: bool,
    concurrency: usize,
    solo: bool,
    json: bool,
) -> bool {
    let repo = match resolve_target_repo(cfg, ctx, repo_override) {
        Ok(r) => r,
        Err(e) => {
            if json {
                println!(
                    "{}",
                    Envelope::failure("orchestrate", "no-repo", format!("bn orchestrate add: {e}"))
                        .to_json()
                );
            } else {
                eprintln!("bn orchestrate add: {e}");
            }
            return false;
        }
    };

    let now = bn_core::util::now_epoch_secs();
    let path = backlog::backlog_path();
    // One `AddSpec` covers a batch, but a task is per *work item* (#1001) — so add item by item,
    // each carrying the task that governs it. `backlog::add` is append-only, so N calls cost the
    // same as one batched call and keep the invariant exact.
    let mut added: Vec<String> = Vec::new();
    for item in items {
        let mut spec = spec.clone();
        spec.task = crate::cmd::task::task_for_enqueue(cfg, &repo, item, &spec.source);
        match backlog::add(&path, &repo, std::slice::from_ref(item), &spec, now) {
            Ok(ids) => added.extend(ids),
            Err(e) => {
                if json {
                    println!(
                        "{}",
                        Envelope::failure(
                            "orchestrate",
                            "backlog-write",
                            format!("bn orchestrate add: could not update backlog: {e}")
                        )
                        .to_json()
                    );
                } else {
                    eprintln!("bn orchestrate add: could not update backlog: {e}");
                }
                return false;
            }
        }
    }
    let added = match Ok::<Vec<String>, String>(added) {
        Ok(added) => added,
        Err(e) => {
            let msg = format!("bn orchestrate add: could not update backlog: {e}");
            if json {
                println!(
                    "{}",
                    Envelope::failure("orchestrate", "backlog-write", msg).to_json()
                );
            } else {
                eprintln!("{msg}");
            }
            return false;
        }
    };
    let depth = backlog::load(&path).pending_depth(&repo);
    log_event(
        "orchestrate_backlog_add",
        &[
            ("repo", &repo),
            ("added", &added.len().to_string()),
            ("depth", &depth.to_string()),
        ],
    );

    // Opt-in: begin draining an idle repo's backlog by ensuring a daemon exists (an empty seed run
    // — the daemon refills it from the backlog). A repo with a live run already drains on its own.
    if start {
        ensure_daemon_for(cfg, &repo, concurrency, solo);
    }

    if json {
        let data = serde_json::json!({
            "repo": repo,
            "added": added,
            "added_count": added.len(),
            "backlog": depth,
            "started": start,
        });
        println!("{}", Envelope::success("orchestrate", data).to_json());
    } else {
        let skipped = items.len().saturating_sub(added.len());
        let dup = if skipped > 0 {
            format!(", {skipped} dup skipped")
        } else {
            String::new()
        };
        println!(
            "orchestrate: {repo} — {} added to backlog{dup} ({depth} pending)",
            added.len()
        );
        if start {
            println!("  daemon draining {repo} — bn orchestrate status --repo {repo}");
        } else if depth > 0 && !has_live_run(&repo) {
            println!(
                "  no run draining {repo} — start one: bn orchestrate add --start <items…> (or --queue …)"
            );
        }
    }
    true
}

// ===========================================================================
// `bn orchestrate backlog` — read view of the durable backlog (#373)
// ===========================================================================

/// One backlog entry as a compact status line: `pri kind status age id — item [source] {touches}`.
fn render_backlog_line(e: &backlog::BacklogEntry, now: i64) -> String {
    let age = crate::cmd::cron::ago((now - e.added_at).max(0));
    let mut line = format!(
        "  p{:<3} {:<5} {:<7} {:>4}  {} — {}",
        e.priority,
        e.kind.label(),
        e.status.label(),
        age,
        e.id,
        e.item,
    );
    if !e.source.is_empty() {
        line.push_str(&format!("  [{}]", e.source));
    }
    if !e.touches.is_empty() {
        line.push_str(&format!("  {{{}}}", e.touches.join(", ")));
    }
    line
}

/// List the durable, prioritized backlog (#373) — the read counterpart to `add`. For one repo
/// (default: the current repo) or, with `all_repos`, every repo present in the backlog, in pull
/// order (priority desc, then FIFO). `status` filters the lifecycle (default pending); `limit` caps
/// rows per repo (0 = unlimited). Returns false only on a fatal setup error (unknown repo).
pub(crate) fn orchestrate_backlog(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo_override: Option<&str>,
    all_repos: bool,
    status_filter: Option<BacklogStatus>,
    limit: usize,
    json: bool,
) -> bool {
    let path = backlog::backlog_path();
    let bl = backlog::load(&path);
    let now = bn_core::util::now_epoch_secs();

    let repos: Vec<String> = if all_repos {
        bl.repos()
    } else {
        match resolve_target_repo(cfg, ctx, repo_override) {
            Ok(r) => vec![r],
            Err(e) => {
                if json {
                    println!(
                        "{}",
                        Envelope::failure(
                            "orchestrate",
                            "no-repo",
                            format!("bn orchestrate backlog: {e}"),
                        )
                        .to_json()
                    );
                } else {
                    eprintln!("bn orchestrate backlog: {e}");
                }
                return false;
            }
        }
    };

    let cap = |v: Vec<&backlog::BacklogEntry>| -> Vec<backlog::BacklogEntry> {
        let take = if limit == 0 { v.len() } else { limit };
        v.into_iter().take(take).cloned().collect()
    };

    if json {
        let repos_json: Vec<serde_json::Value> = repos
            .iter()
            .map(|repo| {
                let all = bl.list_view(repo, status_filter);
                let total = all.len();
                let entries = cap(all);
                serde_json::json!({
                    "repo": repo,
                    "matched": total,
                    "shown": entries.len(),
                    "pending": bl.pending_depth(repo),
                    "entries": entries,
                })
            })
            .collect();
        let data = serde_json::json!({
            "all_repos": all_repos,
            "status": status_filter.map(|s| s.label()).unwrap_or("all"),
            "limit": limit,
            "repos": repos_json,
        });
        println!("{}", Envelope::success("orchestrate", data).to_json());
        return true;
    }

    if repos.is_empty() {
        println!("orchestrate: backlog is empty");
        return true;
    }

    for repo in &repos {
        let all = bl.list_view(repo, status_filter);
        let total = all.len();
        let entries = cap(all);
        let filter_label = status_filter.map(|s| s.label()).unwrap_or("all");
        println!(
            "orchestrate: {repo} — {total} {filter_label} ({} pending)",
            bl.pending_depth(repo)
        );
        if entries.is_empty() {
            println!("  (none)");
        }
        for e in &entries {
            println!("{}", render_backlog_line(e, now));
        }
        if total > entries.len() {
            println!("  … {} more (raise --limit)", total - entries.len());
        }
    }
    true
}

// ===========================================================================
// `bn orchestrate queue-from <check>` — build the backlog from a failing check (#609)
// ===========================================================================

/// `bn orchestrate queue-from "<check>"` — seed the backlog from a corpus of failures instead of
/// hand-listing issue numbers (#609, inspired by Bun's Rust rewrite: `cargo check` → thousands of
/// errors → grouped by file → sharded across agents). Runs `check` as a shell command, parses its
/// stdout+stderr into per-file diagnostics ([`bn_core::queue_from`]), folds them into one ship work
/// item per group (file, or `dir` ≈ module/crate), and appends each to the durable backlog carrying
/// a `touches` glob so the conflict-aware scheduler (#376) never co-schedules two agents on the same
/// file. `dry_run` prints the shards without enqueuing; `start` begins draining. A check that exits
/// 0 (or emits no recognized diagnostics) enqueues nothing — you're already green.
#[allow(clippy::too_many_arguments)]
pub(crate) fn orchestrate_queue_from(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo_override: Option<&str>,
    check: &str,
    group_by: &str,
    priority: i64,
    dry_run: bool,
    start: bool,
    concurrency: usize,
    json: bool,
) -> bool {
    use bn_core::queue_from::{build_queue, GroupBy};

    let fail = |code: &str, msg: String| -> bool {
        if json {
            println!("{}", Envelope::failure("orchestrate", code, msg).to_json());
        } else {
            eprintln!("bn orchestrate queue-from: {msg}");
        }
        false
    };

    let repo = match resolve_target_repo(cfg, ctx, repo_override) {
        Ok(r) => r,
        Err(e) => return fail("no-repo", e),
    };
    let group_by = match GroupBy::parse(group_by) {
        Some(g) => g,
        None => {
            return fail(
                "bad-group-by",
                format!("unknown --group-by '{group_by}' (want: file | dir)"),
            );
        }
    };

    // Run the check as a shell command, capturing both streams — diagnostics land on either.
    if !json {
        println!("orchestrate: running check — {check}");
    }
    let output = match std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(check)
        .output()
    {
        Ok(o) => o,
        Err(e) => return fail("check-spawn", format!("failed to run check: {e}")),
    };
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push('\n');
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    let check_ok = output.status.success();

    let items = build_queue(check, &combined, group_by);
    if items.is_empty() {
        let note = if check_ok {
            "check passed — nothing to queue (already green)"
        } else {
            "check failed but no diagnostics were recognized — is the check output a supported format? (rustc/clippy or `path:line:col: message`)"
        };
        if json {
            println!(
                "{}",
                Envelope::success(
                    "orchestrate",
                    serde_json::json!({
                        "repo": repo,
                        "check": check,
                        "check_ok": check_ok,
                        "items": [],
                        "added": [],
                        "note": note,
                    }),
                )
                .to_json()
            );
        } else {
            println!("orchestrate: {note}");
        }
        return true;
    }

    let total_findings: usize = items.iter().map(|i| i.count).sum();

    if dry_run {
        if json {
            let shards: Vec<_> = items
                .iter()
                .map(|i| {
                    serde_json::json!({
                        "title": i.title,
                        "touches": i.touches,
                        "files": i.files,
                        "count": i.count,
                    })
                })
                .collect();
            println!(
                "{}",
                Envelope::success(
                    "orchestrate",
                    serde_json::json!({
                        "repo": repo,
                        "check": check,
                        "check_ok": check_ok,
                        "dry_run": true,
                        "shards": shards,
                    }),
                )
                .to_json()
            );
        } else {
            println!(
                "orchestrate: {} shard(s) from {total_findings} finding(s) — dry run, nothing enqueued",
                items.len()
            );
            for i in &items {
                println!(
                    "  [{:>3}] {}  {{{}}}",
                    i.count,
                    i.title,
                    i.touches.join(",")
                );
            }
        }
        return true;
    }

    // Append each shard to the durable backlog, carrying its touch area and provenance.
    let now = bn_core::util::now_epoch_secs();
    let path = backlog::backlog_path();
    let source = format!("queue-from:{check}");
    let mut added_total = 0usize;
    for item in &items {
        let spec = AddSpec {
            kind: backlog::BacklogKind::Ship,
            priority,
            touches: item.touches.clone(),
            substrate: Substrate::Auto,
            source: source.clone(),
            controller_id: std::env::var("BN_CONTROLLER_ID").ok(),
            task: crate::cmd::task::task_for_enqueue(cfg, &repo, &item.title, &source),
        };
        let added = match backlog::add(&path, &repo, std::slice::from_ref(&item.title), &spec, now)
        {
            Ok(added) => added,
            Err(e) => {
                eprintln!("bn orchestrate queue-from: could not update backlog: {e}");
                return false;
            }
        };
        added_total += added.len();
    }
    let depth = backlog::load(&path).pending_depth(&repo);
    log_event(
        "orchestrate_queue_from",
        &[
            ("repo", &repo),
            ("shards", &items.len().to_string()),
            ("added", &added_total.to_string()),
            ("findings", &total_findings.to_string()),
        ],
    );

    if start {
        ensure_daemon_for(cfg, &repo, concurrency, false);
    }

    if json {
        println!(
            "{}",
            Envelope::success(
                "orchestrate",
                serde_json::json!({
                    "repo": repo,
                    "check": check,
                    "check_ok": check_ok,
                    "shards": items.len(),
                    "findings": total_findings,
                    "added": added_total,
                    "backlog": depth,
                    "started": start,
                }),
            )
            .to_json()
        );
    } else {
        println!(
            "orchestrate: {repo} — {added_total} shard(s) from {total_findings} finding(s) added to backlog ({depth} pending)"
        );
        if start {
            println!("  daemon draining {repo} — bn orchestrate status --repo {repo}");
        } else if !has_live_run(&repo) {
            println!(
                "  no run draining {repo} — start one: bn orchestrate queue-from --start … (or bn orchestrate supervise --repo {repo})"
            );
        }
    }
    true
}

// ===========================================================================
// `bn orchestrate ask` / `answers` — the ad-hoc priority lane + answer routing (#378)
// ===========================================================================

/// `bn orchestrate ask "<question>"` — the delegate-for-answers fast lane (#378). Appends a
/// read-only `scout` backlog entry for `repo` at the top priority band ([`ASK_PRIORITY`]) so the
/// daemon leases it ahead of the ship backlog and, via the scheduler's reserved lane-0 slot,
/// dispatches it on the next active poll without preempting a running ship. The scout runs the
/// `investigate` skill, writes findings to its branch note, and opens **no PR**; the answer routes
/// back (harvested when it reaches Done — see [`harvest_answers`]). Ensures a daemon is draining the
/// repo (reusing a live/resident run) so the ask actually runs. With `wait`, blocks — polling the
/// state file the daemon writes (no new IPC) — until the item is terminal, then prints the answer
/// inline. Returns false only on a fatal setup error (unknown repo, empty question).
pub(crate) fn orchestrate_ask(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo_override: Option<&str>,
    question: &str,
    wait: bool,
    json: bool,
) -> bool {
    let repo = match resolve_target_repo(cfg, ctx, repo_override) {
        Ok(r) => r,
        Err(e) => {
            emit_ask_error(json, &format!("bn orchestrate ask: {e}"));
            return false;
        }
    };
    let question = question.trim();
    if question.is_empty() {
        emit_ask_error(json, "bn orchestrate ask: a question is required");
        return false;
    }

    let now = bn_core::util::now_epoch_secs();
    let id = orchestrate::derive_id(question);
    let spec = AddSpec {
        kind: backlog::BacklogKind::Scout,
        priority: ASK_PRIORITY,
        touches: Vec::new(),
        substrate: Substrate::Auto,
        source: "ask".to_string(),
        controller_id: std::env::var("BN_CONTROLLER_ID").ok(),
        // Every enqueue names a task (#1001) — minted here so the ergonomic one-command path
        // still works while leaving a durable record behind it.
        task: crate::cmd::task::task_for_enqueue(cfg, &repo, question, "ask"),
    };
    let path = backlog::backlog_path();
    if let Err(e) = backlog::add(&path, &repo, &[question.to_string()], &spec, now) {
        emit_ask_error(
            json,
            &format!("bn orchestrate ask: could not update backlog: {e}"),
        );
        return false;
    }
    log_event(
        "orchestrate_ask",
        &[("repo", &repo), ("id", &id), ("question", question)],
    );
    // Make sure a daemon is draining the repo so the ask actually gets dispatched (idempotent —
    // reuses a live/resident run untouched, else seeds a small self-feeding one).
    ensure_daemon_for(cfg, &repo, ASK_SEED_CONCURRENCY, false);

    if !wait {
        if json {
            let data = serde_json::json!({
                "repo": repo,
                "id": id,
                "question": question,
                "waited": false,
            });
            println!("{}", Envelope::success("orchestrate", data).to_json());
        } else {
            println!("orchestrate: {repo} — asked (scout {id}) on the priority lane");
            println!(
                "  answer routes back — bn orchestrate answers --repo {repo} --id {id} (or add --wait)"
            );
        }
        return true;
    }

    // --wait: poll the state file the daemon writes until the item is terminal, then print inline.
    match wait_for_answer(&repo, &id) {
        Some(item) => emit_answer(json, &repo, &item),
        None => {
            if json {
                let data = serde_json::json!({
                    "repo": repo,
                    "id": id,
                    "question": question,
                    "waited": true,
                    "answered": false,
                });
                println!("{}", Envelope::success("orchestrate", data).to_json());
            } else {
                println!(
                    "orchestrate: {repo} — still working on scout {id}; check back with bn orchestrate answers --repo {repo} --id {id}"
                );
            }
        }
    }
    true
}

fn emit_ask_error(json: bool, msg: &str) {
    if json {
        println!(
            "{}",
            Envelope::failure("orchestrate", "ask", msg.to_string()).to_json()
        );
    } else {
        eprintln!("{msg}");
    }
}

/// Poll this repo's run state for the ask item `id` until it reaches a terminal state (its answer
/// harvested, or it failed), returning the item. Pure CLI-side polling of the file the daemon
/// already writes — no new IPC. Bounded by [`ASK_WAIT_MAX_SECS`] so a stuck/undispatched scout
/// doesn't hang the terminal forever (the caller then points the user at `answers`).
fn wait_for_answer(repo: &str, id: &str) -> Option<orchestrate::QueueItem> {
    const ASK_WAIT_MAX_SECS: u64 = 30 * 60;
    const POLL_SECS: u64 = 3;
    let path = orchestrate::state_path_for(repo);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(ASK_WAIT_MAX_SECS);
    loop {
        if let Some(st) = OrchestrateState::load(&path) {
            if let Some(item) = st.queue.iter().find(|i| i.id == id) {
                // Terminal = answered (merged with findings) or failed — either way, stop waiting.
                if item.answer.is_some() || item.status == ItemStatus::Failed {
                    return Some(item.clone());
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_secs(POLL_SECS));
    }
}

/// `bn orchestrate answers [--id N]` — print the answers routed back from scout/ask items (#378),
/// read straight off `repo`'s run state (the file the daemon writes — no new IPC). Without `--id`,
/// lists every answered item for the run (newest last); with it, shows just that one. Returns false
/// only on a fatal setup error (no such run/repo).
pub(crate) fn orchestrate_answers(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo_override: Option<&str>,
    id: Option<&str>,
    json: bool,
) -> bool {
    let repo = match resolve_target_repo(cfg, ctx, repo_override) {
        Ok(r) => r,
        Err(e) => {
            emit_ask_error(json, &format!("bn orchestrate answers: {e}"));
            return false;
        }
    };
    let Some(st) = OrchestrateState::load(&orchestrate::state_path_for(&repo)) else {
        if json {
            let data = bn_core::payload::orchestrate_answers_data(&repo, &[]);
            println!("{}", Envelope::success("orchestrate", data).to_json());
        } else {
            println!("orchestrate: no run for {repo} — nothing answered");
        }
        return true;
    };
    let answered: Vec<&orchestrate::QueueItem> = st
        .queue
        .iter()
        .filter(|i| i.answer.is_some() && id.is_none_or(|want| i.id == want))
        .collect();

    if json {
        let data = bn_core::payload::orchestrate_answers_data(&repo, &answered);
        println!("{}", Envelope::success("orchestrate", data).to_json());
        return true;
    }

    if answered.is_empty() {
        match id {
            Some(want) => println!("orchestrate: {repo} — no answer for {want} yet"),
            None => println!("orchestrate: {repo} — no answers routed back yet"),
        }
        return true;
    }
    println!("orchestrate: {repo} — {} answer(s)", answered.len());
    for i in &answered {
        emit_answer(false, &repo, i);
    }
    true
}

/// Print one answered scout/ask item — its question and the findings path (the write-up link). In
/// text mode also echoes a short head of the findings so the answer shows inline; JSON emits the
/// structured record.
fn emit_answer(json: bool, repo: &str, item: &orchestrate::QueueItem) {
    let Some(a) = item.answer.as_ref() else {
        // Terminal without an answer → the scout failed. Surface that instead of a silent nothing.
        if json {
            let data = serde_json::json!({
                "repo": repo,
                "id": item.id,
                "answered": false,
                "status": item.status.label(),
            });
            println!("{}", Envelope::success("orchestrate", data).to_json());
        } else {
            println!("  ✗ {} — {} (no findings)", item.id, item.status.label());
        }
        return;
    };
    if json {
        let data = serde_json::json!({
            "repo": repo,
            "id": item.id,
            "question": a.question,
            "path": a.path,
            "harvested_at": a.harvested_at,
            "answered": true,
        });
        println!("{}", Envelope::success("orchestrate", data).to_json());
        return;
    }
    println!("  ▸ {} — {}", item.id, a.question);
    println!("    findings: {}", a.path);
    if let Ok(body) = std::fs::read_to_string(&a.path) {
        for line in body.lines().filter(|l| !l.trim().is_empty()).take(8) {
            println!("    │ {line}");
        }
    }
}

///
/// The seeded/reused run is flagged **self-feeding** (#368): a backlog-drainer is *resident*, so on
/// drain it stays alive pulling the next appended batch instead of exiting — that's what lets a
/// later plain `bn orchestrate add` (no `--start`) be picked up with no scheduled prompt.
fn ensure_daemon_for(cfg: &Config, repo: &str, concurrency: usize, solo: bool) {
    ensure_migrated(true);
    let now = bn_core::util::now_epoch_secs();
    let path = orchestrate::state_path_for(repo);
    let mut st = match OrchestrateState::load(&path) {
        // Reuse a live run or a resident one (#426 supervisor / #368 self-feeder — drained yet still
        // standing), so `add --start` on such a repo keeps its flags instead of resetting it.
        Some(existing)
            if (!existing.is_drained() || existing.is_resident())
                && !existing.stopped
                && existing.repo == repo =>
        {
            existing
        }
        _ => OrchestrateState::new(repo, &[], concurrency, solo, now),
    };
    st.stopped = false;
    // #368: a backlog-draining run is resident — it self-feeds the next batch on drain rather than
    // exiting. Idempotent, and harmless on a supervisor (which is already resident).
    st.self_feed = true;
    ensure_default_harness(cfg, &mut st);
    st.updated_at = now;
    if st.save(&path).is_err() {
        return;
    }
    install_repo_daemon(repo);
    log_event("orchestrate_backlog_start", &[("repo", repo)]);
}
/// With `install_daemon`, a still-live migrated run gets a successor daemon **before** the legacy
/// singleton is booted out, so the in-flight wave is never left driverless; `orchestrate run`
/// passes `false` because the calling process drives the run itself. Returns the migrated repo.
fn ensure_migrated(install_daemon: bool) -> Option<String> {
    // Peek without moving, so we know whether the run still needs a driver.
    let peek = OrchestrateState::load(&orchestrate::legacy_state_path())?;
    let live = !peek.is_drained() && !peek.stopped;
    let (repo, _dest) = orchestrate::migrate_legacy_state()?;
    if live && install_daemon {
        install_repo_daemon(&repo);
    }
    // Retire the singleton daemon last: its next load of the (now-moved) legacy path exits the old
    // loop cleanly, and the bootout kills a mid-tick straggler so it can't resurrect the old file.
    crate::launchd::orchestrate_uninstall_legacy();
    crate::systemd::orchestrate_uninstall_legacy();
    log_event("orchestrate_migrated", &[("repo", &repo)]);
    Some(repo)
}

/// Which supervisor an orchestrate daemon install landed on, so the caller prints the right note.
#[derive(Clone, Copy)]
enum DaemonKind {
    Launchd,
    Systemd,
    Detached,
    Failed,
}

impl DaemonKind {
    /// The human note for how the run was daemonized — the first line every start path prints.
    fn note(self) -> &'static str {
        match self {
            DaemonKind::Launchd => {
                "launchd daemon loaded — drives unattended (survives this shell + reboot)."
            }
            DaemonKind::Systemd => {
                "systemd --user service enabled — drives unattended (survives this shell, a crash, and a reboot)."
            }
            DaemonKind::Detached => {
                "no service manager — started a detached loop (survives this shell, not a reboot)."
            }
            DaemonKind::Failed => {
                "no service manager and the detached fallback failed — run `bn orchestrate run` manually."
            }
        }
    }
}

/// Daemonize a repo's run via the best available supervisor: launchd (macOS), else a systemd
/// `--user` service (Linux, #608), else a detached child. Each manager's install returns `Err` when
/// its tool is absent, so the chain lands on the first that works and only reaches the detached
/// fallback when neither service manager exists.
fn install_repo_daemon(repo: &str) -> DaemonKind {
    match crate::launchd::orchestrate_install(repo) {
        Ok(()) => {
            record_degradation(repo, "daemon-detached-fallback", false, &[], None);
            DaemonKind::Launchd
        }
        Err(launchd_error) => match crate::systemd::orchestrate_install(repo) {
            Ok(()) => {
                record_degradation(repo, "daemon-detached-fallback", false, &[], None);
                DaemonKind::Systemd
            }
            Err(systemd_error) => {
                let error = format!("launchd: {launchd_error}; systemd: {systemd_error}");
                match spawn_detached_run(repo) {
                    Ok(()) => {
                        record_degradation(
                            repo,
                            "daemon-detached-fallback",
                            true,
                            &[],
                            Some(&error),
                        );
                        DaemonKind::Detached
                    }
                    Err(spawn_error) => {
                        let error = format!("{error}; detached spawn: {spawn_error}");
                        record_degradation(
                            repo,
                            "daemon-detached-spawn-failed",
                            true,
                            &[],
                            Some(&error),
                        );
                        DaemonKind::Failed
                    }
                }
            }
        },
    }
}

/// Daemonize the #455 cross-repo supervisor via the same launchd → systemd → detached chain as
/// [`install_repo_daemon`].
fn install_all_daemon() -> DaemonKind {
    const REPO: &str = "*";
    match crate::launchd::orchestrate_install_all() {
        Ok(()) => {
            record_degradation(REPO, "daemon-detached-fallback", false, &[], None);
            DaemonKind::Launchd
        }
        Err(launchd_error) => match crate::systemd::orchestrate_install_all() {
            Ok(()) => {
                record_degradation(REPO, "daemon-detached-fallback", false, &[], None);
                DaemonKind::Systemd
            }
            Err(systemd_error) => {
                let error = format!("launchd: {launchd_error}; systemd: {systemd_error}");
                match spawn_detached_run_all() {
                    Ok(()) => {
                        record_degradation(
                            REPO,
                            "daemon-detached-fallback",
                            true,
                            &[],
                            Some(&error),
                        );
                        DaemonKind::Detached
                    }
                    Err(spawn_error) => {
                        let error = format!("{error}; detached spawn: {spawn_error}");
                        record_degradation(
                            REPO,
                            "daemon-detached-spawn-failed",
                            true,
                            &[],
                            Some(&error),
                        );
                        DaemonKind::Failed
                    }
                }
            }
        },
    }
}

/// Spawn `bn orchestrate run --repo <repo>` as a detached background process (its own session), so
/// the loop keeps running after the launching shell exits. The launchd path is preferred; this is
/// the fallback.
#[allow(
    unsafe_code,
    reason = "CommandExt::pre_exec is required to detach the fallback orchestrator before exec"
)]
fn spawn_detached_run(repo: &str) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(bn_bin());
    cmd.args(["orchestrate", "run", "--repo", repo]);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New session so it detaches from the controlling terminal / process group.
        unsafe {
            cmd.pre_exec(|| {
                libc_setsid();
                Ok(())
            });
        }
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn().map(|_| ())
}

/// Spawn `bn orchestrate run --all-repos` as a detached background process (its own session) — the
/// launchd-absent fallback for the #455 cross-repo supervisor, mirroring [`spawn_detached_run`].
#[allow(
    unsafe_code,
    reason = "CommandExt::pre_exec is required to detach the fallback supervisor before exec"
)]
fn spawn_detached_run_all() -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(bn_bin());
    cmd.args(["orchestrate", "run", "--all-repos"]);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc_setsid();
                Ok(())
            });
        }
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn().map(|_| ())
}

#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "setsid has no standard-library wrapper and is isolated to this FFI boundary"
)]
fn libc_setsid() {
    // Detach into a new session without pulling in the `libc` crate: setsid(2) via raw syscall is
    // unnecessary — std re-exports nothing, so use the POSIX call through `nix`-free FFI.
    extern "C" {
        fn setsid() -> i32;
    }
    unsafe {
        setsid();
    }
}

// ===========================================================================
// `bn orchestrate run` — the daemon loop
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClaimEvidence {
    Absent,
    Local { branch: String },
    Remote,
    Worker { node: String, state: String },
    Unknown,
}

#[derive(Default)]
struct ClaimReconcileSummary {
    promoted: usize,
    released: usize,
    unknown: usize,
}

fn apply_claim_evidence(
    run_path: &Path,
    repo: &str,
    id: &str,
    expected_attempt: &str,
    expected_updated_at: i64,
    evidence: ClaimEvidence,
    now: i64,
) -> std::io::Result<bool> {
    orchestrate::mutate(run_path, |st| {
        let Some(idx) = st
            .queue
            .iter()
            .position(|item| item.repo == repo && item.id == id)
        else {
            return false;
        };
        let current = &st.queue[idx];
        if current.status != ItemStatus::Queued
            || current.dispatch_phase != orchestrate::DispatchPhase::Claimed
            || current.attempt_id.as_deref() != Some(expected_attempt)
            || current.updated_at != expected_updated_at
        {
            return false;
        }
        match evidence {
            ClaimEvidence::Absent => {
                let remote = st.queue[idx].claimed_remote;
                let changed = st.reconcile_claim(idx, false, None, now);
                if changed && remote {
                    let attempts = st.bump_attempts_at(idx, now);
                    if !Backoff::default().should_retry(attempts) {
                        st.mark_failed_at(idx, now);
                    }
                }
                changed
            }
            ClaimEvidence::Local { branch } => {
                if st.queue[idx].claimed_remote
                    || st.queue[idx].node.is_some()
                    || st.queue[idx].branch.as_deref() != Some(branch.as_str())
                {
                    return false;
                }
                st.reconcile_claim(idx, true, Some(branch), now)
            }
            ClaimEvidence::Remote => {
                if !st.queue[idx].claimed_remote {
                    return false;
                }
                st.mark_dispatched_remote(idx, now);
                true
            }
            ClaimEvidence::Worker { node, state } => {
                if st.queue[idx].claimed_remote
                    || st.queue[idx].node.as_deref() != Some(node.as_str())
                {
                    return false;
                }
                let request_id = st.queue[idx].request_id.clone().unwrap_or_default();
                let attempt_id = st.queue[idx].attempt_id.clone().unwrap_or_default();
                st.mark_dispatched_worker(idx, &node, &request_id, &attempt_id, None, now);
                st.set_worker_state_at(idx, &state, now);
                true
            }
            ClaimEvidence::Unknown => false,
        }
    })
    .map(|result| result.unwrap_or(false))
}

fn github_repo_slug(repo: &str) -> Option<String> {
    let runner = bn_core::runner::RealRunner;
    runner
        .gh(&[
            "repo",
            "view",
            repo,
            "--json",
            "nameWithOwner",
            "--jq",
            ".nameWithOwner",
        ])
        .ok_stdout()
}

fn absent_claim_evidence(item: &orchestrate::QueueItem, now: i64) -> ClaimEvidence {
    if now.saturating_sub(item.updated_at) < CLAIM_ABSENCE_GRACE_SECS {
        ClaimEvidence::Unknown
    } else {
        ClaimEvidence::Absent
    }
}

fn observe_github_claim(item: &orchestrate::QueueItem, now: i64) -> ClaimEvidence {
    let Some(slug) = github_repo_slug(&item.repo) else {
        return ClaimEvidence::Unknown;
    };
    let Some(attempt_id) = item.attempt_id.as_deref() else {
        return ClaimEvidence::Unknown;
    };
    let runner = bn_core::runner::RealRunner;
    let out = if item.id.chars().all(|c| c.is_ascii_digit()) {
        let endpoint = format!("repos/{slug}/issues/{}/comments", item.id);
        let jq = format!(".[] | select(.body | contains(\"{attempt_id}\")) | .id");
        runner.gh(&["api", &endpoint, "--paginate", "--jq", &jq])
    } else {
        let jq = format!(".[] | select(.displayTitle | contains(\"{attempt_id}\")) | .databaseId");
        runner.gh(&[
            "run",
            "list",
            "-R",
            &slug,
            "--workflow",
            crate::cmd::dispatch::REMOTE_WORKFLOW,
            "--limit",
            "100",
            "--json",
            "databaseId,displayTitle",
            "--jq",
            &jq,
        ])
    };
    if !out.success {
        ClaimEvidence::Unknown
    } else if out.stdout.trim().is_empty() {
        absent_claim_evidence(item, now)
    } else {
        ClaimEvidence::Remote
    }
}

fn observe_worker_claim(
    cfg: &Config,
    st: &OrchestrateState,
    item: &orchestrate::QueueItem,
    now: i64,
) -> ClaimEvidence {
    let (Some(node_id), Some(attempt_id)) = (item.node.as_deref(), item.attempt_id.as_deref())
    else {
        return ClaimEvidence::Unknown;
    };
    let alias = st
        .nodes
        .iter()
        .find(|node| node.node_id == node_id)
        .and_then(|node| match &node.target {
            bn_core::backlog::ExecutionTarget::SshHost(alias) => Some(alias.as_str()),
            _ => None,
        })
        .unwrap_or(node_id);
    let runner = bn_core::worker::transport::RealSshRunner;
    let Ok(transport) = bn_core::worker::transport::Transport::connect(
        cfg,
        alias,
        bn_core::worker::transport::SshOptions::default(),
        &runner,
    ) else {
        return ClaimEvidence::Unknown;
    };
    let Some(status) = poll_worker_status(&transport) else {
        return ClaimEvidence::Unknown;
    };
    match worker_attempt_state(&status, attempt_id) {
        Some(state) => ClaimEvidence::Worker {
            node: node_id.to_string(),
            state,
        },
        None => absent_claim_evidence(item, now),
    }
}

fn observe_claim(cfg: &Config, st: &OrchestrateState, idx: usize, now: i64) -> ClaimEvidence {
    let item = &st.queue[idx];
    if item.node.is_some() {
        return observe_worker_claim(cfg, st, item, now);
    }
    if item.claimed_remote || item.substrate == Substrate::Remote {
        return observe_github_claim(item, now);
    }
    let Some(branch) = item.branch.as_deref() else {
        return absent_claim_evidence(item, now);
    };
    let worktree = worktree_path(cfg, &item.repo, branch);
    let target = format!("{}:{}", item.repo, wt::sanitize_branch(branch));
    let window_live = list_windows().is_some_and(|windows| windows.contains(&target));
    if worktree.is_dir() || window_live {
        ClaimEvidence::Local {
            branch: branch.to_string(),
        }
    } else {
        absent_claim_evidence(item, now)
    }
}

fn reconcile_dispatch_claims(cfg: &Config, run_path: &Path, now: i64) -> ClaimReconcileSummary {
    let Some(snapshot) = OrchestrateState::load(run_path) else {
        return ClaimReconcileSummary::default();
    };
    let claims: Vec<(String, String, String, i64, ClaimEvidence)> = snapshot
        .queue
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.status == ItemStatus::Queued
                && item.dispatch_phase == orchestrate::DispatchPhase::Claimed
        })
        .map(|(idx, item)| {
            (
                item.repo.clone(),
                item.id.clone(),
                item.attempt_id.clone().unwrap_or_default(),
                item.updated_at,
                observe_claim(cfg, &snapshot, idx, now),
            )
        })
        .collect();
    let mut summary = ClaimReconcileSummary::default();
    for (repo, id, attempt_id, updated_at, evidence) in claims {
        let bucket = match evidence {
            ClaimEvidence::Absent => &mut summary.released,
            ClaimEvidence::Unknown => {
                summary.unknown += 1;
                continue;
            }
            _ => &mut summary.promoted,
        };
        match apply_claim_evidence(run_path, &repo, &id, &attempt_id, updated_at, evidence, now) {
            Ok(true) => *bucket += 1,
            Ok(false) => {}
            Err(e) => {
                summary.unknown += 1;
                eprintln!("bn orchestrate: could not reconcile claim {repo}/{id}: {e}");
            }
        }
    }
    summary
}

pub(crate) fn orchestrate_reconcile(cfg: &Config, repo: Option<&str>, json: bool) -> bool {
    ensure_migrated(true);
    let runs = resolve_runs(repo);
    if runs.is_empty() {
        if !json {
            eprintln!("bn orchestrate reconcile: no run found");
        }
        return false;
    }
    let now = bn_core::util::now_epoch_secs();
    let mut total = ClaimReconcileSummary::default();
    for (path, _) in runs {
        let summary = reconcile_dispatch_claims(cfg, &path, now);
        total.promoted += summary.promoted;
        total.released += summary.released;
        total.unknown += summary.unknown;
    }
    if json {
        println!(
            "{}",
            Envelope::success(
                "orchestrate-reconcile",
                serde_json::json!({
                    "promoted": total.promoted,
                    "released": total.released,
                    "unknown": total.unknown,
                })
            )
            .to_json()
        );
    } else {
        println!(
            "orchestrate reconcile: {} promoted · {} released · {} ambiguous",
            total.promoted, total.released, total.unknown
        );
    }
    total.unknown == 0
}

pub(crate) fn orchestrate_reset(
    cfg: &Config,
    cwd_repo: Option<&str>,
    repo: Option<&str>,
    lane: &str,
    json: bool,
) -> bool {
    ensure_migrated(true);
    let fail_reset = |message: String| {
        if json {
            println!(
                "{}",
                Envelope::failure("orchestrate-reset", "reset-failed", &message).to_json()
            );
        } else {
            eprintln!("bn orchestrate reset: {message}");
        }
        false
    };
    let repo = match repo {
        Some(repo) => repo.to_string(),
        None => {
            let live: Vec<String> = load_runs()
                .into_iter()
                .filter(|(_, st)| !st.stopped && (!st.is_drained() || st.is_resident()))
                .map(|(_, st)| st.repo)
                .collect();
            match live.len() {
                1 => live[0].clone(),
                0 => match cwd_repo {
                    Some(repo) => repo.to_string(),
                    None => {
                        return fail_reset(
                            "no live run — pass --repo <name> to target one".to_string(),
                        );
                    }
                },
                _ => match cwd_repo.filter(|repo| live.iter().any(|live| live == repo)) {
                    Some(repo) => repo.to_string(),
                    None => {
                        return fail_reset(format!(
                            "{} live runs ({}) — pass --repo <name>",
                            live.len(),
                            live.join(", ")
                        ));
                    }
                },
            }
        }
    };
    let path = orchestrate::state_path_for(&repo);
    let Some(snapshot) = OrchestrateState::load(&path) else {
        return fail_reset(format!(
            "no run for {repo} — inspect active runs with `bn orchestrate status`"
        ));
    };
    let now = bn_core::util::now_epoch_secs();
    let mut planned = snapshot.clone();
    let reset = match planned.reset_lane(lane, now) {
        Ok(reset) => reset,
        Err(error) => return fail_reset(error.to_string()),
    };
    let worktree = worktree_path(cfg, &reset.repo, &reset.branch);
    if !worktree.join(".git").exists() {
        return fail_reset(format!(
            "lane '{}' worktree is missing at {} — run `bn orchestrate reconcile --repo {repo}`",
            reset.id,
            worktree.display()
        ));
    }

    let has_wip = worktree_has_wip(&worktree);
    if has_wip
        && !run_bn(
            &[
                "agent",
                "recover",
                "--repo",
                &reset.repo,
                "--branch",
                &reset.branch,
                "--yes",
            ],
            None,
        )
        .0
    {
        return fail_reset(format!(
            "could not recover WIP for lane '{}' — no process was replaced",
            reset.id
        ));
    }

    let target = tmux_target(&reset.repo, &reset.branch);
    if list_windows()
        .unwrap_or_default()
        .iter()
        .any(|window| window.eq_ignore_ascii_case(&target))
        && !crate::proc::tmux_run(&["kill-window", "-t", &format!("={target}")])
    {
        return fail_reset(format!(
            "could not replace tmux window '{target}' — its worktree and recovered work remain intact"
        ));
    }
    let harness = reset
        .harness
        .as_deref()
        .unwrap_or_else(|| snapshot.dispatch_harness());
    if sh_dispatch(
        &reset.repo,
        &reset.item,
        Some(&reset.branch),
        true,
        harness,
        reset.work_class == WorkClass::Research,
        snapshot
            .queue
            .iter()
            .find(|item| item.id == reset.id)
            .and_then(|item| item.controller_id.as_deref()),
    )
    .is_none()
    {
        return fail_reset(format!(
            "fresh dispatch failed for lane '{}' — its worktree and recovered work remain intact",
            reset.id
        ));
    }

    let persisted = match orchestrate::mutate(&path, |state| state.reset_lane(lane, now)) {
        Ok(Some(Ok(reset))) => reset,
        Ok(Some(Err(error))) => {
            return fail_reset(format!(
                "fresh agent started but run state changed before reset was recorded: {error}"
            ));
        }
        Ok(None) => {
            return fail_reset(
                "fresh agent started but the orchestrator run disappeared".to_string(),
            );
        }
        Err(error) => {
            return fail_reset(format!(
                "fresh agent started but the locked run update failed: {error}"
            ));
        }
    };
    log_event(
        "orchestrate_lane_reset",
        &[
            ("repo", &persisted.repo),
            ("id", &persisted.id),
            ("branch", &persisted.branch),
            ("wip", if has_wip { "true" } else { "false" }),
        ],
    );
    if json {
        println!(
            "{}",
            Envelope::success(
                "orchestrate-reset",
                serde_json::json!({
                    "repo": persisted.repo,
                    "id": persisted.id,
                    "branch": persisted.branch,
                    "previous_status": persisted.previous_status.label(),
                    "wip_recovered": has_wip,
                    "reset": true,
                })
            )
            .to_json()
        );
    } else {
        println!(
            "orchestrate reset: {}/{} re-seeded on {}{}",
            persisted.repo,
            persisted.id,
            persisted.branch,
            if has_wip { " (WIP recovered)" } else { "" }
        );
    }
    true
}

// --- single-controller owner lease (prevents two daemons double-driving one repo) -------------

use bn_core::orchestrate::{
    clear_owner, may_claim_owner, read_owner, write_owner, ControllerOwner, OWNER_LEASE_TTL_SECS,
};

/// This process's controller identity, heartbeated `now`.
fn me_owner(now: i64) -> ControllerOwner {
    ControllerOwner {
        pid: std::process::id(),
        host: bn_core::worker::node_id(),
        heartbeat: now,
    }
}

/// Same-host pid liveness for an owner (`Some(true|false)`); `None` when the owner is on another
/// host and thus can only age out by heartbeat TTL. Linux `/proc/<pid>` is the fleet's probe.
fn owner_liveness(owner: &ControllerOwner, me: &ControllerOwner) -> Option<bool> {
    if owner.host != me.host {
        return None;
    }
    Some(std::path::Path::new(&format!("/proc/{}", owner.pid)).exists())
}

/// Whether `me` may drive `repo` right now — fail-open arbitration over the recorded owner lease.
fn may_drive_repo(repo: &str, me: &ControllerOwner, now: i64) -> bool {
    let existing = read_owner(repo);
    let alive = existing.as_ref().and_then(|o| owner_liveness(o, me));
    may_claim_owner(existing.as_ref(), me, alive, now, OWNER_LEASE_TTL_SECS)
}

/// Clears a held owner lease when the run loop returns from any of its exit points.
struct OwnerGuard {
    repo: String,
    me: ControllerOwner,
}

impl Drop for OwnerGuard {
    fn drop(&mut self) {
        clear_owner(&self.repo, &self.me);
    }
}

/// The daemon loop: each pass lands ready PRs, recovers wedged agents, and dispatches the next
/// queued items until the run drains. `once` runs a single pass (cron fallback / manual poke).
/// `repo` pins the loop to one run's state file (#395); bare picks the only live run. With
/// `all_repos` (#455) it drives the cross-repo sweep ([`orchestrate_run_all`]) instead — one loop
/// over every configured repo's session.
pub(crate) fn orchestrate_run(
    cfg: &Config,
    repo: Option<&str>,
    all_repos: bool,
    once: bool,
    json: bool,
) {
    if all_repos {
        orchestrate_run_all(cfg, once, json);
        return;
    }
    // No daemon install here: this process IS the driver for whatever run it resolves — a legacy
    // wave migrated on this entry point is picked up and driven directly below.
    ensure_migrated(false);
    let path = match repo {
        Some(r) => orchestrate::state_path_for(r),
        None => {
            // Bare `run` (the pre-#395 daemon invocation, or a manual poke): drive the only live
            // run — including a resident one (#426 supervisor / #368 self-feeder), which is
            // `is_drained()` yet still standing. With several live, refuse to guess — each needs its own loop.
            let live: Vec<(PathBuf, OrchestrateState)> = load_runs()
                .into_iter()
                .filter(|(_, st)| !st.stopped && (!st.is_drained() || st.is_resident()))
                .collect();
            match live.len() {
                1 => live.into_iter().next().map(|(p, _)| p).unwrap(),
                0 => {
                    if !json {
                        eprintln!(
                            "bn orchestrate run: no run to drive (start one with `bn orchestrate --queue …`)"
                        );
                    }
                    return;
                }
                _ => {
                    let repos: Vec<String> = live.into_iter().map(|(_, st)| st.repo).collect();
                    if !json {
                        eprintln!(
                            "bn orchestrate run: {} live runs ({}) — pass --repo <name>",
                            repos.len(),
                            repos.join(", ")
                        );
                    }
                    std::process::exit(1);
                }
            }
        }
    };
    // #380: a fresh boot (launchd RunAtLoad after a reboot) killed every in-flight tmux window, so
    // the fleet will re-dispatch each surviving item — but that's a machine event, not the item's
    // fault. Reset the relaunch backoff for in-flight items whose worktree survived *before* the
    // loop starts observing panes, so a reboot re-dispatch begins a fresh curve instead of inheriting
    // a burned one and marking a healthy item `failed` after a few reboots.
    reset_backoff_on_fresh_boot(cfg, &path);

    // Single-controller guard (#395/#453): at most one loop drives a given repo. A second daemon
    // (e.g. `run --repo bn` alongside `run --all-repos`) would adopt the same tmux lanes and
    // double-count in-flight, pinning the cap so the backlog stops pulling. Defer to a live owner.
    let owner_repo = OrchestrateState::load(&path)
        .map(|s| s.repo)
        .or_else(|| repo.map(str::to_string))
        .unwrap_or_default();
    let me = me_owner(bn_core::util::now_epoch_secs());
    let _owner_guard = if owner_repo.is_empty() {
        None
    } else if !may_drive_repo(&owner_repo, &me, me.heartbeat) {
        // A `once` pass (the all-repos sweep's per-repo tick) just yields silently; a standalone
        // daemon says why it's stepping aside so the operator isn't left wondering.
        if !once && !json {
            let held = read_owner(&owner_repo);
            eprintln!(
                "bn orchestrate run: {owner_repo} is already driven by another controller{} — exiting (single-controller guard).",
                held.map_or(String::new(), |o| format!(" (pid {} on {})", o.pid, o.host))
            );
        }
        return;
    } else if once {
        // Claimed for a single pass; the persistent owner (the all-repos loop) refreshes the lease.
        None
    } else {
        write_owner(&owner_repo, &me);
        Some(OwnerGuard {
            repo: owner_repo.clone(),
            me,
        })
    };

    let mut wake_watcher = if once {
        None
    } else {
        match crate::wake::WakeWatcher::new(&[]) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                eprintln!(
                    "bn orchestrate: filesystem notifications unavailable ({error}); polling"
                );
                None
            }
        }
    };
    loop {
        // Refresh the owner lease so a co-daemon sees a live heartbeat (only the persistent daemon
        // holds the guard; a `once` pass leaves the lease to its owning all-repos loop).
        if _owner_guard.is_some() {
            write_owner(&owner_repo, &me_owner(bn_core::util::now_epoch_secs()));
        }
        // Claims kept ambiguous during the launcher-visibility grace window are retried every tick;
        // once evidence appears they promote, otherwise they safely return to pending after grace.
        reconcile_dispatch_claims(cfg, &path, bn_core::util::now_epoch_secs());
        // Load an unlocked *snapshot* to drive this pass. The tick's real work — dispatch, merge,
        // pane observation — spawns subprocesses and can run for a while; holding a lock across it
        // would stall every web queue edit. Instead we work on a copy and fold the result back
        // under a short lock below (#392), so a concurrent web edit is merged, never clobbered.
        let Some(snap) = OrchestrateState::load(&path) else {
            if !json {
                eprintln!(
                    "bn orchestrate run: no run to drive (start one with `bn orchestrate --queue …`)"
                );
            }
            return;
        };
        if snap.stopped {
            if !json {
                println!("orchestrate: run stopped.");
            }
            return;
        }
        let now = bn_core::util::now_epoch_secs();
        // #426: in supervise mode, adopt this repo's live standalone dispatch lanes before anything
        // else, so land/recover/reconcile below cover a lane dispatched outside this run. A plain
        // `--queue` run skips this and supervises only its own dispatched items.
        if snap.supervise {
            adopt_lanes(&path, cfg, &snap.repo, now);
        }
        // Adoption mutates the resident run directly; reload so this pass supervises newly adopted
        // standalone lanes instead of waiting for the next tick.
        let snap = OrchestrateState::load(&path).unwrap_or(snap);
        let mut worked = snap.clone();
        tick(cfg, &path, &mut worked, now);

        // Fold the tick's per-item progress onto the CURRENT on-disk state (which may carry web
        // edits made while the tick ran), under the shared required lock. `merge_worked` keeps the
        // web's queue adds/removes/reorders + concurrency/stopped and the daemon's lifecycle moves.
        let mutation = orchestrate::mutate(&path, |disk| {
            disk.merge_worked(&worked, now);
            // #380: cap the in-state recovery trail to a ring buffer so the per-tick atomic rewrite
            // stays bounded no matter how long the run has been recovering agents (the full trail
            // lives in events.jsonl). Applied on the merged on-disk state so every save is bounded.
            disk.trim_recovery(orchestrate::RECOVERY_CAP);
            (
                disk.is_drained(),
                disk.stopped,
                disk.counts(),
                disk.in_flight(),
                disk.is_resident(),
            )
        });
        let (drained, stopped, c, in_flight, resident) = match mutation {
            Ok(Some(result)) => result,
            Ok(None) => {
                // The run file vanished mid-tick (removed out from under us) — nothing left to drive.
                return;
            }
            Err(e) => {
                eprintln!("bn orchestrate: could not lock or save run state: {e}");
                return;
            }
        };

        if stopped {
            if !json {
                println!("orchestrate: run stopped.");
            }
            return;
        }

        // #375: reconcile terminal run items (merged/failed) back to the backlog so a leased entry
        // becomes done/failed there, then compact if the log has bloated. The backlog is the record
        // #368/#348 pull from; keeping it in step lets the run stay a small execution ledger.
        reconcile_backlog(&path, &worked.repo, now);
        // #380: evict terminal items older than the retain window from the live queue into the
        // append-only run-history.jsonl. This bounds the per-tick state rewrite for a weeks-long
        // run (O(in-flight + recent)) while the outcomes stay durably queryable across restarts.
        // Runs after reconcile_backlog so a merged/failed item is reflected in the backlog before it
        // leaves the queue.
        evict_history(&path, &worked.repo, now);
        let backlog_pending = backlog::load(&backlog::backlog_path()).pending_depth(&worked.repo);

        // The run is complete only when its ledger has drained AND the backlog holds nothing pending
        // for this repo AND it isn't resident — otherwise a mid-run `add` (or an entry not yet leased
        // because the working set was full) keeps the daemon alive to drain it, instead of exiting
        // and stranding it. A *resident* run never exits on drain: a supervisor (#426) keeps watching
        // for future standalone lanes + green PRs, and a self-feeder (#368) stays alive to pull the
        // next appended batch from the durable backlog — so a multi-wave backlog completes with zero
        // scheduled prompts. Either idle-polls on instead (only `stop` ends it). This mirrors the
        // pure `OrchestrateState::run_complete`, but takes `drained`/`resident` from the merged
        // on-disk state so a web edit racing the tick (#392) is respected.
        if drained && backlog_pending == 0 && !resident {
            log_event("orchestrate_done", &[("repo", &worked.repo)]);
            // Forget the daemon (keep the exit clean — no self-stop): otherwise a reboot / next
            // login would resurrect a loop for an already-drained run. Both managers no-op when
            // their tool is absent, so the running daemon (launchd on macOS, systemd on Linux) is
            // disabled in place without killing itself.
            crate::launchd::orchestrate_forget(&worked.repo);
            crate::systemd::orchestrate_forget(&worked.repo);
            if json {
                let data =
                    serde_json::json!({ "done": true, "merged": c.merged, "failed": c.failed });
                println!("{}", Envelope::success("orchestrate", data).to_json());
            } else {
                println!(
                    "orchestrate: {} — queue drained ✓ ({} merged, {} failed).",
                    worked.repo, c.merged, c.failed
                );
            }
            // A drained queue is a completion report — `bn orchestrate status` has it whenever
            // you want it, and nothing here is waiting on you.
            crate::toast::toast(
                crate::toast::Class::Completion,
                &format!("orchestrate:{}:drained", worked.repo),
                "orchestrate",
                &format!("bn orchestrate: {} done", worked.repo),
                &format!("{} merged, {} failed", c.merged, c.failed),
            );
            return;
        }
        if once {
            return;
        }
        // #627: release-on-idle companion. On spare capacity (no coding lanes in flight) and an
        // opted-in repo, opportunistically cut a release for a green `main`. Cheap pre-check first
        // (nothing new to ship → skip), then delegate to `bn release` in a repo worktree so all the
        // gating/idempotence/verify logic is the single tested path. Best-effort: never blocks or
        // fails the supervise loop.
        if in_flight == 0 {
            release_companion_tick(cfg, &worked.repo);
        }
        let nap = if in_flight > 0 {
            POLL_ACTIVE_SECS
        } else {
            POLL_IDLE_SECS
        };
        let queue = bn_core::wake::WakeQueue::default();
        let target = bn_core::wake::WakeTarget::Orchestrate {
            repo: worked.repo.clone(),
        };
        if let Ok(pending) = queue.pending(&target) {
            let ids = pending.iter().map(|wake| wake.event_id.clone()).collect();
            let _ = queue.acknowledge(&target, &ids);
        }
        if let Some(watcher) = wake_watcher.as_ref() {
            if let Err(error) = watcher.wait_timeout(std::time::Duration::from_secs(nap)) {
                eprintln!("bn orchestrate: filesystem notifications failed ({error}); polling");
                wake_watcher = None;
                std::thread::sleep(std::time::Duration::from_secs(nap));
            }
        } else {
            std::thread::sleep(std::time::Duration::from_secs(nap));
        }
    }
}

/// #455: the **cross-repo supervise sweep** — one loop that, each tick, supervises every configured
/// repo whose tmux session (or existing supervisor state) shows activity, closing the
/// session-per-repo blind spot (a `bn`-session supervisor never saw `tracker`-session agents).
///
/// Each pass: read the live tmux windows once, seed a resident supervisor for any configured repo
/// that just grew a live agent lane ([`bn_core::crossrepo::repos_with_live_agents`]), then run one
/// existing per-repo supervise pass ([`orchestrate_run`] with `once`) for every supervised repo —
/// so land / recover / reconcile fire across *all* sessions in a single sweep. `once` runs exactly
/// one sweep (cron fallback / manual poke); otherwise it idle-polls until every repo is stopped.
fn orchestrate_run_all(cfg: &Config, once: bool, json: bool) {
    ensure_migrated(false);
    // Single-controller guard: this sweep claims an owner lease per repo it drives (below), so a
    // dedicated `run --repo R` daemon defers to it (and vice-versa). Release every lease we still
    // own on exit — re-derived from live runs at drop so dynamically-seeded repos are freed too —
    // so a restart re-claims cleanly instead of aging out.
    struct AllOwnerGuard {
        me: ControllerOwner,
    }
    impl Drop for AllOwnerGuard {
        fn drop(&mut self) {
            for (_, st) in load_runs() {
                clear_owner(&st.repo, &self.me);
            }
        }
    }
    let _all_owner_guard = AllOwnerGuard {
        me: me_owner(bn_core::util::now_epoch_secs()),
    };
    let mut wake_watcher = if once {
        None
    } else {
        match crate::wake::WakeWatcher::new(&[]) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                eprintln!(
                    "bn orchestrate: filesystem notifications unavailable ({error}); polling"
                );
                None
            }
        }
    };
    loop {
        let now = bn_core::util::now_epoch_secs();
        // Adopt any *newly cloned* repo that has grown a live agent session into a resident
        // supervisor, so a repo dispatched into for the first time mid-sweep is picked up. A repo
        // that was explicitly stopped (`stop --repo`) keeps its state and is left alone — only a
        // repo with no run state at all is seeded, so the sweep never fights a deliberate stop.
        let windows = list_windows().unwrap_or_default();
        let known = configured_repos(cfg);
        for repo in bn_core::crossrepo::repos_with_live_agents(&windows, &known) {
            if OrchestrateState::load(&orchestrate::state_path_for(&repo)).is_none() {
                let policy = resolve_keep_warm_policy(cfg, false, None, &[], &[], None);
                seed_supervisor(
                    cfg,
                    &repo,
                    2,
                    keep_warm_policy_for_repo(policy.as_ref(), &cfg.orchestrate_owned_repos, &repo),
                    now,
                );
            }
        }
        // The repos to sweep this pass: every live, resident supervisor on the machine.
        let targets: Vec<String> = load_runs()
            .into_iter()
            .filter(|(_, st)| !st.stopped && st.supervise)
            .map(|(_, st)| st.repo)
            .collect();
        let targets = bn_core::crossrepo::cross_repo_land_order(&targets);
        if targets.is_empty() {
            if !json {
                println!("orchestrate: cross-repo supervisor — no supervised repos left; exiting.");
            }
            return;
        }
        let mut any_in_flight = false;
        for repo in &targets {
            // Claim/refresh this repo's owner lease as the all-repos controller. If a dedicated
            // `run --repo R` daemon already owns it (live), defer — let that loop drive it.
            let me_now = me_owner(bn_core::util::now_epoch_secs());
            if !may_drive_repo(repo, &me_now, me_now.heartbeat) {
                continue;
            }
            write_owner(repo, &me_now);
            // One existing per-repo supervise pass (land + recover + reconcile + dispatch), quietly.
            // The `once` sub-call re-checks the lease and, seeing our own pid, drives it.
            orchestrate_run(cfg, Some(repo), false, true, false);
            any_in_flight |= OrchestrateState::load(&orchestrate::state_path_for(repo))
                .is_some_and(|st| st.in_flight() > 0);
        }
        if once {
            return;
        }
        let nap = if any_in_flight {
            POLL_ACTIVE_SECS
        } else {
            POLL_IDLE_SECS
        };
        let queue = bn_core::wake::WakeQueue::default();
        for repo in &targets {
            let target = bn_core::wake::WakeTarget::Orchestrate { repo: repo.clone() };
            if let Ok(pending) = queue.pending(&target) {
                let ids = pending.iter().map(|wake| wake.event_id.clone()).collect();
                let _ = queue.acknowledge(&target, &ids);
            }
        }
        if let Some(watcher) = wake_watcher.as_ref() {
            if let Err(error) = watcher.wait_timeout(std::time::Duration::from_secs(nap)) {
                eprintln!("bn orchestrate: filesystem notifications failed ({error}); polling");
                wake_watcher = None;
                std::thread::sleep(std::time::Duration::from_secs(nap));
            }
        } else {
            std::thread::sleep(std::time::Duration::from_secs(nap));
        }
    }
}

/// #426: adopt this repo's live standalone `bn dispatch` lanes into the supervisor run *before* the
/// tick, so the existing land / recover / reconcile machinery covers a lane dispatched outside this
/// run — the drain's lanes, which otherwise get no background supervision at all. Discovery reads the
/// live tmux agent windows ([`discover_lanes`]); when tmux is unreachable we can't observe, so this is
/// a no-op (never adopt a fleet we can't see). Persisted via `mutate` so the adopted lanes survive to
/// the tick's reload. Called only in supervise mode.
fn adopt_lanes(run_path: &Path, cfg: &Config, repo: &str, now: i64) {
    let Some(st) = OrchestrateState::load(run_path) else {
        return;
    };
    if st.stopped {
        return;
    }
    let Some(windows) = list_windows() else {
        record_degradation(repo, "tmux-unreachable", true, &[], None);
        return; // tmux unreachable — can't observe the live fleet this pass
    };
    record_degradation(repo, "tmux-unreachable", false, &[], None);
    let lanes = discover_lanes(cfg, repo, &windows);
    if lanes.is_empty() {
        return;
    }
    match orchestrate::mutate(run_path, |disk| disk.adopt_dispatched_lanes(&lanes, now)) {
        Ok(Some(adopted)) if !adopted.is_empty() => {
            log_event(
                "orchestrate_adopt",
                &[("repo", repo), ("adopted", &adopted.len().to_string())],
            );
        }
        Err(e) => eprintln!("bn orchestrate: could not adopt lanes: {e}"),
        _ => {}
    }
}

/// #375/#459: refill the working set from the durable backlog before each tick — but **autonomously
/// select** which pending candidates to start, not merely the highest-priority prefix. This is the
/// #348 selection loop: gather this repo's pending backlog (`bn next`) + the open quarter goals + the
/// open attention pressure (`bn attn`), rank them with the pure #458 advise policy, and take the top
/// ones that fit the free capacity (#377 caps) + the #376 conflict map + a don't-start-what-can't
/// -merge filter, all via the pure [`bn_core::select::select_wave`]. Only the chosen `(repo, id)`s
/// are leased into the run — so an item added since the last poll (this repo or another's) is pulled
/// in and dispatched on this very tick, and a wave never starts work that would only collide with an
/// in-flight lane or has no path to land. The run state stays O(cap): we only ever lease up to the
/// free slots, so its size is bounded no matter how deep the backlog is.
#[derive(Default)]
struct RefillResult {
    reason: Option<String>,
    skipped: Vec<BacklogSkip>,
}

struct BacklogSkip {
    id: String,
    item: String,
    reason: String,
}

fn refill_from_backlog(cfg: &Config, run_path: &Path, repo: &str, now: i64) -> RefillResult {
    let Some(mut st) = OrchestrateState::load(run_path) else {
        return RefillResult::default();
    };
    if st.stopped {
        return RefillResult::default();
    }
    refresh_node_registry(cfg, &mut st);
    refresh_repo_placement_policies(cfg, &mut st);
    let caps = build_caps(cfg, &st);
    let policy = cfg.repo_placement_policy(repo);
    let probe_item = orchestrate::QueueItem {
        repo: repo.to_string(),
        required_capabilities: policy.requires,
        excluded_capabilities: policy.excludes,
        ..orchestrate::QueueItem::default()
    };
    let worker_slots = st
        .nodes
        .iter()
        .filter(|node| {
            bn_core::scheduler::worker_eligible(node, &probe_item, &caps)
                && !st.queue.iter().any(|item| {
                    item.occupies_dispatch_slot()
                        && item.node.as_deref().is_some_and(|placed| {
                            placed == node.node_id
                                || node.target
                                    == bn_core::backlog::ExecutionTarget::SshHost(
                                        placed.to_string(),
                                    )
                        })
                })
        })
        .count();
    let local_in_flight = st.local_node_in_flight();
    let queued = st
        .queue
        .iter()
        .filter(|item| item.is_dispatchable())
        .count();
    let available_slots = st
        .total_local_cap()
        .saturating_sub(local_in_flight)
        .saturating_add(worker_slots);
    let capacity = available_slots.saturating_sub(queued);
    if capacity == 0 {
        return RefillResult::default(); // the dispatch pass explains the full working set
    }

    // Read the candidate pool before observing resource guards: an empty backlog is the reason
    // when there is nothing to admit, regardless of current machine pressure.
    let bl = backlog::load(&backlog::backlog_path());
    let pending = bl.pending_for(repo);
    if pending.is_empty() {
        return RefillResult {
            reason: Some("backlog empty".to_string()),
            skipped: Vec::new(),
        };
    }

    // #668: resource preflight — don't lease *new* backlog work onto a box under RAM/swap/load (or
    // budget) pressure. A resource pause HOLDs the whole pull: the items stay pending in the durable
    // backlog (the deferred queue) and are pulled on a later tick once pressure clears — the same
    // defer/drain loop the dispatch-side `admit()` gate uses, so *both* work-admitting sites respect
    // capacity, not just dispatch. Resource guards are machine-wide (independent of in-flight), so we
    // probe the coding lane at 0 in-flight and act only on a real pause (never plain local-cap).
    let obs = observe_resources(cfg, &st, now);
    if let Some(bind) = bn_core::scheduler::admit(&caps, &obs, WorkClass::Coding, 0) {
        if bind.is_pause()
            && (worker_slots == 0 || !bn_core::scheduler::bind_allows_worker_handoff(Some(bind)))
        {
            log_event(
                "orchestrate_backlog_hold",
                &[
                    ("repo", repo),
                    ("cap", bind.label()),
                    ("reason", bind.reason()),
                ],
            );
            return RefillResult {
                reason: Some(bind.reason().to_string()),
                skipped: Vec::new(),
            };
        }
    }

    // The candidate pool: this repo's pending backlog entries (`bn next`), carrying their #376
    // touches + #385 kind. An id already tracked in the run (in-flight or queued for the same
    // (repo, id)) would only duplicate a lane that can't cleanly merge beside it, so it's marked
    // not-mergeable — the don't-start-what-can't-merge guard (defense-in-depth over extend's dedup).
    let in_run: std::collections::HashSet<&str> = st
        .queue
        .iter()
        .filter(|q| q.repo == repo)
        .map(|q| q.id.as_str())
        .collect();
    let candidates: Vec<bn_core::select::SelectCandidate> = pending
        .iter()
        .map(|e| bn_core::select::SelectCandidate {
            id: e.id.clone(),
            repo: e.repo.clone(),
            title: e.item.clone(),
            kind: e.kind,
            priority: e.priority,
            goal: None,
            touches: e.touches.clone(),
            mergeable: !in_run.contains(e.id.as_str()),
        })
        .collect();

    // The strategic signals advise ranks by: the open quarter goals (read-only — never create one on
    // a hot tick) + open attention pressure. Best-effort: absent/unreadable degrades to empty, so the
    // rank falls back to priority + attention exactly as before goals existed.
    let today = bn_core::util::date_of(now);
    let goals = open_quarter_goals(cfg, &today);
    let attention: Vec<bn_core::advise::AttnSignal> = bn_core::attention::open_items(&today)
        .into_iter()
        .map(|a| bn_core::advise::AttnSignal {
            repo: a.repo,
            severity: a.severity,
        })
        .collect();

    // The #376 conflict map: the touch-sets of everything already committed to the run (in-flight +
    // queued), so a newly-selected item that would overlap is held rather than leased into a clash.
    let inflight_touches: Vec<Vec<String>> = st
        .queue
        .iter()
        .filter(|q| !q.status.is_terminal())
        .map(|q| q.touches.clone())
        .collect();

    let selection = bn_core::select::select_wave(&bn_core::select::SelectionInputs {
        goals: &goals,
        candidates: &candidates,
        attention: &attention,
        capacity,
        inflight_touches: &inflight_touches,
    });
    let skipped = selection
        .skipped
        .iter()
        .map(|candidate| BacklogSkip {
            id: candidate.id.clone(),
            item: candidate.title.clone(),
            reason: format!("backlog selection: {}", candidate.reason.label()),
        })
        .collect::<Vec<_>>();
    if selection.wave.is_empty() {
        let reason = if selection.skipped_for(bn_core::select::SkipReason::Conflict) > 0 {
            "backlog candidates held by in-flight conflicts"
        } else if selection.skipped_for(bn_core::select::SkipReason::CantMerge) > 0 {
            "backlog candidates are already in this run"
        } else {
            "no backlog candidate was eligible"
        };
        return RefillResult {
            reason: Some(reason.to_string()),
            skipped,
        };
    }

    // Lease exactly the chosen ids — in wave (advise-rank) order, so the run queue preserves the
    // selection priority the scheduler then dispatches in — and extend the run with them.
    let ids: Vec<String> = selection.wave.iter().map(|c| c.id.clone()).collect();
    let leased = match backlog::lease_selected(&backlog::backlog_path(), repo, &ids, now) {
        Ok(leased) => leased,
        Err(e) => {
            return RefillResult {
                reason: Some(format!("backlog lease failed: {e}")),
                skipped,
            };
        }
    };
    if leased.is_empty() {
        return RefillResult {
            reason: Some("backlog lease produced no work".to_string()),
            skipped,
        };
    }
    // #385: carry each entry's kind → its typed concurrency lane (ship ⇒ coding, scout ⇒ research)
    // so the scheduler caps it in the right lane.
    let items: Vec<QueueEntrySpec> = leased
        .iter()
        .map(|e| {
            (
                e.item.clone(),
                e.touches.clone(),
                e.substrate,
                WorkClass::from_kind(e.kind),
                e.controller_id.clone(),
            )
        })
        .collect();
    match orchestrate::mutate(run_path, |disk| disk.extend_with_entries(&items, now)) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return RefillResult {
                reason: Some(release_failed_backlog_lease(
                    repo,
                    &leased,
                    now,
                    "run disappeared while extending leased backlog work",
                )),
                skipped,
            };
        }
        Err(e) => {
            return RefillResult {
                reason: Some(release_failed_backlog_lease(
                    repo,
                    &leased,
                    now,
                    &format!("run update failed after backlog lease: {e}"),
                )),
                skipped,
            };
        }
    }
    log_event(
        "orchestrate_backlog_lease",
        &[
            ("repo", repo),
            ("leased", &leased.len().to_string()),
            ("skipped", &selection.skipped.len().to_string()),
        ],
    );
    RefillResult {
        reason: None,
        skipped,
    }
}

fn release_failed_backlog_lease(
    repo: &str,
    leased: &[bn_core::backlog::BacklogEntry],
    now: i64,
    reason: &str,
) -> String {
    let ids: Vec<&str> = leased.iter().map(|entry| entry.id.as_str()).collect();
    match backlog::release_many(&backlog::backlog_path(), repo, &ids, now) {
        Ok(released) if released == ids.len() => reason.to_string(),
        Ok(released) => format!(
            "{reason}; only {released}/{} backlog leases returned to pending",
            ids.len()
        ),
        Err(e) => format!("{reason}; backlog lease release failed: {e}"),
    }
}

/// The open quarter goals with their latest-review status — the strategic weights the #459 selection
/// loop ranks candidates by. Read-only + best-effort: a missing/unparseable quarter yields no goals
/// (selection then falls back to priority + attention), and it never *creates* a quarter file on a
/// hot tick the way `ensure_current` would.
fn open_quarter_goals(cfg: &Config, today: &str) -> Vec<bn_core::advise::GoalContext> {
    let Some(qid) = bn_core::quarter::quarter_id(today) else {
        return Vec::new();
    };
    match bn_core::quarter::read_quarter(cfg, &qid) {
        Ok(Some(q)) => q
            .open()
            .map(|g| bn_core::advise::GoalContext {
                key: g.key.clone(),
                text: g.text.clone(),
                status: q.status_of(&g.key).to_string(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// #375: reconcile the run ledger's terminal items back to the backlog in one locked pass — a merged
/// run item marks its `(repo, id)` backlog entry `done`, a failed one `failed`. Idempotent: an entry
/// already terminal (or never in the backlog, e.g. a direct `--queue` item) is left untouched, so a
/// steady state appends nothing. Then compact the log if superseded/terminal lines have piled up.
fn reconcile_backlog(run_path: &Path, repo: &str, now: i64) {
    let Some(st) = OrchestrateState::load(run_path) else {
        return;
    };
    let outcomes: Vec<(&str, BacklogStatus)> = st
        .queue
        .iter()
        .filter_map(|i| match i.status {
            ItemStatus::Merged => Some((i.id.as_str(), BacklogStatus::Done)),
            ItemStatus::Failed => Some((i.id.as_str(), BacklogStatus::Failed)),
            _ => None,
        })
        .collect();
    let bl_path = backlog::backlog_path();
    if !outcomes.is_empty() {
        if let Err(e) = backlog::resolve_many(&bl_path, repo, &outcomes, now) {
            eprintln!("bn orchestrate: could not reconcile backlog: {e}");
        }
    }
    match backlog::compact_if_needed(&bl_path) {
        Ok(_) => record_degradation(repo, "backlog-compact-skipped", false, &[], None),
        Err(error) => record_degradation(
            repo,
            "backlog-compact-skipped",
            true,
            &[],
            Some(&error.to_string()),
        ),
    }
}

// ===========================================================================
// #380 durability executors — history eviction + reboot-safe backoff (thin IO)
// ===========================================================================

/// #380: evict terminal (merged/failed) items older than [`orchestrate::HISTORY_RETAIN_SECS`] from
/// the live queue into the append-only `run-history.jsonl`, so a weeks-long run's per-tick state
/// rewrite stays O(in-flight + recent). Durable-first: the outcomes are appended to history *before*
/// they're removed from the queue, so a crash between the two only ever duplicates a record (the
/// read side dedups by id), never loses one. The eviction predicate is the pure, unit-tested
/// [`OrchestrateState::evict_terminal`]; this is only the file IO around it.
fn evict_history(run_path: &Path, repo: &str, now: i64) {
    let Some(st) = OrchestrateState::load(run_path) else {
        return;
    };
    let records: Vec<HistoryRecord> = st
        .evictable_terminal(now, orchestrate::HISTORY_RETAIN_SECS)
        .into_iter()
        .map(|it| HistoryRecord::from_item(repo, it, now))
        .collect();
    if records.is_empty() {
        return;
    }
    // Persist to history first; only remove from the live queue if that succeeded (no data loss).
    if orchestrate::append_history(&records).is_err() {
        return;
    }
    let removed = match orchestrate::mutate(run_path, |disk| {
        disk.evict_terminal(now, orchestrate::HISTORY_RETAIN_SECS)
    }) {
        Ok(Some(removed)) => removed,
        Ok(None) => 0,
        Err(e) => {
            eprintln!("bn orchestrate: could not evict run history: {e}");
            0
        }
    };
    if removed > 0 {
        log_event(
            "orchestrate_history_evict",
            &[("repo", repo), ("evicted", &removed.to_string())],
        );
    }
}

/// #380: snapshot **every** terminal item of an outgoing run to `run-history.jsonl` before a fresh
/// `bn orchestrate --queue` overwrites the run state — so outcomes that hadn't yet aged past the
/// eviction window (and would otherwise vanish with the overwrite) stay durably queryable. Uses the
/// pure eviction predicate with a zero window (all terminal items).
fn archive_all_terminal(st: &OrchestrateState, repo: &str, now: i64) {
    let records: Vec<HistoryRecord> = st
        .evictable_terminal(now, 0)
        .into_iter()
        .map(|it| HistoryRecord::from_item(repo, it, now))
        .collect();
    if records.is_empty() {
        return;
    }
    if orchestrate::append_history(&records).is_ok() {
        log_event(
            "orchestrate_history_archive",
            &[("repo", repo), ("archived", &records.len().to_string())],
        );
    }
}

/// #380: on a detected fresh boot, reset the relaunch backoff for in-flight items whose worktree
/// survived the reboot — so a reboot re-dispatch starts a fresh curve rather than inheriting a
/// burned `attempts` count toward the [`Backoff`] give-up cap. Detection is the pure
/// [`OrchestrateState::is_fresh_boot`] against a persisted boot marker; the worktree-exists check
/// and the OS boot-id read are the executor's IO. When the boot id can't be read we skip entirely
/// (never guess a reboot). Runs once at daemon start, before the loop observes any pane.
fn reset_backoff_on_fresh_boot(cfg: &Config, run_path: &Path) {
    let Some(current) = current_boot_id() else {
        return; // can't observe the boot marker — don't guess
    };
    let now = bn_core::util::now_epoch_secs();
    let Some(st) = OrchestrateState::load(run_path) else {
        return;
    };
    if !st.is_fresh_boot(&current) {
        // First-ever pass (or same boot): just record the marker so a later reboot is detected.
        if let Err(e) = orchestrate::mutate(run_path, |disk| disk.record_boot(&current, now)) {
            eprintln!("bn orchestrate: could not record boot marker: {e}");
        }
        return;
    }
    // Fresh boot: which in-flight items still have a worktree on disk (survived the reboot)?
    let live: std::collections::HashSet<String> = st
        .queue
        .iter()
        .filter(|i| i.status.in_flight())
        .filter(|i| {
            i.branch
                .as_deref()
                .is_some_and(|b| worktree_path(cfg, &st.repo, b).is_dir())
        })
        .map(|i| i.id.clone())
        .collect();
    let candidates = st.boot_reset_candidates(&live);
    let reset = match orchestrate::mutate(run_path, |disk| {
        let reset = disk.reset_backoff(&candidates, now);
        disk.record_boot(&current, now);
        reset
    }) {
        Ok(Some(reset)) => reset,
        Ok(None) => Vec::new(),
        Err(e) => {
            eprintln!("bn orchestrate: could not reset reboot backoff: {e}");
            Vec::new()
        }
    };
    if !reset.is_empty() {
        log_event(
            "orchestrate_boot_reset",
            &[("repo", &st.repo), ("reset", &reset.len().to_string())],
        );
    }
}

/// The system's current boot marker (#380): a value that changes on every reboot so the daemon can
/// tell a fresh boot from a same-boot relaunch. Linux exposes a per-boot UUID; macOS/BSD expose the
/// boot epoch via `sysctl kern.boottime`. `None` when neither is readable — the caller then skips
/// the boot-reset rather than guess.
fn current_boot_id() -> Option<String> {
    // Linux: a per-boot random UUID.
    if let Ok(id) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        let id = id.trim();
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    // macOS/BSD: `kern.boottime` → "{ sec = 1712345678, usec = 0 } <date>"; the sec is the boot epoch.
    let out = std::process::Command::new("sysctl")
        .args(["-n", "kern.boottime"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_boottime_sec(&text).map(|sec| format!("boottime:{sec}"))
}

/// Parse the `sec = NNN` boot epoch out of a `sysctl -n kern.boottime` line (#380). Pure helper so
/// the (fiddly) format parse is unit-testable without shelling out.
fn parse_boottime_sec(s: &str) -> Option<i64> {
    let after = s.split("sec").nth(1)?; // "= 1712345678, usec = 0 } ..."
    let digits: String = after
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<i64>().ok()
}

/// One supervision pass. Order is deliberate: land ready PRs first (frees concurrency slots), then
/// reconcile which items merged, then recover wedged in-flight agents, then dispatch the next
/// queued items. Returns whether the run is now drained.
fn tick(cfg: &Config, run_path: &Path, st: &mut OrchestrateState, now: i64) -> bool {
    // A supervisor lands ready PRs *every* pass (not only when it holds its own in-flight items), so
    // a finished standalone lane whose agent window already closed still gets its green PR landed
    // unattended (#426); a plain run only bothers when it has in-flight work to reconcile.
    if st.supervise || st.in_flight() > 0 {
        land_ready(cfg, st);
        reconcile_landed(cfg, st, now);
    }
    // #377: remote fan-out items have no local pane — reconcile them by their PR/issue completion
    // (the same ExternalState signals the local reconcile uses), *beside* the tmux pane machine.
    reconcile_remote(st, now);
    supervise_inflight(cfg, st, now);
    // Persist recovered/terminal lanes before refilling. On reboot, phantom lanes may have filled
    // the cap at the start of the pass; folding their recovery now lets backlog selection observe
    // the freed slots and dispatch replacement work in this same supervisor tick.
    let _ = orchestrate::mutate(run_path, |disk| disk.merge_worked(st, now));
    let _ = keep_warm_tick(cfg, run_path, &st.repo, now);
    let refill = refill_from_backlog(cfg, run_path, &st.repo, now);
    if let Some(refreshed) = OrchestrateState::load(run_path) {
        *st = refreshed;
    }
    // #378: route scout/ask answers back — harvest findings for any research item that reached Done
    // this pass (or an earlier one) but hasn't been answered yet. Before the drain check, so an
    // answered ask counts as terminal and a self-feeding run can move on.
    harvest_answers(cfg, st, now);
    fill_dispatch_slots(cfg, run_path, st, now, &refill);
    st.is_drained()
}

#[derive(serde::Deserialize)]
struct KeepWarmIssueRow {
    number: u64,
    title: String,
    labels: Vec<KeepWarmLabelRow>,
    assignees: Vec<serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct KeepWarmLabelRow {
    name: String,
}

fn open_keep_warm_issues(cfg: &Config, repo: &str) -> Option<Vec<orchestrate::KeepWarmIssue>> {
    let cwd = a_repo_worktree(cfg, repo)?;
    let slug = crate::gh::gh_capture_in(
        &cwd,
        &[
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "-q",
            ".nameWithOwner",
        ],
    )?;
    let rows: Vec<KeepWarmIssueRow> = serde_json::from_str(&crate::gh::gh_capture_in(
        &cwd,
        &[
            "issue",
            "list",
            "-R",
            &slug,
            "--state",
            "open",
            "--limit",
            "200",
            "--json",
            "number,title,labels,assignees",
        ],
    )?)
    .ok()?;
    Some(
        rows.into_iter()
            .map(|row| orchestrate::KeepWarmIssue {
                number: row.number,
                title: row.title,
                labels: row.labels.into_iter().map(|label| label.name).collect(),
                assigned: !row.assignees.is_empty(),
            })
            .collect(),
    )
}

/// Replenish an opted-in resident supervisor before backlog selection. This intentionally does not
/// consult resource guards: enqueueing is cheap and durable; the existing scheduler throttles the
/// later lease/dispatch step under RAM, load, swap, token, or cost pressure.
fn keep_warm_tick(cfg: &Config, run_path: &Path, repo: &str, now: i64) -> bool {
    let Some(st) = OrchestrateState::load(run_path) else {
        return false;
    };
    let Some(policy) = st.keep_warm.as_ref() else {
        return true;
    };
    let backlog_path = backlog::backlog_path();
    let backlog = backlog::load(&backlog_path);
    let pending = backlog.pending_for(repo);
    let active_run: Vec<&orchestrate::QueueItem> = st
        .queue
        .iter()
        .filter(|item| !item.status.is_terminal())
        .collect();
    let active_depth = pending.len() + active_run.len();
    if active_depth >= policy.low_water {
        return true;
    }
    let Some(issues) = open_keep_warm_issues(cfg, repo) else {
        log_event("orchestrate_keep_warm_unavailable", &[("repo", repo)]);
        eprintln!("keep-warm: could not list open issues for {repo}");
        return false;
    };
    let active_ids: std::collections::HashSet<String> = pending
        .iter()
        .map(|entry| entry.id.clone())
        .chain(active_run.iter().map(|item| item.id.clone()))
        .collect();
    let plan = orchestrate::plan_keep_warm(
        repo,
        policy,
        active_depth,
        &active_ids,
        &issues,
        st.keep_warm_last_brainstorm_at,
        now,
    );

    let mut success = true;
    // **Superseded by `bn task pull` (#1001).** This arm used to file issues straight into the
    // backlog, which is now a second replenisher racing `bn task pull` over the same issue set
    // with a different dedupe key (backlog `(repo, id)` vs the task store's `src:`). Worse, it
    // enqueued work that no task governed — the exact hole the `task:` invariant closes.
    //
    // `plan_keep_warm` survives as the ranking brain; only this realization of its decision is
    // retired. The refill now happens as tasks, which then pass the SMART gate before executing.
    if !plan.enqueue_issues.is_empty() {
        log_event(
            "orchestrate_keep_warm_superseded",
            &[
                ("repo", repo),
                ("would_add", &plan.enqueue_issues.len().to_string()),
                ("low_water", &policy.low_water.to_string()),
            ],
        );
        eprintln!(
            "keep-warm: {repo} is {} below its low-water mark — run `bn task pull --repo {repo}` (this arm was superseded by #1001)",
            plan.enqueue_issues.len()
        );
    }

    if !plan.brainstorm.is_empty() {
        let spec = backlog::AddSpec {
            kind: BacklogKind::Scout,
            priority: -10,
            source: "keep-warm-brainstorm".to_string(),
            // Brainstorm work is bn-authored, but it still needs a task like everything else.
            task: crate::cmd::task::task_for_enqueue(
                cfg,
                repo,
                plan.brainstorm.first().map(String::as_str).unwrap_or("brainstorm"),
                "keep-warm-brainstorm",
            ),
            ..backlog::AddSpec::default()
        };
        match backlog::add(&backlog_path, repo, &plan.brainstorm, &spec, now) {
            Ok(added) if !added.is_empty() => {
                let _ = orchestrate::mutate(run_path, |disk| {
                    disk.keep_warm_last_brainstorm_at = Some(now);
                    disk.updated_at = now;
                });
                log_event(
                    "orchestrate_keep_warm_brainstorm",
                    &[("repo", repo), ("launched", &added.len().to_string())],
                );
            }
            Err(error) => {
                eprintln!("bn orchestrate: keep-warm brainstorm failed for {repo}: {error}");
                success = false;
            }
            _ => {}
        }
    }
    success
}

// ===========================================================================
// Land: gate + merge-queue + reconcile
// ===========================================================================

/// Land every ready PR via the gated merge-queue (`bn wt merge --all`: gate + file-overlap
/// serialize + auto-rebase + cleanup), then reap merged/scout-done worktrees. Run from within a
/// worktree of the target repo (the merge-queue keys off the cwd's bare). Best-effort.
fn land_ready(cfg: &Config, st: &OrchestrateState) {
    let Some(wt) = a_repo_worktree(cfg, &st.repo) else {
        return; // no worktree yet → nothing to land
    };
    // `--ci-wait 0`: check each PR's checks once and move on rather than sitting on a running one.
    // The gate itself still applies (a red or unfinished PR does not land) — but a poller that
    // blocks a whole tick waiting on one PR stops polling everything else, and it will see the
    // finished run on the next tick anyway.
    let (ok, _out) = run_bn(&["wt", "merge", "--all", "--ci-wait", "0"], Some(&wt));
    if ok {
        // Reclaim merged ships + finished scouts (safe: only removes merged / scout-done+clean).
        let _ = wt::cmd_reap(cfg, true, true);
    }
}

/// Reconcile which in-flight items have landed: the merge-queue removes a merged PR's worktree, so a
/// dispatched item whose worktree has vanished is merged. Marks it terminal and frees its slot.
fn reconcile_landed(cfg: &Config, st: &mut OrchestrateState, now: i64) {
    let ids: Vec<(String, String)> = st
        .queue
        .iter()
        .filter(|i| i.status == ItemStatus::Dispatched)
        .filter_map(|i| i.branch.clone().map(|b| (i.id.clone(), b)))
        .collect();
    for (id, branch) in ids {
        let wt = worktree_path(cfg, &st.repo, &branch);
        if !wt.is_dir() {
            let pr = done_pr_link(&st.repo, &branch);
            st.mark_merged(&id, pr, now);
            st.record_recovery(
                &id,
                PaneState::Done,
                SupervisorAction::Land,
                "PR landed + worktree reaped",
                now,
            );
            log_event(
                "orchestrate_merged",
                &[("repo", &st.repo), ("branch", &branch)],
            );
        }
    }
}

// ===========================================================================
// Answer routing (#378) — harvest scout/ask findings and route them back
// ===========================================================================

/// #378: route scout/ask answers back to the asker. For every scout/ask (research-lane) item that
/// has reached a terminal merged state — its findings written, its worktree reaped — but hasn't yet
/// been answered, harvest the findings write-up from its (synced, worktree-independent) branch-note
/// dir and record it on the run item ([`OrchestrateState::record_answer`], the pure terminal
/// transition). Then log an `orchestrate_answered` event, raise an attention item linking the
/// findings, and toast — so the answer surfaces via `bn orchestrate answers` / `ask --wait` (which
/// read the state file) *and* a notification. Idempotent: an already-answered item is skipped, so a
/// scout never relaunch-loops and re-harvest each poll is a no-op. Best-effort IO — the state
/// mutation is the pure half; this is its CLI executor.
fn harvest_answers(cfg: &Config, st: &mut OrchestrateState, now: i64) {
    let pending: Vec<(String, String, String)> = st
        .queue
        .iter()
        .filter(|i| i.is_scout() && i.status == ItemStatus::Merged && i.answer.is_none())
        .filter_map(|i| i.branch.clone().map(|b| (i.id.clone(), b, i.item.clone())))
        .collect();
    for (id, branch, question) in pending {
        let path = findings_path(cfg, &st.repo, &branch);
        let path_str = path.to_string_lossy().into_owned();
        if !st.record_answer(&id, &path_str, &question, now) {
            continue;
        }
        log_event(
            "orchestrate_answered",
            &[
                ("repo", &st.repo),
                ("id", &id),
                ("question", &question),
                ("path", &path_str),
            ],
        );
        let key = format!("orchestrate-answer:{}/{}", st.repo, id);
        let _ = bn_core::attention::add(
            "orchestrate-ask",
            &format!("answer ready: {question}"),
            "info",
            Some(&st.repo),
            Some(&branch),
            Some(&path_str),
            Some(&key),
            &bn_core::util::today(),
        );
        // An `info` attention item for this same answer was raised a few lines above, so the
        // toast was always the second copy of one notification. It stays a completion: the scout
        // finished and the write-up is waiting to be read, not acted on.
        crate::toast::toast(
            crate::toast::Class::Completion,
            &format!("orchestrate:{}/{branch}:answer", st.repo),
            "orchestrate",
            &format!("bn orchestrate: answer ready — {}", st.repo),
            &format!("{question}\n{path_str}"),
        );
    }
}

/// The findings write-up to route back for a scout/ask (#378), in its branch-note dir. Prefer the
/// scout's own longer report — the most-recently-modified sibling markdown file the `investigate`
/// skill wrote under `$(bn --path)` — else fall back to the branch note itself (which always carries
/// at least the goal + `bn add research/decision` findings). The note dir is keyed by the sanitized
/// branch (slash→dash, matching how `bn dispatch` seeds it) and lives in the synced notes root, so
/// it stays readable after the scout worktree is reaped.
fn findings_path(cfg: &Config, repo: &str, branch: &str) -> PathBuf {
    let ctx = bn_core::resolver::Resolved {
        repo: repo.to_string(),
        branch: wt::sanitize_branch(branch),
    };
    let note = cfg.note_file(&ctx);
    let dir = cfg.note_dir(&ctx);
    let latest_writeup = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().and_then(|s| s.to_str()) == Some("md")
                && p.file_name().and_then(|s| s.to_str()) != Some("note.md")
        })
        .filter_map(|p| {
            let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
            Some((mtime, p))
        })
        .max_by_key(|(mtime, _)| *mtime)
        .map(|(_, p)| p);
    latest_writeup.unwrap_or(note)
}

// ===========================================================================
// Supervise: classify each in-flight pane and act
// ===========================================================================

/// Classify and act on every in-flight agent this pass. No-op when tmux is unreachable, so a
/// headless host never mis-reads every agent as `dead` and relaunch-loops.
fn supervise_inflight(cfg: &Config, st: &mut OrchestrateState, now: i64) {
    let Some(windows) = list_windows() else {
        record_degradation(&st.repo, "tmux-unreachable", true, &[], None);
        return; // tmux unreachable — can't observe; leave the fleet untouched
    };
    record_degradation(&st.repo, "tmux-unreachable", false, &[], None);
    let live_processes = crate::reap::live_agent_targets(&cfg.worktree_dir);
    let launches = bn_core::cost::launch_records();
    // Refresh origin/<default> once for the whole fleet so the per-agent wip check is judged
    // against the *live* main, not a stale local ref (#370 defect 3).
    fetch_origin_default(cfg, &st.repo);
    let inflight: Vec<(String, String)> = st
        .queue
        .iter()
        .filter(|i| i.status == ItemStatus::Dispatched)
        .filter_map(|i| i.branch.clone().map(|b| (i.id.clone(), b)))
        .collect();
    for (id, branch) in inflight {
        let obs = observe(cfg, &st.repo, &branch, &windows, &live_processes);
        let wt = worktree_path(cfg, &st.repo, &branch);
        let progress = bn_core::agent::agent_progress_for_cwd(&wt);
        update_stuck_lane(
            cfg,
            st,
            &id,
            &branch,
            &obs,
            progress.as_ref(),
            &launches,
            now,
        );
        // A merged PR is terminal even if the agent still looks busy. Stop the shipped lane before
        // the active-process fast path can skip reconciliation, then reuse the guarded cleanup
        // policy so dirty worktrees are preserved while the in-flight slot is freed immediately.
        let (branch_pr_merged, pr_url) = merged_branch_pr(cfg, &st.repo, &branch);
        let external = ExternalState {
            branch_pr_merged,
            ..ExternalState::default()
        };
        if should_stop_after_ship(external) {
            let killed = crate::reap::terminate_lane(&cfg.worktree_dir, &st.repo, &branch);
            if killed > 0 {
                let killed = killed.to_string();
                log_event(
                    "orchestrate_stop_after_ship",
                    &[
                        ("repo", &st.repo),
                        ("branch", &branch),
                        ("processes", &killed),
                    ],
                );
            }

            if reconcile_external_state(cfg, st, &id, &branch, true, (external, pr_url), now) {
                continue;
            }
        }
        if !obs.has_window && obs.has_process {
            st.set_state(&id, PaneState::Working, now);
            continue;
        }
        let state = orchestrate::classify(&obs.as_obs());
        st.set_state(&id, state, now);
        // #370 defect 2: before recovering a settled/stuck agent, reconcile external completion —
        // a merged PR on the branch or a closed linked issue means the work already landed (even
        // if the PR was opened outside the daemon's tracking), so mark it done instead of looping.
        // Gated to settled panes so a busy `working` agent never triggers the gh reconcile calls.
        if is_reconcilable(state)
            && reconcile_external(
                cfg,
                st,
                &id,
                &branch,
                lane_is_dead(obs.has_window, obs.has_process),
                now,
            )
        {
            continue;
        }
        // #374: an `idle-clean` pane is ambiguous — just-launched, finished, or stalled all look
        // identical. Refine it with the agent's liveness (AIC + PR + last-activity) so a *stalled*
        // agent (worked, then died at an empty prompt) is recovered instead of hogging its slot
        // forever, while a fresh or finished one is still left alone. Gated on a positively-observed
        // idle prompt: an `IdleClean` from `classify`'s ambiguous fallback (spinner scrolled off,
        // no prompt captured) could be a working agent mid-long-turn, so it's never judged stalled.
        // Every other state keeps the deterministic #340 mapping.
        let action =
            if state == PaneState::IdleClean && orchestrate::shows_bare_prompt(&obs.as_obs()) {
                let live = gather_liveness(cfg, &st.repo, &branch, now);
                orchestrate::idle_clean_action(live, IDLE_CLEAN_STALE_SECS)
            } else {
                orchestrate::action_for(state)
            };
        execute_action(st, &id, &branch, state, action, &obs, now);
    }
}

#[allow(clippy::too_many_arguments)]
fn update_stuck_lane(
    cfg: &Config,
    st: &mut OrchestrateState,
    id: &str,
    branch: &str,
    obs: &Observed,
    progress: Option<&bn_core::agent::AgentProgress>,
    launches: &[bn_core::cost::LaunchRecord],
    now: i64,
) {
    let fingerprint = bn_core::agent::pane_activity_fingerprint(&obs.tail, &obs.plane_a);
    let unchanged_secs = st.observe_lane_activity(
        id,
        fingerprint,
        progress.map(|value| value.last_activity_ts),
        now,
    );
    let stuck = bn_core::agent::classify_stuck_lane(&bn_core::agent::StuckLaneObservation {
        tail: &obs.tail,
        has_window: obs.has_window,
        has_process: obs.has_process,
        plane_a: &obs.plane_a,
        has_wip: obs.has_wip,
        unchanged_secs,
        idle_threshold_secs: cfg.orchestrate_stuck_idle_secs,
    });
    st.set_stuck_lane(id, stuck.clone(), now);

    let Some(stuck) = stuck else {
        return;
    };
    let key = stuck.key();
    let Some(item) = st.queue.iter().find(|item| item.id == id) else {
        return;
    };
    if item.stuck_notified_key.as_deref() == Some(key.as_str()) {
        return;
    }
    let Some(controller_id) = item.controller_id.clone() else {
        return;
    };
    let worker_session_id = progress.map(|value| value.session_id.as_str()).or_else(|| {
        launches
            .iter()
            .rev()
            .find(|launch| {
                launch.repo.eq_ignore_ascii_case(&st.repo)
                    && launch.branch.eq_ignore_ascii_case(branch)
                    && launch.controller_id.as_deref() == Some(controller_id.as_str())
            })
            .map(|launch| launch.session_id.as_str())
    });
    let Some(worker_session_id) = worker_session_id else {
        log_event(
            "orchestrate_stuck_event_unattributed",
            &[("repo", &st.repo), ("branch", branch), ("state", &key)],
        );
        return;
    };
    let lane = format!("{}:{}", st.repo, branch);
    let summary = stuck.summary(&lane);
    match crate::cmd::control::append_controller_worker_event(
        &controller_id,
        worker_session_id,
        &st.repo,
        branch,
        bn_core::controller::ControllerWorkerEventType::Blocked,
        &summary,
        None,
    ) {
        Ok(()) => {
            st.mark_stuck_notified(id, key.clone(), now);
            log_event(
                "orchestrate_stuck_event",
                &[
                    ("repo", &st.repo),
                    ("branch", branch),
                    ("controller_id", &controller_id),
                    ("state", &key),
                ],
            );
        }
        Err(error) => {
            eprintln!(
                "bn orchestrate: could not notify controller {controller_id} that {lane} is stuck: {error}"
            );
            let _ = log_orchestrate_event(
                EventLevel::Error,
                "orchestrate_stuck_event_failed",
                "orchestrate-stuck-event-failed",
                &[("repo", &st.repo), ("branch", branch), ("item_id", id)],
                Some(&error.to_string()),
                None,
            );
        }
    }
}

/// Gather the #374 liveness signals for an idle-clean in-flight agent: how much it has actually done
/// (cumulative AIC from the harness session log, read even when the log is quiet/stalled), whether
/// its branch already carries an open/merged PR, and how long since its last sign of life. Feeds
/// [`orchestrate::idle_clean_action`], which decides leave-vs-recover. Best-effort but fail-safe —
/// a missing session reads as `aic = 0` (just-launched → leave) and an *inconclusive* PR lookup (gh
/// offline / auth) reads as `has_pr = true` (finished → leave), so an unobservable agent is never
/// hard-restarted on a hunch (a false-positive would re-do delivered work).
fn gather_liveness(cfg: &Config, repo: &str, branch: &str, now: i64) -> Liveness {
    let wt = worktree_path(cfg, repo, branch);
    let (aic, idle_secs) = match bn_core::agent::agent_progress_for_cwd(&wt) {
        Some(p) => (p.aic.max(0) as u64, (now - p.last_activity_ts).max(0)),
        None => (0, 0),
    };
    // A finished agent's PR is open (awaiting the merge-queue) or merged — either way its work is
    // delivered, so never relaunch it. `None` means the lookup was inconclusive (gh failed); stay
    // conservative and assume a PR exists rather than risk relaunching a completed item (#370-2).
    let has_pr = branch_has_open_or_merged_pr(repo, branch).unwrap_or(true);
    Liveness {
        aic,
        has_pr,
        idle_secs,
    }
}

/// Whether `branch` has an **open or merged** PR — `Some(true/false)` when the answer is definitive,
/// `None` when gh couldn't be reached (so the caller can stay conservative). Distinct from
/// [`wt::review_pr_state`], which collapses a gh failure to `"NONE"`; here a failure must not read as
/// "no PR" or a transient outage could trigger a destructive relaunch of a finished item (#374).
fn branch_has_open_or_merged_pr(repo: &str, branch: &str) -> Option<bool> {
    #[derive(serde::Deserialize)]
    struct Row {
        state: String,
    }
    // `gh_json` is `None` only on a gh failure/parse error; an empty result is `Some(vec![])`, so a
    // definitive "no PR" is distinguishable from "couldn't check".
    bn_core::runner::gh_json::<Vec<Row>>(
        &bn_core::runner::RealRunner,
        &[
            "pr", "list", "--repo", repo, "--head", branch, "--state", "all", "--json", "state",
            "--limit", "10",
        ],
    )
    .map(|rows| {
        rows.iter()
            .any(|r| matches!(r.state.as_str(), "OPEN" | "MERGED"))
    })
}

/// Whether an in-flight pane in this state is worth an external-completion check (#370 defect 2):
/// settled or stuck states where "maybe it already landed elsewhere" is plausible. A `working`
/// agent is mid-turn (skip — no gh calls on the hot path); a menu is a live human decision.
fn is_reconcilable(state: PaneState) -> bool {
    matches!(
        state,
        PaneState::IdleClean
            | PaneState::IdleWithWip
            | PaneState::ApiError
            | PaneState::DnsOutage
            | PaneState::Dead
    )
}

/// Reconcile one in-flight item against external completion signals (#370 defect 2). If its branch
/// has a merged PR, or its linked issue is closed, or its worktree was reaped, mark it `merged`
/// (terminal) and record it — freeing the slot instead of relaunch-looping a finished item. Returns
/// whether the item was reconciled. Best-effort: a gh failure (offline / auth) confirms nothing, so
/// an unfinished item is never falsely marked done.
fn reconcile_external(
    cfg: &Config,
    st: &mut OrchestrateState,
    id: &str,
    branch: &str,
    dead_lane: bool,
    now: i64,
) -> bool {
    let external = external_state(cfg, &st.repo, branch, id);
    reconcile_external_state(cfg, st, id, branch, dead_lane, external, now)
}

fn reconcile_external_state(
    cfg: &Config,
    st: &mut OrchestrateState,
    id: &str,
    branch: &str,
    dead_lane: bool,
    external: (ExternalState, Option<String>),
    now: i64,
) -> bool {
    let (ext, pr_url) = external;
    if !ext.is_resolved() {
        return false;
    }
    if should_cleanup_dead_merged(dead_lane, ext) {
        let worktree = worktree_path(cfg, &st.repo, branch);
        if worktree.is_dir() {
            let _ = wt::cleanup_one_for_reap(cfg, &worktree, &cfg.worktree_dir, false);
        }
    }
    let via = ext.via().unwrap_or("external");
    let pr = pr_url.or_else(|| done_pr_link(&st.repo, branch));
    st.mark_merged(id, pr.clone(), now);
    st.record_recovery(
        id,
        PaneState::Done,
        SupervisorAction::Land,
        &format!(
            "reconciled done via {via}{}",
            pr.as_deref().map(|u| format!(" ({u})")).unwrap_or_default()
        ),
        now,
    );
    // Pure bookkeeping: a PR that merged elsewhere, reconciled into the ledger. Nothing stopped.
    crate::toast::toast(
        crate::toast::Class::Lifecycle,
        &format!("orchestrate:{}/{branch}:reconciled", st.repo),
        "orchestrate",
        &format!("bn orchestrate: {}/{branch} reconciled", st.repo),
        &format!("already done ({via}) — marked merged"),
    );
    log_event(
        "orchestrate_reconciled",
        &[("repo", &st.repo), ("branch", branch), ("via", via)],
    );
    true
}

fn should_stop_after_ship(external: ExternalState) -> bool {
    external.branch_pr_merged
}

fn should_cleanup_dead_merged(dead_lane: bool, external: ExternalState) -> bool {
    dead_lane && external.branch_pr_merged
}

/// #377: reconcile **remote** fan-out items by their issue completion — beside the tmux pane
/// machine, never through `classify`. A remote item has no local worktree or pane, so the
/// branch-based passes (`reconcile_landed`, `supervise_inflight`) skip it (its `branch` is `None`);
/// the branchless [`external_state`] can't be reused either (its `worktree_gone` signal would
/// falsely resolve every remote item at once). Instead, when the item's linked issue has CLOSED —
/// the `@claude` PR merged and closed it — mark it `merged` and free it. Best-effort: a gh failure
/// confirms nothing, so an unfinished remote item is never falsely completed.
fn reconcile_remote(st: &mut OrchestrateState, now: i64) {
    let remote: Vec<String> = st
        .queue
        .iter()
        .filter(|i| i.status == ItemStatus::Dispatched && i.substrate == Substrate::Remote)
        .map(|i| i.id.clone())
        .collect();
    for id in remote {
        // The scheduler only routes issue-linked (numeric-id) items remote, so `id` is the issue.
        if id.chars().all(|c| c.is_ascii_digit()) && issue_is_closed(&st.repo, &id) {
            st.mark_merged(&id, None, now);
            st.record_recovery(
                &id,
                PaneState::Done,
                SupervisorAction::Land,
                "remote reconciled via issue-closed",
                now,
            );
            log_event(
                "orchestrate_reconciled",
                &[("repo", &st.repo), ("id", &id), ("via", "issue-closed")],
            );
        }
    }
}

/// Gather external completion signals for an item (#370 defect 2): a merged PR on the branch (via
/// the repo's configured remote), a closed linked issue (numeric ids only — a text-slug item has no
/// issue), and whether the daemon's worktree was reaped. Also returns the merged PR url, when known.
fn external_state(
    cfg: &Config,
    repo: &str,
    branch: &str,
    id: &str,
) -> (ExternalState, Option<String>) {
    let (branch_pr_merged, pr_url) = merged_branch_pr(cfg, repo, branch);
    let issue_closed = id.chars().all(|c| c.is_ascii_digit()) && issue_is_closed(repo, id);
    let worktree_gone = !worktree_path(cfg, repo, branch).is_dir();
    (
        ExternalState {
            branch_pr_merged,
            issue_closed,
            worktree_gone,
        },
        pr_url,
    )
}

fn merged_branch_pr(cfg: &Config, repo: &str, branch: &str) -> (bool, Option<String>) {
    let merged = wt::review_pr_state(cfg, repo, branch) == "MERGED";
    let url = merged.then(|| merged_pr_url(repo, branch)).flatten();
    (merged, url)
}

/// The url of the merged PR whose head is `branch`, or `None`. Best-effort — used only to decorate
/// the reconcile note/link once [`external_state`] has already confirmed a merged PR exists.
fn merged_pr_url(repo: &str, branch: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Row {
        url: String,
    }
    bn_core::runner::gh_json::<Vec<Row>>(
        &bn_core::runner::RealRunner,
        &[
            "pr", "list", "--repo", repo, "--head", branch, "--state", "merged", "--json", "url",
            "--limit", "1",
        ],
    )
    .and_then(|rows| rows.into_iter().next())
    .map(|r| r.url)
}

/// Whether the linked issue `id` is CLOSED (#370 defect 2). Best-effort via `gh issue view`; a gh
/// failure (offline / auth / the id is a PR, not an issue) reads as "not closed", so reconciliation
/// only ever *confirms* completion, never fabricates it.
fn issue_is_closed(repo: &str, id: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct State {
        state: String,
    }
    bn_core::runner::gh_json::<State>(
        &bn_core::runner::RealRunner,
        &["issue", "view", id, "--repo", repo, "--json", "state"],
    )
    .map(|s| s.state.eq_ignore_ascii_case("CLOSED"))
    .unwrap_or(false)
}

/// Realize one supervisor action against the live fleet. Every branch is best-effort and records a
/// recovery event for `bn orchestrate status`.
fn execute_action(
    st: &mut OrchestrateState,
    id: &str,
    branch: &str,
    state: PaneState,
    action: SupervisorAction,
    obs: &Observed,
    now: i64,
) {
    let repo = st.repo.clone();
    let target = tmux_target(&repo, branch);
    match action {
        SupervisorAction::Leave => {}
        SupervisorAction::Land => {
            // Handled by land_ready/reconcile_landed each pass; nothing extra to force here.
        }
        SupervisorAction::AnswerTrustMenu => {
            // Pre-trust (dispatch --yolo) usually prevents this; if it appears, clear it
            // deterministically — pick the "trust" option (Down, Enter) — never a human keystroke.
            crate::proc::tmux_run(&["send-keys", "-t", &format!("={target}"), "Down"]);
            crate::proc::tmux_run(&["send-keys", "-t", &format!("={target}"), "Enter"]);
            st.record_recovery(
                id,
                state,
                action,
                "cleared trust-folder menu (Down, Enter)",
                now,
            );
            log_event(
                "orchestrate_trust_menu",
                &[("repo", &repo), ("branch", branch)],
            );
        }
        SupervisorAction::AnswerCommandApproval => {
            let option = command_approval_is_trusted_ship(st, id)
                .then(|| orchestrate::command_approval_option(&obs.tail))
                .flatten();
            if let Some(option) = option {
                // Orchestrated ship lanes are dispatched with --yolo. Pick the explicit
                // session-scoped allow option, never the one-shot approval or an arbitrary answer.
                let option = option.to_string();
                crate::proc::tmux_run(&["send-keys", "-t", &format!("={target}"), &option]);
                crate::proc::tmux_run(&["send-keys", "-t", &format!("={target}"), "Enter"]);
                st.record_recovery(
                    id,
                    state,
                    action,
                    &format!("allowed command for this trusted ship session ({option}, Enter)"),
                    now,
                );
                log_event(
                    "orchestrate_command_approval",
                    &[("repo", &repo), ("branch", branch), ("answer", "session")],
                );
            } else {
                // Blocking: only a human can grant this. `execute_action` runs on every supervise
                // pass (15s active), so the key names the *condition* rather than the moment —
                // the gate's cooldown is what finally makes this "once per transition".
                crate::toast::toast(
                    crate::toast::Class::Blocking,
                    &format!("orchestrate:{target}:command-approval"),
                    "orchestrate",
                    &format!("bn orchestrate: {target} needs command approval"),
                    obs.tail
                        .last()
                        .map(|s| s.as_str())
                        .unwrap_or("agent is waiting for command approval"),
                );
                st.record_recovery(
                    id,
                    state,
                    action,
                    "surfaced command approval for an operator",
                    now,
                );
            }
        }
        SupervisorAction::AnswerQuestion => {
            // A blocking question is a genuine decision — surface it rather than guess an answer
            // (auto-answering arbitrary prompts is unsafe). Toast once per transition: this arm
            // runs on every supervise pass, so the condition-shaped key plus the gate's cooldown
            // are what actually deliver the "once" this comment has always claimed.
            crate::toast::toast(
                crate::toast::Class::Blocking,
                &format!("orchestrate:{target}:question"),
                "orchestrate",
                &format!("bn orchestrate: {target} needs a decision"),
                obs.tail
                    .last()
                    .map(|s| s.as_str())
                    .unwrap_or("agent is waiting on a choice"),
            );
            st.record_recovery(
                id,
                state,
                action,
                "surfaced question-menu for a human decision",
                now,
            );
        }
        SupervisorAction::RelaunchToResume => {
            relaunch_to_resume(st, id, branch, state, obs, now);
        }
        SupervisorAction::SwitchHarness => {
            switch_limited_lane(st, id, branch, now);
        }
    }
}

/// Only coding/ship items carry orchestrate's `--yolo` trust contract. Research/scout lanes are
/// intentionally non-yolo, so their command approvals remain operator decisions.
fn command_approval_is_trusted_ship(st: &OrchestrateState, id: &str) -> bool {
    st.queue
        .iter()
        .find(|item| item.id == id)
        .is_some_and(|item| item.command_approvals_trusted && !item.is_scout())
}

/// How long to wait after a usage-limit harness switch before the same lane may be switched again
/// (#382) — long enough for the respawned harness to boot and clear the old limit banner from the
/// pane buffer, so a lingering "wait for limit to reset" line never causes a claude↔copilot
/// flip-flop. Only relevant to a limited pane; a healthy pane is never re-observed as limited.
const SWITCH_COOLDOWN_SECS: i64 = 300;

/// Whether a usage-limited lane is past its switch cooldown and may be flipped now (#382): the first
/// switch (`last_switch_at == 0`) always clears, and thereafter at least [`SWITCH_COOLDOWN_SECS`]
/// must have elapsed. Pure → unit-testable without a live fleet.
fn switch_cooldown_elapsed(last_switch_at: i64, now: i64) -> bool {
    now - last_switch_at >= SWITCH_COOLDOWN_SECS
}

/// Fall a usage-limited lane over to the other harness in place (#382) — the daemon's automatic
/// bias+fallback, so a Claude (or Copilot) lane that hit its cap continues on the other harness with
/// nobody at the keyboard. Reuses the proven `bn switch --in-place` machinery (writes the handoff,
/// grants folder trust, respawns the worktree's pane on the target harness seeded to read it), the
/// same path `bn fleet --fix` uses. The switch is skipped inside the [`SWITCH_COOLDOWN_SECS`] window
/// since the last one, so a just-respawned harness isn't flipped back while it's still booting.
fn switch_limited_lane(st: &mut OrchestrateState, id: &str, branch: &str, now: i64) {
    let (current, last_switch) = st
        .item_mut(id)
        .map(|i| (i.harness.clone(), i.last_switch_at))
        .unwrap_or((None, 0));
    // Cooldown gate: leave a lane we switched recently alone until its new harness has booted.
    if !switch_cooldown_elapsed(last_switch, now) {
        return;
    }
    let repo = st.repo.clone();
    let to = orchestrate::other_harness(current.as_deref().unwrap_or(orchestrate::DEFAULT_HARNESS));
    run_bn(
        &[
            "switch",
            to,
            "--repo",
            &repo,
            "--branch",
            branch,
            "--in-place",
            "--reason",
            "limit",
        ],
        None,
    );
    st.switch_harness(id, to, now);
    st.record_recovery(
        id,
        PaneState::Limited,
        SupervisorAction::SwitchHarness,
        &format!(
            "usage-limited on {} — switched to {to} in place",
            current.as_deref().unwrap_or("?")
        ),
        now,
    );
    // The system recovered by itself — it switched harness and kept working. Knowing is
    // curiosity, not a decision.
    crate::toast::toast(
        crate::toast::Class::Lifecycle,
        &format!("orchestrate:{repo}/{branch}:harness-switch"),
        "orchestrate",
        &format!("bn orchestrate: {repo}/{branch} hit a usage limit"),
        &format!("auto-switched to {to} to keep working"),
    );
    log_event(
        "orchestrate_switch",
        &[
            ("repo", &repo),
            ("branch", branch),
            ("from", current.as_deref().unwrap_or("?")),
            ("to", to),
        ],
    );
}

/// Relaunch-to-resume with exponential backoff, escalating soft→hard: save any WIP (`bn agent
/// recover`), then either **soft-resume** an alive-but-idle agent (`bn agent send --resume`) or
/// **hard-restart** a wedged one — kill its pane process and respawn a fresh harness (a cached
/// `ENOTFOUND` never clears via send-keys, #370 defect 1). Capped so a flapping outage doesn't
/// hot-loop; marks the item failed once the cap is hit (#370 defect 4). All policy (cap / backoff
/// window / soft-vs-hard) is decided by the pure, unit-tested [`orchestrate::plan_relaunch`].
fn relaunch_to_resume(
    st: &mut OrchestrateState,
    id: &str,
    branch: &str,
    state: PaneState,
    obs: &Observed,
    now: i64,
) {
    let backoff = Backoff::default();
    let (attempts, last, harness) = st
        .item_mut(id)
        .map(|i| (i.attempts, i.last_relaunch_at, i.harness.clone()))
        .unwrap_or((0, 0, None));
    // Re-dispatch under the lane's *current* harness so a usage-limit fallback (#382) survives a
    // hard restart — a switched lane relaunches on the harness it was moved to, not the run default.
    let harness = harness.unwrap_or_else(|| st.dispatch_harness().to_string());
    let kind =
        match orchestrate::plan_relaunch(state, attempts, last, now, obs.has_window, &backoff) {
            RelaunchDecision::GiveUp => {
                st.mark_failed(id, now);
                st.record_recovery(
                    id,
                    state,
                    SupervisorAction::RelaunchToResume,
                    "gave up after backoff cap — surfaced",
                    now,
                );
                // Terminal failure: the backoff cap is spent and the lane is not coming back
                // without you.
                crate::toast::toast(
                    crate::toast::Class::Blocking,
                    &format!("orchestrate:{}/{branch}:relaunch-gave-up", st.repo),
                    "orchestrate",
                    &format!("bn orchestrate: {}/{branch} needs you", st.repo),
                    &format!(
                        "relaunch gave up after {attempts} attempts ({})",
                        state.label()
                    ),
                );
                log_event(
                    "orchestrate_failed",
                    &[
                        ("repo", &st.repo),
                        ("branch", branch),
                        ("state", state.label()),
                    ],
                );
                return;
            }
            // Backoff window hasn't elapsed — do nothing this pass.
            RelaunchDecision::Wait => return,
            RelaunchDecision::Relaunch(kind) => kind,
        };

    let repo = st.repo.clone();
    let target = tmux_target(&repo, branch);
    // #378: a scout/ask lane relaunches read-only too (findings, no PR) — reuse its work-class.
    let scout = st
        .queue
        .iter()
        .find(|i| i.id == id)
        .is_some_and(|i| i.is_scout());
    // 1. Save unpushed work first — `bn agent recover --yes` pushes + opens a PR when there are
    //    commits ahead with no PR (idempotent no-op otherwise), so an idle-with-WIP death loses
    //    nothing and its PR then lands via the merge-queue. Do this before a hard restart too, so a
    //    kill never discards committed-but-unpushed work.
    if obs.has_wip {
        run_bn(
            &[
                "agent", "recover", "--repo", &repo, "--branch", branch, "--yes",
            ],
            None,
        );
    }
    // 2. Resume the agent, per the chosen kind:
    match kind {
        RelaunchKind::Soft => {
            // Alive-but-idle pane: nudge it to resume via the reliable `-p --resume` channel.
            run_bn(&["agent", "send", &target, RESUME_MSG, "--detach"], None);
        }
        RelaunchKind::Hard => {
            // Wedged / dead / stalled-idle process: kill the pane's process so a fresh harness can
            // take its place, then re-dispatch **fresh** (`fresh = true`) — a new, re-seeded session,
            // NOT a resume of the old idle/dead one (#383). A live window is killed first because
            // dispatch reuses — not relaunches — an existing window.
            if obs.has_window {
                crate::proc::tmux_run(&["kill-window", "-t", &format!("={target}")]);
            }
            let controller_id = st
                .queue
                .iter()
                .find(|item| item.id == id)
                .and_then(|item| item.controller_id.as_deref());
            sh_dispatch(
                &repo,
                &item_of(st, id),
                Some(branch),
                true,
                &harness,
                scout,
                controller_id,
            );
        }
    }
    let n = st.bump_attempts(id, now);
    st.record_recovery(
        id,
        state,
        SupervisorAction::RelaunchToResume,
        &format!(
            "{} #{n} after {} (wip={})",
            kind.label(),
            state.label(),
            obs.has_wip
        ),
        now,
    );
    log_event(
        "orchestrate_relaunch",
        &[
            ("repo", &repo),
            ("branch", branch),
            ("state", state.label()),
            ("kind", kind.label()),
            ("attempt", &n.to_string()),
        ],
    );
}

// ===========================================================================
// Dispatch: fill open concurrency slots
// ===========================================================================

#[derive(Clone, Copy)]
enum ClaimRoute<'a> {
    Local(&'a str),
    Remote,
    Worker(&'a str),
}

fn apply_claim_route(item: &mut orchestrate::QueueItem, route: ClaimRoute<'_>) {
    match route {
        ClaimRoute::Local(branch) => {
            item.branch = Some(branch.to_string());
            item.claimed_remote = false;
            item.node = None;
            item.worker_state = None;
        }
        ClaimRoute::Remote => {
            item.branch = None;
            item.claimed_remote = true;
            item.node = None;
            item.worker_state = None;
        }
        ClaimRoute::Worker(node) => {
            item.branch = None;
            item.claimed_remote = false;
            item.node = Some(node.to_string());
            item.worker_state = None;
        }
    }
}

/// Persist the dispatch transaction before invoking any external launcher. The returned IDs are
/// the authoritative values from disk: a pending retry keeps its original attempt identity even
/// when the caller proposed a fresh timestamp-derived value.
fn persist_dispatch_claim(
    run_path: &Path,
    st: &mut OrchestrateState,
    idx: usize,
    route: ClaimRoute<'_>,
    command_approvals_trusted: bool,
    now: i64,
) -> Option<(String, String)> {
    let repo = st.queue.get(idx)?.repo.clone();
    let id = st.queue.get(idx)?.id.clone();
    let proposed_request = dispatch_request_id(&repo, &id);
    let proposed_attempt = format!("{proposed_request}-{now}");
    let saved = orchestrate::mutate(run_path, |disk| {
        let disk_idx = disk
            .queue
            .iter()
            .position(|item| item.repo == repo && item.id == id)?;
        if !disk.claim_dispatch(disk_idx, &proposed_request, &proposed_attempt, now) {
            return None;
        }
        disk.queue[disk_idx].command_approvals_trusted = command_approvals_trusted;
        apply_claim_route(&mut disk.queue[disk_idx], route);
        Some((
            disk.queue[disk_idx].request_id.clone().unwrap(),
            disk.queue[disk_idx].attempt_id.clone().unwrap(),
        ))
    });
    let identity = match saved {
        Ok(Some(Some(identity))) => identity,
        Ok(Some(None)) | Ok(None) => return None,
        Err(e) => {
            eprintln!("bn orchestrate: could not persist dispatch claim for {id}: {e}");
            return None;
        }
    };
    if !st.claim_dispatch(idx, &identity.0, &identity.1, now) {
        return None;
    }
    st.queue[idx].command_approvals_trusted = command_approvals_trusted;
    apply_claim_route(&mut st.queue[idx], route);
    Some(identity)
}

fn persist_local_dispatch(
    run_path: &Path,
    st: &mut OrchestrateState,
    idx: usize,
    branch: &str,
    command_approvals_trusted: bool,
    now: i64,
) {
    let repo = st.queue[idx].repo.clone();
    let id = st.queue[idx].id.clone();
    let saved = orchestrate::mutate(run_path, |disk| {
        let Some(disk_idx) = disk
            .queue
            .iter()
            .position(|item| item.repo == repo && item.id == id)
        else {
            return false;
        };
        let reconciled = disk.reconcile_claim(disk_idx, true, Some(branch.to_string()), now);
        if reconciled {
            disk.queue[disk_idx].command_approvals_trusted = command_approvals_trusted;
        }
        reconciled
    });
    match saved {
        Ok(Some(true)) => {
            st.reconcile_claim(idx, true, Some(branch.to_string()), now);
            st.queue[idx].command_approvals_trusted = command_approvals_trusted;
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!(
                "bn orchestrate: launched {id} but could not finalize its dispatch claim: {e}"
            );
        }
    }
}

fn persist_remote_dispatch(run_path: &Path, st: &mut OrchestrateState, idx: usize, now: i64) {
    let repo = st.queue[idx].repo.clone();
    let id = st.queue[idx].id.clone();
    let saved = orchestrate::mutate(run_path, |disk| {
        let Some(disk_idx) = disk
            .queue
            .iter()
            .position(|item| item.repo == repo && item.id == id)
        else {
            return false;
        };
        disk.mark_dispatched_remote(disk_idx, now);
        true
    });
    match saved {
        Ok(Some(true)) => st.mark_dispatched_remote(idx, now),
        Ok(_) => {}
        Err(e) => {
            eprintln!(
                "bn orchestrate: launched remote {id} but could not finalize its dispatch claim: {e}"
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn persist_worker_dispatch(
    run_path: &Path,
    st: &mut OrchestrateState,
    idx: usize,
    node_id: &str,
    request_id: &str,
    attempt_id: &str,
    worker_state: &str,
    now: i64,
) {
    let repo = st.queue[idx].repo.clone();
    let id = st.queue[idx].id.clone();
    let saved = orchestrate::mutate(run_path, |disk| {
        let Some(disk_idx) = disk
            .queue
            .iter()
            .position(|item| item.repo == repo && item.id == id)
        else {
            return false;
        };
        disk.mark_dispatched_worker(disk_idx, node_id, request_id, attempt_id, None, now);
        disk.set_worker_state_at(disk_idx, worker_state, now);
        true
    });
    match saved {
        Ok(Some(true)) => {
            st.mark_dispatched_worker(idx, node_id, request_id, attempt_id, None, now);
            st.set_worker_state_at(idx, worker_state, now);
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!(
                "bn orchestrate: launched worker attempt {attempt_id} but could not finalize its claim: {e}"
            );
        }
    }
}

fn persist_failed_dispatch(run_path: &Path, st: &mut OrchestrateState, idx: usize, now: i64) {
    let repo = st.queue[idx].repo.clone();
    let id = st.queue[idx].id.clone();
    let attempt_id = st.queue[idx].attempt_id.clone();
    let saved = orchestrate::mutate(run_path, |disk| {
        let Some(disk_idx) = disk
            .queue
            .iter()
            .position(|item| item.repo == repo && item.id == id)
        else {
            return false;
        };
        let disk_item = &disk.queue[disk_idx];
        if disk_item.status != ItemStatus::Queued
            || disk_item.dispatch_phase != orchestrate::DispatchPhase::Claimed
            || disk_item.attempt_id != attempt_id
        {
            return false;
        }
        disk.mark_failed_at(disk_idx, now);
        true
    });
    match saved {
        Ok(Some(true)) => st.mark_failed_at(idx, now),
        Ok(_) => {}
        Err(e) => {
            eprintln!("bn orchestrate: could not finalize failed dispatch claim for {id}: {e}");
        }
    }
}

fn release_dispatch_claim(run_path: &Path, st: &mut OrchestrateState, idx: usize, now: i64) {
    let repo = st.queue[idx].repo.clone();
    let id = st.queue[idx].id.clone();
    let saved = orchestrate::mutate(run_path, |disk| {
        let Some(disk_idx) = disk
            .queue
            .iter()
            .position(|item| item.repo == repo && item.id == id)
        else {
            return false;
        };
        disk.reconcile_claim(disk_idx, false, None, now)
    });
    match saved {
        Ok(Some(true)) => {
            st.reconcile_claim(idx, false, None, now);
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("bn orchestrate: could not release dispatch claim for {id}: {e}");
        }
    }
}

/// Dispatch queued items this pass, honoring the resource-aware elastic-concurrency scheduler
/// (#377). The CLI captures the live machine + budget signals ([`observe_resources`]) and the
/// configured caps ([`build_caps`]) and hands them to the pure [`bn_core::scheduler::pick`], which
/// composes with #376 conflict-holding to decide which queued items dispatch **locally** (up to the
/// resource-aware cap), which fan out to a **remote** runner (occupying no local slot), and which
/// binding cap — if any — is *pausing* the rest (items stay pending; the reason is recorded on the
/// state and emitted as an event). Never fails an item on budget: exhaustion pauses, it doesn't
/// give up. Each local dispatch launches a self-gating agent seeded from the issue, under the run's
/// configured harness (`bn orchestrate --agent`, #382).
fn fill_dispatch_slots(
    cfg: &Config,
    run_path: &Path,
    st: &mut OrchestrateState,
    now: i64,
    refill: &RefillResult,
) {
    refresh_node_registry(cfg, st);
    refresh_repo_placement_policies(cfg, st);
    let caps = build_caps(cfg, st);
    let obs = observe_resources(cfg, st, now);
    let plan = bn_core::scheduler::pick(st, &caps, &obs);
    let bind_label = plan.bind.map(|b| b.label());
    let paused = plan.paused();
    let bind = plan.bind;
    let mut attempted = 0usize;
    let mut launched = 0usize;

    let harness = st.dispatch_harness().to_string();
    // Iterate the placements (idx → chosen node) rather than `plan.local` alone, so an item placed
    // on a remote SSH worker dispatches over SSH (#729) while a local placement keeps the existing
    // in-process launch. `plan.local` and `plan.placements` are parallel (same order); placements
    // carries the node each landed on.
    for (idx, target) in plan.placements {
        attempted += 1;
        let item = st.queue[idx].item.clone();
        let id = st.queue[idx].id.clone();
        let scout = st.queue[idx].is_scout();
        if let bn_core::backlog::ExecutionTarget::SshHost(alias) = &target {
            if persist_dispatch_claim(run_path, st, idx, ClaimRoute::Worker(alias), false, now)
                .is_none()
            {
                continue;
            }
            if dispatch_worker_item(cfg, run_path, st, idx, alias, now) {
                launched += 1;
            }
            continue;
        }
        let claimed_branch = format!("dispatch/{id}");
        if persist_dispatch_claim(
            run_path,
            st,
            idx,
            ClaimRoute::Local(&claimed_branch),
            !scout,
            now,
        )
        .is_none()
        {
            continue;
        }
        let branch = sh_dispatch(
            &st.repo,
            &item,
            Some(&claimed_branch),
            false,
            &harness,
            scout,
            st.queue[idx].controller_id.as_deref(),
        );
        match branch {
            Some(b) => {
                launched += 1;
                persist_local_dispatch(run_path, st, idx, &b, !scout, now);
                st.set_harness(idx, &harness, now);
                st.record_recovery(
                    &id,
                    PaneState::Working,
                    SupervisorAction::Leave,
                    "dispatched",
                    now,
                );
                log_event(
                    "orchestrate_dispatch",
                    &[("repo", &st.repo), ("item", &item), ("branch", &b)],
                );
            }
            None => {
                // Dispatch failed (bad item, git error) — mark failed so the loop makes progress
                // instead of retrying the same broken item forever.
                persist_failed_dispatch(run_path, st, idx, now);
                st.record_recovery(
                    &id,
                    PaneState::Dead,
                    SupervisorAction::RelaunchToResume,
                    "dispatch failed — surfaced",
                    now,
                );
                log_event(
                    "orchestrate_dispatch_failed",
                    &[("repo", &st.repo), ("item", &item)],
                );
            }
        }
    }

    // #729: record the items the scheduler could not place this pass (no READY node satisfied the
    // repo's required capabilities, per #728) — fail-loud so a doomed dispatch (e.g. mt5 onto a
    // Linux node) is refused with a visible reason instead of hanging on an ineligible node.
    for (idx, reason) in &plan.unplaced {
        if let Some(it) = st.queue.get(*idx) {
            log_event(
                "orchestrate_unplaced",
                &[("repo", &st.repo), ("item", &it.item), ("reason", reason)],
            );
        }
    }

    // #377: fan out the remote-substrate / overflow items via `bn dispatch --remote` — a
    // GitHub-hosted run that occupies no local slot and is reconciled by its PR/issue completion.
    for idx in plan.remote {
        attempted += 1;
        if persist_dispatch_claim(run_path, st, idx, ClaimRoute::Remote, false, now).is_none() {
            continue;
        }
        if dispatch_remote_item(run_path, st, idx, now) {
            launched += 1;
        }
    }

    // #729: refresh the liveness of in-flight remote worker attempts (poll `bn worker status` over
    // SSH); an unreachable node marks the attempt UNKNOWN, which blocks its redispatch.
    poll_worker_liveness(cfg, st, now);

    // #377: record why local dispatch was capped/paused this pass so `bn orchestrate status` shows
    // the reason without re-observing the machine, and emit a `paused` event for a resource/budget
    // pause (not for plain local-cap fullness). Only queued items still waiting warrant the event.
    if paused && st.counts().queued > 0 {
        if let Some(bind) = bind {
            log_event(
                "orchestrate_paused",
                &[
                    ("repo", &st.repo),
                    ("cap", bind.label()),
                    ("reason", bind.reason()),
                ],
            );
        }
    }
    st.set_dispatch_bind(bind_label, now);

    // #376: refresh the held markers from the post-dispatch in-flight set and log each *new* hold
    // once (not every poll), so `bn orchestrate status`/watch can show "held: conflicts with #NNN".
    for (id, holder) in st.refresh_holds(now) {
        log_event(
            "orchestrate_held",
            &[("repo", &st.repo), ("item", &id), ("held_by", &holder)],
        );
    }
    let reason = no_dispatch_reason(
        st,
        &plan.unplaced,
        bind,
        refill.reason.as_deref(),
        attempted,
        launched,
    );
    st.set_dispatch_reason(reason.as_deref(), now);
    let decision = dispatch_decision(
        st,
        DispatchTick {
            caps: &caps,
            obs: &obs,
            unplaced: &plan.unplaced,
            backlog_skips: &refill.skipped,
            bind,
            reason,
            attempted,
            launched,
            now,
        },
    );
    st.set_dispatch_decision(decision, now);
}

struct DispatchTick<'a> {
    caps: &'a bn_core::scheduler::Caps,
    obs: &'a bn_core::scheduler::Observed,
    unplaced: &'a [(usize, String)],
    backlog_skips: &'a [BacklogSkip],
    bind: Option<bn_core::scheduler::Bind>,
    reason: Option<String>,
    attempted: usize,
    launched: usize,
    now: i64,
}

fn dispatch_decision(
    st: &OrchestrateState,
    tick: DispatchTick<'_>,
) -> orchestrate::DispatchDecision {
    let previous_waits: std::collections::HashMap<&str, i64> = st
        .dispatch_decision
        .as_ref()
        .into_iter()
        .flat_map(|decision| decision.skipped.iter())
        .map(|candidate| (candidate.id.as_str(), candidate.waiting_since))
        .collect();
    let unplaced: std::collections::HashMap<usize, &str> = tick
        .unplaced
        .iter()
        .map(|(idx, reason)| (*idx, reason.as_str()))
        .collect();
    let typed = st.typed_lanes();
    let local_total = st.local_node_in_flight_class(WorkClass::Coding)
        + st.local_node_in_flight_class(WorkClass::Research);
    let mut skipped: Vec<orchestrate::SkippedCandidate> = st
        .queue
        .iter()
        .enumerate()
        .filter(|(_, item)| item.status == ItemStatus::Queued)
        .map(|(idx, item)| {
            let held_reason = item
                .held_by
                .as_deref()
                .map(|holder| format!("conflicts with #{holder}"));
            let in_flight = if typed {
                st.local_node_in_flight_class(item.work_class)
            } else {
                local_total
            };
            let gate_reason =
                bn_core::scheduler::admit(tick.caps, tick.obs, item.work_class, in_flight)
                    .map(|gate| gate.reason().to_string());
            let reason = held_reason
                .or_else(|| unplaced.get(&idx).map(|reason| (*reason).to_string()))
                .or(gate_reason)
                .unwrap_or_else(|| "not selected this tick".to_string());
            orchestrate::SkippedCandidate {
                id: item.id.clone(),
                item: item.item.clone(),
                reason,
                held_by: item.held_by.clone(),
                waiting_since: previous_waits
                    .get(item.id.as_str())
                    .copied()
                    .unwrap_or(tick.now),
            }
        })
        .collect();
    let queued_ids: std::collections::HashSet<String> = skipped
        .iter()
        .map(|candidate| candidate.id.clone())
        .collect();
    skipped.extend(
        tick.backlog_skips
            .iter()
            .filter(|candidate| !queued_ids.contains(candidate.id.as_str()))
            .map(|candidate| orchestrate::SkippedCandidate {
                id: candidate.id.clone(),
                item: candidate.item.clone(),
                reason: candidate.reason.clone(),
                held_by: None,
                waiting_since: previous_waits
                    .get(candidate.id.as_str())
                    .copied()
                    .unwrap_or(tick.now),
            }),
    );
    let lane = |name: &str, class: WorkClass, cap: usize| {
        let in_flight = st.local_node_in_flight_class(class);
        let queued = st
            .queue
            .iter()
            .filter(|item| item.status == ItemStatus::Queued && item.work_class == class)
            .count();
        let available = cap.saturating_sub(in_flight);
        orchestrate::LaneCapacity {
            lane: name.to_string(),
            cap,
            in_flight,
            queued,
            available,
            saturated: queued > 0 && available == 0,
        }
    };
    let lanes = if let Some(research_cap) = st.research_cap() {
        vec![
            lane("coding", WorkClass::Coding, st.coding_cap()),
            lane("research", WorkClass::Research, research_cap),
        ]
    } else {
        let cap = st.coding_cap();
        let in_flight = local_total;
        let queued = st
            .queue
            .iter()
            .filter(|item| item.status == ItemStatus::Queued)
            .count();
        vec![orchestrate::LaneCapacity {
            lane: "shared".to_string(),
            cap,
            in_flight,
            queued,
            available: cap.saturating_sub(in_flight),
            saturated: queued > 0 && in_flight >= cap,
        }]
    };
    orchestrate::DispatchDecision {
        at: tick.now,
        outcome: if tick.launched > 0 {
            "dispatched"
        } else if tick.attempted > 0 || !skipped.is_empty() {
            "blocked"
        } else if matches!(
            tick.reason.as_deref(),
            None | Some("backlog empty") | Some("all work is already in flight")
        ) {
            "idle"
        } else {
            "blocked"
        }
        .to_string(),
        gate: tick.bind.map(|gate| gate.label().to_string()),
        reason: tick.reason,
        attempted: tick.attempted,
        launched: tick.launched,
        lanes,
        skipped,
    }
}

fn no_dispatch_reason(
    st: &OrchestrateState,
    unplaced: &[(usize, String)],
    bind: Option<bn_core::scheduler::Bind>,
    backlog_reason: Option<&str>,
    attempted: usize,
    launched: usize,
) -> Option<String> {
    if launched > 0 {
        return None;
    }
    if attempted > 0 {
        return Some("dispatch attempt failed".to_string());
    }
    if let Some((_, reason)) = unplaced.first() {
        return Some(format!("blocked — {reason}"));
    }
    if let Some(bind) = bind {
        return Some(bind.reason().to_string());
    }
    let counts = st.counts();
    if counts.queued > 0
        && st
            .queue
            .iter()
            .filter(|i| i.status == ItemStatus::Queued)
            .all(|i| i.held_by.is_some())
    {
        return Some("all queued items are held by in-flight conflicts".to_string());
    }
    if let Some(reason) = backlog_reason.filter(|reason| *reason != "backlog empty") {
        return Some(reason.to_string());
    }
    if counts.queued == 0 && counts.in_flight > 0 {
        return Some("all work is already in flight".to_string());
    }
    if let Some(reason) = backlog_reason {
        return Some(reason.to_string());
    }
    if counts.queued == 0 {
        return Some("backlog empty".to_string());
    }
    Some("no dispatchable candidates".to_string())
}

/// Fan out the queue item at `idx` to a GitHub-hosted runner (#377): trigger `bn dispatch --remote
/// --issue <id>` in a child process (isolated — a failure never takes down the daemon), and on
/// success mark it dispatched-remote (no local slot; reconciled by PR/issue completion). A trigger
/// failure is ambiguous (GitHub may have accepted before the response was lost), so the claim stays
/// reserved until reconciliation observes the stable token or the visibility grace expires.
fn dispatch_remote_item(run_path: &Path, st: &mut OrchestrateState, idx: usize, now: i64) -> bool {
    let item = st.queue[idx].item.clone();
    let id = st.queue[idx].id.clone();
    let request_id = st.queue[idx].request_id.clone().unwrap_or_default();
    let attempt_id = st.queue[idx].attempt_id.clone().unwrap_or_default();
    if sh_dispatch_remote(&st.repo, &id, &item, &request_id, &attempt_id) {
        persist_remote_dispatch(run_path, st, idx, now);
        st.record_recovery(
            &id,
            PaneState::Working,
            SupervisorAction::Leave,
            "dispatched remote (github-hosted)",
            now,
        );
        log_event(
            "orchestrate_dispatch_remote",
            &[("repo", &st.repo), ("item", &item), ("id", &id)],
        );
        return true;
    }
    st.record_recovery(
        &id,
        PaneState::Dead,
        SupervisorAction::RelaunchToResume,
        "remote dispatch outcome unknown — reconciling stable attempt",
        now,
    );
    log_event(
        "orchestrate_dispatch_remote_unknown",
        &[
            ("repo", &st.repo),
            ("item", &item),
            ("attempt_id", &attempt_id),
        ],
    );
    false
}

// ===========================================================================
// #377 resource + budget observation — the IO half of the pure scheduler
// ===========================================================================

/// Refresh the run's node registry from config and run the shared SSH transport preflight on each
/// node (#729). The declarative `[nodes]` table is the source of truth for identity/labels/RAM; the
/// preflight is what promotes a node to placement-eligible: a local node is always `Ready` (it's the
/// box the daemon runs on), while an SSH node is probed through the same daemon-owned ControlMaster
/// profile used for dispatch and status polling. A failed probe or mux setup marks it `Offline`
/// (never placed on), leaving local execution available. An empty `[nodes]` table clears the
/// registry, restoring single-node behavior (placement always lands `Local`).
fn refresh_node_registry(cfg: &Config, st: &mut OrchestrateState) {
    if cfg.nodes.is_empty() {
        st.nodes.clear();
        return;
    }
    st.nodes = cfg
        .nodes
        .iter()
        .map(|n| {
            let mut n = n.clone();
            match &n.target {
                bn_core::backlog::ExecutionTarget::Local
                | bn_core::backlog::ExecutionTarget::GitHubRunner => {
                    n.status = bn_core::hosts::NodeStatus::Ready;
                    if n.target.is_local() && n.owned_repos.is_empty() {
                        n.owned_repos = cfg.orchestrate_owned_repos.clone();
                    }
                }
                bn_core::backlog::ExecutionTarget::SshHost(alias) => {
                    let status = crate::connect::worker_status(cfg, alias);
                    apply_worker_snapshot(&mut n, status.as_ref());
                }
            }
            n
        })
        .collect();
}

fn apply_worker_snapshot(
    node: &mut bn_core::hosts::NodeRecord,
    status: Option<&serde_json::Value>,
) {
    let Some(status) = status else {
        node.status = bn_core::hosts::NodeStatus::Offline;
        node.capacity = None;
        node.active_attempts = 0;
        return;
    };
    node.status = bn_core::hosts::NodeStatus::Ready;
    node.capacity = status
        .get("capacity")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
    node.active_attempts = status
        .get("attempts")
        .and_then(serde_json::Value::as_array)
        .map(|attempts| {
            attempts
                .iter()
                .filter(|attempt| {
                    matches!(
                        attempt.get("state").and_then(serde_json::Value::as_str),
                        Some("QUEUED" | "RUNNING")
                    )
                })
                .count()
        })
        .unwrap_or_default();
}



/// Refresh each queued item's declarative repo→node capability policy before placement (#728).
/// Doing this every pass makes config changes take effect without rewriting backlog entries or
/// recreating the run state.
fn refresh_repo_placement_policies(cfg: &Config, st: &mut OrchestrateState) {
    for item in &mut st.queue {
        let policy = cfg.repo_placement_policy(&item.repo);
        item.required_capabilities = policy.requires;
        item.excluded_capabilities = policy.excludes;
    }
}



/// Build the resource-aware scheduler caps for this run (#377/#385): the typed concurrency lanes —
/// `max_local` is the coding-lane cap and `research` the (optional) research-lane cap, both honoring
/// `--solo` ([`OrchestrateState::coding_cap`]/[`research_cap`](OrchestrateState::research_cap)) — plus
/// the machine/budget guards from the `[orchestrate]` config (all off by default → the pre-#377
/// fixed-concurrency behavior). `plan_gated` reflects the dispatch plan-review default, so an `auto`
/// item isn't fanned out to a runner that can't do the #349 handshake.
fn build_caps(cfg: &Config, st: &OrchestrateState) -> bn_core::scheduler::Caps {
    bn_core::scheduler::Caps {
        max_local: st.coding_cap(),
        research: st.research_cap(),
        min_free_ram_mb: cfg.orchestrate_min_free_ram_mb,
        max_loadavg: cfg.orchestrate_max_loadavg,
        max_swap_pct: cfg.orchestrate_max_swap_pct,
        tokens_per_hour: cfg.orchestrate_tokens_per_hour,
        daily_cost_usd: cfg.orchestrate_daily_cost_usd,
        daily_cost_usd_coding: cfg.orchestrate_daily_cost_usd_coding,
        daily_cost_usd_research: cfg.orchestrate_daily_cost_usd_research,
        plan_gated: cfg.dispatch_plan_review,
        remote_overflow: cfg.orchestrate_remote_overflow,
        local_owned: bn_core::scheduler::repo_owned(&cfg.orchestrate_owned_repos, &st.repo),
    }
}

/// Capture the live machine + budget signals the pure scheduler folds against (#377). Each signal
/// is observed **only when its cap is configured**, so an unconfigured run (every guard off) does
/// exactly zero extra work — the pre-#377 fast path. Best-effort: an unreadable load / RAM signal
/// is left `None` so its guard fails **open** (never blocking dispatch on an unknown). Token
/// consumption is this repo's trailing-hour fold from the shipped #309/#267 usage records **plus**
/// the whole-fleet live output tokens (a running agent is spending the shared account's rate budget
/// *now*, and its `agent_session` record only lands at shutdown). Cost is today's trailing-24h
/// API-equiv `$` benchmark; the per-lane spend (#381 GAP 5) is that same day-window folded
/// by category, attributed to the coding / research lane — observed only when a per-class cap is
/// set. Reuses the existing cost surface — no new meter.
fn observe_resources(
    cfg: &Config,
    st: &OrchestrateState,
    now: i64,
) -> bn_core::scheduler::Observed {
    // Read host metrics from the single cross-platform `bn_core::machine` reader — the same source
    // the health dashboard uses, so the admission gate and dashboard can't disagree (and the gate
    // is no longer blind on macOS). One `read_meminfo` covers both the RAM and swap guards.
    let loadavg = cfg
        .orchestrate_max_loadavg
        .and_then(|_| bn_core::machine::read_loadavg1());
    let mem = (cfg.orchestrate_min_free_ram_mb.is_some() || cfg.orchestrate_max_swap_pct.is_some())
        .then(bn_core::machine::read_meminfo)
        .unwrap_or_default();
    let free_ram_mb = cfg.orchestrate_min_free_ram_mb.and(mem.available_mb);
    let swap_pct = cfg
        .orchestrate_max_swap_pct
        .and_then(|_| bn_core::machine::swap_used_pct(&mem));
    let tokens_this_hour = if cfg.orchestrate_tokens_per_hour.is_some() {
        let hour =
            bn_core::cost::rollup(cfg, &bn_core::cost::CostFilter::secs(3600, &st.repo, now));
        let live = bn_core::cost::live_rollup(cfg);
        (hour.totals.input_tokens + hour.totals.output_tokens + live.output_tokens).max(0) as u64
    } else {
        0
    };
    let cost_today_usd = if cfg.orchestrate_daily_cost_usd.is_some() {
        cost_today(cfg, &st.repo, now).1.unwrap_or(0.0)
    } else {
        0.0
    };
    // Per-lane spend (#381 GAP 5): fold today's cost-by-category once when either per-class cap is
    // set, then attribute each lane its own category's `$` — the figure the pure scheduler checks
    // against the lane's cap. Unconfigured lanes stay `0.0` (no extra work when no per-class cap).
    let (cost_today_coding_usd, cost_today_research_usd) = if cfg
        .orchestrate_daily_cost_usd_coding
        .or(cfg.orchestrate_daily_cost_usd_research)
        .is_some()
    {
        let by_cat = cost_by_category_today(cfg, &st.repo, now);
        let lane_cost = |wc: bn_core::cost::WorkClass| {
            by_cat
                .iter()
                .find(|r| r.key == wc.as_str())
                .and_then(|r| r.cost_usd)
                .unwrap_or(0.0)
        };
        (
            lane_cost(bn_core::cost::WorkClass::Coding),
            lane_cost(bn_core::cost::WorkClass::Research),
        )
    } else {
        (0.0, 0.0)
    };
    bn_core::scheduler::Observed {
        loadavg,
        free_ram_mb,
        swap_pct,
        tokens_this_hour,
        cost_today_usd,
        cost_today_coding_usd,
        cost_today_research_usd,
    }
}

// ===========================================================================
// `bn orchestrate doctor`
// ===========================================================================

/// Read-only health check for the filesystem anchors and persisted fleet state that can silently
/// wedge every dispatch. Informational findings (healthy holds/saturation) stay exit-zero; warnings
/// and errors return false so shell automation can gate on the result.
pub(crate) fn orchestrate_doctor(cfg: &Config, repo: Option<&str>, json: bool) -> bool {
    let findings = collect_orchestrate_doctor(cfg, repo, bn_core::util::now_epoch_secs());
    let healthy = orchestrate_doctor_healthy(&findings);

    if json {
        let data = serde_json::json!({
            "healthy": healthy,
            "findings": findings,
        });
        println!(
            "{}",
            Envelope::success("orchestrate-doctor", data).to_json()
        );
        return healthy;
    }

    if findings.is_empty() {
        println!("orchestrate doctor: healthy ✓");
        return true;
    }

    let errors = findings
        .iter()
        .filter(|finding| finding.severity == OrchestrateDoctorSeverity::Error)
        .count();
    let warnings = findings
        .iter()
        .filter(|finding| finding.severity == OrchestrateDoctorSeverity::Warning)
        .count();
    let infos = findings.len().saturating_sub(errors + warnings);
    println!(
        "orchestrate doctor: {} — {errors} error(s) · {warnings} warning(s) · {infos} info",
        if healthy { "healthy" } else { "unhealthy" }
    );
    for finding in &findings {
        let mark = match finding.severity {
            OrchestrateDoctorSeverity::Info => "·",
            OrchestrateDoctorSeverity::Warning => "!",
            OrchestrateDoctorSeverity::Error => "✗",
        };
        let repo = finding
            .repo
            .as_deref()
            .map(|repo| format!(" [{repo}]"))
            .unwrap_or_default();
        println!(
            "  {mark} {}{}: {}",
            finding.severity.label(),
            repo,
            finding.detail
        );
        println!("    fix: {}", finding.fix);
    }
    healthy
}

// ===========================================================================
// `bn orchestrate status`
// ===========================================================================

/// Print structured orchestrator events from the shared machine-local event log.
pub(crate) fn orchestrate_logs(
    repo: Option<&str>,
    level: Option<EventLevel>,
    limit: usize,
    json: bool,
) {
    let mut events: Vec<_> = bn_core::events::orchestrator_events()
        .into_iter()
        .filter(|event| {
            repo.is_none_or(|want| {
                event.repo.as_deref() == Some(want) || event.repo.as_deref() == Some("*")
            })
        })
        .filter(|event| level.is_none_or(|want| event.level == want))
        .collect();
    events.sort_by_key(|event| std::cmp::Reverse(event.ts));
    events.truncate(limit);

    if json {
        println!(
            "{}",
            Envelope::success("orchestrate-logs", serde_json::json!({ "events": events }))
                .to_json()
        );
        return;
    }
    if events.is_empty() {
        println!("orchestrate logs: no matching events");
        return;
    }
    let now = bn_core::util::now_epoch_secs();
    for event in events {
        let repo = event
            .repo
            .as_deref()
            .map(|value| format!(" repo={value}"))
            .unwrap_or_default();
        let item = event
            .item_id
            .as_deref()
            .map(|value| format!(" item={value}"))
            .unwrap_or_default();
        let error = event
            .error
            .as_deref()
            .map(|value| format!(" — {value}"))
            .unwrap_or_default();
        println!(
            "{:<5} {:>6} {}{}{}{}",
            event.level,
            crate::cmd::cron::ago((now - event.ts).max(0)),
            event.reason,
            repo,
            item,
            error
        );
    }
}

/// Show only active orchestrator degradations. Persisted warning/recovery transitions cover daemon
/// and backlog failures; tmux reachability is also probed live so status remains truthful before the
/// next supervisor tick records it.
pub(crate) fn orchestrate_degraded_status(repo: Option<&str>, json: bool) {
    let events = bn_core::events::orchestrator_events();
    let mut degraded = bn_core::events::active_orchestrator_degradations(&events, None);
    degraded.retain(|event| {
        repo.is_none_or(|want| {
            event.repo.as_deref() == Some(want) || event.repo.as_deref() == Some("*")
        })
    });

    if list_windows().is_none()
        && !degraded
            .iter()
            .any(|event| event.reason == "tmux-unreachable")
    {
        degraded.push(bn_core::events::OrchestratorEvent {
            kind: "orchestrate_degradation".into(),
            ts: bn_core::util::now_epoch_secs(),
            level: EventLevel::Warn,
            reason: "tmux-unreachable".into(),
            repo: repo.map(str::to_string),
            item_id: None,
            error: None,
            active: Some(true),
            fields: serde_json::Map::new(),
        });
    }

    if json {
        println!(
            "{}",
            Envelope::success(
                "orchestrate-degraded",
                serde_json::json!({ "degradations": degraded })
            )
            .to_json()
        );
        return;
    }
    if degraded.is_empty() {
        println!("orchestrate: no active degradations");
        return;
    }
    println!("orchestrate: {} active degradation(s)", degraded.len());
    let now = bn_core::util::now_epoch_secs();
    for event in degraded {
        let repo = event.repo.as_deref().unwrap_or("machine");
        let error = event
            .error
            .as_deref()
            .map(|value| format!(" — {value}"))
            .unwrap_or_default();
        println!(
            "  warn {} · {} · {}{}",
            repo,
            event.reason,
            crate::cmd::cron::ago((now - event.ts).max(0)),
            error
        );
    }
}

/// Render runs: bare lists **every** run on the machine (#395 — status must show all live runs);
/// `--repo` (or a single existing run) shows one in full — queue / in-flight / merged / recovery,
/// the observable replacement for the hand-typed "one terse line per agent".
pub(crate) fn orchestrate_status(cfg: &Config, repo: Option<&str>, why: bool, json: bool) {
    ensure_migrated(true);
    let now = bn_core::util::now_epoch_secs();
    let runs: Vec<(PathBuf, OrchestrateState)> = resolve_runs(repo);
    // #375: the durable backlog is separate from the runs, so load it and surface its depth — both
    // per-run (the pending waiting to be pulled into that run) and for repos with a backlog but no
    // live run (added via `bn orchestrate add`, not yet draining).
    let backlog = backlog::load(&backlog::backlog_path());
    let backlog_depths: Vec<(String, usize)> = backlog
        .repos_with_pending()
        .into_iter()
        .filter(|r| repo.is_none_or(|want| want == r))
        .map(|r| {
            let n = backlog.pending_depth(&r);
            (r, n)
        })
        .collect();
    let run_repos: std::collections::HashSet<&str> =
        runs.iter().map(|(_, st)| st.repo.as_str()).collect();
    let orphan_backlog: Vec<&(String, usize)> = backlog_depths
        .iter()
        .filter(|(r, _)| !run_repos.contains(r.as_str()))
        .collect();

    if runs.is_empty() && orphan_backlog.is_empty() {
        if json {
            println!(
                "{}",
                Envelope::success(
                    "orchestrate",
                    serde_json::json!({ "runs": [], "backlog": [] })
                )
                .to_json()
            );
        } else if let Some(r) = repo {
            println!(
                "orchestrate: no run for {r} (start one with `bn orchestrate --queue <issues…> --repo {r}`)"
            );
        } else {
            println!("orchestrate: no run (start one with `bn orchestrate --queue <issues…>`)");
        }
        return;
    }
    if json {
        let data = serde_json::json!({
            "runs": runs
                .iter()
                .map(|(_, st)| run_json(cfg, st, backlog.pending_depth(&st.repo), now, why))
                .collect::<Vec<_>>(),
            "backlog": backlog_depths
                .iter()
                .map(|(r, n)| serde_json::json!({ "repo": r, "pending": n }))
                .collect::<Vec<_>>(),
        });
        println!("{}", Envelope::success("orchestrate", data).to_json());
        return;
    }
    if runs.len() > 1 {
        println!("orchestrate: {} runs", runs.len());
    }
    for (i, (_, st)) in runs.iter().enumerate() {
        if i > 0 {
            println!();
        }
        print_run(cfg, st, backlog.pending_depth(&st.repo), now, why);
    }
    if !orphan_backlog.is_empty() {
        if !runs.is_empty() {
            println!();
        }
        println!("backlog (no active run):");
        for (r, n) in &orphan_backlog {
            println!(
                "  {r} — {n} pending (start draining: bn orchestrate add --start --repo {r} …)"
            );
        }
    }
    if runs.len() > 1 {
        println!("  (target one: bn orchestrate status --repo <name> · stop --repo <name>)");
    }
}

/// The JSON payload for one run (the pre-#395 single-run shape, now one element of `runs`).
/// Delegates to [`bn_core::payload::orchestrate_status_data`] so the CLI, the web
/// `GET /api/orchestrate` endpoint, and MCP share one byte-identical core shape (#389); on top the
/// CLI adds the `backlog` pending depth (#375) and the #379 monitoring surface — the `rollups`
/// block (throughput / ETA / cost) and the per-item usage fields (`age_secs`, live `tokens`).
fn run_json(
    cfg: &Config,
    st: &OrchestrateState,
    backlog_depth: usize,
    now: i64,
    why: bool,
) -> serde_json::Value {
    let reviews = bn_core::plan_review::open_reviews();
    let mut v = bn_core::payload::orchestrate_status_data(st, &reviews);
    if let Some(obj) = v.as_object_mut() {
        obj.insert("backlog".to_string(), serde_json::json!(backlog_depth));
        obj.insert(
            "rollups".to_string(),
            orchestrate_rollups(cfg, st, backlog_depth, now),
        );
        obj.insert(
            "cost_by_category".to_string(),
            serde_json::to_value(cost_by_category_today(cfg, &st.repo, now))
                .unwrap_or(serde_json::Value::Null),
        );
        // #380: recent completed outcomes read back from run-history.jsonl, so a consumer sees
        // merged/failed results that have been evicted from the live queue or survived an overwrite.
        let history = orchestrate::summarize_history(&orchestrate::read_history(), &st.repo, 10);
        obj.insert(
            "history".to_string(),
            serde_json::to_value(&history).unwrap_or(serde_json::Value::Null),
        );
        if why {
            obj.insert("why".to_string(), dispatch_why_data(st, now));
        }
    }
    enrich_queue_items(cfg, st, &mut v, now);
    v
}

/// Build the #379 monitoring `rollups` block for one run: the pure throughput / ETA / recovery math
/// ([`orchestrate::Rollups::compute`] over the lifecycle event log) plus today's cost burn from the
/// #381 `cost::rollup` surface (disk-bound, so layered here rather than in the pure core). Queue
/// depth folds the run-queued count with the durable backlog's pending depth (#375).
fn orchestrate_rollups(
    cfg: &Config,
    st: &OrchestrateState,
    backlog_depth: usize,
    now: i64,
) -> serde_json::Value {
    let events = bn_core::events::read_all();
    let queue_depth = st.counts().queued + backlog_depth;
    let core = orchestrate::Rollups::compute(
        &events,
        &st.repo,
        queue_depth,
        st.in_flight(),
        st.total_local_cap(),
        orchestrate::ROLLUP_WINDOW_SECS,
        now,
    );
    let (aic, usd, tokens) = cost_today(cfg, &st.repo, now);
    bn_core::payload::orchestrate_rollups_data(&core, aic, usd, tokens)
}

/// Today's cost burn for `repo` from the #381 cost surface, over a trailing 24h window: the AIC
/// real Copilot bill, the cache-aware $ API-equiv benchmark, and fresh tokens (fresh input +
/// output, excluding the ~free cache reads that would inflate the figure ~10×).
fn cost_today(cfg: &Config, repo: &str, now: i64) -> (Option<f64>, Option<f64>, i64) {
    let view = bn_core::cost::rollup(
        cfg,
        &bn_core::cost::CostFilter {
            window_days: 1,
            now,
            repo: Some(repo.to_string()),
            window_secs: None,
        },
    );
    let t = &view.totals;
    (t.aic, t.cost_usd, t.input_tokens + t.output_tokens)
}

fn cost_by_category_today(cfg: &Config, repo: &str, now: i64) -> Vec<bn_core::cost::CostRow> {
    bn_core::cost::rollup(
        cfg,
        &bn_core::cost::CostFilter {
            window_days: 1,
            now,
            repo: Some(repo.to_string()),
            window_secs: None,
        },
    )
    .by_category
}

/// The live per-agent output-token map, keyed by the lowercased `repo:branch` target — the fleet
/// join (#309) the per-item `tokens` enrichment and the watch rows both read. `None`-token rows
/// (a window-only agent whose session log isn't attributable yet) are dropped.
fn live_tokens(cfg: &Config) -> std::collections::HashMap<String, i64> {
    agent_live::collect(
        cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    )
    .into_iter()
    .filter_map(|r| r.tokens.map(|t| (r.target, t)))
    .collect()
}

/// Enrich each queue item in `v` with the #379 per-item usage fields: `age_secs` (now − dispatched,
/// present once dispatched) and live `tokens` (cumulative output so far, joined from the fleet by
/// `repo:branch`). The #372 location fields (repo / tmux / harness) already ride in from the shared
/// core shape. The payload's `queue` array is built in `st.queue` order, so the zip stays aligned.
fn enrich_queue_items(cfg: &Config, st: &OrchestrateState, v: &mut serde_json::Value, now: i64) {
    let tokens = live_tokens(cfg);
    let Some(items) = v.get_mut("queue").and_then(|q| q.as_array_mut()) else {
        return;
    };
    for (item, out) in st.queue.iter().zip(items.iter_mut()) {
        let Some(obj) = out.as_object_mut() else {
            continue;
        };
        if item.dispatched_at > 0 {
            obj.insert(
                "age_secs".to_string(),
                serde_json::json!((now - item.dispatched_at).max(0)),
            );
        }
        if let Some(branch) = &item.branch {
            let target = format!("{}:{}", st.repo.to_lowercase(), branch.to_lowercase());
            if let Some(tok) = tokens.get(&target) {
                obj.insert("tokens".to_string(), serde_json::json!(tok));
            }
        }
    }
}

/// A compact human label for the run's concurrency caps (#385). Untyped runs show the single cap
/// (`"2"`); a typed run shows the total plus its per-lane breakdown (`"7 (coding 5 · research 2)"`),
/// so `bn orchestrate status`/`watch` make the distinct coding vs research budgets visible.
fn caps_label(st: &OrchestrateState) -> String {
    match st.research_cap() {
        Some(r) => format!(
            "{} (coding {} · research {})",
            st.total_local_cap(),
            st.coding_cap(),
            r
        ),
        None => st.coding_cap().to_string(),
    }
}

/// The one-word lifecycle label for a run, shared by `status` and the `watch` header so both agree.
/// A *resident* run reports its standing role rather than "drained ✓": a #426 supervisor is
/// "supervising", a #368 self-feeder is "self-feeding" — neither is terminal on an empty ledger.
fn run_state_label(st: &OrchestrateState) -> &'static str {
    if st.stopped {
        "stopped"
    } else if st.supervise {
        "supervising"
    } else if st.self_feed {
        "self-feeding"
    } else if st.is_drained() {
        "drained ✓"
    } else {
        "running"
    }
}

/// Print one run's full human-readable status block, including the durable backlog depth (#375).
fn print_run(cfg: &Config, st: &OrchestrateState, backlog_depth: usize, now: i64, why: bool) {
    let c = st.counts();
    let reviews = bn_core::plan_review::open_reviews();
    let state = run_state_label(st);
    // #368: name the source a resident run pulls its next work from — the durable backlog (#375),
    // not just the fixed queue — so `status` makes the self-feeding contract visible. A supervisor
    // additionally adopts standalone dispatch lanes (#426).
    let feed = if st.supervise {
        " · adopts dispatch lanes · feeds from durable backlog"
    } else if st.self_feed {
        " · feeds from durable backlog"
    } else {
        ""
    };
    println!(
        "orchestrate {} — {state} · cap {}{}{}",
        st.repo,
        caps_label(st),
        if st.solo { ", solo" } else { "" },
        feed
    );
    println!(
        "  queued {} · in-flight {}{} · merged {} · failed {} · total {} · backlog {}",
        c.queued,
        c.in_flight,
        if c.remote > 0 {
            format!(" ({} remote)", c.remote)
        } else {
            String::new()
        },
        c.merged,
        c.failed,
        c.total,
        backlog_depth
    );
    print_cost_categories(cfg, &st.repo, bn_core::util::now_epoch_secs());
    if let Some(line) = dispatch_reason_line(st) {
        println!("{line}");
    }
    if why {
        print_dispatch_why(st, now);
    }
    for i in &st.queue {
        let mark = match i.status {
            ItemStatus::Queued => "·",
            ItemStatus::Dispatched => "●",
            ItemStatus::Merged => "✓",
            ItemStatus::Failed => "✗",
        };
        let stt = i.last_state.map(|s| s.label()).unwrap_or("");
        // #349: a dispatched agent paused on an un-approved plan reads as waiting-for-review — a
        // distinct state, so it's clear it's blocked on you, not stuck or working. #376: a *queued*
        // item held because its touch-area overlaps an in-flight lane's shows *which* lane holds it.
        let stt: String = if let Some(stuck) = &i.stuck {
            format!("stuck/{}", stuck.kind.label())
        } else if i.status.in_flight() {
            i.branch
                .as_deref()
                .and_then(|b| bn_core::plan_review::pending_for(&reviews, &st.repo, b))
                .map(|_| "waiting-review")
                .unwrap_or(stt)
                .to_string()
        } else if let Some(holder) = &i.held_by {
            format!("held: conflicts with #{holder}")
        } else {
            stt.to_string()
        };
        let br = i.branch.as_deref().unwrap_or("");
        // #372: point straight at the running agent — its tmux `session:window` (+ harness), the
        // copy-pasteable `tmux switch-client -t <target>`. Only in-flight items have a window.
        let nav = match st.tmux_target(i) {
            Some(target) => {
                let h = i
                    .harness
                    .as_deref()
                    .map(|h| format!(" [{h}]"))
                    .unwrap_or_default();
                format!("  → {target}{h}")
            }
            None => String::new(),
        };
        println!(
            "  {mark} {:<10} {:<12} {br} {stt}{nav}",
            i.id,
            i.status.label()
        );
        if let Some(stuck) = &i.stuck {
            println!(
                "      blocked: {} · unblock: {}",
                stuck.reason, stuck.suggested_unblock
            );
        }
    }
    if !st.recovery.is_empty() {
        println!("  recovery (latest):");
        for r in st.recovery.iter().rev().take(6) {
            println!(
                "    {} {} → {} — {}",
                r.id,
                r.state.label(),
                r.action.label(),
                r.note
            );
        }
    }

    // #380: recent completed outcomes read back from run-history.jsonl — so merged/failed results
    // survive the terminal items being evicted from the live queue, and a fresh `--queue` overwrite.
    let history = orchestrate::summarize_history(&orchestrate::read_history(), &st.repo, 5);
    if history.merged + history.failed > 0 {
        let cyc = history
            .avg_cycle_secs
            .map(|s| format!(" · avg cycle {}", fmt_dur(s)))
            .unwrap_or_default();
        println!(
            "  history: {} merged · {} failed{} (run-history.jsonl)",
            history.merged, history.failed, cyc
        );
        for r in &history.recent {
            let mark = if r.status == ItemStatus::Merged {
                "✓"
            } else {
                "✗"
            };
            let pr = r.pr.as_deref().map(|p| format!(" {p}")).unwrap_or_default();
            println!("    {mark} {} {}{pr}", r.id, r.status.label());
        }
    }
}

fn dispatch_reason_line(st: &OrchestrateState) -> Option<String> {
    st.dispatch_reason
        .as_deref()
        .map(|reason| format!("  dispatch: {reason}"))
}

fn dispatch_why_data(st: &OrchestrateState, now: i64) -> serde_json::Value {
    let Some(decision) = &st.dispatch_decision else {
        return serde_json::Value::Null;
    };
    let candidates = decision
        .skipped
        .iter()
        .map(|candidate| {
            serde_json::json!({
                "id": candidate.id,
                "item": candidate.item,
                "reason": candidate.reason,
                "held_by": candidate.held_by,
                "waiting_since": candidate.waiting_since,
                "wait_secs": now.saturating_sub(candidate.waiting_since),
            })
        })
        .collect::<Vec<_>>();
    let held = candidates
        .iter()
        .filter(|candidate| {
            candidate
                .get("held_by")
                .is_some_and(|value| !value.is_null())
        })
        .cloned()
        .collect::<Vec<_>>();
    serde_json::json!({
        "last_tick_at": decision.at,
        "age_secs": now.saturating_sub(decision.at),
        "outcome": decision.outcome,
        "gate": decision.gate,
        "reason": decision.reason,
        "attempted": decision.attempted,
        "launched": decision.launched,
        "lanes": decision.lanes,
        "held": held,
        "skipped": candidates,
    })
}

fn print_dispatch_why(st: &OrchestrateState, now: i64) {
    let Some(decision) = &st.dispatch_decision else {
        println!("  why: no dispatch tick recorded yet");
        return;
    };
    let age = fmt_dur(now.saturating_sub(decision.at));
    let reason = decision
        .reason
        .as_deref()
        .map(|reason| format!(" — {reason}"))
        .unwrap_or_default();
    println!(
        "  why: {}{} (last tick {age} ago; attempted {}, launched {})",
        decision.outcome, reason, decision.attempted, decision.launched
    );
    for lane in &decision.lanes {
        println!(
            "    lane {}: {}/{} in flight · {} queued · {} available{}",
            lane.lane,
            lane.in_flight,
            lane.cap,
            lane.queued,
            lane.available,
            if lane.saturated { " · saturated" } else { "" }
        );
    }
    for candidate in decision
        .skipped
        .iter()
        .filter(|candidate| candidate.held_by.is_some())
    {
        println!(
            "    held #{} by #{} · waiting {} — {}",
            candidate.id,
            candidate.held_by.as_deref().unwrap_or("?"),
            fmt_dur(now.saturating_sub(candidate.waiting_since)),
            candidate.reason
        );
    }
    for candidate in decision
        .skipped
        .iter()
        .filter(|candidate| candidate.held_by.is_none())
    {
        println!(
            "    skipped #{} · waiting {} — {}",
            candidate.id,
            fmt_dur(now.saturating_sub(candidate.waiting_since)),
            candidate.reason
        );
    }
}

fn print_cost_categories(cfg: &Config, repo: &str, now: i64) {
    let rows = cost_by_category_today(cfg, repo, now);
    if rows.is_empty() {
        return;
    }
    let wanted = [
        bn_core::cost::WorkClass::Orchestration,
        bn_core::cost::WorkClass::Coding,
        bn_core::cost::WorkClass::Scheduled,
    ];
    let parts: Vec<String> = wanted
        .iter()
        .filter_map(|class| {
            rows.iter()
                .find(|r| r.key == class.as_str())
                .map(|r| format!("{} {}", class.label(), fmt_cost(r.aic, r.cost_usd)))
        })
        .collect();
    if !parts.is_empty() {
        println!("  cost by category (24h): {}", parts.join(" · "));
    }
}

/// Format a duration in seconds compactly (`45s`, `12m`, `3h`) for the history/cycle line.
fn fmt_dur(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h", s / 3600)
    }
}

/// Load every per-run state file on the machine, in deterministic (sorted-path) order.
fn load_runs() -> Vec<(PathBuf, OrchestrateState)> {
    orchestrate::run_paths()
        .into_iter()
        .filter_map(|p| OrchestrateState::load(&p).map(|st| (p, st)))
        .collect()
}

/// Resolve the runs a status/watch view targets: one repo's run (mirroring `--repo`), or every
/// live run on the machine. Shared by `bn orchestrate status` and `bn orchestrate watch`.
fn resolve_runs(repo: Option<&str>) -> Vec<(PathBuf, OrchestrateState)> {
    match repo {
        Some(r) => OrchestrateState::load(&orchestrate::state_path_for(r))
            .map(|st| vec![(orchestrate::state_path_for(r), st)])
            .unwrap_or_default(),
        None => load_runs(),
    }
}

// ===========================================================================
// `bn orchestrate watch` (#379) — the live monitoring view
// ===========================================================================

/// Live monitoring view: a rollup header (queue depth, in-flight, merged/hr, ETA, cost today) over
/// one row per in-flight agent with its jumpable tmux `session:window`. `--json` emits the enriched
/// contract once (identical shape to `status --json`); `--plain` prints one static frame (for
/// `watch -n`/pipes); otherwise it self-refreshes every `interval`s until interrupted.
pub(crate) fn orchestrate_watch(
    cfg: &Config,
    repo: Option<&str>,
    interval: u64,
    plain: bool,
    json: bool,
) {
    ensure_migrated(true);
    if json {
        let now = bn_core::util::now_epoch_secs();
        println!(
            "{}",
            Envelope::success("orchestrate", watch_data(cfg, repo, now)).to_json()
        );
        return;
    }
    if plain {
        print!(
            "{}",
            watch_frame(cfg, repo, bn_core::util::now_epoch_secs())
        );
        return;
    }
    // Self-refresh: clear + home the cursor, paint the frame, sleep. Ctrl-C exits.
    loop {
        let frame = watch_frame(cfg, repo, bn_core::util::now_epoch_secs());
        print!("\x1b[2J\x1b[H{frame}");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        std::thread::sleep(std::time::Duration::from_secs(interval.max(1)));
    }
}

/// The enriched `{ runs, backlog }` JSON for `watch --json` — the same shape `status --json` emits,
/// so #345 (cockpit) and #347 (picker) consume one contract regardless of which command produced
/// it. Each run carries its `rollups` block and per-item usage fields via [`run_json`].
fn watch_data(cfg: &Config, repo: Option<&str>, now: i64) -> serde_json::Value {
    let runs = resolve_runs(repo);
    let backlog = backlog::load(&backlog::backlog_path());
    serde_json::json!({
        "runs": runs
            .iter()
            .map(|(_, st)| run_json(cfg, st, backlog.pending_depth(&st.repo), now, false))
            .collect::<Vec<_>>(),
        "backlog": backlog
            .repos_with_pending()
            .into_iter()
            .filter(|r| repo.is_none_or(|want| want == r))
            .map(|r| serde_json::json!({ "repo": r, "pending": backlog.pending_depth(&r) }))
            .collect::<Vec<_>>(),
    })
}

/// Render one human frame: a rollup header + in-flight rows for every targeted run. Pure string
/// output so `--plain` and the refresh loop share one renderer.
fn watch_frame(cfg: &Config, repo: Option<&str>, now: i64) -> String {
    let runs = resolve_runs(repo);
    if runs.is_empty() {
        return match repo {
            Some(r) => format!("orchestrate watch: no run for {r}\n"),
            None => {
                "orchestrate watch: no run (start one with `bn orchestrate --queue …`)\n".into()
            }
        };
    }
    let backlog = backlog::load(&backlog::backlog_path());
    let tokens = live_tokens(cfg);
    let mut out = String::new();
    for (i, (_, st)) in runs.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&render_watch_run(
            cfg,
            st,
            backlog.pending_depth(&st.repo),
            &tokens,
            now,
        ));
    }
    out
}

/// Render one run's watch block: the rollup header line + one row per in-flight agent (its id,
/// branch, age, jumpable tmux target, harness, and live token burn).
fn render_watch_run(
    cfg: &Config,
    st: &OrchestrateState,
    backlog_depth: usize,
    tokens: &std::collections::HashMap<String, i64>,
    now: i64,
) -> String {
    use bn_core::track::fmt_dur;
    let events = bn_core::events::read_all();
    let queue_depth = st.counts().queued + backlog_depth;
    let r = orchestrate::Rollups::compute(
        &events,
        &st.repo,
        queue_depth,
        st.in_flight(),
        st.total_local_cap(),
        orchestrate::ROLLUP_WINDOW_SECS,
        now,
    );
    let (aic, usd, _) = cost_today(cfg, &st.repo, now);
    let c = st.counts();
    let state = run_state_label(st);
    let cycle = r.avg_cycle_secs.map(fmt_dur).unwrap_or_else(|| "—".into());
    let eta = r.eta_secs.map(fmt_dur).unwrap_or_else(|| "—".into());

    let mut out = format!(
        "orchestrate {} — {state} · cap {}{}\n",
        st.repo,
        r.effective_cap,
        if st.solo { ", solo" } else { "" }
    );
    out.push_str(&format!(
        "  depth {} (q{} + backlog {}) · in-flight {}/{} · {:.1} merged/hr · cycle {} · ETA {} · {}\n",
        r.queue_depth, c.queued, backlog_depth, r.in_flight, r.effective_cap, r.merged_per_hour, cycle, eta, fmt_cost(aic, usd),
    ));
    if r.relaunch_recent > 0 || r.failed_recent > 0 {
        out.push_str(&format!(
            "  recovery (last {}): {} relaunch · {} failed\n",
            fmt_dur(r.window_secs),
            r.relaunch_recent,
            r.failed_recent
        ));
    }
    let inflight: Vec<&orchestrate::QueueItem> =
        st.queue.iter().filter(|i| i.status.in_flight()).collect();
    if inflight.is_empty() {
        out.push_str("  (no agents in flight)\n");
        return out;
    }
    for i in &inflight {
        let age = if i.dispatched_at > 0 {
            fmt_dur(now - i.dispatched_at)
        } else {
            "—".into()
        };
        let target = st.tmux_target(i).unwrap_or_default();
        // #453: attribute tokens by the item's own repo (its session), falling back to the run
        // repo for a legacy item — identical to before for a single-repo run.
        let item_repo = if i.repo.is_empty() {
            st.repo.as_str()
        } else {
            i.repo.as_str()
        };
        let harness = i
            .harness
            .as_deref()
            .map(|h| format!(" [{h}]"))
            .unwrap_or_default();
        let stt = i.last_state.map(|s| s.label()).unwrap_or("");
        let tok = i
            .branch
            .as_deref()
            .and_then(|b| {
                tokens.get(&format!(
                    "{}:{}",
                    item_repo.to_lowercase(),
                    b.to_lowercase()
                ))
            })
            .map(|t| format!(" · {t} tok"))
            .unwrap_or_default();
        let id = &i.id;
        let branch = i.branch.as_deref().unwrap_or("");
        out.push_str(&format!(
            "  ● {id:<8} {branch:<16} {age:>6}  → {target}{harness}{tok}  {stt}\n"
        ));
    }
    out
}

/// Render the dual-unit cost burn for the rollup header: `12.5 AIC ($3.20)`, degrading gracefully
/// when only one unit (or neither) is available.
fn fmt_cost(aic: Option<f64>, usd: Option<f64>) -> String {
    match (aic, usd) {
        (Some(a), Some(u)) => format!("{a:.1} AIC (${u:.2})"),
        (Some(a), None) => format!("{a:.1} AIC"),
        (None, Some(u)) => format!("${u:.2}"),
        (None, None) => "no cost today".to_string(),
    }
}

// ===========================================================================
// `bn orchestrate stop`
// ===========================================================================

/// Stop one run: mark it stopped (the daemon's next tick exits) and unload its launchd daemon.
/// `--repo` picks the run; bare stop resolves the only live run, else the cwd repo's (#395 — with
/// several live runs, an untargeted stop must never guess and kill someone else's wave).
pub(crate) fn orchestrate_stop(
    cfg: &Config,
    repo: Option<&str>,
    all_repos: bool,
    cwd_repo: Option<&str>,
    json: bool,
) {
    let _ = cfg;
    ensure_migrated(true);
    if all_repos {
        // Stop the cross-repo supervisor: unload its daemon so it can't re-seed, then mark every
        // live supervised run stopped (its own next tick, if any, exits).
        crate::launchd::orchestrate_uninstall_all();
        crate::systemd::orchestrate_uninstall_all();
        let stopped: Vec<String> = load_runs()
            .into_iter()
            .filter(|(_, st)| !st.stopped && st.supervise)
            .map(|(path, mut st)| {
                st.stopped = true;
                st.updated_at = bn_core::util::now_epoch_secs();
                let _ = st.save(&path);
                crate::launchd::orchestrate_uninstall(&st.repo);
                crate::systemd::orchestrate_uninstall(&st.repo);
                st.repo
            })
            .collect();
        log_event(
            "orchestrate_stop_all",
            &[("repos", &stopped.len().to_string())],
        );
        if json {
            println!(
                "{}",
                Envelope::success(
                    "orchestrate",
                    serde_json::json!({ "stopped": true, "all_repos": true, "repos": stopped })
                )
                .to_json()
            );
        } else {
            println!(
                "orchestrate: cross-repo supervisor stopped ({} repo run(s) ended, daemon unloaded).",
                stopped.len()
            );
        }
        return;
    }
    let repo: String = match repo {
        Some(r) => r.to_string(),
        None => {
            let live: Vec<String> = load_runs()
                .into_iter()
                .filter(|(_, st)| !st.stopped && (!st.is_drained() || st.is_resident()))
                .map(|(_, st)| st.repo)
                .collect();
            match live.len() {
                1 => live.into_iter().next().unwrap(),
                0 => match cwd_repo {
                    // Nothing live: still allow stopping the cwd repo's (finished) run — clears
                    // the daemon/plist. Otherwise there is nothing to stop.
                    Some(r) => r.to_string(),
                    None => fail("bn orchestrate stop: no live run — pass --repo <name>"),
                },
                _ => match cwd_repo.filter(|r| live.iter().any(|l| l == r)) {
                    Some(r) => r.to_string(),
                    None => fail(&format!(
                        "bn orchestrate stop: {} live runs ({}) — pass --repo <name>",
                        live.len(),
                        live.join(", ")
                    )),
                },
            }
        }
    };
    let path = orchestrate::state_path_for(&repo);
    if let Some(mut st) = OrchestrateState::load(&path) {
        st.stopped = true;
        st.updated_at = bn_core::util::now_epoch_secs();
        let _ = st.save(&path);
        log_event("orchestrate_stop", &[("repo", &st.repo)]);
    }
    crate::launchd::orchestrate_uninstall(&repo);
    crate::systemd::orchestrate_uninstall(&repo);
    if json {
        println!(
            "{}",
            Envelope::success(
                "orchestrate",
                serde_json::json!({ "stopped": true, "repo": repo })
            )
            .to_json()
        );
    } else {
        println!("orchestrate: {repo} stopped (daemon unloaded).");
    }
}

/// Live-adjust the typed concurrency lanes of a **running** run without stop/re-declare (#385) —
/// the IO half of the pure [`OrchestrateState::scale`]. Resolves the target run (explicit `--repo`,
/// else the only live run, else the cwd repo's), then atomically rewrites the persisted caps via
/// [`orchestrate::mutate`] (the same locked read-modify-write the daemon tick + web layer share, so
/// a concurrent edit is never clobbered) and echoes the new budgets. The running daemon reloads
/// state every poll, so it picks the new caps up on its next tick — no restart. Returns false on a
/// no-op (neither flag given) or when no run exists for the repo.
pub(crate) fn orchestrate_scale(
    cfg: &Config,
    cwd_repo: Option<&str>,
    repo: Option<&str>,
    coding: Option<usize>,
    research: Option<usize>,
    json: bool,
) -> bool {
    let _ = cfg;
    ensure_migrated(true);
    if coding.is_none() && research.is_none() {
        eprintln!("bn orchestrate scale: nothing to change — pass --coding N and/or --research M");
        return false;
    }
    // Resolve the run to scale, mirroring `stop`: explicit `--repo`, else the only live run, else
    // the cwd repo's — never guessing between several live runs.
    let repo: String = match repo {
        Some(r) => r.to_string(),
        None => {
            let live: Vec<String> = load_runs()
                .into_iter()
                .filter(|(_, st)| !st.stopped && (!st.is_drained() || st.is_resident()))
                .map(|(_, st)| st.repo)
                .collect();
            match live.len() {
                1 => live.into_iter().next().unwrap(),
                0 => match cwd_repo {
                    Some(r) => r.to_string(),
                    None => fail("bn orchestrate scale: no live run — pass --repo <name>"),
                },
                _ => match cwd_repo.filter(|r| live.iter().any(|l| l == r)) {
                    Some(r) => r.to_string(),
                    None => fail(&format!(
                        "bn orchestrate scale: {} live runs ({}) — pass --repo <name>",
                        live.len(),
                        live.join(", ")
                    )),
                },
            }
        }
    };
    let path = orchestrate::state_path_for(&repo);
    let (coding_cap, research_cap) = match orchestrate::mutate(&path, |st| {
        st.scale(coding, research)
    }) {
        Ok(Some(caps)) => caps,
        Ok(None) => {
            eprintln!(
                "bn orchestrate scale: no run for {repo} — start one with `bn orchestrate --queue …`"
            );
            return false;
        }
        Err(e) => {
            eprintln!("bn orchestrate scale: could not update run: {e}");
            return false;
        }
    };
    log_event(
        "orchestrate_scale",
        &[
            ("repo", &repo),
            ("coding", &coding_cap.to_string()),
            (
                "research",
                &research_cap.map(|r| r.to_string()).unwrap_or_default(),
            ),
        ],
    );
    if json {
        println!(
            "{}",
            Envelope::success(
                "orchestrate",
                serde_json::json!({
                    "repo": repo,
                    "coding": coding_cap,
                    "research": research_cap,
                }),
            )
            .to_json()
        );
    } else {
        match research_cap {
            Some(r) => println!(
                "orchestrate: {repo} scaled — coding {coding_cap} · research {r} (daemon picks it up next tick)."
            ),
            None => println!(
                "orchestrate: {repo} scaled — coding {coding_cap} (single pool; daemon picks it up next tick)."
            ),
        }
    }
    true
}

// ===========================================================================
// Live-signal capture + executor plumbing
// ===========================================================================

/// An owned pane observation — the buffers a borrowed [`PaneObservation`] points at.
struct Observed {
    tail: Vec<String>,
    has_window: bool,
    has_process: bool,
    plane_a: String,
    has_wip: bool,
}

impl Observed {
    fn as_obs(&self) -> PaneObservation<'_> {
        PaneObservation {
            tail: &self.tail,
            has_window: self.has_window,
            plane_a: &self.plane_a,
            has_wip: self.has_wip,
        }
    }
}

/// The tmux window name for an agent: `repo:sanitized-branch` (matching how `bn dispatch` opens it).
fn tmux_target(repo: &str, branch: &str) -> String {
    format!("{repo}:{}", wt::sanitize_branch(branch))
}

/// The sanitized-branch window suffixes of `repo`'s **live agent windows** — the candidate standalone
/// dispatch lanes a supervisor adopts (#426). Thin delegate to the pure #455 policy
/// ([`bn_core::crossrepo::lane_windows_in_session`]): keeps windows in this repo's session
/// (`<repo>:<window>`), dropping the repo's own `main`/`master` checkout, warm `_pool` slots, and
/// empty names. The window name is the sanitized branch (`bn dispatch`'s naming), so this is the
/// tmux half of discovery; [`discover_lanes`] resolves each to its real branch on disk.
fn lane_windows_for(repo: &str, windows: &[String]) -> Vec<String> {
    bn_core::crossrepo::lane_windows_in_session(repo, windows)
}

/// Discover this repo's live standalone `bn dispatch` lanes for supervisor adoption (#426): every
/// live agent window ([`lane_windows_for`]) whose worktree still exists on disk, resolved to its real
/// git branch (the form `bn dispatch --json` records and the daemon tracks). A window without a
/// worktree, or one whose branch can't be read, is skipped. Best-effort and read-only — a lane the
/// supervisor can't resolve is simply not adopted this pass (its PR still lands via `bn wt merge
/// --all`), never mis-recovered.
fn discover_lanes(cfg: &Config, repo: &str, windows: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for win in lane_windows_for(repo, windows) {
        // The window name is the sanitized branch == the worktree dir name.
        let wt = cfg.worktree_dir.join(repo).join(&win);
        if !wt.join(".git").exists() {
            continue; // no live worktree — nothing to supervise (PRs still land unconditionally)
        }
        let branch = wt::git_out(&[
            "-C",
            &wt.to_string_lossy(),
            "rev-parse",
            "--abbrev-ref",
            "HEAD",
        ])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "HEAD")
        .unwrap_or(win);
        if !out.iter().any(|e: &String| e == &branch) {
            out.push(branch);
        }
    }
    out
}

/// Gather the live signals for one agent into an owned [`Observed`]: the pane tail, whether a live
/// window exists, its Plane-A `@bn_agent` state, and whether the worktree carries WIP.
fn observe(
    cfg: &Config,
    repo: &str,
    branch: &str,
    windows: &[String],
    live_processes: &std::collections::BTreeSet<String>,
) -> Observed {
    let target = tmux_target(repo, branch);
    let has_window = windows.iter().any(|w| w.eq_ignore_ascii_case(&target));
    let has_process = live_processes.contains(&target.to_ascii_lowercase());
    let tail = if has_window {
        capture_pane_tail(&target, 8)
    } else {
        Vec::new()
    };
    let plane_a = crate::proc::tmux(&[
        "display-message",
        "-p",
        "-t",
        &format!("={target}"),
        "-F",
        "#{@bn_agent}",
    ])
    .unwrap_or_default();
    let wt = worktree_path(cfg, repo, branch);
    let has_wip = worktree_has_wip(&wt);
    Observed {
        tail,
        has_window,
        has_process,
        plane_a,
        has_wip,
    }
}

fn lane_is_dead(has_window: bool, has_process: bool) -> bool {
    !has_window && !has_process
}

/// Whether a worktree carries genuine WIP the supervisor should recover — uncommitted changes, or
/// commits unique to HEAD vs **current** `origin/<default>` (#370 defect 3). The old check compared
/// against the stale *local* `main`/`master` ref (which a bare-repo worktree layout rarely updates),
/// so a clean branch that had advanced to current main read as "ahead of the old base" → false
/// `wip=true` → an infinite relaunch loop. Judging against `origin/<default>` (refreshed once per
/// tick by [`fetch_origin_default`]) means a clean tree at main's tip has zero unique commits and is
/// correctly `idle-clean`, never `idle-with-wip`.
fn worktree_has_wip(wt: &Path) -> bool {
    if !wt.is_dir() {
        return false;
    }
    // Uncommitted work (staged, unstaged, or untracked) — `git status --porcelain` non-empty.
    if wt::dirty_count(&wt.to_string_lossy()) > 0 {
        return true;
    }
    // Committed-but-unmerged work — commits on HEAD not reachable from current origin/<default>.
    commits_ahead_of_origin_default(wt) > 0
}

/// Commits unique to the worktree's HEAD vs current `origin/<default>` (0 when HEAD sits at or
/// behind main's tip). Pure read against the already-fetched ref — no network here; the per-tick
/// refresh lives in [`fetch_origin_default`] so this stays cheap per agent.
fn commits_ahead_of_origin_default(wt: &Path) -> u32 {
    let wts = wt.to_string_lossy();
    let base = format!("origin/{}", origin_default_branch(wt));
    wt::git_out(&["-C", &wts, "rev-list", "--count", &format!("{base}..HEAD")])
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

/// The remote's default branch name for a worktree (`origin/HEAD` → `main`), falling back to `main`
/// then `master` when `origin/HEAD` isn't set — so wip is judged against the branch the repo
/// actually merges into.
fn origin_default_branch(wt: &Path) -> String {
    let wts = wt.to_string_lossy();
    if let Some(name) = wt::git_out(&[
        "-C",
        &wts,
        "symbolic-ref",
        "--short",
        "refs/remotes/origin/HEAD",
    ])
    .map(|s| s.trim().trim_start_matches("origin/").to_string())
    .filter(|s| !s.is_empty())
    {
        return name;
    }
    for cand in ["main", "master"] {
        if wt::git_out(&[
            "-C",
            &wts,
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("origin/{cand}"),
        ])
        .is_some()
        {
            return cand.to_string();
        }
    }
    "main".to_string()
}

/// Best-effort refresh of `origin/<default>` on the repo's shared bare, once per tick — so the wip
/// check ([`worktree_has_wip`]) is judged against the *live* main, not a ref last fetched at
/// dispatch time (#370 defect 3). Silent on failure (offline / DNS outage): the comparison then
/// falls back to the last-known origin tip, which is still current relative to the (also-stalled)
/// branch, so no false wip is introduced.
fn fetch_origin_default(cfg: &Config, repo: &str) {
    if let Some(wt) = a_repo_worktree(cfg, repo) {
        let wts = wt.to_string_lossy().to_string();
        let default = origin_default_branch(&wt);
        let _ = wt::run_git(&["-C", &wts, "fetch", "--quiet", "origin", &default]);
    }
}

/// The last `n` non-empty lines of an agent pane's active pane, addressed by its exact window name.
fn capture_pane_tail(target: &str, n: usize) -> Vec<String> {
    let out = crate::proc::tmux_raw(&["capture-pane", "-p", "-t", &format!("={target}")])
        .unwrap_or_default();
    let lines: Vec<String> = out
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].to_vec()
}

/// Every live tmux window as `session:window`, or `None` when tmux is unreachable (so callers can
/// distinguish "no windows" from "no tmux" and avoid a false `dead` classification). A missing
/// tmux server after reboot is an observable empty fleet, not an unreachable probe: recovery may
/// safely recreate the session through the normal fresh dispatch path.
fn list_windows() -> Option<Vec<String>> {
    let out =
        crate::proc::tmux_command(&["list-windows", "-a", "-F", "#{session_name}:#{window_name}"])
            .output()
            .ok()?;
    classify_window_listing(
        out.status.success(),
        &String::from_utf8_lossy(&out.stdout),
        &String::from_utf8_lossy(&out.stderr),
    )
}

fn classify_window_listing(success: bool, stdout: &str, stderr: &str) -> Option<Vec<String>> {
    if success {
        return Some(
            stdout
                .lines()
                .map(|line| line.trim().to_string())
                .filter(|line| !line.is_empty())
                .collect(),
        );
    }
    let error = stderr.to_ascii_lowercase();
    if error.contains("no server running")
        || error.contains("failed to connect to server")
        || error.contains("error connecting to")
    {
        return Some(Vec::new());
    }
    None
}

/// The worktree dir for an agent: `<worktree_dir>/<repo>/<sanitized-branch>`.
fn worktree_path(cfg: &Config, repo: &str, branch: &str) -> PathBuf {
    cfg.worktree_dir
        .join(repo)
        .join(wt::sanitize_branch(branch))
}

/// Any existing worktree of the repo (to `cd` into for the cwd-scoped merge-queue), or `None`.
fn a_repo_worktree(cfg: &Config, repo: &str) -> Option<PathBuf> {
    let repo_dir = cfg.worktree_dir.join(repo);
    let rd = std::fs::read_dir(&repo_dir).ok()?;
    rd.flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir() && p.join(".git").exists())
}

/// The raw dispatch subject of an item (for a re-dispatch relaunch).
fn item_of(st: &OrchestrateState, id: &str) -> String {
    st.queue
        .iter()
        .find(|i| i.id == id)
        .map(|i| i.item.clone())
        .unwrap_or_else(|| id.to_string())
}

/// The best-known PR link for a landed branch, from the `bn agent done` completion item (if any).
fn done_pr_link(repo: &str, branch: &str) -> Option<String> {
    let key = format!("done:{repo}/{branch}");
    let today = bn_core::util::today();
    bn_core::attention::completed_items(&today)
        .into_iter()
        .chain(bn_core::attention::open_items(&today))
        .find(|a| a.key.as_deref() == Some(key.as_str()))
        .and_then(|a| a.link)
}

/// Seed a run's dispatch harness from the `[orchestrate].agent` config default (#382) when it isn't
/// already set — so a supervisor (#426) or backlog-fed run (#368) honors the configured default even
/// though only `bn orchestrate --agent` sets it explicitly. A run that already carries a harness
/// (a prior `--agent`, or an earlier config default) is left untouched, and an absent/blank config
/// leaves it `None` so [`OrchestrateState::dispatch_harness`] supplies `copilot`.
fn ensure_default_harness(cfg: &Config, st: &mut OrchestrateState) {
    if st.harness.is_none() {
        if let Some(h) = cfg
            .orchestrate_agent
            .as_deref()
            .and_then(crate::cmd::dispatch::normalize_agent)
        {
            st.harness = Some(h.to_string());
        }
    }
}

/// Dispatch one item into a fresh (or reused) worktree with a self-gating agent under `harness`
/// (`bn orchestrate --agent`, #382), returning the git branch parsed from `bn dispatch --json`.
/// `branch` forces a specific branch on relaunch. `fresh` forces a **brand-new harness session**
/// (`bn dispatch --fresh`) instead of resuming the worktree's recorded one — the #383 fix: on a hard
/// restart the old session is idle/dead, so restoring it just brings back the empty prompt; a fresh,
/// re-seeded session actually resumes the work. A first dispatch (`fresh = false`) has no prior
/// session to restore anyway.
///
/// #378: a `scout` item (an `ask`, or any research-lane entry) dispatches with `--scout` instead of
/// the ship flags — the read-only `investigate` skill that writes findings to the branch note,
/// opens no PR, and skips the verify gate — so the same delegate-for-answers primitive `bn dispatch
/// --scout` already provides is reused verbatim. A ship keeps `--yolo --verify` (implement → verify
/// → PR → done).
fn sh_dispatch(
    repo: &str,
    item: &str,
    branch: Option<&str>,
    fresh: bool,
    harness: &str,
    scout: bool,
    controller_id: Option<&str>,
) -> Option<String> {
    // Pooling is the default dispatch path now: `bn dispatch` leases a warm, reap-safe slot for
    // every repo with a bn-managed bare clone (falling back to a cold worktree when there's no bare
    // clone or the pool is full), so the orchestrator no longer needs to opt in per-repo.
    // #378: scouts investigate & report (no PR, no verify gate); ships implement & land a PR.
    let args = crate::cmd::dispatch::DispatchInvocation::new(item, repo, harness)
        .branch(branch)
        .scout(scout)
        .verify(!scout)
        .yolo(!scout)
        .fresh(fresh)
        .json(true)
        .args();
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let (ok, out) = run_bn_owned(&refs, None, controller_id, Some(repo));
    if !ok {
        return None;
    }
    let v: serde_json::Value =
        serde_json::from_str(out.trim().lines().last().unwrap_or("")).ok()?;
    v.get("data")
        .and_then(|d| d.get("branch"))
        .and_then(|b| b.as_str())
        .map(|s| s.to_string())
        .or_else(|| branch.map(|s| s.to_string()))
}

/// Fan out an item to a GitHub-hosted runner (#377) via `bn dispatch --remote` (#281). An
/// issue-linked item (numeric `id`) uses the issue path (`--issue <id>`): it posts an `@claude`
/// comment on the issue, framing the issue's own title/body as the task — and reconciles cleanly via
/// [`reconcile_remote`] when that issue closes. A free-text explicit-remote item uses the free-text
/// seed path (its user-authored subject as the `prompt`). Run as an isolated child (like
/// [`sh_dispatch`]) so a `fail()`/exit inside the remote path can never take down the daemon.
/// Returns whether the trigger succeeded; a failure (workflow not installed, gh unauthed) leaves
/// the item for the caller to back off. No local worktree/slot is created.
fn sh_dispatch_remote(
    repo: &str,
    id: &str,
    item: &str,
    request_id: &str,
    attempt_id: &str,
) -> bool {
    let issue_linked = !id.is_empty() && id.chars().all(|c| c.is_ascii_digit());
    let token = format!("bn dispatch request {request_id} attempt {attempt_id}");
    let prompt = if issue_linked {
        format!("Please pick up this issue: propose a plan, implement it, and open a PR. {token}")
    } else if item.trim().is_empty() {
        token
    } else {
        format!("{item}\n\n{token}")
    };
    let args: Vec<&str> = if issue_linked {
        vec![
            "dispatch", "--remote", "--issue", id, "--repo", repo, "--json", &prompt,
        ]
    } else {
        vec![
            "dispatch", "--remote", &prompt, "--repo", repo, "--corr", attempt_id, "--json",
        ]
    };
    run_bn(&args, None).0
}

/// Dispatch the placed item at `idx` onto a remote SSH worker `alias` (#729) via the #718-style
/// SSH transport: run `bn worker start --request-id … --attempt-id … --memory-ceiling-mb … --json`
/// on the node (idempotent atomic admission, #720). On a successful admission (`accepted` /
/// `already_exists`) the control-plane persists `{node, request_id, attempt_id, state=QUEUED}` on
/// this side, mirroring the worker's own registry. A definite rejection / unreachable node leaves
/// the item queued for a later retry. An indeterminate outcome is persisted as `UNKNOWN`, blocking
/// redispatch until reconciliation because the admission may have committed remotely. The
/// `request_id` is **stable per item**; the `attempt_id` is unique per attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkerStartResult {
    Admitted,
    Retryable(String),
    Indeterminate(String),
}

fn start_worker<R: bn_core::worker::transport::SshRunner>(
    transport: &bn_core::worker::transport::Transport<'_, R>,
    request_id: &str,
    attempt_id: &str,
    task: &str,
    repo: &str,
    harness: &str,
    memory_ceiling_mb: u64,
) -> WorkerStartResult {
    let ceiling = memory_ceiling_mb.to_string();
    let argv = [
        "bn",
        "worker",
        "start",
        "--request-id",
        request_id,
        "--attempt-id",
        attempt_id,
        "--task",
        task,
        "--repo",
        repo,
        "--harness",
        harness,
        "--memory-ceiling-mb",
        ceiling.as_str(),
        "--json",
    ];
    match transport.exec(&argv, None) {
        Ok(bn_core::worker::transport::ExecOutcome::Completed {
            exit_status,
            stdout,
            stderr,
        }) => {
            let admitted = exit_status == 0
                && bn_core::worker::parse_start_verdict(&stdout)
                    .as_deref()
                    .is_some_and(bn_core::worker::start_admitted);
            if admitted {
                WorkerStartResult::Admitted
            } else {
                let reason = bn_core::worker::parse_start_verdict(&stdout)
                    .unwrap_or_else(|| stderr.lines().next().unwrap_or("start rejected").into());
                WorkerStartResult::Retryable(reason)
            }
        }
        Ok(bn_core::worker::transport::ExecOutcome::NotStarted) => {
            WorkerStartResult::Retryable("ssh connection was not established".into())
        }
        Ok(bn_core::worker::transport::ExecOutcome::Indeterminate { .. }) => {
            WorkerStartResult::Indeterminate(
                "ssh outcome indeterminate; reconciliation required".into(),
            )
        }
        Err(e) => WorkerStartResult::Retryable(e.to_string()),
    }
}

fn dispatch_worker_item(
    cfg: &Config,
    run_path: &Path,
    st: &mut OrchestrateState,
    idx: usize,
    alias: &str,
    now: i64,
) -> bool {
    let repo = st.repo.clone();
    let item = st.queue[idx].item.clone();
    let request_id = st.queue[idx].request_id.clone().unwrap_or_default();
    let attempt_id = st.queue[idx].attempt_id.clone().unwrap_or_default();
    let ceiling = worker_memory_ceiling_mb(st, alias);
    let harness = st.harness.as_deref().unwrap_or("copilot");
    let runner = bn_core::worker::transport::RealSshRunner;
    let result = match bn_core::worker::transport::Transport::connect(
        cfg,
        alias,
        bn_core::worker::transport::SshOptions::default(),
        &runner,
    ) {
        Ok(transport) => start_worker(
            &transport,
            &request_id,
            &attempt_id,
            &item,
            &repo,
            harness,
            ceiling,
        ),
        Err(e) => WorkerStartResult::Retryable(e.to_string()),
    };
    apply_worker_start_result(
        run_path,
        st,
        idx,
        alias,
        &request_id,
        &attempt_id,
        &repo,
        &item,
        result,
        now,
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_worker_start_result(
    run_path: &Path,
    st: &mut OrchestrateState,
    idx: usize,
    alias: &str,
    request_id: &str,
    attempt_id: &str,
    repo: &str,
    item: &str,
    result: WorkerStartResult,
    now: i64,
) -> bool {
    let node_id = st
        .nodes
        .iter()
        .find(|n| {
            n.node_id == alias
                || n.target == bn_core::backlog::ExecutionTarget::SshHost(alias.to_string())
        })
        .map(|n| n.node_id.clone())
        .unwrap_or_else(|| alias.to_string());
    match result {
        WorkerStartResult::Admitted => {
            persist_worker_dispatch(
                run_path, st, idx, &node_id, request_id, attempt_id, "QUEUED", now,
            );
            log_event(
                "orchestrate_dispatch_worker",
                &[
                    ("repo", repo),
                    ("item", item),
                    ("node", &node_id),
                    ("attempt_id", attempt_id),
                ],
            );
            true
        }
        WorkerStartResult::Indeterminate(reason) => {
            persist_worker_dispatch(
                run_path, st, idx, &node_id, request_id, attempt_id, "UNKNOWN", now,
            );
            log_event(
                "orchestrate_dispatch_worker_unknown",
                &[
                    ("repo", repo),
                    ("item", item),
                    ("node", &node_id),
                    ("attempt_id", attempt_id),
                    ("reason", &reason),
                ],
            );
            false
        }
        WorkerStartResult::Retryable(reason) => {
            release_dispatch_claim(run_path, st, idx, now);
            log_event(
                "orchestrate_dispatch_worker_failed",
                &[
                    ("repo", repo),
                    ("item", item),
                    ("node", alias),
                    ("reason", &reason),
                ],
            );
            false
        }
    }
}

/// A stable per-item dispatch `request_id` (#712/#729/#720): `<repo-name>-<id>`, sanitized to a safe
/// slug. Stability is what makes re-dispatch idempotent — the same request re-admits the same
/// attempt-family on the worker rather than piling up duplicates.
fn dispatch_request_id(repo: &str, id: &str) -> String {
    let name = repo.rsplit(['/', '\\']).next().unwrap_or(repo);
    let slug: String = format!("{name}-{id}")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    slug.trim_matches('-').to_string()
}

/// The memory ceiling (MB) to admit a remote worker agent under (#729/#720). Uses the node's
/// declared headroom (`ram_total_mb - ram_reserved_mb`) when known, else a conservative default —
/// the worker itself enforces the cgroup cap; this is the estimate the admission weighs.
fn worker_memory_ceiling_mb(st: &OrchestrateState, alias: &str) -> u64 {
    const DEFAULT_CEILING_MB: u64 = 2048;
    st.nodes
        .iter()
        .find(|n| {
            n.node_id == alias
                || n.target == bn_core::backlog::ExecutionTarget::SshHost(alias.to_string())
        })
        .and_then(|n| n.ram_total_mb.checked_sub(n.ram_reserved_mb))
        .filter(|&mb| mb > 0)
        .unwrap_or(DEFAULT_CEILING_MB)
}

/// Poll the liveness of every in-flight remote worker attempt (#729): group the in-flight items
/// carrying a `node`/`attempt_id` by their SSH alias, run `bn worker status --json` on each node
/// once, and fold each attempt's state (`QUEUED`/`RUNNING`/`SUCCEEDED`/`FAILED`) back into the
/// control-plane mirror. A node that's unreachable (or whose status carries no matching attempt)
/// marks its attempts `UNKNOWN` — per epic principle 5 that blocks their redispatch until a
/// reconcile lines the two sides back up. Best-effort: a poll failure never crashes the tick.
fn poll_worker_liveness(cfg: &Config, st: &mut OrchestrateState, now: i64) {
    // Collect (item index, ssh alias, attempt_id) for in-flight items placed on an SSH node.
    let mut probes: Vec<(usize, String, String)> = Vec::new();
    for (idx, it) in st.queue.iter().enumerate() {
        if !it.status.in_flight() {
            continue;
        }
        let (Some(node_id), Some(attempt_id)) = (it.node.as_ref(), it.attempt_id.as_ref()) else {
            continue;
        };
        // Resolve the node's SSH alias from the registry (node_id may differ from the alias).
        let alias = st
            .nodes
            .iter()
            .find(|n| &n.node_id == node_id)
            .and_then(|n| match &n.target {
                bn_core::backlog::ExecutionTarget::SshHost(a) => Some(a.clone()),
                _ => None,
            });
        if let Some(alias) = alias {
            probes.push((idx, alias, attempt_id.clone()));
        }
    }
    if probes.is_empty() {
        return;
    }
    // Cache one `bn worker status` fetch per node (max_concurrent = 1 keeps this to one attempt
    // per node in M1, but cache anyway so a future multi-slot node is polled once).
    let mut status_cache: std::collections::HashMap<String, Option<serde_json::Value>> =
        std::collections::HashMap::new();
    for (idx, alias, attempt_id) in probes {
        let out = status_cache
            .entry(alias.clone())
            .or_insert_with(|| worker_status(cfg, &alias))
            .clone();
        let state = out
            .as_ref()
            .and_then(|o| worker_attempt_state(o, &attempt_id))
            .unwrap_or_else(|| "UNKNOWN".to_string());
        st.set_worker_state_at(idx, &state, now);
    }
}

fn worker_status(cfg: &Config, alias: &str) -> Option<serde_json::Value> {
    crate::connect::worker_status(cfg, alias)
}

fn poll_worker_status<R: bn_core::worker::transport::SshRunner>(
    transport: &bn_core::worker::transport::Transport<'_, R>,
) -> Option<serde_json::Value> {
    transport.poll_status().ok()
}

fn worker_attempt_state(status: &serde_json::Value, attempt_id: &str) -> Option<String> {
    status
        .get("attempts")?
        .as_array()?
        .iter()
        .find(|a| a.get("attempt_id").and_then(|x| x.as_str()) == Some(attempt_id))
        .and_then(|a| a.get("state").and_then(|x| x.as_str()))
        .map(str::to_string)
}

/// #627: the release-on-idle companion tick. Called from the idle branch of [`orchestrate_run`]
/// when no coding lanes are in flight. Opt-in via `BN_RELEASE_ON_IDLE`; a cheap pre-check
/// ([`crate::cmd::release::companion_should_attempt`]) avoids paying the verify cost until the repo
/// has a tip newer than its last tag. When there's something to ship, it runs `bn release --json`
/// in one of the repo's worktrees — reusing the single tested gate/idempotence/verify path — and
/// logs the outcome. Best-effort: any failure is swallowed so the supervise loop is never blocked.
fn release_companion_tick(cfg: &Config, repo: &str) {
    let Some(wt) = a_repo_worktree(cfg, repo) else {
        return;
    };
    if !crate::cmd::release::companion_should_attempt(&wt) {
        return;
    }
    // Refresh origin/<default> so the release is judged against the live main tip.
    fetch_origin_default(cfg, repo);
    let (ok, out) = run_bn(&["release", "--json"], Some(&wt));
    if ok && out.contains("\"released\":true") {
        log_event("release_companion_cut", &[("repo", repo)]);
    }
}

/// The `bn` binary driving this process (so children run the same build).
fn bn_bin() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("bn"))
}

/// Run a `bn` subcommand as a child process, returning (success, stdout). Optional cwd for the
/// cwd-scoped commands (the merge-queue). Never panics — a spawn failure is `(false, "")`.
fn run_bn(args: &[&str], cwd: Option<&Path>) -> (bool, String) {
    run_bn_owned(args, cwd, None, None)
}

fn run_bn_owned(
    args: &[&str],
    cwd: Option<&Path>,
    controller_id: Option<&str>,
    worker_repo: Option<&str>,
) -> (bool, String) {
    let mut cmd = std::process::Command::new(bn_bin());
    cmd.args(args);
    if let Some(controller_id) = controller_id {
        cmd.env("BN_CONTROLLER_ID", controller_id);
    }
    // Declare the target repo's session so the dispatch's repo-session guard sees the supervisor as
    // that repo's own session (a supervisor's cwd can be neutral or another repo's worktree, which
    // would otherwise trip the cross-repo block).
    if let Some(repo) = worker_repo {
        cmd.env("BN_WORKER_REPO", repo);
    }
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    match cmd.output() {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
        ),
        Err(_) => (false, String::new()),
    }
}

/// Append an orchestrate lifecycle event to the machine-local event log (observability / the #326
/// metrics fold). Best-effort — observability, never correctness.
fn log_event(kind: &str, fields: &[(&str, &str)]) {
    let _ = log_orchestrate_event(EventLevel::Info, kind, kind, fields, None, None);
}

fn log_orchestrate_event(
    level: EventLevel,
    kind: &str,
    reason: &str,
    fields: &[(&str, &str)],
    error: Option<&str>,
    active: Option<bool>,
) -> std::io::Result<()> {
    let mut map = serde_json::Map::new();
    for (k, v) in fields {
        map.insert((*k).to_string(), serde_json::json!(v));
    }
    map.insert("level".into(), serde_json::json!(level));
    map.insert("reason".into(), serde_json::json!(reason));
    if !map.contains_key("item_id") {
        if let Some(id) = map.get("id").cloned() {
            map.insert("item_id".into(), id);
        }
    }
    if let Some(error) = error {
        map.insert("error".into(), serde_json::json!(error));
    }
    if let Some(active) = active {
        map.insert("active".into(), serde_json::json!(active));
    }
    // Mirror the decision into the opt-in verbose/debug action log (no-op when disabled) so an
    // orchestrate session's placement/skip/degradation decisions are inspectable after the fact.
    if bn_core::log::enabled() {
        let log_level = match level {
            EventLevel::Info => bn_core::log::LogLevel::Info,
            EventLevel::Warn => bn_core::log::LogLevel::Warn,
            EventLevel::Error => bn_core::log::LogLevel::Error,
        };
        bn_core::log::record(
            log_level,
            "orchestrate",
            kind,
            serde_json::Value::Object(map.clone()),
            serde_json::Value::Null,
            None,
        );
    }
    bn_core::events::append(kind, map.clone())?;
    let transition = match kind {
        "orchestrate_ask" => Some(bn_core::wake::FleetTransition::Waiting),
        "orchestrate_paused" | "orchestrate_degradation" if active != Some(false) => {
            Some(bn_core::wake::FleetTransition::Blocked)
        }
        "orchestrate_failed" => Some(bn_core::wake::FleetTransition::Failed),
        "orchestrate_merged" | "orchestrate_answered" | "orchestrate_done" => {
            Some(bn_core::wake::FleetTransition::Done)
        }
        _ => None,
    };
    if let (Some(transition), Some(repo)) = (
        transition,
        map.get("repo").and_then(serde_json::Value::as_str),
    ) {
        let item = map
            .get("item_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-");
        let now = bn_core::util::now_epoch_secs();
        let wake = bn_core::wake::ActionableWake::classify(
            format!("orchestrate:{kind}:{repo}:{item}:{now}"),
            now,
            transition,
            reason,
            bn_core::wake::WakeOwner::Orchestrate(repo.to_string()),
        )
        .expect("mapped orchestrate transition is actionable");
        bn_core::wake::WakeQueue::default().publish(wake)?;
    }
    Ok(())
}

fn record_degradation(
    repo: &str,
    reason: &str,
    active: bool,
    fields: &[(&str, &str)],
    error: Option<&str>,
) {
    type DegradationKey = (PathBuf, String, String);
    static STATES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<DegradationKey, bool>>,
    > = std::sync::OnceLock::new();

    let key = (
        bn_core::events::log_path(),
        repo.to_string(),
        reason.to_string(),
    );
    let states = STATES.get_or_init(Default::default);
    let mut states = states
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let already_active = *states.entry(key.clone()).or_insert_with(|| {
        let events = bn_core::events::orchestrator_events();
        bn_core::events::active_orchestrator_degradations(&events, Some(repo))
            .iter()
            .any(|event| event.reason == reason)
    });
    if already_active == active {
        return;
    }
    let mut contextual = Vec::with_capacity(fields.len() + 1);
    contextual.push(("repo", repo));
    contextual.extend_from_slice(fields);
    if log_orchestrate_event(
        if active {
            EventLevel::Warn
        } else {
            EventLevel::Info
        },
        "orchestrate_degradation",
        reason,
        &contextual,
        error,
        Some(active),
    )
    .is_ok()
    {
        states.insert(key, active);
    }
}

