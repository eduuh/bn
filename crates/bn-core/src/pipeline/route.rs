//! How much pipeline a task gets — the complexity classifier.
//!
//! [`super::sweep::SWEPT`] runs the same three stages for every admitted task, so a one-line fix
//! and an ambiguous rewrite cost the same. This module decides what a task *should* get: which
//! stages, how many sub-investigators, how many plan reviewers, how many critique rounds.
//!
//! **Nothing reads the answer yet.** `bn pipeline route` reports it and stops there, deliberately:
//! the thresholds below are guesses until they have been run over a real store, and a classifier
//! that changes behaviour before anyone has looked at its output is a classifier nobody can tune.
//!
//! # The trust boundary is a type
//!
//! Signals arrive in two structs, and which one a field lives in decides what it may do:
//!
//! - [`Declared`] is what bn itself authored or computed. It may narrow a route as well as widen
//!   it, because it cannot be forged: [`crate::todo::neutralize_meta`] strips meta keys out of
//!   untrusted text, and `task_pull` imports only a neutralized title plus `src:` — never the issue
//!   body — so an `est:` or a `## Done When` reaching this module was written by bn or by a person.
//! - [`Observed`] is derived from text or systems a third party can influence: GitHub labels, the
//!   shape of a title. It is **widening only**. [`classify`] reads it in no branch that can lower
//!   the route, so an issue an attacker files can make bn spend *more* and never less.
//!
//! That asymmetry is the same one the gate ratchet enforces for `gate` in
//! [`super::profile`], and it exists for the reason recorded in [`crate::queue::classify`]: a
//! substring match on prompt text once froze a person's priority-100 work. The worst case for a
//! widening-only signal is a wasted investigation.
//!
//! Narrowing *spend* (model, fan-out) is a budget decision and is allowed from the task line.
//! Narrowing *review* (the gate, a held stage) is not, and never enters here — [`RoutePlan::gate`]
//! is a floor that the profile ratchet may raise and nothing here can lower.

use super::stage::Stage;
use crate::resolver::Config;
use crate::todo::Task;
use serde::{Deserialize, Serialize};

/// The shape of pipeline a task gets, ordered cheapest → most thorough so that `Ord::max` *is*
/// "widen".
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize, Hash,
)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    /// A mechanical, fully-specified change: a rename, a config value, a one-line guard.
    Mechanical,
    /// Today's arc, unchanged. The default for anything the signals do not clearly place.
    #[default]
    Standard,
    /// Ambiguous, large, or repeatedly un-scopable: fan out, and critique the result.
    Deep,
}

impl Route {
    pub const ALL: [Route; 3] = [Route::Mechanical, Route::Standard, Route::Deep];

    pub fn as_str(self) -> &'static str {
        match self {
            Route::Mechanical => "mechanical",
            Route::Standard => "standard",
            Route::Deep => "deep",
        }
    }

    /// Parse a declared route. An unrecognised value is `None` so a typo inherits the default
    /// rather than silently picking a route nobody meant — the rule `GatePolicy::parse` follows.
    pub fn parse(raw: &str) -> Option<Route> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "mechanical" => Some(Route::Mechanical),
            "standard" => Some(Route::Standard),
            "deep" => Some(Route::Deep),
            _ => None,
        }
    }
}

/// The task meta key carrying a stamped or hand-pinned route.
pub const ROUTE_KEY: &str = "route";

/// Signals bn itself authored or computed. May narrow a route as well as widen it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Declared {
    /// `est:` in minutes, already through [`crate::smart::parse_est_minutes`].
    pub est_minutes: Option<i64>,
    /// How many `## Done When` criteria the task's context doc lists.
    pub done_when: usize,
    /// `smart_tries` — how many times the cheap enhance agent failed to scope this.
    pub smart_tries: i64,
    /// A route already stamped on the task, or typed by a person.
    pub pinned: Option<Route>,
}

/// Signals derived from text or systems a third party can influence.
///
/// **Widening only.** No field here is read by a branch that can lower the route.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    /// [`crate::plan_review::risk_reasons`] — the existing sensitive-surface list, reused rather
    /// than duplicated.
    pub risk_reasons: Vec<String>,
    /// The title opens with an activity rather than an outcome.
    pub vague_title: bool,
    /// How much the operator captured with the task: body, links, attachments. Counts only.
    pub evidence_items: usize,
    /// The context doc records an open `## Blockers`.
    pub blocked: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signals {
    pub declared: Declared,
    pub observed: Observed,
}

/// One reason a route landed where it did.
///
/// `code` is a stable token; routing and reporting switch on it, never on the prose. Mirrors
/// [`crate::smart::SmartGap`]'s discipline, for the same reason: a message is for a person, and a
/// decision that reads one is a decision that breaks when the wording improves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouteReason {
    pub code: &'static str,
    pub detail: String,
}

fn reason(code: &'static str, detail: impl Into<String>) -> RouteReason {
    RouteReason {
        code,
        detail: detail.into(),
    }
}

/// What a route buys: the stages to run and the budget for each multi-agent step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoutePlan {
    pub route: Route,
    /// A sensitive surface is in scope, so this task is reviewed whatever its size says.
    pub guarded: bool,
    /// The stages to sweep, in flow order.
    pub stages: Vec<Stage>,
    /// Most sub-investigators the investigate stage may fan out to; `0` disables fan-out.
    pub fanout_cap: usize,
    /// Independent plan reviewers; `0` keeps today's human-only gate.
    pub reviewers: usize,
    /// Execute↔critique rounds before escalating to a person.
    pub critic_rounds: u32,
    /// The **floor** this route puts under the stage gate. Enters the profile ratchet as a layer
    /// that may only tighten; it can never release a hold another layer set.
    pub gate: super::profile::GatePolicy,
    pub reasons: Vec<RouteReason>,
}

/// Where the ceiling on est-derived size sits: past this a task is Deep whatever else it says.
///
/// Below [`crate::smart::MAX_EST_MINUTES`], which is the gate's "too large to be one task at all"
/// line — this is the softer "too large to plan in one pass" line.
const DEEP_EST_MINUTES: i64 = 180;

/// A task this small, with criteria and no failed scoping attempts, is mechanical.
const MECHANICAL_EST_MINUTES: i64 = 30;

/// Classify a task from its signals. Pure: no store, no clock, no config.
pub fn classify(sig: &Signals) -> RoutePlan {
    let mut route = Route::Standard;
    let mut reasons = Vec::new();

    // --- widen, from what bn declared -----------------------------------------------------
    if let Some(est) = sig.declared.est_minutes.filter(|m| *m > DEEP_EST_MINUTES) {
        route = route.max(Route::Deep);
        reasons.push(reason("large_estimate", format!("est is {est}m")));
    }
    if sig.declared.done_when >= 5 {
        route = route.max(Route::Deep);
        reasons.push(reason(
            "many_criteria",
            format!("{} done-when criteria", sig.declared.done_when),
        ));
    }
    if sig.declared.smart_tries >= 2 {
        route = route.max(Route::Deep);
        reasons.push(reason(
            "hard_to_scope",
            format!("{} failed enhance attempts", sig.declared.smart_tries),
        ));
    }

    // --- widen, from what was observed ----------------------------------------------------
    // Everything below may only raise the route. See the module docs.
    if sig.observed.blocked {
        route = route.max(Route::Deep);
        reasons.push(reason("blocked", "the doc records an open blocker"));
    }
    if sig.observed.vague_title {
        route = route.max(Route::Deep);
        reasons.push(reason(
            "vague_title",
            "the title names an activity, not an outcome",
        ));
    }
    if sig.observed.evidence_items >= 3 {
        route = route.max(Route::Deep);
        reasons.push(reason(
            "much_evidence",
            format!("{} captured items to read", sig.observed.evidence_items),
        ));
    }

    let guarded = !sig.observed.risk_reasons.is_empty();
    if guarded {
        route = route.max(Route::Deep);
        reasons.push(reason(
            "sensitive_surface",
            sig.observed.risk_reasons.join(", "),
        ));
    }

    // --- narrow, from declarations only ---------------------------------------------------
    // Reached only from Standard: anything widened above stays widened, and `guarded` blocks it
    // outright. Note this branch reads `sig.declared` and never `sig.observed`.
    if route == Route::Standard && !guarded {
        let small = sig
            .declared
            .est_minutes
            .is_some_and(|m| m <= MECHANICAL_EST_MINUTES);
        let specified = (1..=4).contains(&sig.declared.done_when);
        if small && specified && sig.declared.smart_tries == 0 {
            route = Route::Mechanical;
            reasons.push(reason(
                "small_and_specified",
                format!(
                    "est <= {MECHANICAL_EST_MINUTES}m with {} criteria, scoped first try",
                    sig.declared.done_when
                ),
            ));
        }
    }

    // --- an explicit pin is the decision --------------------------------------------------
    if let Some(pinned) = sig.declared.pinned {
        if pinned != route {
            reasons.push(reason(
                "pinned",
                format!(
                    "stamped {} (signals said {})",
                    pinned.as_str(),
                    route.as_str()
                ),
            ));
        }
        route = pinned;
    }

    plan_for(route, guarded, reasons)
}

/// The budget a route and its guard buy.
fn plan_for(route: Route, guarded: bool, reasons: Vec<RouteReason>) -> RoutePlan {
    let (mut stages, fanout_cap, reviewers, critic_rounds) = match route {
        // Investigate is kept even here, with fan-out off. It is the only stage the SMART gate is
        // anchored on (`gate::is_smart_gated`), so dropping it would delete that gate for the
        // dispatch path too — a routing change must not quietly remove a check. Only Plan goes.
        Route::Mechanical => (vec![Stage::Investigate, Stage::Execute], 0, 0, 0),
        Route::Standard => (
            vec![Stage::Investigate, Stage::Plan, Stage::Execute],
            3,
            0,
            3,
        ),
        Route::Deep => (
            vec![
                Stage::Investigate,
                Stage::Plan,
                Stage::Execute,
                Stage::Critique,
            ],
            super::investigate::MAX_FANOUT,
            3,
            3,
        ),
    };

    let (gate, reviewers, critic_rounds) = if guarded {
        if !stages.contains(&Stage::Critique) {
            stages.push(Stage::Critique);
        }
        (
            super::profile::GatePolicy::Hold,
            reviewers.max(3),
            critic_rounds.max(3),
        )
    } else {
        (super::profile::GatePolicy::Auto, reviewers, critic_rounds)
    };

    RoutePlan {
        route,
        guarded,
        stages,
        fanout_cap,
        reviewers,
        critic_rounds,
        gate,
        reasons,
    }
}

/// Read a task's signals off the store, then [`classify`].
///
/// The IO half, mirroring the [`super::profile::layer`] / [`super::profile::resolve`] split: every
/// read here tolerates failure by contributing nothing, so a missing doc yields a Standard route
/// rather than an error.
pub fn resolve(cfg: &Config, task: &Task) -> RoutePlan {
    let doc = crate::task_doc::read(cfg, task);
    let captured = crate::task_agents::CapturedContext::read(cfg, task);
    let labels: Vec<String> = task
        .meta("labels")
        .map(|raw| {
            raw.split(',')
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default();

    classify(&Signals {
        declared: Declared {
            est_minutes: task.meta("est").and_then(crate::smart::parse_est_minutes),
            done_when: doc.as_ref().map(|d| d.done_when().len()).unwrap_or(0),
            smart_tries: task
                .meta(super::gate::SMART_TRIES_KEY)
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            pinned: task.meta(ROUTE_KEY).and_then(Route::parse),
        },
        observed: Observed {
            risk_reasons: crate::plan_review::risk_reasons(&labels, &[]),
            vague_title: crate::smart::opens_vaguely(&task.summary()),
            evidence_items: usize::from(captured.has_body)
                + captured.links
                + captured.attachments.len(),
            blocked: doc
                .as_ref()
                .and_then(|d| d.section("Blockers"))
                .is_some_and(|b| !b.trim().is_empty()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::profile::GatePolicy;

    fn declared(est: i64, done_when: usize, tries: i64) -> Signals {
        Signals {
            declared: Declared {
                est_minutes: Some(est),
                done_when,
                smart_tries: tries,
                pinned: None,
            },
            observed: Observed::default(),
        }
    }

    /// The zero-diff guarantee: an unremarkable task walks exactly today's arc.
    #[test]
    fn standard_is_todays_pipeline_unchanged() {
        let plan = classify(&Signals::default());
        assert_eq!(plan.route, Route::Standard);
        assert_eq!(plan.stages, super::super::sweep::SWEPT.to_vec());
        assert_eq!(plan.gate, GatePolicy::Auto);
        assert!(!plan.guarded);
    }

    #[test]
    fn small_specified_work_scoped_first_try_is_mechanical() {
        let plan = classify(&declared(20, 2, 0));
        assert_eq!(plan.route, Route::Mechanical);
        assert_eq!(plan.fanout_cap, 0);
        assert_eq!(plan.stages, vec![Stage::Investigate, Stage::Execute]);
    }

    /// Even the cheapest route keeps Investigate: it is the only stage the SMART gate is anchored
    /// on, so dropping it would remove that check for the dispatch path as a side effect.
    #[test]
    fn every_route_keeps_the_smart_gated_stage() {
        for route in Route::ALL {
            for guarded in [false, true] {
                let plan = plan_for(route, guarded, Vec::new());
                assert!(
                    plan.stages.contains(&Stage::Investigate),
                    "{route:?} guarded={guarded} dropped Investigate"
                );
                assert!(
                    plan.stages.contains(&Stage::Execute),
                    "{route:?} guarded={guarded} dropped Execute"
                );
            }
        }
    }

    #[test]
    fn a_big_or_hard_to_scope_task_goes_deep() {
        assert_eq!(classify(&declared(240, 2, 0)).route, Route::Deep);
        assert_eq!(classify(&declared(20, 2, 2)).route, Route::Deep);
        assert_eq!(classify(&declared(20, 6, 0)).route, Route::Deep);
    }

    /// **The test that matters.** Cheapness evidence that lives only in `Observed` must never buy a
    /// cheaper route. The types already enforce this — the narrowing branch reads no `Observed`
    /// field — so this asserts a property a future refactor merging the structs would break.
    #[test]
    fn observed_signals_can_never_narrow_a_route() {
        let mut sig = declared(20, 2, 0);
        assert_eq!(classify(&sig).route, Route::Mechanical, "baseline");

        // Every observed signal, one at a time: each may only hold or raise the route.
        for mutate in [
            (|s: &mut Signals| s.observed.vague_title = true) as fn(&mut Signals),
            |s: &mut Signals| s.observed.blocked = true,
            |s: &mut Signals| s.observed.evidence_items = 9,
            |s: &mut Signals| s.observed.risk_reasons = vec!["auth".into()],
        ] {
            let mut probe = sig.clone();
            mutate(&mut probe);
            assert!(
                classify(&probe).route >= Route::Mechanical,
                "an observed signal lowered the route"
            );
        }

        // And they cannot rescue a Deep task down to Standard either.
        sig.declared.est_minutes = Some(600);
        sig.observed = Observed::default();
        assert_eq!(classify(&sig).route, Route::Deep);
    }

    /// A sensitive surface forces review whatever the size says.
    #[test]
    fn a_guarded_task_is_held_and_critiqued() {
        let mut sig = declared(15, 1, 0);
        sig.observed.risk_reasons = vec!["auth".to_string()];
        let plan = classify(&sig);
        assert!(plan.guarded);
        assert_eq!(plan.gate, GatePolicy::Hold);
        assert!(plan.stages.contains(&Stage::Critique));
        assert!(plan.reviewers >= 3);
        assert_ne!(plan.route, Route::Mechanical, "guarded blocks narrowing");
    }

    #[test]
    fn a_pin_overrides_the_signals_and_says_so() {
        let mut sig = declared(600, 9, 3);
        sig.declared.pinned = Some(Route::Mechanical);
        let plan = classify(&sig);
        assert_eq!(plan.route, Route::Mechanical);
        assert!(plan.reasons.iter().any(|r| r.code == "pinned"));
    }

    #[test]
    fn a_route_round_trips_through_its_token() {
        for route in Route::ALL {
            assert_eq!(Route::parse(route.as_str()), Some(route));
        }
        assert_eq!(Route::parse("mechanicl"), None, "a typo must not resolve");
    }

    /// Deep never funds more sub-investigators than the fan-out ceiling allows.
    #[test]
    fn the_fanout_budget_respects_the_hard_ceiling() {
        for route in Route::ALL {
            let plan = plan_for(route, false, Vec::new());
            assert!(plan.fanout_cap <= super::super::investigate::MAX_FANOUT);
        }
    }
}
