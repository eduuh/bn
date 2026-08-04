//! `bn doctor` — the attention list. A deterministic health check that aggregates everything
//! wanting a nudge into one ranked list: untouched/blocked/goal-less local notes (filesystem),
//! failing cron jobs (the event log), attention items raised by hand, by a script, or by an agent
//! (`bn attn`), and pressing todos (overdue / priority A) from the current repo's `TODO.txt`
//! (`bn task`). All read-time and offline — no network — so it runs free in cron or at session
//! start. Each source renders as its own `Finding` variant; the list is ordered by priority then
//! age, so what's actively broken floats to the top.

use crate::action_rank::{self, Rank};
use crate::attention::{self, AttnItem};
use crate::events;
use crate::model::Note;
use crate::render;
use crate::resolver::Config;
use crate::rollups::all_notes;
use crate::util::idle_days_since;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default staleness threshold (days) for flagging an untouched note — the `bn doctor --stale-days`
/// default, shared so the `stale-branches` findings gatherer ([`crate::findings_gather`]) and the
/// CLI can't drift apart.
pub const DEFAULT_STALE_DAYS: i64 = 7;

/// One local note wanting attention, with the reasons why.
#[derive(Debug, Serialize, Deserialize)]
pub struct NoteFinding {
    pub repo: String,
    pub branch: String,
    pub path: String,
    /// Why it surfaced: e.g. `stale:14d`, `no_goal`, `blockers:2`.
    pub flags: Vec<String>,
    pub open_todos: usize,
    pub blockers: usize,
    pub last_activity: String,
    pub idle_days: i64,
}

/// A cron job whose latest run failed — what's broken on the machine right now.
#[derive(Debug, Serialize, Deserialize)]
pub struct CronFinding {
    pub job: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    /// Total failed runs on record for this job (context for how chronic it is).
    pub failures: usize,
    /// Epoch seconds of the (failed) latest run.
    pub last_run: i64,
}

/// A staff agent whose most recent scheduled run never happened (#273) — silence that is *death*,
/// not a by-design quiet day (which records an `ok-silent` run, see [`crate::staff_schedule`]).
/// Detected read-only by joining the roster's declared schedule with the run ledger, so it is
/// flagged *once* per missed slot by construction: doctor recomputes it each invocation from the
/// same inputs rather than raising a fresh item, and a slot stops surfacing the moment a run (of
/// any status) is recorded at or after it. A skip or a silent run both write a ledger row, so
/// neither is ever mistaken for a miss.
#[derive(Debug, Serialize, Deserialize)]
pub struct StaffFinding {
    pub agent: String,
    pub on_calendar: String,
    /// Epoch seconds of the missed scheduled slot.
    pub scheduled: i64,
    /// Epoch seconds of the agent's last recorded run, or `0` if it has never run.
    pub last_run: i64,
    /// Seconds the scheduled run is overdue (`now − scheduled`) — how long the silence has lasted.
    pub overdue_secs: i64,
}

/// A hand-/script-/agent-raised attention item (see `bn attn`).
#[derive(Debug, Serialize, Deserialize)]
pub struct AttnFinding {
    pub id: String,
    pub title: String,
    pub severity: String,
    pub source: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub link: Option<String>,
    /// Rich multi-line context for the detail/second view (carried from the attention item).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A pressing item from the current repo's `TODO.txt` (see `bn task`) — an open todo that is
/// overdue or priority A. Ordinary open todos stay in `bn task`; only the pressing ones nag here,
/// so doctor keeps signal.
#[derive(Debug, Serialize, Deserialize)]
pub struct TodoFinding {
    pub id: String,
    pub summary: String,
    pub repo: String,
    /// A | B | C, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<char>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// True when `due` is on or before today.
    pub overdue: bool,
    /// Days past `due` (0 when due today or not due-driven).
    pub overdue_days: i64,
}

/// A `bn q` capture sitting in the inbox past the triage window. The inbox is a landing
/// zone, never a home (same philosophy as stale-note hygiene) — rollover and focus-end
/// drains are the normal path out, so doctor only nags when draining has stopped.
#[derive(Debug, Serialize, Deserialize)]
pub struct InboxFinding {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub created: String,
    pub idle_days: i64,
}

/// An MCP server configured in the current repo that the agent harness couldn't use: either a
/// local command PATH can't resolve, or an HTTP endpoint that isn't reachable. The bn server
/// itself is a stdio launcher (`bn mcp serve`), so it's checked as a PATH command like any other.
/// `fix` is the exact one-line remedy (#323, #640).
#[derive(Debug, Serialize, Deserialize)]
pub struct McpFinding {
    /// The server's name in the config (e.g. `time`, `bn`).
    pub server: String,
    /// The unresolvable command (e.g. `uvx`) or the unreachable endpoint url.
    pub command: String,
    /// Repo-relative config file that references it (e.g. `.mcp.json`).
    pub config: String,
    /// Short phrase describing what's wrong (e.g. `not on PATH`, `unreachable`).
    pub problem: String,
    /// One-line fix, e.g. "install uv to get uvx — e.g. `brew install uv`".
    pub fix: String,
}

/// The whole agent staff has gone silent (#270): no card written anywhere in the synced,
/// person-scoped feed for more than [`crate::staff::STALE_STAFF_DAYS`]. Because the feed is the
/// one cross-machine signal, this fires identically on every box — it means the single home
/// machine ([`crate::resolver::Config::staff_home`]) has likely been down. Manual re-home onto a
/// live box is the v1 fix. Distinct from [`StaffFinding`] (#273), which flags a *single* agent
/// that missed its next scheduled slot; this flags the *whole* staff going quiet across machines.
#[derive(Debug, Serialize, Deserialize)]
pub struct StaleStaffFinding {
    /// The newest card date anywhere in the feed (`YYYY-MM-DD`) — when the staff last ran.
    pub last_card: String,
    /// Whole days since that card (always `> STALE_STAFF_DAYS` when this surfaces).
    pub idle_days: i64,
}

/// A cross-machine notes leak (#leak): on a work-classified machine, `personal_root` resolves to
/// a *different* repo than `work_root` and carries branch-note subtrees for machines other than
/// this one. Because the cross-machine views ([`crate::resolver::Config::metrics_logs_all_machines`],
/// [`crate::resolver::Config::each_notes_root`]) read every `<personal_root>/<machine>/…` subtree,
/// those foreign machines' sessions/notes surface on this box — the exact failure mode where a
/// personal-laptop session shows up on a work laptop. Detected read-only by listing the machine
/// subtrees under `personal_root` that carry a metrics log; goes quiet once the roots are
/// collapsed onto the work repo (the fix) or the foreign clone is gone.
#[derive(Debug, Serialize, Deserialize)]
pub struct NotesLeakFinding {
    /// The personal notes root carrying foreign machines' subtrees.
    pub personal_root: String,
    /// The work notes root this machine should route to instead.
    pub work_root: String,
    /// Machine subtrees present under `personal_root` that aren't this machine — the leak set.
    pub foreign_machines: Vec<String>,
}

/// Live orchestrate backlog entries that name no task (#1001) — work that predates the invariant
/// and would therefore execute with no durable record behind it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BacklogFinding {
    /// How many live entries carry no `task:`.
    pub untasked: usize,
    /// A sample of their subjects, for the one-line render.
    pub sample: Vec<String>,
}

/// The `claude`/`copilot` shell wrappers from `bn shell-init` are not driving launches here, so a
/// harness typed in this shell starts outside bn: unleased, unrecorded, and steerable only by
/// whatever the window join can infer. Nothing installs the wrappers automatically (the rc file is
/// usually a symlink into a dotfiles repo), so the failure is silent by construction — which is
/// exactly why it belongs on the attention list rather than in a README.
#[derive(Debug, Serialize, Deserialize)]
pub struct ShellInitFinding {
    /// The login shell the check read (`zsh` / `bash`).
    pub shell: String,
    /// The rc file inspected.
    pub rc_path: String,
    /// What is wrong, as a short phrase.
    pub problem: String,
    /// One-line fix.
    pub fix: String,
}

/// Why the wrappers aren't driving launches — the three distinguishable states, each with a
/// different remedy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellInitProblem {
    /// The rc never sources `bn shell-init`.
    NotInstalled,
    /// The rc sources it, but this shell predates the change (or the line is inside a guard that
    /// didn't run), so nothing is active right now.
    SourcedNotActive,
    /// The rc sources it and then *redefines* `claude`/`copilot`/`yolo` afterwards — the later
    /// definition wins, so the wrapper is loaded and immediately shadowed.
    Shadowed,
}

impl ShellInitProblem {
    /// The short phrase and the one-line remedy, keyed to the shell's rc file.
    pub fn describe(self, shell: &str) -> (&'static str, String) {
        match self {
            ShellInitProblem::NotInstalled => (
                "the claude/copilot wrappers are not installed",
                format!("add to your rc: source <(bn shell-init {shell})"),
            ),
            ShellInitProblem::SourcedNotActive => (
                "sourced in your rc but not active in this shell",
                "start a new shell (or re-source your rc) to pick the wrappers up".to_string(),
            ),
            ShellInitProblem::Shadowed => (
                "a later claude/copilot definition in your rc shadows the wrappers",
                format!(
                    "remove the hand-written claude()/copilot() functions below the \
                     `bn shell-init {shell}` line"
                ),
            ),
        }
    }
}

/// One row on the attention list, tagged by where it came from. Serializes with a `kind`
/// discriminator (`note` | `cron` | `staff` | `attn` | `todo` | `inbox` | `mcp` | `stalestaff` |
/// `notesleak`) and the variant's fields flattened alongside. (The discriminator is `kind`, not
/// `source`, because an `attn` row carries its own `source` field — who raised it — which would
/// otherwise collide with the tag.)
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Finding {
    Note(NoteFinding),
    Cron(CronFinding),
    Staff(StaffFinding),
    Attn(AttnFinding),
    Todo(TodoFinding),
    Inbox(InboxFinding),
    Mcp(McpFinding),
    StaleStaff(StaleStaffFinding),
    NotesLeak(NotesLeakFinding),
    Backlog(BacklogFinding),
    ShellInit(ShellInitFinding),
}

impl Finding {
    /// Primary sort weight — higher floats up. Actively-broken signals (failing cron, urgent
    /// items, blocked work) outrank passive ones (a stale note or lingering inbox item).
    fn rank(&self) -> Rank {
        match self {
            Finding::Cron(_) => action_rank::URGENT,
            // A staff agent that missed its scheduled run is actively broken (a dead brief), on a
            // par with a failing cron job.
            Finding::Staff(_) => action_rank::URGENT,
            Finding::Attn(a) => action_rank::from_severity(&a.severity),
            Finding::Note(n) => {
                if n.blockers > 0 {
                    action_rank::BLOCKED
                } else {
                    action_rank::INFO
                }
            }
            // Overdue todos are actively pressing (a deadline passed) → urgent; a priority-A todo
            // maps to urgent by the severity mapping. Only these two surface, so both rank 3.
            Finding::Todo(_) => action_rank::URGENT,
            // A lingering capture is passive hygiene, like a stale note.
            Finding::Inbox(_) => action_rank::INFO,
            // A configured-but-unresolvable MCP server breaks the next agent boot here — worth a
            // nag ahead of passive hygiene, below actively-failing signals.
            Finding::Mcp(_) => action_rank::WARN,
            // A silent staff likely means its home machine is down — an operational problem worth
            // surfacing above passive hygiene, below what's actively breaking work right now.
            Finding::StaleStaff(_) => action_rank::WARN,
            // A cross-machine notes leak means another box's data is physically present and being
            // read here — an operational misconfig, on a par with the other WARN-band signals.
            Finding::NotesLeak(_) => action_rank::WARN,
            // Passive: the work still runs, it just has no record. Worth clearing, not urgent.
            Finding::Backlog(_) => action_rank::WARN,
            // Inactive wrappers mean every agent you start by hand lands outside bn's control
            // plane — a setup misconfig in the same band as an unusable MCP server.
            Finding::ShellInit(_) => action_rank::WARN,
        }
    }

    /// Secondary sort within a priority — larger first. Stale-r notes and chronic cron failures
    /// surface ahead of fresher ones; attn items keep their incoming (urgent-first) order.
    fn age(&self) -> i64 {
        match self {
            Finding::Note(n) => n.idle_days,
            Finding::Cron(c) => c.failures as i64,
            // The longer the silence, the higher within the actively-broken band — in hours, so it
            // is comparable to the day/count units the other priority-3 rows sort by.
            Finding::Staff(s) => s.overdue_secs / 3600,
            Finding::Attn(_) => 0,
            // The more overdue, the higher within the urgent band; A-not-overdue todos (days 0)
            // sit just under overdue ones.
            Finding::Todo(t) => t.overdue_days,
            Finding::Inbox(i) => i.idle_days,
            Finding::Mcp(_) => 0,
            // The longer the staff has been silent, the higher within its band.
            Finding::StaleStaff(s) => s.idle_days,
            // The more foreign machines are leaking in, the higher within its band.
            Finding::NotesLeak(l) => l.foreign_machines.len() as i64,
            Finding::Backlog(b) => b.untasked as i64,
            // A configuration state, not a duration — nothing to age within its band.
            Finding::ShellInit(_) => 0,
        }
    }

    /// One-line label for the `--brief` hook line, naming the single most-pressing row.
    pub fn brief_label(&self) -> String {
        match self {
            Finding::Note(n) => format!("{}/{} idle {}d", n.repo, n.branch, n.idle_days),
            Finding::Cron(c) => format!("cron {} failing", c.job),
            Finding::Staff(s) => format!("staff {} missed its scheduled run", s.agent),
            Finding::Attn(a) => a.title.clone(),
            Finding::Todo(t) if t.overdue => {
                format!("{} overdue {}d", t.summary, t.overdue_days)
            }
            Finding::Todo(t) => format!("{} (A)", t.summary),
            Finding::Inbox(i) => format!("{} in inbox {}d", i.text, i.idle_days),
            Finding::Mcp(m) => format!("mcp {}: '{}' {}", m.server, m.command, m.problem),
            Finding::StaleStaff(s) => format!("staff idle {}d", s.idle_days),
            Finding::NotesLeak(l) => format!(
                "personal notes leak: {} foreign machine(s) under {}",
                l.foreign_machines.len(),
                l.personal_root
            ),
            Finding::Backlog(b) => format!(
                "{} queued item(s) with no task — work with no durable record",
                b.untasked
            ),
            Finding::ShellInit(s) => format!("{}: {}", s.shell, s.problem),
        }
    }
}

/// Live backlog entries that name no task (#1001) — work that predates the invariant.
///
/// Reported rather than deleted: an entry may be mid-flight, and an upgrade must never orphan
/// running work. Goes quiet on its own as those entries reach a terminal state.
fn backlog_findings() -> Vec<Finding> {
    let backlog = crate::backlog::load(&crate::backlog::backlog_path());
    let untasked = backlog.untasked();
    if untasked.is_empty() {
        return Vec::new();
    }
    Vec::from([Finding::Backlog(BacklogFinding {
        untasked: untasked.len(),
        sample: untasked
            .iter()
            .take(5)
            .map(|e| format!("{}/{}", e.repo, e.item))
            .collect(),
    })])
}

/// The aggregated attention list, most-pressing first. Folds three sources: local notes with
/// unfinished work that is stale, goal-less, or blocked; cron jobs whose latest run failed; and
/// open `attn` items (manual / script / agent). A fresh, goal-bearing, unblocked note with no
/// failing jobs and no raised items stays quiet.
pub fn doctor(
    cfg: &Config,
    today: &str,
    stale_days: i64,
    repo: Option<(&Path, &str)>,
) -> Vec<Finding> {
    let mut out = Vec::new();
    out.extend(note_findings(cfg, today, stale_days));
    out.extend(cron_findings());
    out.extend(staff_findings(cfg));
    out.extend(attn_findings(today));
    out.extend(inbox_findings(cfg, today));
    out.extend(stale_staff_findings(cfg, today));
    out.extend(notes_leak_findings(cfg));
    out.extend(backlog_findings());
    if let Some((root, name)) = repo {
        out.extend(todo_findings(cfg, name, today));
        out.extend(mcp_findings(
            root,
            &std::env::var("PATH").unwrap_or_default(),
        ));
    }
    sort_findings(&mut out);
    out
}

/// Order an attention list: shared base rank, then age desc. Stable, so source order survives a tie.
/// Public because a caller that folds in its own findings — the CLI adds the shell-wrapper check,
/// which reads *its* process environment rather than the notes tree — must re-establish the same
/// order rather than tacking rows on the end.
pub fn sort_findings(out: &mut [Finding]) {
    out.sort_by(|a, b| a.rank().compare(b.rank()).then(b.age().cmp(&a.age())));
}

// ---------------------------------------------------------------------------
// Attention snapshot cache — the prefix-J doctor picker (`bn doctor --pick`) recomputes this whole
// list on every open *and* on every in-popup action-reload, and `note_findings` makes that
// I/O-heavy (one `store::read` YAML parse per branch note — hundreds on a busy machine, seconds of
// wall time). This mirrors the `next` attention cache: a short-lived on-disk snapshot of the
// global (`repo = None`) finding list so the picker opens instantly and its reloads stay snappy.
// Kept warm alongside the `next` cache by the `attention-sync` cron, self-heals on a miss/stale
// read, and ctrl-r forces a live recompute + rewrite.
// ---------------------------------------------------------------------------

/// Maximum age for the precomputed doctor snapshot. Matches the `next` cache: the `attention-sync`
/// warmer runs every five minutes, and this ceiling keeps a stuck warmer from serving a stale list
/// forever — the first read past it recomputes live and rewrites.
const CACHE_MAX_AGE_SECS: i64 = 10 * 60;

/// Path to the precomputed doctor snapshot — the global attention list the prefix-J picker serves.
pub fn cache_path() -> PathBuf {
    crate::resolver::state_dir().join("doctor.json")
}

/// Borrowed view for writing (no `Clone` on `Finding` required).
#[derive(Serialize)]
struct DoctorCacheWrite<'a> {
    generated_at_epoch: i64,
    stale_days: i64,
    findings: &'a [Finding],
}

/// Owned view for reading back the snapshot.
#[derive(Deserialize)]
struct DoctorCacheRead {
    #[serde(default)]
    generated_at_epoch: i64,
    #[serde(default)]
    stale_days: i64,
    findings: Vec<Finding>,
}

fn write_cache_at(findings: &[Finding], stale_days: i64, now: i64) -> std::io::Result<()> {
    let json = serde_json::to_string(&DoctorCacheWrite {
        generated_at_epoch: now,
        stale_days,
        findings,
    })
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let path = cache_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)
}

/// Persist an already-computed global finding list, stamped now. The picker calls this after
/// trimming a just-acted row so the reload serves the trimmed list without a recompute.
pub fn write_cache(findings: &[Finding], stale_days: i64) -> std::io::Result<()> {
    write_cache_at(findings, stale_days, crate::util::now_epoch_secs())
}

/// Raw read of the cached list regardless of age — used by the picker to trim a just-acted row.
/// `None` when the snapshot is absent or unparseable.
pub fn read_cache() -> Option<Vec<Finding>> {
    let text = std::fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str::<DoctorCacheRead>(&text)
        .ok()
        .map(|c| c.findings)
}

/// Read the cached list only when it is fresh *and* was computed under the same `stale_days`.
/// Older, mismatched, or future-dated snapshots read as a miss so the caller recomputes live and
/// rewrites (a future stamp — clock skew / corruption — would otherwise freeze a stale list).
pub fn read_fresh_cache(stale_days: i64, now: i64) -> Option<Vec<Finding>> {
    let text = std::fs::read_to_string(cache_path()).ok()?;
    let cache = serde_json::from_str::<DoctorCacheRead>(&text).ok()?;
    let ts = cache.generated_at_epoch;
    if cache.stale_days != stale_days
        || ts <= 0
        || ts > now
        || now.saturating_sub(ts) > CACHE_MAX_AGE_SECS
    {
        return None;
    }
    Some(cache.findings)
}

/// Remove the precomputed doctor snapshot. `Ok(false)` when there was nothing to clear.
pub fn clear_cache() -> std::io::Result<bool> {
    match std::fs::remove_file(cache_path()) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Recompute the global finding list and rewrite the snapshot, returning it. The forced-refresh
/// path (ctrl-r) and the self-heal on a cache miss.
pub fn recompute_and_cache(cfg: &Config, today: &str, stale_days: i64, now: i64) -> Vec<Finding> {
    let findings = doctor(cfg, today, stale_days, None);
    crate::util::best_effort(
        write_cache_at(&findings, stale_days, now),
        "doctor: warm attention cache",
    );
    findings
}

/// Warm the snapshot without returning it — the write-only path the `attention-sync` cron calls.
pub fn refresh_cache(cfg: &Config, today: &str, stale_days: i64, now: i64) -> std::io::Result<()> {
    write_cache_at(&doctor(cfg, today, stale_days, None), stale_days, now)
}

/// Serve a fresh snapshot when available, else compute live and warm the cache — the picker's
/// instant-open fast path.
pub fn cached_or_compute(cfg: &Config, today: &str, stale_days: i64, now: i64) -> Vec<Finding> {
    match read_fresh_cache(stale_days, now) {
        Some(f) => f,
        None => recompute_and_cache(cfg, today, stale_days, now),
    }
}

/// A compact tally for the ambient status-bar badge. Counts only the *actively-raised* signals
/// — failing cron jobs and open `attn` items — not stale-note findings, which already show in
/// the per-window badges. `urgent` is cron failures plus `urgent` attn items; `other` is the
/// warn/info attn items. Cheap on purpose (two event-log reads, no note scan) so it can run
/// every status-bar refresh.
#[derive(Debug, Serialize, Default, PartialEq, Eq)]
pub struct Badge {
    pub urgent: usize,
    pub other: usize,
}

impl Badge {
    pub fn is_empty(&self) -> bool {
        self.urgent == 0 && self.other == 0
    }
}

/// The status-bar tally — see [`Badge`].
pub fn badge(today: &str) -> Badge {
    let mut b = Badge::default();
    for j in events::cron_status() {
        if !j.ok {
            b.urgent += 1;
        }
    }
    for i in attention::open_items(today) {
        if i.severity == "urgent" {
            b.urgent += 1;
        } else {
            b.other += 1;
        }
    }
    b
}

/// Notes surfacing only if they have unfinished work (open todos or blockers) *and* at least
/// one flag fires (stale / no_goal / blockers). Public so the morning-planning `stale-branches`
/// gatherer ([`crate::findings_gather`]) reuses this exact stale/blocked logic rather than
/// reimplementing it; every returned `Finding` is a [`Finding::Note`].
pub fn note_findings(cfg: &Config, today: &str, stale_days: i64) -> Vec<Finding> {
    let mut out = Vec::new();
    for bn in all_notes(cfg) {
        let open_todos = bn
            .note
            .front
            .todos
            .iter()
            .filter(|t| t.is_open_todo())
            .count();
        let blockers = bn.note.front.blockers.len();
        if open_todos == 0 && blockers == 0 {
            continue; // nothing unfinished — nothing to nag about
        }

        let last_activity = last_activity(&bn.note);
        let idle_days = idle_days_since(&last_activity, today);

        let mut flags = Vec::new();
        if idle_days >= stale_days {
            flags.push(format!("stale:{idle_days}d"));
        }
        if render::goal_text(&bn.note.body).is_none() {
            flags.push("no_goal".to_string());
        }
        if blockers > 0 {
            flags.push(format!("blockers:{blockers}"));
        }
        if flags.is_empty() {
            continue;
        }

        out.push(Finding::Note(NoteFinding {
            repo: bn.repo,
            branch: bn.branch,
            path: bn.path.display().to_string(),
            flags,
            open_todos,
            blockers,
            last_activity,
            idle_days,
        }));
    }
    out
}

/// Cron jobs whose latest run failed, newest-failed first (the event log already sorts
/// `cron_status` most-recent-run first).
fn cron_findings() -> Vec<Finding> {
    events::cron_status()
        .into_iter()
        .filter(|j| !j.ok)
        .map(|j| {
            Finding::Cron(CronFinding {
                job: j.job,
                repo: j.repo,
                branch: j.branch,
                failures: j.failures,
                last_run: j.last_run,
            })
        })
        .collect()
}

/// Staff agents whose most recent scheduled run is missed (#273) — a dead brief with no ledger row
/// at or after its slot. Read-only: it joins the declared roster schedule with the run ledger and
/// raises nothing, so it "flags once" by construction (recomputed each invocation, and it clears
/// the moment a run of any status lands). Empty when no staff agents are declared, keeping the
/// common no-staff path free. Global-tier roster only — staff agents are declared globally.
fn staff_findings(cfg: &Config) -> Vec<Finding> {
    let agents = crate::staff::discover(cfg, None);
    if agents.is_empty() {
        return Vec::new();
    }
    let last = crate::staff::last_runs();
    let now = crate::util::now_epoch_secs();
    crate::staff_schedule::missed_runs(
        &agents,
        &last,
        now,
        crate::staff_schedule::DEFAULT_MISS_GRACE_SECS,
    )
    .into_iter()
    .map(|m| {
        Finding::Staff(StaffFinding {
            agent: m.agent,
            on_calendar: m.on_calendar,
            scheduled: m.scheduled,
            last_run: m.last_run,
            overdue_secs: now.saturating_sub(m.scheduled),
        })
    })
    .collect()
}

/// The staff-staleness finding (#270). Flags the whole agent staff when it hasn't produced a card
/// anywhere in the person-scoped, synced feed for more than [`crate::staff::STALE_STAFF_DAYS`] —
/// the failover signal that the single home machine is down. Reads only the offline feed, so it
/// fires identically on every box. Distinct from [`staff_findings`] (#273, a *single* agent's
/// missed slot): this is the *whole* staff going quiet across machines. Silent when the staff has
/// never run (nothing to be stale about) or ran recently enough; at most one finding.
fn stale_staff_findings(cfg: &Config, today: &str) -> Vec<Finding> {
    let Some(last_card) = crate::staff::newest_card_date(cfg) else {
        return Vec::new();
    };
    let idle_days = idle_days_since(&last_card, today);
    if idle_days <= crate::staff::STALE_STAFF_DAYS {
        return Vec::new();
    }
    vec![Finding::StaleStaff(StaleStaffFinding {
        last_card,
        idle_days,
    })]
}

/// A cross-machine notes leak (the #leak guard). Fires only on a **work-classified** machine
/// (one with `[work].work_repos` set or `[work].only` on) whose `personal_root` is a *separate*
/// repo from `work_root` and physically carries branch-note subtrees for machines other than this
/// one. That is exactly the state where the whole-person views ([`Config::metrics_logs_all_machines`],
/// [`Config::each_notes_root`]) pull another box's sessions/notes onto this work laptop. On a purely
/// personal machine multiple machine subtrees are the *intended* whole-person view, so the check is
/// gated to work machines; once the roots are collapsed onto the work repo (`personal_root ==
/// work_root`) or the foreign clone is removed, it goes quiet. Read-only and offline: it lists the
/// direct children of `personal_root` that carry a `metrics/events.jsonl` — the precise files the
/// leak reads — and reports the ones that aren't this machine.
fn notes_leak_findings(cfg: &Config) -> Vec<Finding> {
    // Only a work machine cares about personal/work separation.
    if cfg.work_repos.is_empty() && !cfg.work_only {
        return Vec::new();
    }
    // Roots collapsed onto one repo ⇒ no separate personal repo to leak from (the recommended fix).
    if cfg.personal_root == cfg.work_root {
        return Vec::new();
    }
    let me = cfg.machine();
    let mut foreign: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&cfg.personal_root) {
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            if name == me {
                continue;
            }
            // The exact leak vector: `metrics_logs_all_machines()` globs
            // `<personal_root>/<machine>/metrics/events.jsonl`. A subtree that has one and isn't
            // this machine is another box's data being read here.
            if p.join("metrics").join("events.jsonl").is_file() {
                foreign.push(name);
            }
        }
    }
    if foreign.is_empty() {
        return Vec::new();
    }
    foreign.sort();
    vec![Finding::NotesLeak(NotesLeakFinding {
        personal_root: cfg.personal_root.display().to_string(),
        work_root: cfg.work_root.display().to_string(),
        foreign_machines: foreign,
    })]
}

/// Pressing open tasks from the global store (`Config::tasks_file`, #996), scoped to the repo
/// doctor was asked about — those overdue (`due` on/before today) or priority A. Ordinary open
/// tasks are deliberately excluded so the list stays a signal, not a backlog dump; they remain in
/// `bn task`. A missing/unreadable store yields nothing.
/// Days a capture may sit in the inbox before doctor nags — the inbox is never a home.
const INBOX_STALE_DAYS: i64 = 3;

/// Captures that have sat in the `bn q` inbox [`INBOX_STALE_DAYS`]+ days. Fresh captures stay
/// quiet: rollover and focus-end drains are the normal path out, so a finding here means the
/// drain has stopped happening.
fn inbox_findings(cfg: &Config, today: &str) -> Vec<Finding> {
    crate::inbox::read(cfg)
        .into_iter()
        .filter_map(|i| {
            let idle = idle_days_since(&i.created, today);
            if idle < INBOX_STALE_DAYS {
                return None;
            }
            Some(Finding::Inbox(InboxFinding {
                id: i.id,
                text: i.text,
                repo: i.repo,
                branch: i.branch,
                created: i.created,
                idle_days: idle,
            }))
        })
        .collect()
}

/// The per-repo MCP config files `bn mcp init` writes, with the JSON key holding each one's
/// server map — the surface doctor verifies.
const MCP_CONFIGS: &[(&str, &str)] = &[
    (".mcp.json", "mcpServers"),
    (".vscode/mcp.json", "servers"),
    (".github/mcp.json", "mcpServers"),
];

/// The environment variable `bn shell-init`'s wrappers export. A child process cannot inspect its
/// parent shell's functions, so this marker is the only honest signal that the wrappers were
/// actually sourced into the shell that ran `bn`.
pub const SHELL_WRAPPER_MARKER: &str = "BN_SHELL_WRAPPERS";

/// Why the `claude`/`copilot` wrappers aren't driving launches in this shell, or `None` when they
/// are.
///
/// Two inputs, because neither alone is sufficient: `marker` is `$BN_SHELL_WRAPPERS` inherited from
/// the shell that spawned bn — the only proof of "sourced" — while `rc` is the login file's text,
/// the only place a *shadowing* redefinition is visible at all. A rc that sources the wrappers and
/// then defines its own `claude()` looks perfectly healthy from the environment; it is the case
/// this check exists for. Pure.
pub fn shell_init_problem(rc: &str, marker: Option<&str>) -> Option<ShellInitProblem> {
    let sourced_at = rc
        .lines()
        .position(|l| !is_comment(l) && l.contains("bn shell-init"));
    let active = marker.is_some_and(|m| !m.trim().is_empty());
    match (sourced_at, active) {
        (None, _) => Some(ShellInitProblem::NotInstalled),
        (Some(_), false) => Some(ShellInitProblem::SourcedNotActive),
        (Some(at), true) => rc
            .lines()
            .skip(at + 1)
            .any(defines_harness_function)
            .then_some(ShellInitProblem::Shadowed),
    }
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// A shell function definition for one of the harness commands the wrappers own — `claude() {`,
/// `function copilot {`, `yolo() {`. Matched loosely on purpose: any later definition of these
/// names wins over the sourced wrapper, whichever syntax declared it.
fn defines_harness_function(line: &str) -> bool {
    let line = line.trim();
    if is_comment(line) {
        return false;
    }
    let body = line.strip_prefix("function ").unwrap_or(line);
    ["claude", "copilot", "yolo"].iter().any(|name| {
        body.strip_prefix(name).is_some_and(|rest| {
            let rest = rest.trim_start();
            rest.starts_with("()") || rest.starts_with('{')
        })
    })
}

/// MCP servers configured in the current repo the agent harness couldn't use: a local `command`
/// that doesn't resolve against `path_var`, or an HTTP `url` whose endpoint isn't reachable. Each
/// carries the exact one-line fix — curated extras get their catalog fix, other commands a generic
/// one, an unreachable endpoint a "start the server" hint. The bn server itself is a stdio launcher
/// (`bn mcp serve`), so it's validated as a PATH command like any other. A missing/unparseable
/// config file yields nothing (not doctor's fight); a (server, target) pair configured in several
/// files surfaces once.
fn mcp_findings(root: &Path, path_var: &str) -> Vec<Finding> {
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut out = Vec::new();
    for (rel, key) in MCP_CONFIGS {
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(servers) = v.get(*key).and_then(|s| s.as_object()) else {
            continue;
        };
        for (name, entry) in servers {
            if let Some(cmd) = entry.get("command").and_then(|c| c.as_str()) {
                let pair = (name.clone(), cmd.to_string());
                if seen.contains(&pair) || crate::mcp::command_on_path(cmd, path_var) {
                    continue;
                }
                seen.push(pair);
                let fix = crate::mcp::EXTRAS
                    .iter()
                    .find(|e| e.name == name || e.command == cmd)
                    .map(|e| e.runner_fix.to_string())
                    .unwrap_or_else(|| format!("install '{cmd}' or fix the entry in {rel}"));
                out.push(Finding::Mcp(McpFinding {
                    server: name.clone(),
                    command: cmd.to_string(),
                    config: rel.to_string(),
                    problem: "not on PATH".to_string(),
                    fix,
                }));
            } else if let Some(url) = entry.get("url").and_then(|u| u.as_str()) {
                // HTTP transport: a user's custom remote server — verify the endpoint is reachable
                // instead of a PATH binary. (bn itself is stdio, so it never lands here.)
                let pair = (name.clone(), url.to_string());
                if seen.contains(&pair) || http_endpoint_reachable(url) {
                    continue;
                }
                seen.push(pair);
                out.push(Finding::Mcp(McpFinding {
                    server: name.clone(),
                    command: url.to_string(),
                    config: rel.to_string(),
                    problem: "unreachable".to_string(),
                    fix: "start the MCP server behind this endpoint".to_string(),
                }));
            }
        }
    }
    out
}

/// Best-effort reachability probe for an HTTP MCP endpoint: parse the `http(s)://host:port`
/// authority and attempt a short-timeout TCP connect. `true` (don't flag) when the url isn't a
/// resolvable `http(s)` authority — doctor never invents a problem from a shape it can't verify;
/// `false` only when a real host:port refuses/times out (the server is down).
fn http_endpoint_reachable(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    else {
        return true; // not an http url we can probe
    };
    let authority = rest.split('/').next().unwrap_or(rest);
    // Drop any `user:pass@` userinfo prefix — only host:port is dialable.
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    use std::net::ToSocketAddrs;
    let Ok(addrs) = authority.to_socket_addrs() else {
        return true; // no explicit port / unresolvable → not doctor's to flag
    };
    let mut any = false;
    for addr in addrs {
        any = true;
        if std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300))
            .is_ok()
        {
            return true;
        }
    }
    // Resolved to at least one address but none accepted a connection → down.
    !any
}

fn todo_findings(cfg: &Config, repo: &str, today: &str) -> Vec<Finding> {
    let Ok(file) = crate::todo::load(&cfg.tasks_file()) else {
        return Vec::new();
    };
    file.tasks()
        .filter(|t| t.is_open(today))
        // The store is global now (#996), so scope the fold to the repo doctor was asked about.
        // A task with no `repo:` is unattributable and would otherwise nag in every repo.
        .filter(|t| t.meta("repo").is_some_and(|r| r.eq_ignore_ascii_case(repo)))
        .filter_map(|t| {
            let due = t.meta("due").map(str::to_string);
            let overdue = due.as_deref().is_some_and(|d| d <= today);
            if !overdue && t.priority != Some('A') {
                return None;
            }
            let overdue_days = due
                .as_deref()
                .filter(|_| overdue)
                .map(|d| idle_days_since(d, today))
                .unwrap_or(0);
            Some(Finding::Todo(TodoFinding {
                id: t.id().unwrap_or("?").to_string(),
                summary: t.summary(),
                repo: repo.to_string(),
                priority: t.priority,
                due,
                branch: t.meta("branch").map(str::to_string),
                overdue,
                overdue_days,
            }))
        })
        .collect()
}

/// Open attention items as findings (already urgent-first from `open_items`).
fn attn_findings(today: &str) -> Vec<Finding> {
    attention::open_items(today)
        .into_iter()
        .map(|i: AttnItem| {
            Finding::Attn(AttnFinding {
                id: i.id,
                title: i.title,
                severity: i.severity,
                source: i.source,
                repo: i.repo,
                branch: i.branch,
                link: i.link,
                detail: i.detail,
            })
        })
        .collect()
}

/// The most recent `YYYY-MM-DD` touched anywhere in the note: its creation date and every
/// item's created/done_at across all sections. Date strings sort lexicographically, so the
/// max is the latest. Falls back to the note's created date.
pub(crate) fn last_activity(note: &Note) -> String {
    let f = &note.front;
    let sections = [
        &f.todos,
        &f.blockers,
        &f.decisions,
        &f.research,
        &f.collab,
        &f.ask,
    ];
    let mut latest = f.created.clone();
    for section in sections {
        for item in section {
            consider(&mut latest, &item.created);
            if let Some(done) = &item.done_at {
                consider(&mut latest, done);
            }
        }
    }
    latest
}

fn consider(latest: &mut String, candidate: &str) {
    if candidate > latest.as_str() {
        *latest = candidate.to_string();
    }
}


