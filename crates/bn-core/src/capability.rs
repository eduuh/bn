//! Node capability matching — does this box advertise what a piece of work needs?
//!
//! Split out of [`crate::scheduler`] (which was near the 1000-line ratchet) so the matcher can
//! grow the skill/harness/MCP axes the queue needs without the admission arithmetic growing with
//! it. The predicates here are byte-for-byte the #728 behaviour they were extracted from; the
//! typed [`crate::queue::kind::Requirements`] projection layers on top rather than replacing them.
//!
//! Everything is matched **case-insensitively**, deliberately: config spelling must not be able to
//! weaken a hard policy by accident.
use crate::hosts::NodeRecord;
use crate::queue::kind::Requirements;

/// Reserved label prefixes that project a **typed** requirement into the flat node-label namespace
/// the #728 capability matcher already understands: `skill:investigate`, `harness:claude`, `mcp:bn`.
///
/// One namespace rather than parallel typed lists on both [`NodeRecord`] and [`Requirements`] is
/// what lets the existing, working matcher be reused instead of duplicated. A node advertises one
/// flat `labels` list; every axis is a prefix within it, so there is exactly one place a
/// capability can be recorded and exactly one that reads it.
pub const SKILL_PREFIX: &str = "skill:";
/// See [`SKILL_PREFIX`]. Harness is the one axis matched as an OR set — see [`harness_ok`].
pub const HARNESS_PREFIX: &str = "harness:";
/// See [`SKILL_PREFIX`].
pub const MCP_PREFIX: &str = "mcp:";

/// Every **AND**-required label a [`Requirements`] implies: one `skill:<name>` per required skill,
/// one `mcp:<name>` per required MCP, plus the raw capability labels — sorted and deduped for a
/// stable, diffable order.
///
/// Harness is deliberately excluded: it is an OR set ("any of these will do"), which an AND matcher
/// cannot express. [`harness_ok`] handles it separately.
pub fn required_labels(req: &Requirements) -> Vec<String> {
    let mut out: Vec<String> = req
        .skills
        .iter()
        .map(|s| format!("{SKILL_PREFIX}{s}"))
        .chain(req.mcps.iter().map(|m| format!("{MCP_PREFIX}{m}")))
        .chain(req.capabilities.iter().cloned())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The labels that disqualify a node — the raw [`Requirements::excludes`] list, unchanged.
pub fn excluded_labels(req: &Requirements) -> Vec<String> {
    req.excludes.clone()
}

/// Whether an advertised label set offers at least one acceptable harness.
///
/// An empty [`Requirements::harness`] accepts any node (fail-open: the executor then picks its
/// configured default), which is what keeps every pre-queue entry placeable.
pub fn harness_ok(req: &Requirements, advertised: &[String]) -> bool {
    req.harness.is_empty() || pick_harness(req, advertised).is_some()
}

/// The harness to run an entry under on a node advertising `advertised`: the first entry of
/// [`Requirements::harness`] that node offers, else `None` (meaning "executor default").
///
/// Deterministic — the requirement's declared order *is* the preference order, so a producer that
/// writes `["claude", "copilot"]` gets claude wherever claude exists.
pub fn pick_harness(req: &Requirements, advertised: &[String]) -> Option<String> {
    req.harness
        .iter()
        .find(|want| {
            advertised.iter().any(|label| {
                label
                    .strip_prefix(HARNESS_PREFIX)
                    .is_some_and(|have| have.eq_ignore_ascii_case(want))
            })
        })
        .cloned()
}

/// The whole match: the AND/NOT label filter plus the harness OR.
pub fn node_serves(advertised: &[String], req: &Requirements) -> bool {
    let has = |cap: &str| advertised.iter().any(|l| l.eq_ignore_ascii_case(cap));
    required_labels(req).iter().all(|cap| has(cap))
        && excluded_labels(req).iter().all(|cap| !has(cap))
        && harness_ok(req, advertised)
}

/// **Which** requirement disqualified a node — the operator-facing reason behind a deferred entry
/// and the `no-capable-node` bind.
///
/// Names the most specific unmet thing, preferring a missing skill over a missing raw capability,
/// because "no node has skill:auto-review-pr" tells you what to install and "no capable node" does
/// not. Returns `None` when the node does serve the requirement.
pub fn unmet(advertised: &[String], req: &Requirements) -> Option<String> {
    let has = |cap: &str| advertised.iter().any(|l| l.eq_ignore_ascii_case(cap));
    if let Some(skill) = req
        .skills
        .iter()
        .find(|s| !has(&format!("{SKILL_PREFIX}{s}")))
    {
        return Some(format!("missing skill: {skill}"));
    }
    if !harness_ok(req, advertised) {
        return Some(format!(
            "no acceptable harness: {}",
            req.harness.join(" | ")
        ));
    }
    if let Some(mcp) = req.mcps.iter().find(|m| !has(&format!("{MCP_PREFIX}{m}"))) {
        return Some(format!("missing mcp: {mcp}"));
    }
    if let Some(cap) = req.capabilities.iter().find(|c| !has(c)) {
        return Some(format!("missing capability: {cap}"));
    }
    if let Some(cap) = req.excludes.iter().find(|c| has(c)) {
        return Some(format!("has excluded capability: {cap}"));
    }
    None
}

/// Whether `node` advertises every required capability and none of the excluded capabilities
/// (#728).
///
/// A [`crate::backlog::ExecutionTarget::GitHubRunner`] fails any non-empty requirement outright —
/// the cloud runner carries no capability tags, so it can never satisfy one, and claiming
/// otherwise would place work on a node guaranteed to fail.
pub fn node_satisfies(node: &NodeRecord, required: &[String], excluded: &[String]) -> bool {
    if (!required.is_empty() || !excluded.is_empty())
        && node.target == crate::backlog::ExecutionTarget::GitHubRunner
    {
        return false;
    }
    required.iter().all(|cap| advertises(node, cap))
        && excluded.iter().all(|cap| !advertises(node, cap))
}

/// Whether `node`'s advertised label set contains `cap`.
fn advertises(node: &NodeRecord, cap: &str) -> bool {
    node.labels
        .iter()
        .any(|label| label.eq_ignore_ascii_case(cap))
}

/// The required capabilities `node` does **not** advertise — the operator-facing "why not this
/// box" list. Returned rather than a bool so the reason can name the specific missing label.
pub fn missing_required(node: &NodeRecord, required: &[String]) -> Vec<String> {
    required
        .iter()
        .filter(|cap| !advertises(node, cap))
        .cloned()
        .collect()
}

/// The excluded capabilities `node` advertises anyway — the disqualifying labels, named.
pub fn matching_excluded(node: &NodeRecord, excluded: &[String]) -> Vec<String> {
    excluded
        .iter()
        .filter(|cap| advertises(node, cap))
        .cloned()
        .collect()
}

/// The operator-facing reason no node in the registry could take a constrained item. Names the
/// unmet requirement rather than saying "no capable node", because the former is actionable.
pub fn no_capable_node_reason(required: &[String], excluded: &[String]) -> String {
    match (required.is_empty(), excluded.is_empty()) {
        (false, true) => format!(
            "no READY node satisfies required capability: {}",
            required.join(", ")
        ),
        (true, false) => format!(
            "no READY node avoids excluded capability: {}",
            excluded.join(", ")
        ),
        (false, false) => format!(
            "no READY node satisfies required capability: {} while avoiding: {}",
            required.join(", "),
            excluded.join(", ")
        ),
        (true, true) => "no READY node available".to_string(),
    }
}
