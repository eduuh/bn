//! Harness availability + relaunch policy (#501).
//!
//! One pure place answering "which agent harnesses can this machine launch, and may the dashboard
//! relaunch a running agent under harness X?". Two surfaces share it: the web read
//! ([`availability`], folded into `/api/fleet` + `/api/agents/:id` so the switcher renders each
//! option's usable/disabled state) and the web write ([`resolve_relaunch`], which gates
//! `POST /api/agents/:target/relaunch {harness}` before it swaps the agent's harness in place).
//!
//! The policy is deliberately small: **copilot is always available** — it's the guaranteed
//! fallback — and **claude is available unless the account is org-disabled** ("organization has
//! disabled Claude subscription access"), configured per machine via `[harness].disabled` /
//! `$BN_DISABLED_HARNESSES` ([`Config::disabled_harnesses`]). No I/O, so it's unit-tested straight
//! against a `crate::test_support::config`.

use crate::resolver::Config;
use serde::Serialize;

/// The harnesses bn can launch, in the order the dashboard renders its segmented switcher. Copilot
/// leads — it's the always-available default.
pub const KNOWN_HARNESSES: [&str; 2] = ["copilot", "claude"];

/// The reason a disabled claude carries — the account-level message the harness itself prints, so
/// the switcher's disabled-option tooltip matches what an operator would otherwise see in a pane.
pub const CLAUDE_DISABLED_REASON: &str = "organization has disabled Claude subscription access";

/// One harness option for the dashboard switcher: its canonical `name`, whether it's `available`
/// to launch on this machine, and a human `reason` when it isn't — rendered as a disabled-option
/// tooltip, never a silent drop.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HarnessOption {
    pub name: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Canonicalize a user/UI harness value to `copilot` | `claude`, accepting the same aliases as
/// `bn dispatch --agent`. Trimmed + case-insensitive. `None` for an unknown value.
pub fn normalize(value: &str) -> Option<&'static str> {
    match value.trim().to_lowercase().as_str() {
        "copilot" | "gh" | "github" => Some("copilot"),
        "claude" | "claudecode" | "claude-code" | "cc" => Some("claude"),
        _ => None,
    }
}

/// The reason a canonical `harness` is unavailable on this machine, or `None` when it's usable.
/// Copilot is never disable-able (the guaranteed fallback); every other known harness is disabled
/// when it appears (by any alias) in [`Config::disabled_harnesses`]. Pure.
fn unavailable_reason(cfg: &Config, harness: &str) -> Option<String> {
    if harness == "copilot" {
        return None;
    }
    let disabled = cfg
        .disabled_harnesses
        .iter()
        .any(|h| normalize(h) == Some(harness) || h.eq_ignore_ascii_case(harness));
    if !disabled {
        return None;
    }
    Some(match harness {
        "claude" => CLAUDE_DISABLED_REASON.to_string(),
        other => format!("{other} is disabled on this machine"),
    })
}

/// The availability of every [`KNOWN_HARNESSES`] entry for the dashboard switcher: copilot always
/// usable, claude usable unless org-disabled. Stable order so the segmented control never
/// reshuffles between ticks. Pure.
pub fn availability(cfg: &Config) -> Vec<HarnessOption> {
    KNOWN_HARNESSES
        .iter()
        .map(|&name| {
            let reason = unavailable_reason(cfg, name);
            HarnessOption {
                name: name.to_string(),
                available: reason.is_none(),
                reason,
            }
        })
        .collect()
}

/// Whether `harness` (any accepted spelling) is a known, currently-usable harness on this machine.
pub fn is_available(cfg: &Config, harness: &str) -> bool {
    normalize(harness)
        .map(|h| unavailable_reason(cfg, h).is_none())
        .unwrap_or(false)
}

/// Validate the harness the dashboard asked to relaunch a running agent under: canonicalize it,
/// then confirm it's usable on this machine. `Ok(canonical)` to proceed with the in-place swap,
/// `Err(reason)` to reject — an unknown harness, or a disabled one — so the relaunch never fails
/// silently. Pure: the caller (bn-web) turns `Err` into a `400` and `Ok` into the `bn switch` argv.
pub fn resolve_relaunch(cfg: &Config, requested: &str) -> Result<&'static str, String> {
    let Some(canonical) = normalize(requested) else {
        return Err(format!(
            "unknown harness '{}' — use {}",
            requested.trim(),
            KNOWN_HARNESSES.join(" or ")
        ));
    };
    match unavailable_reason(cfg, canonical) {
        None => Ok(canonical),
        Some(reason) => Err(reason),
    }
}

/// The harness a dispatch will actually launch, after applying the machine's availability gate.
#[derive(Debug, Clone, PartialEq)]
pub struct DispatchHarness {
    /// The canonical harness to launch — always one bn can start on this machine.
    pub harness: &'static str,
    /// Set when the requested harness was downgraded to the fallback: `(requested, reason)`. The
    /// caller logs/records this so the swap is visible, never silent.
    pub fallback_from: Option<(&'static str, String)>,
}

/// The harness a launcher uses when nothing is explicitly requested: the configured default
/// ([`Config::default_harness`] — `[harness].default` / `$BN_HARNESS`) when it's known and usable on
/// this machine, else the guaranteed `copilot` fallback. A configured-but-unavailable default (e.g.
/// `claude` on an org-disabled box) also yields `copilot`. This is the single source of truth every
/// launcher consults instead of hardcoding `"copilot"`. Pure.
pub fn preferred_harness(cfg: &Config) -> &'static str {
    cfg.default_harness
        .as_deref()
        .and_then(normalize)
        .filter(|h| unavailable_reason(cfg, h).is_none())
        .unwrap_or("copilot")
}

/// Resolve the harness a `bn dispatch`/worker spawn should actually launch. Unlike
/// [`resolve_relaunch`] (which rejects an unusable harness), a dispatch must always launch
/// *something*, so an unusable or unknown request transparently falls back to the guaranteed
/// `copilot` — carrying the reason so the caller can surface the downgrade. This is the spawn-side
/// capability gate: a machine where Claude is org-disabled (`[harness].disabled = ["claude"]` /
/// `$BN_DISABLED_HARNESSES`) dispatches copilot instead of launching a claude that dies at boot.
/// With no request it uses the configured [`preferred_harness`] (default `copilot`). Pure.
pub fn resolve_dispatch_harness(cfg: &Config, requested: Option<&str>) -> DispatchHarness {
    let Some(raw) = requested.map(str::trim).filter(|s| !s.is_empty()) else {
        // No `--agent`: use the machine's configured default harness (copilot when unset). The
        // preferred harness is already availability-gated, so this never launches a dead harness.
        return DispatchHarness {
            harness: preferred_harness(cfg),
            fallback_from: None,
        };
    };
    match normalize(raw) {
        Some(canonical) => match unavailable_reason(cfg, canonical) {
            None => DispatchHarness {
                harness: canonical,
                fallback_from: None,
            },
            Some(reason) => DispatchHarness {
                harness: "copilot",
                fallback_from: Some((canonical, reason)),
            },
        },
        None => DispatchHarness {
            harness: "copilot",
            fallback_from: Some((
                "copilot",
                format!("unknown harness '{raw}' — falling back to copilot"),
            )),
        },
    }
}
