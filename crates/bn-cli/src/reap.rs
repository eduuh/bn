//! Stale-agent garbage collection (#503).
//!
//! The side-effecting realization of [`bn_core::fleet_gc`]: this module captures the live signals
//! the pure policy needs — the process table, tmux windows/panes, worktree dirs, PR state — feeds
//! each discovered agent through [`fleet_gc::classify`], and *acts* on the verdicts: terminate a
//! stale agent's **full process tree** (SIGTERM → SIGKILL), then prune its now-dead tmux window.
//! It also prunes `remain-on-exit` dead panes/windows that accumulate across dispatch waves.
//!
//! Discovery is **harness-process-centric**: an agent is a running `copilot`/`claude` process
//! rooted in a managed worktree (`worktree_dir/<repo>/<branch>`). That single lens catches both
//! windowed agents (their pane's process tree) and the windowless stragglers #503 is about — a
//! copilot that reparented away from its killed pane and kept burning CPU with no window and no PR.
//!
//! Everything that can be pure is: the process-tree walk, the ps/lsof parsers, and the
//! harness/orchestrator command classifiers are unit-tested; only the actual `ps`/`lsof`/`tmux`
//! spawns and signal sends touch the OS.

use crate::cmd::killport::{process_exists, send_signal, signal_kill, signal_term};
use crate::proc::{tmux_raw, tmux_run_try, warn_unreachable};
use bn_core::agent::{AgentHealth, DEFAULT_IDLE_MINS, DEFAULT_STUCK_MINS};
use bn_core::fleet_gc::{self, AgentObservation, Disposition, PrStatus, ReapPolicy};
use bn_core::orchestrate::{classify as classify_pane, PaneObservation, PaneState};
use bn_core::render::Envelope;
use bn_core::resolver::Config;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The idle sentinel for a windowless straggler that isn't in the live fleet: if it were a fresh
/// launch it'd have a live session log (and appear in `supervise`), so a windowless process the
/// fleet doesn't know about is by definition long-quiet — well past any grace window.
const STALE_IDLE_SENTINEL: i64 = 1_000_000_000;

/// A row from `ps -axo pid=,ppid=,command=`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Proc {
    pub pid: i32,
    pub ppid: i32,
    pub command: String,
}

/// One tmux pane, as the reaper reads it.
#[derive(Debug, Clone)]
struct Pane {
    window_id: String,
    pane_pid: i32,
    dead: bool,
    cwd: String,
}

/// A discovered agent: the process tree we'd reap, plus everything the CLI needs to build a
/// [`AgentObservation`] and act on the verdict.
#[derive(Debug, Clone)]
struct Candidate {
    repo: String,
    branch: String,
    target: String,
    /// The pid whose **tree** we terminate: the tmux pane root when windowed (so shells + the
    /// harness + any grandchildren all die), else the topmost harness process itself.
    root_pid: i32,
    harness: Option<String>,
    window_id: Option<String>,
    has_live_window: bool,
    is_orchestrator: bool,
    worktree_path: PathBuf,
}

/// Options for a reap pass.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReapOpts {
    /// Act (kill + prune). `false` ⇒ dry-run (the default): report what *would* be reaped.
    pub kill: bool,
    /// Override the working / uncommitted / grace safety gates (never the orchestrator guard).
    pub force: bool,
    /// Idle-grace seconds before a stale agent is eligible.
    pub min_idle_secs: i64,
    /// Emit the JSON envelope instead of the human summary.
    pub as_json: bool,
    /// Query PR state per candidate (a network `gh`/`az` call each). Off for the fast opportunistic
    /// sweep; on for scheduled cleanup.
    pub check_pr: bool,
    /// Suppress the human summary entirely (for the opportunistic sweep folded into other commands).
    pub quiet: bool,
}

impl Default for ReapOpts {
    fn default() -> Self {
        ReapOpts {
            kill: false,
            force: false,
            min_idle_secs: fleet_gc::DEFAULT_MIN_IDLE_SECS,
            as_json: false,
            check_pr: true,
            quiet: false,
        }
    }
}

// --- pure helpers (unit-tested) --------------------------------------------

/// Parse one `ps -axo pid=,ppid=,command=` row → `(pid, ppid, command)`. `None` on a malformed line.
pub(crate) fn parse_ps_line(line: &str) -> Option<Proc> {
    let t = line.trim();
    let (pid_s, rest) = t.split_once(char::is_whitespace)?;
    let (ppid_s, cmd) = rest.trim_start().split_once(char::is_whitespace)?;
    Some(Proc {
        pid: pid_s.parse().ok()?,
        ppid: ppid_s.parse().ok()?,
        command: cmd.trim_start().to_string(),
    })
}

/// Parse the full `ps` dump.
pub(crate) fn parse_ps(out: &str) -> Vec<Proc> {
    out.lines().filter_map(parse_ps_line).collect()
}

/// Parse `lsof -d cwd -Fpn` field output → `(pid, cwd)` per process. `p<pid>` lines set the current
/// pid; `n<path>` lines record its cwd.
// On Linux the binary reads cwds from /proc, so this lsof parser is only reached on non-Linux
// targets (and by unit tests); allow it to be unused in the Linux `bin` build.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) fn parse_lsof_cwds(out: &str) -> Vec<(i32, PathBuf)> {
    let mut cur: Option<i32> = None;
    let mut rows = Vec::new();
    for line in out.lines() {
        match line.as_bytes().first() {
            Some(b'p') => cur = line[1..].trim().parse().ok(),
            Some(b'n') => {
                if let Some(pid) = cur {
                    let path = &line[1..];
                    if !path.is_empty() {
                        rows.push((pid, PathBuf::from(path)));
                    }
                }
            }
            _ => {}
        }
    }
    rows
}

/// Every pid in `root`'s process tree (including `root`), via a breadth-first walk of the
/// `(pid, ppid)` edges. Pure so the tree walk is testable without a live process table.
pub(crate) fn descendants(procs: &[(i32, i32)], root: i32) -> Vec<i32> {
    let mut children: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    for &(pid, ppid) in procs {
        children.entry(ppid).or_default().push(pid);
    }
    let mut out = vec![root];
    let mut queue = vec![root];
    let mut seen: BTreeSet<i32> = [root].into_iter().collect();
    while let Some(p) = queue.pop() {
        for &c in children.get(&p).map(Vec::as_slice).unwrap_or(&[]) {
            if seen.insert(c) {
                out.push(c);
                queue.push(c);
            }
        }
    }
    out
}

/// The basename of a path-like token (the part after the last `/`).
fn basename(tok: &str) -> &str {
    tok.rsplit('/').next().unwrap_or(tok)
}

/// Whether a command line launches an agent harness — argv carries a `copilot` or `claude`
/// executable token. Matches `node /opt/homebrew/bin/copilot …` and the packaged
/// `…/copilot-darwin-arm64/copilot …`, but **not** an agent-spawned subprocess whose name merely
/// contains the string (e.g. `copilot-detached-supervisor …`), so a reaper never mistakes a build
/// step for the harness itself.
pub(crate) fn is_harness_command(cmd: &str) -> Option<&'static str> {
    for tok in cmd.split_whitespace() {
        match basename(tok) {
            "copilot" => return Some("copilot"),
            "claude" => return Some("claude"),
            _ => {}
        }
    }
    None
}

/// Map a `pr_state` string (`MERGED`/`OPEN`/`CLOSED`/`NONE`) to a [`PrStatus`]. Anything else —
/// including the `NONE` a failed lookup degrades to — reads as [`PrStatus::None`], which never
/// triggers a reap.
pub(crate) fn map_pr_status(state: &str) -> PrStatus {
    match state.trim().to_ascii_uppercase().as_str() {
        "MERGED" => PrStatus::Merged,
        "CLOSED" => PrStatus::Closed,
        "OPEN" => PrStatus::Open,
        _ => PrStatus::None,
    }
}

// --- OS captures -----------------------------------------------------------

/// The process table: `pid ppid command` for every process. Empty on failure.
pub(crate) fn ps_all() -> Vec<Proc> {
    let out = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
        .ok();
    out.filter(|o| o.status.success())
        .map(|o| parse_ps(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// Every process's current working directory. Linux reads `/proc/<pid>/cwd`; elsewhere a single
/// `lsof -d cwd` pass. Best-effort — empty when the tool is missing (windowed agents still resolve
/// via their pane cwd).
pub(crate) fn process_cwds() -> Vec<(i32, PathBuf)> {
    #[cfg(target_os = "linux")]
    {
        let mut rows = Vec::new();
        if let Ok(rd) = std::fs::read_dir("/proc") {
            for e in rd.flatten() {
                if let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() {
                    if let Ok(cwd) = std::fs::read_link(e.path().join("cwd")) {
                        rows.push((pid, cwd));
                    }
                }
            }
        }
        rows
    }
    #[cfg(not(target_os = "linux"))]
    {
        let out = std::process::Command::new("lsof")
            .args(["-n", "-P", "-d", "cwd", "-Fpn"])
            .output()
            .ok();
        out.filter(|o| o.status.success())
            .map(|o| parse_lsof_cwds(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_default()
    }
}

/// Every tmux pane across every session.
fn read_panes() -> Vec<Pane> {
    // No `#{session_name}`: the reaper no longer has any question a session name answers. It reads
    // the worktree a pane sits in, which is what actually says whose agent this is.
    let fmt = "#{window_id}\t#{window_name}\t#{pane_pid}\t#{pane_dead}\t#{pane_current_path}";
    let Some(out) = tmux_raw(&["list-panes", "-a", "-F", fmt]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 5 {
                return None;
            }
            Some(Pane {
                window_id: f[0].to_string(),
                pane_pid: f[2].parse().unwrap_or(0),
                dead: f[3] == "1",
                cwd: f[4].to_string(),
            })
        })
        .collect()
}

/// The last non-empty lines of a window's active pane — the live "is it mid-turn?" glance.
fn capture_tail(target: &str, n: usize) -> Vec<String> {
    let t = crate::proc::exact_target(target);
    let Some(out) = tmux_raw(&["capture-pane", "-p", "-t", &t]) else {
        return Vec::new();
    };
    let lines: Vec<String> = out
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].to_vec()
}

/// SIGTERM the whole set, wait up to ~2s, then SIGKILL the survivors. Returns how many pids were
/// alive to begin with. Reuses the graceful→forceful primitives in `cmd::killport`.
fn kill_tree(pids: &[i32]) -> usize {
    let mut alive: Vec<i32> = pids
        .iter()
        .copied()
        .filter(|p| process_exists(*p))
        .collect();
    let started = alive.len();
    for pid in &alive {
        let _ = send_signal(*pid, signal_term());
    }
    for _ in 0..4 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        alive.retain(|p| process_exists(*p));
        if alive.is_empty() {
            break;
        }
    }
    for pid in &alive {
        let _ = send_signal(*pid, signal_kill());
    }
    started
}

/// The `(pid, ppid)` edges of the live process table — the input to [`descendants`].
fn ps_edges() -> Vec<(i32, i32)> {
    ps_all().iter().map(|p| (p.pid, p.ppid)).collect()
}

/// The live (non-dead) pane pids of a specific tmux window.
fn live_pane_pids_of_window(window_id: &str) -> Vec<i32> {
    let Some(out) = tmux_raw(&[
        "list-panes",
        "-t",
        window_id,
        "-F",
        "#{pane_pid} #{pane_dead}",
    ]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|line| {
            let (pid, dead) = line.split_once(' ')?;
            (dead == "0")
                .then(|| pid.parse::<i32>().ok())
                .flatten()
                .filter(|p| *p > 0)
        })
        .collect()
}

/// Terminate the **full process tree** rooted at each live pane of the given windows — the piece
/// `bn wt cleanup` was missing (#503 gap 2): with `remain-on-exit on` a killed window leaves the
/// pane's process reparented and alive, so cleanup must kill the tree, not just the window. The
/// window the caller is sitting in (our own shell) is always skipped, so an interactive `bn close`
/// from inside a worktree never shoots its own shell out mid-cleanup. Returns how many processes
/// were signalled.
pub(crate) fn kill_window_trees(wins: &[String]) -> usize {
    if wins.is_empty() {
        return 0;
    }
    let cur = current_window_id();
    let edges = ps_edges();
    let mut killed = 0;
    for wid in wins {
        if cur.as_deref() == Some(wid.as_str()) {
            continue; // never tree-kill the window we're running in
        }
        for pane_pid in live_pane_pids_of_window(wid) {
            killed += kill_tree(&descendants(&edges, pane_pid));
        }
    }
    killed
}

/// The tmux window id the caller is attached to, or `None` when not inside tmux.
fn current_window_id() -> Option<String> {
    std::env::var("TMUX")
        .ok()
        .and_then(|_| tmux_raw(&["display-message", "-p", "#{window_id}"]))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// --- discovery + reconciliation --------------------------------------------

/// The agent identity and checkout root for a process's working directory: `(repo, branch, root)`,
/// or `None` when the cwd isn't inside a managed worktree at all.
///
/// Two layouts answer to this. A conventional worktree spells its identity in the path
/// (`<worktree_dir>/<repo>/<branch>`). A **pool slot** does not — `_pool/3` names a slot that many
/// branches pass through — so its tenant comes from the pool ledger. Reconstructing the identity
/// from the path alone used to yield the branch `"_pool"` for every pooled agent, which is now the
/// whole fleet: one meaningless identity that no PR lookup and no fleet-health row could ever
/// match, so the reaper's verdicts for pooled agents were noise.
fn agent_at(worktree_dir: &Path, cwd: &Path) -> Option<(String, String, PathBuf)> {
    if let Some((repo, branch)) = fleet_gc::worktree_repo_branch(worktree_dir, cwd) {
        let root = worktree_dir.join(&repo).join(&branch);
        return Some((repo, branch, root));
    }
    let (repo, slot) = fleet_gc::pool_slot(worktree_dir, cwd)?;
    let root = worktree_dir
        .join(&repo)
        .join(fleet_gc::POOL_DIR_NAME)
        .join(slot.to_string());
    let branch = crate::wt_pool::slot_branch(&root)?;
    Some((repo, branch, root))
}

/// Whether the worktree carries uncommitted (dirty) or unpushed work — either spares an agent from
/// a non-forced reap so nothing in flight is discarded.
fn has_wip(wt: &Path) -> bool {
    let wt_s = wt.to_string_lossy();
    if crate::wt::dirty_count(&wt_s) > 0 {
        return true;
    }
    // Unpushed commits: prefer the tracking branch, fall back to ahead-of-default (via wt helper).
    if let Some(n) = crate::wt::git_out(&["-C", &wt_s, "rev-list", "--count", "@{upstream}..HEAD"])
        .and_then(|s| s.trim().parse::<u32>().ok())
    {
        return n > 0;
    }
    crate::wt::commits_ahead(wt) > 0
}

/// PR status for a worktree's branch (a network call). [`PrStatus::None`] when the worktree/branch
/// or remote can't be resolved — never a reap trigger on its own.
fn pr_status_for(wt: &Path, branch: &str) -> PrStatus {
    let Some(bare) = crate::wt::find_bare_repo(wt) else {
        return PrStatus::None;
    };
    let remote = crate::wt::remote_for(&bare);
    if remote == crate::wt::Remote::Other {
        return PrStatus::None;
    }
    map_pr_status(&crate::wt::pr_state(&remote, branch))
}

/// Observe one **already-identified** agent for [`fleet_gc::classify`] — the same signals
/// [`reconcile`] gathers per discovered candidate, for a caller that already knows who and where
/// the agent is (`wt pool return --force`, deciding whether a slot's occupant is finished).
///
/// Keeping this beside `reconcile` is the point: "is this agent still needed?" must have one
/// answer, whether it is asked by the fleet GC or by the pool.
pub(crate) fn observe_agent(
    cfg: &Config,
    worktree: &Path,
    repo: &str,
    branch: &str,
    tmux_target: &str,
) -> AgentObservation {
    let exists = worktree.is_dir();
    let health = bn_core::agent::supervise(cfg, DEFAULT_IDLE_MINS, DEFAULT_STUCK_MINS)
        .agents
        .into_iter()
        .find(|agent| agent.repo == repo && agent.branch == branch)
        .map(|agent| (agent.health, agent.idle_secs));
    AgentObservation {
        has_live_window: true,
        worktree_exists: exists,
        pr: if exists && !branch.is_empty() {
            pr_status_for(worktree, branch)
        } else {
            PrStatus::None
        },
        is_working: is_working(tmux_target, true, health.map(|(h, _)| h)),
        has_uncommitted: exists && has_wip(worktree),
        attended: is_attended(None, tmux_target, true),
        is_orchestrator: false,
        // Unknown idle reads as freshly active, so the grace window protects the agent — the same
        // conservative default `reconcile` uses for a live-windowed agent the fleet hasn't logged.
        idle_secs: health.map(|(_, idle)| idle).unwrap_or(0),
    }
}

/// Discover every live agent rooted in a managed worktree, resolving each to the process tree we'd
/// reap and whether it still has a live window.
fn discover(worktree_dir: &Path) -> Vec<Candidate> {
    let procs = ps_all();
    let cwds: BTreeMap<i32, PathBuf> = process_cwds().into_iter().collect();
    let panes = read_panes();

    // Live (non-dead) pane roots, and the set of targets that still have a live window.
    let live_panes: Vec<&Pane> = panes.iter().filter(|p| !p.dead && p.pane_pid > 0).collect();
    let live_pane_pids: BTreeSet<i32> = live_panes.iter().map(|p| p.pane_pid).collect();
    let mut live_targets: BTreeSet<String> = BTreeSet::new();
    for p in &live_panes {
        if let Some((r, b, _)) = agent_at(worktree_dir, Path::new(&p.cwd)) {
            live_targets.insert(target_key(&r, &b));
        }
    }

    let by_pid: BTreeMap<i32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut out: Vec<Candidate> = Vec::new();

    // Iterate every harness process (not just tree roots): the `node …/copilot` wrapper and its
    // packaged-binary child can report *different* cwds (the wrapper stays where it launched, the
    // worker chdir's into the session's worktree), so keying only on the wrapper would miss the
    // real worktree. Each harness proc that maps to a managed worktree yields a candidate; the
    // per-`root_pid` dedupe below collapses the wrapper+child of one agent into a single row.
    for proc in procs
        .iter()
        .filter(|p| is_harness_command(&p.command).is_some())
    {
        let harness = is_harness_command(&proc.command).unwrap_or("copilot");
        // The harness's cwd: prefer the process table, fall back to a containing pane's cwd.
        let cwd = cwds.get(&proc.pid).cloned().or_else(|| {
            panes
                .iter()
                .find(|pane| pane.pane_pid == proc.pid)
                .map(|pane| PathBuf::from(&pane.cwd))
        });
        let Some(cwd) = cwd else { continue };
        let Some((repo, branch, worktree_path)) = agent_at(worktree_dir, &cwd) else {
            continue; // not rooted in a managed worktree — leave it entirely alone.
        };
        if bn_core::agent::is_reserved_branch(&branch) {
            continue; // never reap the repo's main worktree / its agent
        }

        // Walk up the ppid chain: if we hit a live pane root, this agent is windowed — reap that
        // pane's whole tree; else it's a windowless straggler — reap the topmost harness ancestor's
        // tree (so the `node` wrapper dies with its worker child, not just the child).
        let (root_pid, window_id) = resolve_root(proc.pid, &by_pid, &live_pane_pids, &live_panes);
        let target = target_key(&repo, &branch);
        let has_live_window = window_id.is_some() || live_targets.contains(&target);

        // Orchestrator guard: the controller is sacrosanct (#345). Asked of the *worktree*, not of
        // the tmux session that happens to host it. The lease layer already enforces the two
        // directions of this — a `Control` lease may live only in `_pool/1`, and nothing else may
        // live in it (`validate_kind_placement`) — so "this checkout is the controller slot" and
        // "this is the controller" are the same statement, and it stays true for a straggler whose
        // window has gone. The old check read the session name, which needed a live window to
        // resolve and therefore silently failed to protect exactly the windowless case.
        //
        // We deliberately do NOT sniff the command for "orchestrate" — a dispatched agent's seed
        // prompt (passed as `copilot -i <prompt>`) routinely contains the word, and a harness
        // process is never the orchestrator daemon itself anyway (that's the `bn orchestrate`
        // process that spawns them).
        let is_orchestrator = bn_core::agent::is_controller_slot_path(&worktree_path);

        out.push(Candidate {
            repo,
            branch,
            target,
            root_pid,
            harness: Some(harness.to_string()),
            window_id,
            has_live_window,
            is_orchestrator,
            worktree_path,
        });
    }

    // Dedupe: the wrapper + worker of one agent (and any duplicate harness procs) collapse onto a
    // single row per process tree we'd kill.
    out.sort_by(|a, b| a.root_pid.cmp(&b.root_pid).then(a.target.cmp(&b.target)));
    out.dedup_by(|a, b| a.root_pid == b.root_pid);
    out
}

/// Resolve the tree root to reap for a harness process. Walk up the ppid chain: the first **live
/// tmux pane** root found means the agent is windowed → reap that pane's whole tree (shell +
/// harness + children) and report its window id. With no pane in the chain the agent is a
/// windowless straggler → reap the **topmost harness ancestor** (so the `node` wrapper is killed
/// alongside its worker child, leaving nothing reparented).
fn resolve_root(
    harness_pid: i32,
    by_pid: &BTreeMap<i32, &Proc>,
    live_pane_pids: &BTreeSet<i32>,
    live_panes: &[&Pane],
) -> (i32, Option<String>) {
    let mut cur = harness_pid;
    let mut topmost_harness = harness_pid;
    let mut guard = 0;
    loop {
        if live_pane_pids.contains(&cur) {
            let wid = live_panes
                .iter()
                .find(|p| p.pane_pid == cur)
                .map(|p| p.window_id.clone());
            return (cur, wid);
        }
        let Some(p) = by_pid.get(&cur) else { break };
        if is_harness_command(&p.command).is_some() {
            topmost_harness = cur;
        }
        if p.ppid <= 1 || guard > 64 {
            break;
        }
        cur = p.ppid;
        guard += 1;
    }
    (topmost_harness, None)
}

/// The lowercase `repo:branch` join key (matches `agent_live` / `supervise`).
fn target_key(repo: &str, branch: &str) -> String {
    format!("{}:{}", repo.to_lowercase(), branch.to_lowercase())
}

/// Windows whose panes are **all** dead (`remain-on-exit` leftovers) — safe to prune outright: the
/// process already exited, so there's nothing to kill, just an inspectable husk to clear.
fn dead_windows(panes: &[Pane]) -> Vec<String> {
    let mut alive: BTreeSet<&str> = BTreeSet::new();
    let mut all: BTreeSet<&str> = BTreeSet::new();
    for p in panes {
        all.insert(&p.window_id);
        if !p.dead {
            alive.insert(&p.window_id);
        }
    }
    all.difference(&alive).map(|s| s.to_string()).collect()
}

/// Is a live-windowed agent mid-turn? The definitive spinner check (reuses the orchestrate pane
/// classifier), so a working agent is never surprise-killed. Windowless agents have no pane to
/// read → never "working".
fn is_working(target: &str, has_live_window: bool, health: Option<AgentHealth>) -> bool {
    if !has_live_window {
        return false;
    }
    let tail = capture_tail(target, 6);
    if !tail.is_empty() {
        let obs = PaneObservation {
            tail: &tail,
            has_window: true,
            plane_a: "",
            has_wip: false,
        };
        if classify_pane(&obs) == PaneState::Working {
            return true;
        }
    }
    // Fall back to fleet health when the pane tail is empty/ambiguous.
    health == Some(AgentHealth::Active)
}

/// Is a human at this agent's window — now, or within the decay window? The guard that keeps a GC
/// pass from pulling a pane out from under whoever is reading it: the one signal the work-preserving
/// guards (`is_working`, `has_uncommitted`) can't see, since reviewing produces neither a spinner nor
/// a dirty tree. No live window ⇒ nothing to watch.
///
/// Asks by **window id** whenever [`discover`] resolved one, falling back to the `repo:branch`
/// window name. The id is what makes this work for a pooled agent: its window is named for the pool
/// slot it borrowed, not for the branch it holds, so the name-based lookup would quietly miss — and
/// answering "unattended" by accident is exactly the failure this guard exists to prevent.
fn is_attended(window_id: Option<&str>, target: &str, has_live_window: bool) -> bool {
    has_live_window && bn_core::tmux::window_attended(window_id.unwrap_or(target))
}

/// One reconciled row: the candidate, its observation, and the verdict.
struct Verdict {
    candidate: Candidate,
    obs: AgentObservation,
    disposition: Disposition,
}

/// Build observations + verdicts for every discovered agent.
fn reconcile(cfg: &Config, worktree_dir: &Path, opts: &ReapOpts) -> Vec<Verdict> {
    let fleet = bn_core::agent::supervise(cfg, DEFAULT_IDLE_MINS, DEFAULT_STUCK_MINS);
    let health: BTreeMap<String, (AgentHealth, i64)> = fleet
        .agents
        .iter()
        .map(|a| (target_key(&a.repo, &a.branch), (a.health, a.idle_secs)))
        .collect();
    // The second half of the orchestrator guard. `discover` answers for the controller by its pool
    // slot, but a `bn dispatch --orchestrator` agent runs the wave from an ordinary slot on a real
    // branch, so placement cannot see it. Its role can: the fleet stamps `AgentRole::Orchestrator`
    // from the machine's own attribution log, which outlives the window that used to carry the
    // answer in its session name.
    let orchestrators: BTreeSet<String> = fleet
        .agents
        .iter()
        .filter(|a| a.role == bn_core::agent::AgentRole::Orchestrator)
        .map(|a| target_key(&a.repo, &a.branch))
        .collect();

    let policy = ReapPolicy {
        force: opts.force,
        min_idle_secs: opts.min_idle_secs,
    };

    discover(worktree_dir)
        .into_iter()
        .map(|c| {
            let exists = c.worktree_path.is_dir();
            let fleet_hit = health.get(&c.target);
            let idle_secs = match fleet_hit {
                Some((_, idle)) => *idle,
                None if c.has_live_window => 0, // fresh launch, no log yet → grace protects it
                None => STALE_IDLE_SENTINEL,    // windowless + unknown to the fleet → long-quiet
            };
            let working = is_working(&c.target, c.has_live_window, fleet_hit.map(|(h, _)| *h));
            let uncommitted = exists && has_wip(&c.worktree_path);
            let pr = if opts.check_pr && exists {
                pr_status_for(&c.worktree_path, &c.branch)
            } else {
                PrStatus::None
            };
            let obs = AgentObservation {
                has_live_window: c.has_live_window,
                worktree_exists: exists,
                pr,
                is_working: working,
                has_uncommitted: uncommitted,
                attended: is_attended(c.window_id.as_deref(), &c.target, c.has_live_window),
                is_orchestrator: c.is_orchestrator || orchestrators.contains(&c.target),
                idle_secs,
            };
            let disposition = fleet_gc::classify(&obs, &policy);
            Verdict {
                candidate: c,
                obs,
                disposition,
            }
        })
        .collect()
}

/// Run a stale-agent reconciliation pass.
/// Returns a process exit code (always 0 — a GC pass with nothing to do is success).
pub(crate) fn run_reap(cfg: &Config, opts: ReapOpts) -> i32 {
    let (_, worktree_dir) = crate::wt::layout(cfg);
    let verdicts = reconcile(cfg, &worktree_dir, &opts);
    let panes = read_panes();
    let husks = dead_windows(&panes);

    let mut reaped = 0u32;
    let mut pruned = 0u32;

    // Act: reap stale process trees, then prune their (now-dead) windows.
    if opts.kill {
        let edges: Vec<(i32, i32)> = ps_all().iter().map(|p| (p.pid, p.ppid)).collect();
        for v in verdicts.iter().filter(|v| v.disposition.is_reap()) {
            let tree = descendants(&edges, v.candidate.root_pid);
            kill_tree(&tree);
            if let Some(wid) = &v.candidate.window_id {
                warn_unreachable("reap", tmux_run_try(&["kill-window", "-t", wid]));
            }
            reaped += 1;
        }
        // Prune remain-on-exit husks (independent of any live process).
        for wid in &husks {
            warn_unreachable("reap", tmux_run_try(&["kill-window", "-t", wid]));
            pruned += 1;
        }
    } else {
        reaped = verdicts.iter().filter(|v| v.disposition.is_reap()).count() as u32;
        pruned = husks.len() as u32;
    }

    if opts.as_json {
        emit_json(&verdicts, &husks, &opts, reaped, pruned);
    } else if !opts.quiet {
        emit_human(&verdicts, &husks, &opts, reaped, pruned);
    }
    0
}

/// The fast opportunistic sweep folded into `bn dispatch` / `bn next`: prune dead windows and reap
/// clearly-stale stragglers (window/worktree gone), skipping the per-candidate PR network calls.
/// Safe by default — never touches working / WIP / orchestrator agents. Best-effort and quiet.
pub(crate) fn sweep(cfg: &Config) {
    // Best-effort and test-safe: skip entirely when there's no managed worktree root (a test
    // config points it at /nonexistent), so a sweep never reads the real fleet from unit tests.
    let (_, worktree_dir) = crate::wt::layout(cfg);
    if !worktree_dir.is_dir() {
        return;
    }
    // A slightly wider grace than the default so a just-detached window isn't reaped by a sweep
    // that fires the instant after a dispatch.
    run_reap(
        cfg,
        ReapOpts {
            kill: true,
            force: false,
            min_idle_secs: fleet_gc::DEFAULT_MIN_IDLE_SECS,
            as_json: false,
            check_pr: false,
            quiet: true,
        },
    );
}

/// Launch the opportunistic sweep in the background, so `bn next` keeps its read path under a
/// second and the cleanup runs out of band.
pub(crate) fn launch_next_sweep() {
    if cfg!(test) {
        return;
    }
    let now = bn_core::util::now_epoch_secs();
    let path = next_sweep_launch_path();
    let last = read_launch_stamp(&path);
    if !should_launch_next_sweep(last, now) {
        return;
    }
    if let Err(e) = std::fs::write(&path, now.to_string()) {
        eprintln!(
            "bn: could not record next-sweep launch time at {}: {e}",
            path.display()
        );
    }
    match bn_core::bn_subprocess::spawn_detached(&["run", "next-sweep"]) {
        Ok(_) => {}
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            eprintln!("bn: could not schedule background fleet sweep: {e}");
        }
    }
}

const NEXT_SWEEP_LAUNCH_STALE_SECS: i64 = 60;

fn next_sweep_launch_path() -> PathBuf {
    bn_core::resolver::state_dir().join("next-sweep.last")
}

fn read_launch_stamp(path: &Path) -> Option<i64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<i64>().ok())
}

pub(crate) fn should_launch_next_sweep(last_launch: Option<i64>, now: i64) -> bool {
    last_launch.is_none_or(|ts| now.saturating_sub(ts) >= NEXT_SWEEP_LAUNCH_STALE_SECS)
}

// --- output ----------------------------------------------------------------

fn row_json(v: &Verdict) -> serde_json::Value {
    json!({
        "repo": v.candidate.repo,
        "branch": v.candidate.branch,
        "target": v.candidate.target,
        "harness": v.candidate.harness,
        "pid": v.candidate.root_pid,
        "window_id": v.candidate.window_id,
        "has_live_window": v.obs.has_live_window,
        "worktree_exists": v.obs.worktree_exists,
        "pr": v.obs.pr,
        "is_working": v.obs.is_working,
        "has_uncommitted": v.obs.has_uncommitted,
        "is_orchestrator": v.obs.is_orchestrator,
        "idle_secs": v.obs.idle_secs,
        "disposition": v.disposition,
        "reason": v.disposition.reason_label(),
    })
}

fn emit_json(verdicts: &[Verdict], husks: &[String], opts: &ReapOpts, reaped: u32, pruned: u32) {
    let data = json!({
        "acted": opts.kill,
        "force": opts.force,
        "reaped": reaped,
        "dead_windows_pruned": pruned,
        "dead_windows": husks,
        "agents": verdicts.iter().map(row_json).collect::<Vec<_>>(),
    });
    println!("{}", Envelope::success("fleet reap", data).to_json());
}

fn emit_human(verdicts: &[Verdict], husks: &[String], opts: &ReapOpts, reaped: u32, pruned: u32) {
    use crate::wt::{GREEN, NC, YELLOW};
    let verb = if opts.kill { "reaped" } else { "would reap" };
    let mut any = false;
    for v in verdicts {
        match v.disposition {
            Disposition::Reap { reason } => {
                any = true;
                println!(
                    "{YELLOW}{verb} {} (pid {}, {}){NC}",
                    v.candidate.target,
                    v.candidate.root_pid,
                    reason.label()
                );
            }
            Disposition::Keep { reason } => {
                // Only surface kept agents that had a stale signal (so the reason is interesting).
                if !matches!(reason, bn_core::fleet_gc::KeepReason::Healthy) {
                    println!(
                        "  keep {} (pid {}, {})",
                        v.candidate.target,
                        v.candidate.root_pid,
                        reason.label()
                    );
                }
            }
        }
    }
    if !husks.is_empty() {
        let pverb = if opts.kill { "pruned" } else { "would prune" };
        println!("{pverb} {} dead tmux window(s)", husks.len());
    }
    if !any && husks.is_empty() {
        println!("{GREEN}fleet clean — nothing to reap{NC}");
    } else if opts.kill {
        println!("{GREEN}reaped {reaped} agent(s), pruned {pruned} dead window(s){NC}");
    } else {
        println!("{reaped} reapable, {pruned} dead window(s) — pass --kill to act");
    }
}

#[cfg(test)]
mod orchestrator_guard_tests {
    use bn_core::agent::is_controller_slot_path;
    use std::path::Path;

    /// The controller is the one agent a reap must never take, force or not. It is identified by
    /// where it runs — `_pool/1`, the slot the lease layer reserves for it and refuses to anyone
    /// else — rather than by the name of the tmux session that happens to host its window.
    #[test]
    fn the_controller_slot_is_what_marks_an_orchestrator() {
        assert!(is_controller_slot_path(Path::new(
            "/home/me/projects/worktree/bn/_pool/1"
        )));
        assert!(!is_controller_slot_path(Path::new(
            "/home/me/projects/worktree/bn/_pool/2"
        )));
        assert!(!is_controller_slot_path(Path::new(
            "/home/me/projects/worktree/bn/main"
        )));
    }

    /// The regression the old check could not cover: it resolved the session through a live window,
    /// so a controller whose window had gone read as an ordinary agent and became reapable. A path
    /// answers whether or not anything is still on screen.
    #[test]
    fn a_windowless_controller_is_still_protected() {
        // No tmux state consulted at all — that is the point.
        assert!(is_controller_slot_path(Path::new(
            "/home/me/projects/worktree/BigMono/_pool/1"
        )));
    }
}

#[cfg(test)]
mod next_sweep_launch_tests {
    use super::should_launch_next_sweep;

    #[test]
    fn a_recent_launch_is_throttled() {
        assert!(!should_launch_next_sweep(Some(1_000), 1_030));
    }

    #[test]
    fn an_old_or_missing_launch_is_allowed() {
        assert!(should_launch_next_sweep(None, 1_030));
        assert!(should_launch_next_sweep(Some(900), 1_030));
    }
}

#[cfg(test)]
#[path = "reap_tests.rs"]
mod kill_path_tests;
