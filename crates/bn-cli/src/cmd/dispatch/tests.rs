//! Seed-assembly guardrails for `bn dispatch`.
//!
//! Its own `tests.rs` rather than an inline module: the repo's structure gates read production
//! `.rs` for stray prompt text and hold every file to a never-grow line ratchet, and a test that
//! must assert on real seed wording trips the first while spending the headroom of the second.
//!
//! What it covers: assemble the full dispatch seed for every routing combination and assert which
//! envelope was chosen and that the suppression slots behaved. This is the test that would have
//! caught both round-2 critical bugs — the unreachable ship envelope and the double/contradicting
//! gate instructions — so keep it in sync when envelopes move.

use super::*;

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn seed(scout: bool, yolo: bool, verify: bool, plan_gate: bool, skill_name: &str) -> String {
    let cfg = Config::default();
    let root = repo_root();
    let skill = bn_core::skills::load_skill(&cfg, &root, skill_name)
        .expect("skill catalog loads")
        .unwrap_or_else(|| panic!("shipped skill '{skill_name}' exists"));
    skill_seed_prompt(
        &cfg,
        &root,
        SkillSeedContext {
            title: "Test goal",
            url: None,
            scout,
            yolo,
            verify,
            plan_gate,
        },
        &skill,
    )
    .expect("seed renders")
}

fn gate(verify: bool) -> String {
    plan_gate_seed(
        &Config::default(),
        &repo_root(),
        PlanGateContext {
            repo: "r",
            branch: "b",
            sid: "sid-1",
            verify,
            reviewers: 2,
            quorum: 2,
            plan_file: Some("plans/agents/sid-1.md"),
        },
    )
    .expect("gate renders")
}

/// The routing table itself: with the pipeline flag off (the default) even the skill-less ship
/// dispatch stays on the single-session ship lane; with it on, that lane enters pipeline
/// investigate, while scout and explicit-skill lanes always seed their own envelopes.
#[test]
fn routing_table() {
    // Off by default: the ordinary ship dispatch runs one `ship-solo` session, not the pipeline.
    assert!(!routes_to_pipeline_investigate(false, true, false, None));
    // Opt in via `$BN_DISPATCH_PIPELINE=1`: a skill-less non-scout agent dispatch enters investigate.
    assert!(routes_to_pipeline_investigate(true, true, false, None));
    assert!(!routes_to_pipeline_investigate(true, true, true, None));
    assert!(!routes_to_pipeline_investigate(true, true, false, Some("plan-work")));
    assert!(!routes_to_pipeline_investigate(true, true, false, Some("execute")));
    assert!(!routes_to_pipeline_investigate(true, false, false, None));
}

/// The plan-review gate's decision table.
///
/// `--yolo` raising the gate is the rule that surprises people, so it is pinned here rather than
/// left to the one call site: trusted autonomy buys freedom from approval prompts, not freedom from
/// plan review. `Off` is the only thing that overrides it — which is what lets the repo-setup lane
/// run as one window with no review round trip.
#[test]
fn plan_gate_decision_table() {
    // Auto: config and yolo decide.
    assert!(!plan_gate_enabled(PlanGate::Auto, false, false, false));
    assert!(plan_gate_enabled(PlanGate::Auto, true, false, false));
    assert!(plan_gate_enabled(PlanGate::Auto, false, true, false));
    // Force wins over an unconfigured default.
    assert!(plan_gate_enabled(PlanGate::Force, false, false, false));
    // Off wins over both config and yolo — the whole reason it is not just another bool.
    assert!(!plan_gate_enabled(PlanGate::Off, true, true, false));
    // A scout still skips the plan-review gate: it investigates first, then either ships a bounded
    // fix directly or finishes report-only.
    for request in [PlanGate::Auto, PlanGate::Force, PlanGate::Off] {
        assert!(!plan_gate_enabled(request, true, true, true));
    }
}

#[test]
fn scout_seed_uses_scout_envelope_with_ship_or_report_only_completion() {
    let s = seed(true, false, false, false, "investigate");
    assert!(s.contains("You are a SCOUT"));
    assert!(s.contains("--- WORKFLOW: investigate ---"));
    assert!(s.contains("run `bn verify`, push, open a PR"));
    assert!(s.contains("`bn_pr <url>`"));
    assert!(s.contains("bn agent done --report-only"));
    assert!(!s.contains("CONCURRENCY DISCIPLINE"));
}

#[test]
fn ship_solo_seed_uses_the_solo_stance_even_under_yolo() {
    // `ship-solo` is the default ship lane. Its stance is autonomous-but-ask-on-ambiguity: it must
    // neither force a plan proposal (interactive) nor forbid `ask_user` (unattended), even under
    // `--yolo`, and it never carries a plan-review gate.
    for yolo in [false, true] {
        let s = seed(false, yolo, false, false, "ship-solo");
        assert!(s.contains("--- WORKFLOW: ship-solo ---"));
        assert!(s.contains("ask the human with `ask_user`"));
        assert!(!s.contains("propose a concrete plan"));
        assert!(!s.contains("unattended worktree"));
        // Exactly one completion protocol and one reflect hook, like every ship seed.
        assert_eq!(s.matches("bn agent done --summary").count(), 1);
        assert_eq!(s.matches("bn reflect \"skill:").count(), 1);
    }
}

#[test]
fn explicit_skill_ship_seed_is_interactive_by_default() {
    let s = seed(false, false, false, false, "plan-work");
    assert!(s.contains("propose a concrete plan"));
    assert!(!s.contains("unattended worktree"));
    assert!(s.contains("--- WORKFLOW: plan-work ---"));
    assert!(s.contains("CONCURRENCY DISCIPLINE"));
    // Exactly one completion protocol and one reflect hook.
    assert_eq!(s.matches("bn agent done --summary").count(), 1);
    assert_eq!(s.matches("bn reflect \"skill:").count(), 1);
}

#[test]
fn yolo_seed_carries_the_unattended_stance() {
    let s = seed(false, true, false, false, "plan-work");
    assert!(s.contains("unattended worktree"));
    assert!(!s.contains("propose a concrete plan"));
}

#[test]
fn verify_gate_renders_once_and_never_for_self_gating_workflows() {
    // plan-work/execute gate step 6 on `bn verify` themselves — the envelope must not
    // re-inject the explicit clause for them.
    let s = seed(false, true, true, false, "plan-work");
    assert!(!s.contains("Gate completion on `bn verify`"));
    let s = seed(false, true, true, false, "execute");
    assert!(!s.contains("Gate completion on `bn verify`"));
    // Other workflows get it exactly once.
    let s = seed(false, true, true, false, "debug");
    assert_eq!(s.matches("Gate completion on `bn verify`").count(), 1);
}

#[test]
fn plan_gate_suppresses_bootstrap_verification_and_completion() {
    let s = seed(false, true, true, true, "plan-work");
    // The bootstrap ("Markdown execution contract" is its sentinel phrase) must vanish: the
    // gate owns the plan file, and the two instructions contradicted each other pre-fix.
    assert!(!s.contains("Markdown execution contract"));
    assert!(!s.contains("bn agent done --summary"));
    assert!(!s.contains("Gate completion on `bn verify`"));
    // The appended gate then carries exactly one copy of each.
    let full = format!("{s}{}", gate(true));
    assert_eq!(full.matches("bn agent done --summary").count(), 1);
    assert_eq!(full.matches("Gate completion on `bn verify`").count(), 1);
    // And the approved branch promotes the reviewed draft to the active plan.
    assert!(full.contains("bn plan promote sid-1"));
    assert!(full.contains("--plan-file plans/agents/sid-1.md"));
}

#[test]
fn ungated_plan_work_seed_keeps_the_bootstrap() {
    let s = seed(false, true, false, false, "plan-work");
    assert!(s.contains("Markdown execution contract"));
    assert!(s.contains("bn plan init"));
}
