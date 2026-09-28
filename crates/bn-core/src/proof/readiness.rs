//! Readiness: where each `done-when` criterion stands, derived from proofs and verdicts. Pure.

use super::model::{Judgement, Proof, Verdict};
use crate::model::Item;
use serde::Serialize;
use std::collections::HashMap;

/// How current an accepted proof's commit must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Freshness {
    /// Taken at the current head. Any new commit makes earlier evidence stale.
    #[default]
    Exact,
    /// Taken at a commit the current head descends from.
    Ancestor,
    /// Any commit.
    Any,
}

impl Freshness {
    pub fn parse(s: &str) -> Option<Freshness> {
        match s.trim() {
            "exact" => Some(Freshness::Exact),
            "ancestor" => Some(Freshness::Ancestor),
            "any" => Some(Freshness::Any),
            _ => None,
        }
    }
}

/// One criterion's standing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum ItemState {
    /// No proof speaks to it.
    Unproven,
    /// Proof is attached, but nobody has judged it yet.
    Attached { proofs: Vec<String> },
    /// Someone other than its author accepted current evidence.
    Accepted { proof: String, judge: String },
    /// Evidence was accepted, but at a commit the head has moved past.
    Stale { proof: String, sha: String },
    /// Its evidence was rejected.
    Rejected { proof: String, judge: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ItemReadiness {
    pub id: String,
    pub text: String,
    #[serde(flatten)]
    pub state: ItemState,
    /// Ticked done in the note without accepted evidence behind it. Stays visible as such.
    pub hand_ticked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Readiness {
    pub items: Vec<ItemReadiness>,
    /// Every criterion accepted. False when there are none: nothing was asked, nothing is proven.
    pub ready: bool,
}

impl Readiness {
    pub fn item(&self, id: &str) -> Option<&ItemReadiness> {
        self.items.iter().find(|i| i.id == id)
    }
}

/// Where each criterion stands. `is_ancestor(sha, head)` answers the `Ancestor` policy.
pub fn derive(
    done_when: &[Item],
    proofs: &[Proof],
    verdicts: &[Verdict],
    head: Option<&str>,
    freshness: Freshness,
    is_ancestor: &dyn Fn(&str, &str) -> bool,
) -> Readiness {
    // The standing verdict per proof: the last accept/reject, unless its author withdrew it.
    let mut last: HashMap<&str, &Verdict> = HashMap::new();
    let mut withdrawn: Vec<&str> = Vec::new();
    for v in verdicts {
        match v.verdict {
            Judgement::Withdraw => withdrawn.push(&v.proof),
            _ => {
                last.insert(&v.proof, v);
            }
        }
    }
    // Under `exact` and `ancestor`, evidence with no commit — or a head nobody can read — can't be
    // shown current, so it isn't: it reads as stale rather than proving anything indefinitely.
    let fresh = |p: &Proof| match (freshness, p.sha.as_deref(), head) {
        (Freshness::Any, _, _) => true,
        (_, None, _) | (_, _, None) => false,
        (Freshness::Exact, Some(s), Some(h)) => h.starts_with(s) || s.starts_with(h),
        (Freshness::Ancestor, Some(s), Some(h)) => {
            h.starts_with(s) || s.starts_with(h) || is_ancestor(s, h)
        }
    };
    let items: Vec<ItemReadiness> = done_when
        .iter()
        .map(|w| {
            let mine: Vec<&Proof> = proofs
                .iter()
                .filter(|p| p.for_.contains(&w.id) && !withdrawn.contains(&p.id.as_str()))
                .collect();
            let verdict = |p: &Proof| last.get(p.id.as_str()).copied();
            let accepted = |p: &Proof| {
                !p.failed() && verdict(p).is_some_and(|v| v.verdict == Judgement::Accept)
            };
            let state = if let Some(p) = mine.iter().find(|p| accepted(p) && fresh(p)) {
                ItemState::Accepted {
                    proof: p.id.clone(),
                    judge: verdict(p).map(|v| v.judge.clone()).unwrap_or_default(),
                }
            } else if let Some(p) = mine.iter().find(|p| accepted(p)) {
                ItemState::Stale {
                    proof: p.id.clone(),
                    sha: p.sha.clone().unwrap_or_default(),
                }
            } else if mine.iter().any(|p| verdict(p).is_none()) {
                ItemState::Attached {
                    proofs: mine
                        .iter()
                        .filter(|p| verdict(p).is_none())
                        .map(|p| p.id.clone())
                        .collect(),
                }
            } else if let Some(p) = mine.first() {
                ItemState::Rejected {
                    proof: p.id.clone(),
                    judge: verdict(p).map(|v| v.judge.clone()).unwrap_or_default(),
                }
            } else {
                ItemState::Unproven
            };
            let is_accepted = matches!(state, ItemState::Accepted { .. });
            ItemReadiness {
                id: w.id.clone(),
                text: w.text.clone(),
                hand_ticked: w.done == Some(true) && !is_accepted,
                state,
            }
        })
        .collect();
    let ready = !items.is_empty()
        && items
            .iter()
            .all(|i| matches!(i.state, ItemState::Accepted { .. }));
    Readiness { items, ready }
}
