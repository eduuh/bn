//! `bn orchestrate cockpit …` (#647) — the orchestrator cockpit in the shared fleet/cockpit session
//! (`wt::orchestrator_session`, default `cron`, `[jobs].session` to override).
//!
//! One place where an operator sees *what the orchestrator is thinking* — a live grounding view of
//! backlog + todos + attention. The cockpit is pure composition over existing reads — `bn
//! orchestrate status`, `bn attn`, `bn next` — so there is no new state store to drift and tmux
//! stays the source of truth (mirroring `bn fleet pick`, `agent_live.rs`).
//!
//! Invariants (design r14 §5): the cockpit only ever creates the fleet/cockpit session and its own
//! grounding window; it never links, unlinks, moves, swaps, or kills fleet windows, and never
//! touches the `main` worktree. Each lane keeps running untouched in its home repo session.

use crate::*;

/// The cockpit window name for a scope: one shared `cockpit` window for the cross-repo (`--all-repos`)
/// view, else a per-repo `cockpit-<repo>` window (design r14 §6, "how many").
pub(crate) fn cockpit_window_name(repo: Option<&str>) -> String {
    match repo {
        Some(r) => format!("cockpit-{}", bn_core::orchestrate::sanitize_run_key(r)),
        None => "cockpit".to_string(),
    }
}

// ── grounding text ──────────────────────────────────────────────────────────────────────────────

/// One orchestrate run's rolled-up counts, for the grounding header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunLine {
    pub repo: String,
    pub queued: usize,
    pub in_flight: usize,
    pub merged: usize,
    pub failed: usize,
    pub backlog: usize,
}

/// The composed, read-only grounding snapshot — everything the cockpit shows. Pure data so the
/// renderer is unit-testable without touching disk or tmux.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GroundData {
    pub runs: Vec<RunLine>,
    /// Repos with a pending backlog but no live run.
    pub orphan_backlog: Vec<(String, usize)>,
    pub needs_you: Vec<String>,
    pub in_progress: Vec<String>,
    pub to_start: Vec<String>,
    pub next: Vec<String>,
}

/// Render the grounding snapshot to plain text (no ANSI) — the seed the orchestrator starts
/// oriented on and what `bn orchestrate cockpit ground` prints. Pure over [`GroundData`].
pub(crate) fn render_ground(d: &GroundData) -> String {
    let mut out = String::new();
    out.push_str("═══ ORCHESTRATOR COCKPIT ═══\n\n");

    out.push_str("ORCHESTRATE\n");
    if d.runs.is_empty() && d.orphan_backlog.is_empty() {
        out.push_str("  (no active run)\n");
    } else {
        for r in &d.runs {
            out.push_str(&format!(
                "  {} — queued {} · in-flight {} · merged {} · failed {} · backlog {}\n",
                r.repo, r.queued, r.in_flight, r.merged, r.failed, r.backlog
            ));
        }
        for (repo, n) in &d.orphan_backlog {
            out.push_str(&format!("  {repo} — {n} pending (no active run)\n"));
        }
    }
    out.push('\n');

    let section = |out: &mut String, title: &str, items: &[String]| {
        out.push_str(title);
        out.push('\n');
        if items.is_empty() {
            out.push_str("  —\n");
        } else {
            for it in items {
                out.push_str(&format!("  • {it}\n"));
            }
        }
        out.push('\n');
    };

    section(&mut out, "NEEDS YOU", &d.needs_you);
    section(&mut out, "IN PROGRESS", &d.in_progress);
    section(&mut out, "TO START", &d.to_start);
    section(&mut out, "NEXT", &d.next);
    out
}

/// How many `bn next` / to-start / lane rows to embed in the grounding snapshot — enough to
/// orient without dumping a 100-deep backlog.
const GROUND_LIMIT: usize = 8;

/// Gather the grounding data from on-disk state (orchestrate runs + backlog + attention + `bn
/// next`). Reads only — no tmux — so it is fixture-testable via BN_STATE_DIR / BN_NOTES_DIR.
pub(crate) fn gather_ground(cfg: &Config, repo: Option<&str>) -> GroundData {
    use bn_core::attention::Lane;
    use bn_core::clock::Clock;

    let backlog = bn_core::backlog::load(&bn_core::backlog::backlog_path());
    let mut runs = Vec::new();
    let mut run_repos: std::collections::HashSet<String> = std::collections::HashSet::new();
    for path in bn_core::orchestrate::run_paths() {
        if let Some(st) = bn_core::orchestrate::OrchestrateState::load(&path) {
            if repo.is_some_and(|want| want != st.repo) {
                continue;
            }
            let c = st.counts();
            run_repos.insert(st.repo.clone());
            runs.push(RunLine {
                queued: c.queued,
                in_flight: c.in_flight,
                merged: c.merged,
                failed: c.failed,
                backlog: backlog.pending_depth(&st.repo),
                repo: st.repo,
            });
        }
    }
    let orphan_backlog: Vec<(String, usize)> = backlog
        .repos_with_pending()
        .into_iter()
        .filter(|r| repo.is_none_or(|want| want == r))
        .filter(|r| !run_repos.contains(r))
        .map(|r| {
            let n = backlog.pending_depth(&r);
            (r, n)
        })
        .collect();

    let (classified, open) = crate::attn_lanes();
    let loc = |repo: &Option<String>, branch: &Option<String>| match (repo, branch) {
        (Some(r), Some(b)) => format!(" ({r}/{b})"),
        (Some(r), None) => format!(" ({r})"),
        _ => String::new(),
    };
    let lane_titles = |want: Lane| -> Vec<String> {
        classified
            .iter()
            .filter(|(_, st)| st.lane() == want)
            .take(GROUND_LIMIT)
            .map(|(s, _)| format!("{}{}", s.item.title, loc(&s.item.repo, &s.item.branch)))
            .collect()
    };
    let needs_you = lane_titles(Lane::NeedsYou);
    let in_progress = lane_titles(Lane::InProgress);
    let to_start: Vec<String> = open
        .iter()
        .take(GROUND_LIMIT)
        .map(|i| format!("{}{}", i.title, loc(&i.repo, &i.branch)))
        .collect();

    let clock = bn_core::clock::RealClock;
    let today = clock.today();
    let now = clock.now_epoch_secs();
    let next: Vec<String> = bn_core::next::next_actions(cfg, &today, now)
        .into_iter()
        .take(GROUND_LIMIT)
        .map(|a| a.title)
        .collect();

    GroundData {
        runs,
        orphan_backlog,
        needs_you,
        in_progress,
        to_start,
        next,
    }
}

/// The full grounding snapshot text — [`gather_ground`] + [`render_ground`].
pub(crate) fn cockpit_ground_text(cfg: &Config, repo: Option<&str>) -> String {
    render_ground(&gather_ground(cfg, repo))
}

// ── tmux window enumeration (liveness probe + status listing) ────────────────────────────────────

/// One tmux window as the cockpit sees it: its raw-case `session`/`window` names and
/// `@bn_agent_since` (0 for an undecorated, non-agent window). Used only to (a) decide whether any
/// fleet lane is live enough to warrant auto-creating the grounding session, and (b) list the
/// `planning` session's own windows for `status`. The cockpit never links or unlinks these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CockWin {
    pub session: String,
    pub window: String,
    pub since: i64,
}

/// Read every tmux window across all sessions. `None`/empty when tmux is absent.
fn read_windows() -> Vec<CockWin> {
    let fmt = "#{session_name}\t#{window_name}\t#{@bn_agent_since}";
    let Some(out) = crate::proc::tmux_raw(&["list-windows", "-a", "-F", fmt]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 2 || f[0].is_empty() {
                return None;
            }
            Some(CockWin {
                session: f[0].to_string(),
                window: f[1].to_string(),
                since: f.get(2).and_then(|s| s.parse().ok()).unwrap_or(0),
            })
        })
        .collect()
}

/// Is the cockpit session currently a live tmux session?
fn planning_exists() -> bool {
    let planning = wt::orchestrator_session();
    crate::proc::tmux_ok(&["has-session", "-t", &format!("={planning}:")])
}

/// Race-safe create of the `planning` session with a first window `win` running `ground`.
/// Returns whether the session exists afterwards.
///
/// A check-then-`new-session` is a TOCTOU race: two concurrent `up`/`ensure_cockpit` callers
/// both see "absent" and both try to create, and the loser's `new-session` fails with
/// `duplicate session: planning`. We treat that as benign — [`crate::proc::tmux_ok`] captures (and
/// discards) tmux's stderr so nothing leaks to the terminal, and on any failure we simply re-check
/// `has-session` and proceed if the session is now present (the winner created it).
fn ensure_planning_session(win: &str, ground: &str) -> bool {
    if planning_exists() {
        return true;
    }
    // Root the session in the personal-notes repo so planning lands next to the notes — mirroring
    // the `planning` session tmux.conf creates (`-c ~/projects/personal-notes`). Whichever creator
    // wins the race, the session's start dir is then the same. tmux falls back to $HOME if the path
    // is absent (a fresh machine), so passing it unconditionally is safe.
    let root = format!("{}/projects/personal-notes", wt::home_str());
    if crate::proc::tmux_ok(&[
        "new-session",
        "-d",
        "-s",
        wt::orchestrator_session(),
        "-c",
        &root,
        "-n",
        win,
        ground,
    ]) {
        return true;
    }
    // Create failed — most likely a concurrent winner already made it. Re-check rather than error.
    planning_exists()
}

/// Is `session:window` a live *fleet* window (a staff/dispatch lane), rather than an infrastructure
/// window the cockpit must ignore? Drops the `main`/`master` worktree window (design r14 §5 "never
/// touch main"), the personal-notes root, and anything already in the cockpit session
/// (`cockpit_session`). Used only to gate auto-create on there being real fleet worth a grounding
/// view. Name-based → pure/unit-testable.
pub(crate) fn is_fleet_window(session: &str, window: &str, cockpit_session: &str) -> bool {
    if session.eq_ignore_ascii_case(cockpit_session) {
        return false; // the cockpit session itself is never fleet.
    }
    let w = window.to_ascii_lowercase();
    !matches!(
        w.as_str(),
        "main" | "master" | "notes" | "branch-notes" | ""
    )
}

// ── up / status / ground handlers ────────────────────────────────────────────────────────────────

/// The shell command a cockpit window runs: a self-refreshing grounding view (no dependency on the
/// external `watch(1)`). Re-invokes `orchestrate cockpit ground` every `interval_secs` seconds
/// (the resolved `[cockpit].ground_secs`, default 30 — `bn_core`'s `DEFAULT_COCKPIT_GROUND_SECS`).
///
/// The redraw is done **in place** rather than with a hard `clear`: each tick homes the cursor
/// (`\033[H`) before printing the frame — so the new snapshot overwrites the old one without a
/// blank flash — then clears from the cursor to the end of the screen (`\033[J`) after it, to
/// erase any trailing lines a shorter frame leaves behind. This cuts the flicker of the old
/// clear-every-tick loop while staying dependency-free (no `tput`/`watch`).
///
/// It prefers `bn` resolved from `PATH` **at each iteration**, falling back to this binary's baked
/// absolute path only when PATH has no `bn`. A long-lived loop must not hard-bake `current_exe()`:
/// if bn is reinstalled or self-updates while the cockpit runs, the old inode can be replaced or
/// deleted, and the loop would keep invoking a stale/missing path. Resolving `bn` per tick lets the
/// loop pick up the new binary automatically, and if neither PATH nor the baked path resolves the
/// failed sub-command just errors for that tick — the loop keeps ticking (degrades gracefully)
/// instead of dying.
fn ground_loop_cmd(repo: Option<&str>, interval_secs: u64) -> String {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "bn".to_string());
    let scope = match repo {
        Some(r) => format!(" --repo {}", crate::wt::shq(r)),
        None => String::new(),
    };
    format!(
        "while :; do printf '\\033[H'; \"$(command -v bn 2>/dev/null || echo {})\" orchestrate cockpit ground{scope}; printf '\\033[J'; sleep {interval_secs}; done",
        crate::wt::shq(&exe)
    )
}

/// `bn orchestrate cockpit up` — create-or-attach the `planning` session and its cockpit grounding
/// window (idempotent). `--attach` follows a human in.
pub(crate) fn cockpit_up(cfg: &Config, repo: Option<&str>, attach: bool, json: bool) {
    let planning = wt::orchestrator_session();
    let win = cockpit_window_name(repo);
    let ground = ground_loop_cmd(repo, cfg.cockpit_ground_secs);
    let existed = planning_exists();
    let has_window = read_windows()
        .iter()
        .any(|w| w.session.eq_ignore_ascii_case(planning) && w.window == win);

    // Ensure the planning session + cockpit grounding window exist (idempotent, race-safe).
    if !existed {
        ensure_planning_session(&win, &ground);
    } else if !has_window {
        let target = format!("={planning}:");
        crate::proc::tmux_run(&["new-window", "-d", "-t", &target, "-n", &win, &ground]);
    }

    if json {
        println!(
            "{}",
            bn_core::render::Envelope::success(
                "cockpit",
                serde_json::json!({
                    "session": planning,
                    "window": win,
                    "created_session": !existed,
                    "created_window": !existed || !has_window,
                }),
            )
            .to_json()
        );
    } else {
        println!("cockpit: {planning}:{win} ready");
    }

    // Follow the human in — only at an interactive tty inside tmux (mirrors dispatch's rule).
    if attach && std::env::var_os("TMUX").is_some() {
        crate::proc::tmux_run(&["switch-client", "-t", &format!("={planning}:")]);
    }
}


/// `bn orchestrate cockpit status` — the cockpit session's own window(s). It holds only the
/// grounding window(s); fleet lanes are never linked in.
pub(crate) fn cockpit_status(json: bool) {
    let planning_session = wt::orchestrator_session();
    let planning: Vec<String> = read_windows()
        .into_iter()
        .filter(|w| w.session.eq_ignore_ascii_case(planning_session))
        .map(|w| w.window)
        .collect();
    if json {
        println!(
            "{}",
            bn_core::render::Envelope::success(
                "cockpit",
                serde_json::json!({
                    "exists": planning_exists(),
                    "windows": planning,
                }),
            )
            .to_json()
        );
        return;
    }
    if !planning_exists() {
        println!("cockpit: not up (start it with `bn orchestrate cockpit up`)");
        return;
    }
    println!("cockpit: {planning_session} — {} window(s)", planning.len());
    for w in &planning {
        println!("  {w}");
    }
}

/// `bn orchestrate cockpit ground` — print the read-only grounding snapshot once (the cockpit
/// window wraps this in a refresh loop).
pub(crate) fn cockpit_ground(cfg: &Config, repo: Option<&str>, json: bool) {
    if json {
        let d = gather_ground(cfg, repo);
        println!(
            "{}",
            bn_core::render::Envelope::success(
                "cockpit",
                serde_json::json!({
                    "runs": d.runs.iter().map(|r| serde_json::json!({
                        "repo": r.repo, "queued": r.queued, "in_flight": r.in_flight,
                        "merged": r.merged, "failed": r.failed, "backlog": r.backlog,
                    })).collect::<Vec<_>>(),
                    "orphan_backlog": d.orphan_backlog.iter()
                        .map(|(r, n)| serde_json::json!({ "repo": r, "pending": n }))
                        .collect::<Vec<_>>(),
                    "needs_you": d.needs_you,
                    "in_progress": d.in_progress,
                    "to_start": d.to_start,
                    "next": d.next,
                }),
            )
            .to_json()
        );
    } else {
        print!("{}", cockpit_ground_text(cfg, repo));
    }
}

// ── auto-create + single-owner gate ──────────────────────────────────────────────────────────────

/// Single-owner rule (design r14 §7): cockpit auto-create is driven by **one** owner so two drivers
/// never race. The `--all-repos` supervisor owns it whenever it is running; a lone per-repo `run
/// --once` acts only when no all-repos daemon owns the scope. Pure → unit-testable.
///
/// Since [`ensure_planning_session`] makes auto-create idempotent and race-safe (a concurrent
/// create is benign, no `duplicate session` leak), this gate is now a **best-effort de-dup** — it
/// avoids redundant work, not a correctness barrier. That is why a transient probe miss in
/// [`all_repos_daemon_active`] (which then reads "not running" → the per-repo run co-creates) is
/// harmless, and why the cron ([`job_cockpit_ensure`]) can skip the gate entirely.
pub(crate) fn cockpit_owner_ok(is_all_repos_driver: bool, all_repos_daemon_running: bool) -> bool {
    if is_all_repos_driver {
        true
    } else {
        !all_repos_daemon_running
    }
}

/// The native `cockpit-ensure` cron job (registered in `native_job`): ensure the grounding cockpit
/// exists when any lane is live for a supervised scope. No-op when `planning` is absent and nothing
/// is live, and idempotent otherwise. Never links, unlinks, or kills any window.
///
/// **Intentionally exempt from [`cockpit_owner_ok`]**: the cron is the single machine-global
/// ensurer (one instance, no `--all-repos` context to gate on), and auto-create is idempotent +
/// race-safe ([`ensure_planning_session`]), so it can never double-create or race a `run`/
/// `supervise` driver. The owner gate stays on the `run` entry paths only, where it saves redundant
/// work; the cron just needs to (re-)ensure the grounding view exists on its tick.
pub(crate) fn job_cockpit_ensure(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    ensure_cockpit(cfg);
    0
}

/// Is there at least one live fleet lane (a decorated `@bn_agent_since > 0` window that
/// [`is_fleet_window`]) worth auto-creating a grounding view for? Pure over the window list.
pub(crate) fn any_live_fleet(wins: &[CockWin], cockpit_session: &str) -> bool {
    wins.iter()
        .any(|w| w.since > 0 && is_fleet_window(&w.session, &w.window, cockpit_session))
}

/// The shared auto-create core (used by the cron and by [`ensure_on_run`]): create the `planning`
/// session + a `cockpit` grounding window only when there is live fleet worth a grounding view (no
/// empty cockpits). Idempotent; never links a fleet window in.
fn ensure_cockpit(cfg: &Config) {
    if planning_exists() {
        return;
    }
    if !any_live_fleet(&read_windows(), wt::orchestrator_session()) {
        return;
    }
    // Race-safe: a concurrent driver's create is benign (see `ensure_planning_session`).
    ensure_planning_session("cockpit", &ground_loop_cmd(None, cfg.cockpit_ground_secs));
}

/// Auto-create hook (design r14 §6 primary trigger) called **once** at the entry of an orchestrate
/// `run`/`supervise` process (never per tick). Under the single-owner rule ([`cockpit_owner_ok`])
/// only the responsible driver acts, so a per-repo run stands down while an `--all-repos` daemon
/// owns the scope — two drivers never both auto-create.
pub(crate) fn ensure_on_run(cfg: &Config, all_repos: bool) {
    if !cockpit_owner_ok(all_repos, all_repos_daemon_active()) {
        return;
    }
    ensure_cockpit(cfg);
}

/// Best-effort check: is the cross-repo (`--all-repos`) orchestrate supervisor daemon active? Used
/// only by the single-owner gate ([`cockpit_owner_ok`]); any error/absence reads as "not running"
/// (false). This fail-to-false direction is safe: the worst case is a per-repo run co-creating the
/// cockpit alongside the daemon, and auto-create is idempotent + race-safe
/// ([`ensure_planning_session`]), so the co-create is benign — no duplicate, no error leak.
fn all_repos_daemon_active() -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::systemd::orchestrate_all_active()
    }
    #[cfg(not(target_os = "linux"))]
    {
        // launchd (macOS): the daemon is loaded iff `launchctl list <label>` exits 0.
        std::process::Command::new("launchctl")
            .arg("list")
            .arg(crate::launchd::orchestrate_all_label())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(session: &str, window: &str, since: i64) -> CockWin {
        CockWin {
            session: session.to_string(),
            window: window.to_string(),
            since,
        }
    }

    #[test]
    fn render_ground_shows_every_section() {
        let d = GroundData {
            runs: vec![RunLine {
                repo: "bn".to_string(),
                queued: 3,
                in_flight: 2,
                merged: 5,
                failed: 1,
                backlog: 7,
            }],
            orphan_backlog: vec![("mt5".to_string(), 4)],
            needs_you: vec!["review eduuh/bn#1 (bn/feat)".to_string()],
            in_progress: vec!["ship #2".to_string()],
            to_start: vec!["triage #3".to_string()],
            next: vec!["unblock bn/x".to_string()],
        };
        let out = render_ground(&d);
        assert!(out.contains("ORCHESTRATE"));
        assert!(out.contains("bn — queued 3 · in-flight 2 · merged 5 · failed 1 · backlog 7"));
        assert!(out.contains("mt5 — 4 pending (no active run)"));
        assert!(out.contains("NEEDS YOU"));
        assert!(out.contains("review eduuh/bn#1 (bn/feat)"));
        assert!(out.contains("IN PROGRESS"));
        assert!(out.contains("TO START"));
        assert!(out.contains("NEXT"));
        assert!(out.contains("unblock bn/x"));
    }

    #[test]
    fn render_ground_empty_is_graceful() {
        let out = render_ground(&GroundData::default());
        assert!(out.contains("(no active run)"));
        // Every lane renders its em-dash placeholder rather than vanishing.
        assert_eq!(out.matches("  —").count(), 4);
    }

    #[test]
    fn is_fleet_window_drops_infra_windows_and_the_cockpit_session() {
        assert!(is_fleet_window("bn", "feat-x", "cron"));
        assert!(!is_fleet_window("bn", "main", "cron"));
        assert!(!is_fleet_window("bn", "MASTER", "cron"));
        assert!(!is_fleet_window("bn", "notes", "cron"));
        assert!(!is_fleet_window("bn", "branch-notes", "cron"));
        assert!(!is_fleet_window("bn", "", "cron"));
        // A window in the cockpit session is never counted as fleet.
        assert!(!is_fleet_window("cron", "cockpit", "cron"));
        assert!(!is_fleet_window("CRON", "feat-x", "cron"));
    }

    #[test]
    fn any_live_fleet_ignores_infra_and_undecorated_windows() {
        // Only a decorated (`since > 0`) fleet window counts as live.
        assert!(any_live_fleet(&[win("bn", "feat-x", 200)], "cron"));
        // main worktree, notes, and the cockpit itself are never fleet, even when decorated.
        assert!(!any_live_fleet(
            &[
                win("bn", "main", 200),
                win("bn", "notes", 200),
                win("cron", "cockpit", 200),
            ],
            "cron"
        ));
        // An undecorated fleet window (no agent running) is not "live".
        assert!(!any_live_fleet(&[win("bn", "feat-x", 0)], "cron"));
        assert!(!any_live_fleet(&[], "cron"));
    }

    #[test]
    fn owner_gate_encodes_single_owner_rule() {
        // The --all-repos supervisor always owns the cockpit.
        assert!(cockpit_owner_ok(true, false));
        assert!(cockpit_owner_ok(true, true));
        // A lone per-repo driver acts only when no all-repos daemon owns the scope.
        assert!(cockpit_owner_ok(false, false));
        assert!(!cockpit_owner_ok(false, true));
    }

    #[test]
    fn cockpit_window_name_scopes_by_repo() {
        assert_eq!(cockpit_window_name(None), "cockpit");
        assert_eq!(cockpit_window_name(Some("bn")), "cockpit-bn");
        assert_eq!(
            cockpit_window_name(Some("owner/repo")),
            "cockpit-owner-repo"
        );
    }

    #[test]
    fn ground_loop_cmd_reinvokes_ground_and_scopes_repo() {
        let c = ground_loop_cmd(Some("bn"), 45);
        assert!(c.contains("orchestrate cockpit ground"));
        assert!(c.contains("--repo 'bn'"));
        // Honors the configured cadence instead of the old hard-coded 15s.
        assert!(c.contains("sleep 45"));
        assert!(!c.contains("sleep 15"));
        // In-place cursor-home redraw (no hard `clear`) to cut flicker.
        assert!(c.contains("\\033[H"));
        assert!(!c.contains("clear"));
        // Prefers `bn` from PATH at runtime, falling back to the baked exe (no stale hard-bake).
        assert!(c.contains("command -v bn"));
        assert!(!ground_loop_cmd(None, 30).contains("--repo"));
    }
}
