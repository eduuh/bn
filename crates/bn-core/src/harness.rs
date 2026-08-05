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

/// Resolve the harness a `bn dispatch`/worker spawn should actually launch. Unlike
/// [`resolve_relaunch`] (which rejects an unusable harness), a dispatch must always launch
/// *something*, so an unusable or unknown request transparently falls back to the guaranteed
/// `copilot` — carrying the reason so the caller can surface the downgrade. This is the spawn-side
/// capability gate: a machine where Claude is org-disabled (`[harness].disabled = ["claude"]` /
/// `$BN_DISABLED_HARNESSES`) dispatches copilot instead of launching a claude that dies at boot.
/// Pure.
pub fn resolve_dispatch_harness(cfg: &Config, requested: Option<&str>) -> DispatchHarness {
    let Some(raw) = requested.map(str::trim).filter(|s| !s.is_empty()) else {
        // No `--agent`: dispatch's own default is copilot; nothing was downgraded.
        return DispatchHarness {
            harness: "copilot",
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn cfg_with_disabled(disabled: &[&str]) -> Config {
        let mut cfg = crate::test_support::config(Path::new("/tmp/bn-harness-test"));
        cfg.disabled_harnesses = disabled.iter().map(|s| s.to_string()).collect();
        cfg
    }

    #[test]
    fn normalize_maps_aliases_and_rejects_unknown() {
        for v in ["copilot", "gh", "GitHub", " copilot "] {
            assert_eq!(normalize(v), Some("copilot"), "{v}");
        }
        for v in ["claude", "claudecode", "claude-code", "CC"] {
            assert_eq!(normalize(v), Some("claude"), "{v}");
        }
        assert_eq!(normalize("gpt4"), None);
        assert_eq!(normalize("bash;rm -rf"), None);
        assert_eq!(normalize(""), None);
    }

    #[test]
    fn availability_defaults_all_usable() {
        let cfg = crate::test_support::config(Path::new("/tmp/bn-harness-test"));
        let av = availability(&cfg);
        assert_eq!(av.len(), 2);
        // Copilot leads and is always usable, no reason.
        assert_eq!(av[0].name, "copilot");
        assert!(av[0].available && av[0].reason.is_none());
        // Claude usable by default (not org-disabled).
        assert_eq!(av[1].name, "claude");
        assert!(av[1].available && av[1].reason.is_none());
    }

    #[test]
    fn org_disabled_claude_is_unavailable_with_reason() {
        let cfg = cfg_with_disabled(&["claude"]);
        let av = availability(&cfg);
        let claude = av.iter().find(|o| o.name == "claude").unwrap();
        assert!(!claude.available);
        assert_eq!(claude.reason.as_deref(), Some(CLAUDE_DISABLED_REASON));
        // Copilot stays usable — the guaranteed fallback.
        assert!(av.iter().find(|o| o.name == "copilot").unwrap().available);
    }

    #[test]
    fn disabled_accepts_aliases() {
        // A `claudecode` entry disables claude just as `claude` would.
        let cfg = cfg_with_disabled(&["claudecode"]);
        assert!(!is_available(&cfg, "claude"));
        assert!(!is_available(&cfg, "cc"));
    }

    #[test]
    fn copilot_cannot_be_disabled() {
        // Even if someone lists copilot, it's never rendered unavailable — it's the fallback.
        let cfg = cfg_with_disabled(&["copilot"]);
        assert!(is_available(&cfg, "copilot"));
        assert!(
            availability(&cfg)
                .iter()
                .find(|o| o.name == "copilot")
                .unwrap()
                .available
        );
    }

    #[test]
    fn resolve_relaunch_ok_for_available_canonicalizes() {
        let cfg = crate::test_support::config(Path::new("/tmp/bn-harness-test"));
        assert_eq!(resolve_relaunch(&cfg, "copilot"), Ok("copilot"));
        assert_eq!(resolve_relaunch(&cfg, "claudecode"), Ok("claude"));
    }

    #[test]
    fn resolve_relaunch_rejects_unknown_harness() {
        let cfg = crate::test_support::config(Path::new("/tmp/bn-harness-test"));
        let err = resolve_relaunch(&cfg, "gpt4").unwrap_err();
        assert!(err.contains("unknown harness 'gpt4'"));
        assert!(err.contains("copilot or claude"));
    }

    #[test]
    fn resolve_relaunch_rejects_disabled_harness_with_reason() {
        let cfg = cfg_with_disabled(&["claude"]);
        assert_eq!(
            resolve_relaunch(&cfg, "claude"),
            Err(CLAUDE_DISABLED_REASON.to_string())
        );
        // The still-usable harness resolves fine on the same machine.
        assert_eq!(resolve_relaunch(&cfg, "copilot"), Ok("copilot"));
    }

    #[test]
    fn dispatch_harness_launches_requested_when_available() {
        let cfg = crate::test_support::config(Path::new("/tmp/bn-harness-test"));
        // An available claude launches as claude, no downgrade.
        let r = resolve_dispatch_harness(&cfg, Some("claude-code"));
        assert_eq!(r.harness, "claude");
        assert_eq!(r.fallback_from, None);
        // Copilot as requested.
        let r = resolve_dispatch_harness(&cfg, Some("copilot"));
        assert_eq!(r.harness, "copilot");
        assert_eq!(r.fallback_from, None);
    }

    #[test]
    fn dispatch_harness_falls_back_to_copilot_when_claude_org_disabled() {
        let cfg = cfg_with_disabled(&["claude"]);
        let r = resolve_dispatch_harness(&cfg, Some("cc"));
        assert_eq!(r.harness, "copilot", "org-disabled claude must not launch");
        assert_eq!(
            r.fallback_from,
            Some(("claude", CLAUDE_DISABLED_REASON.to_string())),
            "the downgrade carries the requested harness + reason for the caller to surface"
        );
    }

    #[test]
    fn dispatch_harness_defaults_to_copilot_without_request() {
        let cfg = crate::test_support::config(Path::new("/tmp/bn-harness-test"));
        for req in [None, Some(""), Some("   ")] {
            let r = resolve_dispatch_harness(&cfg, req);
            assert_eq!(r.harness, "copilot");
            assert_eq!(r.fallback_from, None, "no request is not a downgrade");
        }
    }

    #[test]
    fn dispatch_harness_unknown_request_falls_back_to_copilot() {
        let cfg = crate::test_support::config(Path::new("/tmp/bn-harness-test"));
        let r = resolve_dispatch_harness(&cfg, Some("gpt4"));
        assert_eq!(r.harness, "copilot");
        let (_, reason) = r.fallback_from.expect("unknown harness is a downgrade");
        assert!(reason.contains("unknown harness 'gpt4'"));
    }
}
