//! What this machine advertises it can do.
//!
//! The single source of truth for a node's capabilities: what a remote node reports through
//! `bn worker status --json`, **and** what the local box's synthesized registry record carries. One
//! function for both sides, so the two can never disagree about the same machine — the failure mode
//! where a box is passed over for work it could have run, and nothing says why.
//!
//! Everything is flattened into the prefixed label namespace [`crate::capability`] matches on, so
//! placement keeps using the one predicate it already has instead of growing a special case per
//! axis.
use crate::capability::{HARNESS_PREFIX, MCP_PREFIX, SKILL_PREFIX};
use crate::resolver::Config;

/// Every capability this box advertises:
///
///   * the intrinsic `os:` / `arch:` pair and any `$BN_WORKER_LABELS` extras — i.e. the existing
///     [`crate::worker::node_labels`], unchanged;
///   * one `skill:<name>` per **effective** bn skill (the highest-precedence definition per name,
///     so a repo-local override does not double-count);
///   * one `harness:<name>` per harness actually usable here — an org-disabled harness is not
///     advertised, because advertising it would attract work the box then cannot run;
///   * one `mcp:<name>` per configured MCP server;
///   * `home:<machine>`, so an agent pinned to a specific box by `[staff].home` can express that
///     pin as an ordinary requirement rather than a bespoke check.
///
/// Sorted and deduped, so a status diff shows real change rather than reordering.
pub fn advertised_labels(cfg: &Config, base_dir: &std::path::Path) -> Vec<String> {
    let mut labels = crate::worker::node_labels();
    labels.push(format!("home:{}", cfg.machine()));
    if let Ok(skills) = crate::skills::discover(cfg, base_dir) {
        labels.extend(
            skills
                .into_iter()
                .filter(|s| s.effective)
                .map(|s| format!("{SKILL_PREFIX}{}", s.name)),
        );
    }
    labels.extend(
        crate::harness::availability(cfg)
            .into_iter()
            .filter(|h| h.available)
            .map(|h| format!("{HARNESS_PREFIX}{}", h.name)),
    );
    labels.extend(
        mcp_names(cfg)
            .into_iter()
            .map(|m| format!("{MCP_PREFIX}{m}")),
    );
    labels.sort();
    labels.dedup();
    labels
}

/// The MCP servers that are a property of **this node**, by name.
///
/// Only bn's own server qualifies today. Project-scoped servers (`.vscode/mcp.json`,
/// `.github/mcp.json`) are deliberately excluded: they belong to a worktree, not a machine, so
/// advertising them here would claim a capability that varies per checkout — and placement would
/// then send work to a box where the requirement happens not to hold.
fn mcp_names(_cfg: &Config) -> Vec<String> {
    vec!["bn".to_string()]
}

/// Split an advertised label set back out by axis for display: `(skills, harnesses, mcps, other)`.
///
/// The inverse of the flattening above. Status surfaces want the axes named; the matcher wants one
/// flat list. Keeping the projection in one place means neither has to know the other's shape.
pub fn split_labels(labels: &[String]) -> AdvertisedAxes {
    let mut axes = AdvertisedAxes::default();
    for label in labels {
        if let Some(skill) = label.strip_prefix(SKILL_PREFIX) {
            axes.skills.push(skill.to_string());
        } else if let Some(harness) = label.strip_prefix(HARNESS_PREFIX) {
            axes.harnesses.push(harness.to_string());
        } else if let Some(mcp) = label.strip_prefix(MCP_PREFIX) {
            axes.mcps.push(mcp.to_string());
        } else {
            axes.other.push(label.clone());
        }
    }
    axes
}

/// An advertised label set, split by axis for display.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct AdvertisedAxes {
    pub skills: Vec<String>,
    pub harnesses: Vec<String>,
    pub mcps: Vec<String>,
    /// Everything without a reserved prefix — `os:`, `arch:`, `home:`, and operator-defined tags.
    pub other: Vec<String>,
}
