//! Supervisor pane-classification **corpus** + recovery **fault-injection** tests (#463).
//!
//! The `bn orchestrate` supervisor's pane→state [`classify`](bn_core::orchestrate::classify) and
//! its recovery [`SupervisorAction`]s are safety-critical: a misclassification has caused real
//! incidents in this project — over-cleanup of a lane when a merge actually **FAILED**, a
//! false-`dead` verdict on a still-**working** pane, and burning an item's failure budget on
//! reboot-caused re-dispatch. This file encodes those post-mortems as regression tests, in two
//! halves:
//!
//!   1. a **classification corpus** — one checked-in captured pane tail per documented
//!      [`PaneState`] (under `tests/fixtures/supervisor_panes/`), driven table-wise through the pure
//!      classifier and asserted to its expected state + next [`SupervisorAction`]. The fixtures are
//!      the trimmed, non-empty pane lines the live supervisor sees (mirroring the CLI's
//!      `capture_pane_tail`), so a heuristic regression (a new marker, a reordered priority) trips a
//!      named row here instead of shipping to the fleet.
//!   2. **recovery fault-injection** — the three incident safety-rules asserted against the pure
//!      policy: a failed merge must not reconcile/clean up the lane
//!      ([`ExternalState::is_resolved`]), a reboot must not exhaust the relaunch budget
//!      (`is_fresh_boot` + `boot_reset_candidates` + `reset_backoff`), a hung/frozen agent is judged
//!      *stuck* rather than *working* ([`idle_clean_action`] + [`shows_bare_prompt`]), and
//!      dead-vs-working is disambiguated structurally by `has_window`.
//!
//! Validates #340 (supervision) / #338. Additive: no supervisor logic is changed here.

use bn_core::orchestrate::{
    action_for, classify, decide, idle_clean_action, plan_relaunch, shows_bare_prompt, Backoff,
    ExternalState, ItemStatus, Liveness, OrchestrateState, PaneObservation, PaneState,
    RelaunchDecision, SupervisorAction,
};
use std::collections::HashSet;

/// The CLI's stall dwell for an idle-clean pane (`IDLE_CLEAN_STALE_SECS`) — mirrored so the
/// hung-gate fault-injection exercises the same 10-minute threshold the daemon uses.
const IDLE_CLEAN_STALE_SECS: i64 = 600;

/// Load a captured pane snapshot into the trimmed, non-empty tail lines the live supervisor feeds
/// [`classify`] — the exact shape the CLI's `capture_pane_tail` produces (`trim_end` each line,
/// drop the empties), so a fixture classifies here identically to the real capture.
fn tail_lines(snapshot: &str) -> Vec<String> {
    snapshot
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Embed a captured pane snapshot from `tests/fixtures/supervisor_panes/<name>.txt` at compile time
/// (a missing fixture is a build error, so the corpus can never reference a snapshot that isn't
/// checked in).
macro_rules! pane {
    ($name:literal) => {
        include_str!(concat!("fixtures/supervisor_panes/", $name, ".txt"))
    };
}

/// One corpus row: a named captured pane plus the live signals the CLI gathers alongside it
/// (`has_window`, the Plane-A `@bn_agent` badge, whether the worktree carries WIP), and the
/// [`PaneState`] + [`SupervisorAction`] the supervisor must reach for it.
struct Case {
    name: &'static str,
    snapshot: &'static str,
    has_window: bool,
    plane_a: &'static str,
    has_wip: bool,
    want_state: PaneState,
    want_action: SupervisorAction,
}

impl Case {
    fn obs<'a>(&'a self, tail: &'a [String]) -> PaneObservation<'a> {
        PaneObservation {
            tail,
            has_window: self.has_window,
            plane_a: self.plane_a,
            has_wip: self.has_wip,
        }
    }
}

/// The classification corpus: a representative captured pane for every documented [`PaneState`],
/// plus the incident cases the spec calls out (spinner-beats-error, waiting-review #349,
/// permission-prompt, idle "/ commands", ENOTFOUND/DNS, request-timeout, "Pane is dead",
/// rebase-in-progress). Several states carry more than one snapshot on purpose — `Working` covers a
/// Claude spinner, a "Thinking…" turn, an error-mentioned-mid-turn, and an in-flight rebase; the
/// error states carry a stale `working` Plane-A badge to prove a pane marker beats the badge.
fn corpus() -> Vec<Case> {
    use PaneState::*;
    use SupervisorAction::*;
    vec![
        // --- Working: a live spinner beats everything below it -------------------------------
        Case {
            name: "working-spinner",
            snapshot: pane!("working"),
            has_window: true,
            plane_a: "working",
            has_wip: true,
            want_state: Working,
            want_action: Leave,
        },
        Case {
            name: "thinking",
            snapshot: pane!("thinking"),
            has_window: true,
            plane_a: "working",
            has_wip: false,
            want_state: Working,
            want_action: Leave,
        },
        Case {
            // An API/DNS error merely *mentioned* while the spinner is still up must read as working
            // — never trigger a relaunch/keystroke on a busy agent (the "spinner beats error" rule).
            name: "spinner-beats-error",
            snapshot: pane!("spinner_beats_error"),
            has_window: true,
            plane_a: "",
            has_wip: false,
            want_state: Working,
            want_action: Leave,
        },
        Case {
            // A rebase the harness is actively running (spinner up) — leave it alone; interrupting a
            // rebase mid-apply is exactly the kind of over-recovery the corpus guards against.
            name: "rebase-in-progress",
            snapshot: pane!("rebase_in_progress"),
            has_window: true,
            plane_a: "working",
            has_wip: true,
            want_state: Working,
            want_action: Leave,
        },
        // --- Done: the harness's own Plane-A signal is authoritative --------------------------
        Case {
            name: "done-plane-a",
            snapshot: pane!("done_plane_a"),
            has_window: true,
            plane_a: "done",
            has_wip: false,
            want_state: Done,
            want_action: Land,
        },
        // --- Idle prompts ---------------------------------------------------------------------
        Case {
            // #349 plan-review: the agent posted its plan and waits at a clean idle prompt — leave
            // it for the human to approve (a `waiting` badge is not authoritative, the prompt is).
            name: "waiting-review",
            snapshot: pane!("waiting_review"),
            has_window: true,
            plane_a: "waiting",
            has_wip: false,
            want_state: IdleClean,
            want_action: Leave,
        },
        Case {
            name: "idle-slash-commands",
            snapshot: pane!("idle_slash_commands"),
            has_window: true,
            plane_a: "",
            has_wip: false,
            want_state: IdleClean,
            want_action: Leave,
        },
        Case {
            // A bare idle prompt but the worktree carries unpushed WIP — the agent stalled/died with
            // work to recover (the case that was hand-driven all session).
            name: "idle-with-wip",
            snapshot: pane!("idle_with_wip"),
            has_window: true,
            plane_a: "",
            has_wip: true,
            want_state: IdleWithWip,
            want_action: RelaunchToResume,
        },
        // --- Menus ----------------------------------------------------------------------------
        Case {
            name: "trust-menu",
            snapshot: pane!("trust_menu"),
            has_window: true,
            plane_a: "",
            has_wip: false,
            want_state: TrustMenu,
            want_action: AnswerTrustMenu,
        },
        Case {
            // A harness command permission with a session-scoped allow choice is distinct from an
            // arbitrary agent question, so trusted ship lanes can clear only this safe shape.
            name: "permission-prompt",
            snapshot: pane!("permission_prompt"),
            has_window: true,
            plane_a: "working",
            has_wip: false,
            want_state: CommandApproval,
            want_action: AnswerCommandApproval,
        },
        Case {
            name: "question-yes-no",
            snapshot: pane!("question_yn"),
            has_window: true,
            plane_a: "",
            has_wip: false,
            want_state: QuestionMenu,
            want_action: AnswerQuestion,
        },
        // --- Outages: a pane marker beats a stale `working` badge -----------------------------
        Case {
            name: "api-error-5xx",
            snapshot: pane!("api_error"),
            has_window: true,
            plane_a: "working",
            has_wip: false,
            want_state: ApiError,
            want_action: RelaunchToResume,
        },
        Case {
            name: "dns-enotfound",
            snapshot: pane!("dns_enotfound"),
            has_window: true,
            plane_a: "working",
            has_wip: false,
            want_state: DnsOutage,
            want_action: RelaunchToResume,
        },
        Case {
            // A request/connection timeout is a network-family failure (ETIMEDOUT) → dns-outage lane.
            name: "request-timeout",
            snapshot: pane!("request_timeout"),
            has_window: true,
            plane_a: "working",
            has_wip: false,
            want_state: DnsOutage,
            want_action: RelaunchToResume,
        },
        // --- Usage limit: the blocking dialog is a numbered menu, but must switch, not surface ---
        Case {
            name: "usage-limited",
            snapshot: pane!("usage_limited"),
            has_window: true,
            plane_a: "working",
            has_wip: false,
            want_state: Limited,
            want_action: SwitchHarness,
        },
        // --- Dead: structural — no live window (the process is gone) --------------------------
        Case {
            // "Pane is dead": the tmux window is gone (the harness process exited). `has_window`
            // is false, so death is decided structurally regardless of the frozen scrollback — and
            // because the lane still has WIP, the action is to recover it.
            name: "pane-dead",
            snapshot: pane!("pane_dead"),
            has_window: false,
            plane_a: "working",
            has_wip: true,
            want_state: Dead,
            want_action: RelaunchToResume,
        },
    ]
}

/// Every documented [`PaneState`]. The exhaustiveness `match` is a compile-time tripwire: adding a
/// variant fails to build until it is listed here **and** given a corpus fixture (see
/// [`corpus_covers_every_documented_pane_state`]).
fn all_pane_states() -> Vec<PaneState> {
    use PaneState::*;
    let all = vec![
        Working,
        IdleClean,
        IdleWithWip,
        TrustMenu,
        CommandApproval,
        QuestionMenu,
        ApiError,
        DnsOutage,
        Limited,
        Done,
        Dead,
    ];
    for s in &all {
        match s {
            Working | IdleClean | IdleWithWip | TrustMenu | CommandApproval | QuestionMenu
            | ApiError | DnsOutage | Limited | Done | Dead => {}
        }
    }
    all
}

// ===========================================================================
// (1) Classification corpus
// ===========================================================================

#[test]
fn every_captured_pane_classifies_to_its_documented_state() {
    for c in corpus() {
        let tail = tail_lines(c.snapshot);
        let obs = c.obs(&tail);

        let got = classify(&obs);
        assert_eq!(
            got, c.want_state,
            "classify drifted for `{}` — want {:?}, got {:?}\n--- captured pane ---\n{}--- end ---",
            c.name, c.want_state, got, c.snapshot
        );

        // The next-action is deterministic from the state: pin `decide` (classify + action_for) so
        // each row asserts both the classification *and* the recovery action the supervisor takes.
        let (state, action) = decide(&obs);
        assert_eq!(state, c.want_state, "decide state for `{}`", c.name);
        assert_eq!(
            action, c.want_action,
            "decide action for `{}` — want {:?}, got {:?}",
            c.name, c.want_action, action
        );
        assert_eq!(
            action_for(c.want_state),
            c.want_action,
            "action_for `{}`",
            c.name
        );
    }
}

#[test]
fn corpus_covers_every_documented_pane_state() {
    let covered: Vec<PaneState> = corpus().iter().map(|c| c.want_state).collect();
    for s in all_pane_states() {
        assert!(
            covered.contains(&s),
            "no corpus fixture classifies to {:?} ({}) — add a captured pane for it",
            s,
            s.label()
        );
    }
}

// ===========================================================================
// (2) Recovery fault-injection — the incident safety-rules
// ===========================================================================

/// merge-fail-no-cleanup: a FAILED merge must NOT reconcile the item to `merged` or reap its lane.
/// The daemon's reconcile pass gates the terminal transition on [`ExternalState::is_resolved`]:
/// cleanup happens only when the branch PR is **merged**, the linked issue is **closed**, or the
/// worktree was **reaped**. A failed gate produces none of these — the PR stays open, the issue
/// open, the worktree on disk — so the guard must refuse and leave the lane in-flight to retry.
#[test]
fn merge_fail_does_not_clean_up_the_lane() {
    let merge_failed = ExternalState {
        branch_pr_merged: false,
        issue_closed: false,
        worktree_gone: false,
    };
    assert!(
        !merge_failed.is_resolved(),
        "a failed merge must NOT resolve the item (no cleanup)"
    );
    assert_eq!(merge_failed.via(), None);

    // Drive it through the queue the way `reconcile_external` does: an in-flight item whose external
    // state is unresolved is never marked merged — it stays Dispatched, its worktree/window intact.
    let mut st = OrchestrateState::new("bn", &["338".to_string()], 1, false, 0);
    st.mark_dispatched(0, Some("dispatch/338".into()), 10);
    if merge_failed.is_resolved() {
        st.mark_merged("338", None, 20); // must NOT run
    }
    let item = st.queue.iter().find(|i| i.id == "338").unwrap();
    assert_eq!(
        item.status,
        ItemStatus::Dispatched,
        "a failed merge must leave the lane in-flight, not reconciled to merged"
    );
    assert_eq!(
        st.counts().merged,
        0,
        "nothing may be cleaned up on a failed merge"
    );

    // Each genuine completion signal, on its own, DOES resolve → the cleanup is allowed, and `via`
    // reports the most specific evidence.
    for (ext, via) in [
        (
            ExternalState {
                branch_pr_merged: true,
                ..Default::default()
            },
            "branch-merged-pr",
        ),
        (
            ExternalState {
                issue_closed: true,
                ..Default::default()
            },
            "issue-closed",
        ),
        (
            ExternalState {
                worktree_gone: true,
                ..Default::default()
            },
            "worktree-reaped",
        ),
    ] {
        assert!(ext.is_resolved(), "{via} must resolve the item");
        assert_eq!(ext.via(), Some(via));
    }
}

/// reboot-no-attempt-burn: a reboot kills every in-flight tmux window, so each item next reads
/// `Dead` and is re-dispatched — burning one relaunch `attempt`. A machine that reboots more than
/// `max_attempts` times would drive a perfectly healthy item to `Failed`. The #380 fix: on a fresh
/// boot marker, reset the backoff of in-flight items whose worktree still lives, so each reboot
/// starts a fresh budget. Inject far more reboots than the cap and assert the item never fails.
#[test]
fn reboot_re_dispatch_never_burns_the_attempt_budget() {
    let policy = Backoff::default();
    let mut st = OrchestrateState::new("bn", &["500".to_string()], 1, false, 0);
    st.mark_dispatched(0, Some("dispatch/500".into()), 1);
    let live: HashSet<String> = ["500".to_string()].into_iter().collect();

    let mut now = 100i64;
    for boot in 0..(policy.max_attempts * 4) {
        let boot_id = format!("boot-{boot}");
        // The daemon's boot pass: on a *changed* boot marker, reset the in-flight backoff budget.
        if st.is_fresh_boot(&boot_id) {
            let cands = st.boot_reset_candidates(&live);
            st.reset_backoff(&cands, now);
        }
        st.record_boot(&boot_id, now);

        // Post-reboot the window is gone → Dead. Plan the relaunch on the (reset) budget.
        let (attempts, last) = st
            .queue
            .iter()
            .find(|i| i.id == "500")
            .map(|i| (i.attempts, i.last_relaunch_at))
            .unwrap();
        match plan_relaunch(PaneState::Dead, attempts, last, now, false, &policy) {
            RelaunchDecision::GiveUp => panic!("a reboot must never exhaust the failure budget"),
            RelaunchDecision::Wait => {}
            RelaunchDecision::Relaunch(_) => {
                st.bump_attempts("500", now);
            }
        }
        now += 10_000; // well past any backoff window before the next reboot
    }

    let item = st.queue.iter().find(|i| i.id == "500").unwrap();
    assert_ne!(
        item.status,
        ItemStatus::Failed,
        "reboot cycling must never fail a healthy item"
    );
    assert!(
        item.attempts <= 1,
        "each reboot re-dispatch starts from a fresh budget, never accreting attempts"
    );
}

/// hung-gate detection: an agent that kicked off a gate and then froze (process alive but 0% CPU,
/// output frozen) must be judged **stuck** and recovered — not left as if it were still working.
/// The daemon models "frozen" from the harness liveness: it worked (`aic > 0`), produced no PR
/// (`has_pr == false`), and has been silent past the stall dwell (`idle_secs >= stale`). That
/// combination resolves to [`SupervisorAction::RelaunchToResume`], while a between-turns pause, a
/// just-launched agent, or a finished one all stay `Leave`.
#[test]
fn hung_gate_is_detected_as_stuck_not_working() {
    let stale = IDLE_CLEAN_STALE_SECS;

    // The hung gate: worked, no PR, frozen well past the dwell → stuck → recover.
    let hung = Liveness {
        aic: 4_200,
        has_pr: false,
        idle_secs: stale + 300,
    };
    assert_eq!(
        idle_clean_action(hung, stale),
        SupervisorAction::RelaunchToResume,
        "a worked-then-frozen agent is stuck and must be recovered, not treated as working"
    );

    // Look-alikes that must NOT be cut off:
    let between_turns = Liveness {
        aic: 4_200,
        has_pr: false,
        idle_secs: stale - 1, // frozen, but not yet past the dwell — could be mid-gate
    };
    assert_eq!(
        idle_clean_action(between_turns, stale),
        SupervisorAction::Leave,
        "a brief between-turns idle must not be cut off before the dwell elapses"
    );
    let just_launched = Liveness {
        aic: 0, // never produced a turn — nothing to resume
        has_pr: false,
        idle_secs: stale + 10_000,
    };
    assert_eq!(
        idle_clean_action(just_launched, stale),
        SupervisorAction::Leave
    );
    let finished = Liveness {
        aic: 4_200,
        has_pr: true, // PR is up — the merge-queue lands it; never relaunch delivered work
        idle_secs: stale + 10_000,
    };
    assert_eq!(idle_clean_action(finished, stale), SupervisorAction::Leave);

    // The stall verdict is gated on a positively-observed idle prompt: an ambiguous pane (spinner
    // scrolled off the tail, no prompt captured) is a possible mid-long-turn agent and is never
    // judged stalled. A real idle prompt is; a spinner/scrollback pane is not.
    let idle_tail = tail_lines(pane!("idle_slash_commands"));
    assert!(
        shows_bare_prompt(&PaneObservation {
            tail: &idle_tail,
            has_window: true,
            plane_a: "",
            has_wip: false,
        }),
        "an idle `❯` prompt must be eligible for the stall check"
    );
    let working_tail = tail_lines(pane!("working"));
    assert!(
        !shows_bare_prompt(&PaneObservation {
            tail: &working_tail,
            has_window: true,
            plane_a: "working",
            has_wip: false,
        }),
        "a spinner/scrollback pane has no bare prompt — never eligible to be judged stalled"
    );
}

/// dead-vs-working disambiguation: `Dead` is decided **structurally** — only when no live tmux
/// window exists. A live window with a spinner is `Working` no matter how quiet, and is never
/// misread as dead (the false-`dead`-on-a-working-pane incident); conversely a vanished window is
/// `Dead` even if its frozen scrollback still shows a spinner (a stale "working" tail can't keep a
/// gone process alive).
#[test]
fn dead_vs_working_is_disambiguated_by_window_presence() {
    let spinner = tail_lines(pane!("working"));

    // Same captured pane, window gone → Dead (structural death wins over the stale spinner).
    let gone = PaneObservation {
        tail: &spinner,
        has_window: false,
        plane_a: "working",
        has_wip: true,
    };
    assert_eq!(classify(&gone), PaneState::Dead);
    assert_eq!(
        action_for(classify(&gone)),
        SupervisorAction::RelaunchToResume
    );

    // Same pane, window live → Working (a live spinner is never called dead).
    let live = PaneObservation {
        tail: &spinner,
        has_window: true,
        plane_a: "working",
        has_wip: true,
    };
    assert_eq!(classify(&live), PaneState::Working);
    assert_eq!(action_for(classify(&live)), SupervisorAction::Leave);

    // A live pane that merely *mentions* a crash/DNS/API error while the spinner is up stays
    // Working — the busy agent is never mistaken for a dead or errored one.
    let noisy = tail_lines(pane!("spinner_beats_error"));
    let obs = PaneObservation {
        tail: &noisy,
        has_window: true,
        plane_a: "",
        has_wip: false,
    };
    assert_eq!(classify(&obs), PaneState::Working);
}
