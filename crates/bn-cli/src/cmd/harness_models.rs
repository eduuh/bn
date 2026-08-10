//! Asking a harness what it serves — the subprocess half of [`bn_core::harness_models`].
//!
//! `bn harness models` is the one source both the CLI and the Planning UI read, so a stage can never
//! be configured with a model the harness would refuse. The parsing is pure and lives in bn-core;
//! this runs the command and caches the answer.
//!
//! # Cached, because it is a subprocess on a read path
//!
//! The Planning page reads this on every render and the model roster changes maybe monthly, so the
//! answer is cached under the state dir for [`CACHE_TTL_SECS`]. A stale-but-present cache is served
//! rather than re-probed on failure: a harness that has since been uninstalled should not empty a
//! picker that was working a minute ago.
use crate::*;
use serde::{Deserialize, Serialize};

/// How long a discovered roster is trusted. A day: long enough that the page never waits on a
/// subprocess twice, short enough that a new model shows up without anyone clearing a cache.
pub(crate) const CACHE_TTL_SECS: i64 = 86_400;

/// What one harness serves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct HarnessRoster {
    pub(crate) harness: String,
    /// Model ids, in the harness's own preference order (newest first).
    pub(crate) models: Vec<String>,
    /// Reasoning-effort levels this harness accepts.
    pub(crate) efforts: Vec<String>,
    /// True when the list came from the harness itself rather than bn's built-in fallback. Surfaced
    /// so a picker can say "these are the ids Copilot reported" versus "these are the aliases bn
    /// knows" — the difference between an exhaustive list and a safe one.
    pub(crate) discovered: bool,
    pub(crate) fetched_at: i64,
}

fn cache_path() -> std::path::PathBuf {
    bn_core::resolver::state_dir().join("harness-models.json")
}

fn read_cache() -> Vec<HarnessRoster> {
    std::fs::read_to_string(cache_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_cache(rosters: &[HarnessRoster]) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(rosters) {
        let _ = std::fs::write(&path, text);
    }
}

/// Ask Copilot's shell completion what it offers. `None` when the CLI is missing or said nothing
/// useful — which is not an error, just a reason to fall back.
fn probe_copilot() -> Option<(Vec<String>, Vec<String>)> {
    let out = std::process::Command::new("copilot")
        .args(["completion", "zsh"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let script = String::from_utf8_lossy(&out.stdout);
    let models = bn_core::harness_models::parse_completion_values(&script, "--model");
    let efforts = bn_core::harness_models::parse_completion_values(&script, "--effort");
    // Models are the point; without them the probe has told us nothing worth caching.
    (!models.is_empty()).then_some((models, efforts))
}

/// The roster for `harness`, from cache when fresh, else probed, else bn's built-in fallback.
///
/// Never fails and never returns an empty model list: an empty picker is indistinguishable from a
/// broken page, so the floor is always the set [`bn_core::mcp::Harness`] is willing to validate.
pub(crate) fn roster(harness: &str, now: i64) -> HarnessRoster {
    let Some(canonical) = bn_core::harness::normalize(harness) else {
        return fallback(harness, now);
    };
    let cached = read_cache();
    if let Some(hit) = cached
        .iter()
        .find(|r| r.harness == canonical && now.saturating_sub(r.fetched_at) <= CACHE_TTL_SECS)
    {
        return hit.clone();
    }

    let probed = match canonical {
        "copilot" => probe_copilot(),
        // Claude Code ships no enumeration, so there is nothing to probe — the aliases *are* the
        // answer, and they are the same set the launch guard accepts.
        _ => None,
    };
    let fresh = match probed {
        Some((models, efforts)) => HarnessRoster {
            harness: canonical.to_string(),
            models,
            efforts: if efforts.is_empty() {
                default_efforts(canonical)
            } else {
                efforts
            },
            discovered: true,
            fetched_at: now,
        },
        // A probe that failed while a stale entry exists keeps the stale one: yesterday's real list
        // beats today's fallback.
        None => match cached.iter().find(|r| r.harness == canonical && r.discovered) {
            Some(stale) => stale.clone(),
            None => fallback(canonical, now),
        },
    };
    let mut merged: Vec<HarnessRoster> = cached
        .into_iter()
        .filter(|r| r.harness != fresh.harness)
        .collect();
    merged.push(fresh.clone());
    merged.sort_by(|a, b| a.harness.cmp(&b.harness));
    write_cache(&merged);
    fresh
}

fn default_efforts(harness: &str) -> Vec<String> {
    bn_core::mcp::Harness::parse(harness)
        .map(|h| h.efforts().iter().map(|s| s.to_string()).collect())
        .unwrap_or_default()
}

/// The built-in floor: what bn knows without asking anyone.
fn fallback(harness: &str, now: i64) -> HarnessRoster {
    HarnessRoster {
        harness: harness.to_string(),
        models: bn_core::harness_models::CLAUDE_ALIASES
            .iter()
            .map(|s| s.to_string())
            .collect(),
        efforts: default_efforts(harness),
        discovered: false,
        fetched_at: now,
    }
}

/// Every known harness's roster.
pub(crate) fn all_rosters(now: i64) -> Vec<HarnessRoster> {
    bn_core::harness::KNOWN_HARNESSES
        .iter()
        .map(|h| roster(h, now))
        .collect()
}

/// `bn harness models [--harness X]` — what each harness serves, for a picker or a sanity check.
pub(crate) fn emit_harness_models(harness: Option<&str>, as_json: bool) {
    let now = bn_core::util::now_epoch_secs();
    let rosters = match harness {
        Some(h) => match bn_core::harness::normalize(h) {
            Some(canonical) => vec![roster(canonical, now)],
            None => fail(&format!(
                "bn harness models: unknown harness '{h}' (use copilot | claude)"
            )),
        },
        None => all_rosters(now),
    };
    if as_json {
        println!(
            "{}",
            Envelope::success("harness-models", serde_json::json!({ "harnesses": rosters }))
                .to_json()
        );
        return;
    }
    for r in &rosters {
        let source = if r.discovered {
            "reported by the harness"
        } else {
            "bn's built-in aliases (the harness offers no list)"
        };
        println!("{} — {} model(s), {source}", r.harness, r.models.len());
        for m in &r.models {
            println!("  {m}");
        }
        println!("  effort: {}", r.efforts.join(" | "));
    }
}
