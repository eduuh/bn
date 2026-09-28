//! How full a live Claude session's context window is, read off its transcript.
//!
//! The figure is the last API response's prompt size (`input + cache_read + cache_creation`) plus
//! what it generated (`output`), which is what the next turn starts from. It is an estimate — the
//! harness's own accounting adds system prompt and tool definitions it doesn't log — so it reads a
//! little low, and the bands start early to allow for that.
//!
//! Unknown is a state, not zero. A session with no usage yet, a Copilot session (which logs no
//! prompt sizes live), or one that just compacted reports [`UsageState::Unknown`] /
//! [`UsageState::JustCompacted`] with no percentage, and every consumer must treat that as "can't
//! tell", never as "empty".

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The window assumed for a model the table doesn't know, unless usage has already passed it.
pub const DEFAULT_WINDOW: i64 = 200_000;
/// The long-context window.
pub const LONG_WINDOW: i64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageState {
    Measured,
    /// A compaction happened after the last measured response; the next response will re-measure.
    JustCompacted,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ContextUsage {
    pub used_tokens: i64,
    pub window_tokens: i64,
    /// `used / window`, 0–100. `None` unless [`UsageState::Measured`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pct: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub state: UsageState,
    /// Compactions seen in this transcript.
    #[serde(default)]
    pub compactions: u32,
}

/// Where a session's usage sits against the thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Band {
    Ok,
    /// Time to write a recap and plan a compaction.
    Warn,
    /// Compact soon, deliberately — before the harness does it on its own.
    Compact,
    Unknown,
}

/// Percent thresholds for [`Band::Warn`] and [`Band::Compact`].
pub const WARN_PCT: u8 = 70;
pub const COMPACT_PCT: u8 = 85;

pub fn band(u: &ContextUsage) -> Band {
    match u.pct {
        None => Band::Unknown,
        Some(p) if p >= COMPACT_PCT => Band::Compact,
        Some(p) if p >= WARN_PCT => Band::Warn,
        Some(_) => Band::Ok,
    }
}

/// The context window for `model`. `$BN_CONTEXT_WINDOW` overrides; an explicit `[1m]` suffix and
/// the long-context families get [`LONG_WINDOW`]; anything else [`DEFAULT_WINDOW`].
pub fn window_for(model: Option<&str>) -> i64 {
    if let Some(n) = std::env::var("BN_CONTEXT_WINDOW")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
    {
        return n;
    }
    let m = model.unwrap_or("").to_ascii_lowercase();
    let long = m.contains("[1m]")
        || ["claude-opus-5", "claude-sonnet-5", "claude-fable-5"]
            .iter()
            .any(|f| m.starts_with(f));
    if long {
        LONG_WINDOW
    } else {
        DEFAULT_WINDOW
    }
}

fn int(u: &Value, key: &str) -> i64 {
    u.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// One transcript line, reduced to what the usage read needs.
enum Event<'a> {
    /// A `compact_boundary` system line, with its `compactMetadata.preTokens`.
    Boundary(Option<i64>),
    /// A main-thread assistant response: its token total and model.
    Response(i64, Option<&'a str>),
    Other,
}

/// Context usage from a Claude transcript's parsed lines, oldest first.
pub fn from_claude_lines(lines: &[Value]) -> ContextUsage {
    from_events(lines.iter().map(|v| {
        match v.get("type").and_then(Value::as_str) {
            Some("system")
                if v.get("subtype").and_then(Value::as_str) == Some("compact_boundary") =>
            {
                Event::Boundary(
                    v.pointer("/compactMetadata/preTokens")
                        .and_then(Value::as_i64),
                )
            }
            // Sidechain (subagent) turns have their own context; they don't fill this one.
            Some("assistant") if v.get("isSidechain").and_then(Value::as_bool) != Some(true) => {
                let msg = v.get("message");
                match msg.and_then(|m| m.get("usage")) {
                    Some(u) => Event::Response(
                        int(u, "input_tokens")
                            + int(u, "cache_read_input_tokens")
                            + int(u, "cache_creation_input_tokens")
                            + int(u, "output_tokens"),
                        msg.and_then(|m| m.get("model")).and_then(Value::as_str),
                    ),
                    None => Event::Other,
                }
            }
            _ => Event::Other,
        }
    }))
}

/// [`from_claude_lines`] over the live view's lean typed lines, which read the same keys.
pub(crate) fn from_live_lines(lines: &[super::live_line::LiveLine]) -> ContextUsage {
    from_events(lines.iter().map(|l| match l.typ.0.as_deref() {
        Some("system") if l.subtype.0.as_deref() == Some("compact_boundary") => {
            Event::Boundary(l.compact_pre_tokens)
        }
        Some("assistant") if l.is_sidechain.0 != Some(true) => match l.message.usage {
            Some(u) => Event::Response(
                u.input + u.cache_read + u.cache_write + u.output,
                l.message.model.as_deref(),
            ),
            None => Event::Other,
        },
        _ => Event::Other,
    }))
}

fn from_events<'a>(events: impl Iterator<Item = Event<'a>>) -> ContextUsage {
    let mut last: Option<(usize, i64, Option<String>)> = None;
    let mut boundary: Option<usize> = None;
    let mut compactions = 0u32;
    let mut peak = 0i64;
    for (i, e) in events.enumerate() {
        match e {
            Event::Boundary(pre) => {
                boundary = Some(i);
                compactions += 1;
                if let Some(pre) = pre {
                    peak = peak.max(pre);
                }
            }
            Event::Response(used, model) if used > 0 => {
                peak = peak.max(used);
                last = Some((i, used, model.map(str::to_string)));
            }
            _ => {}
        }
    }
    let Some((at, used, model)) = last else {
        return ContextUsage {
            window_tokens: window_for(None),
            compactions,
            state: if boundary.is_some() {
                UsageState::JustCompacted
            } else {
                UsageState::Unknown
            },
            ..Default::default()
        };
    };
    let mut window = window_for(model.as_deref());
    // A session already past the assumed window must have a bigger one.
    if peak > window {
        window = LONG_WINDOW.max(peak);
    }
    let just_compacted = boundary.is_some_and(|b| b > at);
    ContextUsage {
        used_tokens: used,
        window_tokens: window,
        pct: (!just_compacted).then(|| ((used * 100) / window).clamp(0, 100) as u8),
        model,
        state: if just_compacted {
            UsageState::JustCompacted
        } else {
            UsageState::Measured
        },
        compactions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant(model: &str, cache_read: i64, output: i64) -> Value {
        json!({"type": "assistant", "message": {"model": model, "usage": {
            "input_tokens": 2, "cache_read_input_tokens": cache_read,
            "cache_creation_input_tokens": 10, "output_tokens": output}}})
    }

    fn boundary(pre: i64) -> Value {
        json!({"type": "system", "subtype": "compact_boundary",
               "compactMetadata": {"trigger": "auto", "preTokens": pre}})
    }

    #[test]
    fn measures_the_last_response_against_the_models_window() {
        let lines = [
            assistant("claude-opus-5-5", 100_000, 500),
            assistant("claude-opus-5-5", 514_756, 1_062),
        ];
        let u = from_claude_lines(&lines);
        assert_eq!(u.state, UsageState::Measured);
        assert_eq!(u.used_tokens, 2 + 514_756 + 10 + 1_062);
        assert_eq!(u.window_tokens, LONG_WINDOW);
        assert_eq!(u.pct, Some(51));
        assert_eq!(band(&u), Band::Ok);
    }

    #[test]
    fn an_unknown_model_past_200k_must_have_the_long_window() {
        let u = from_claude_lines(&[assistant("claude-x", 180_000, 0)]);
        assert_eq!((u.window_tokens, u.pct), (DEFAULT_WINDOW, Some(90)));
        assert_eq!(band(&u), Band::Compact);
        let u = from_claude_lines(&[
            assistant("claude-x", 400_000, 0),
            assistant("claude-x", 150_000, 0),
        ]);
        assert_eq!(u.window_tokens, LONG_WINDOW);
        assert_eq!(u.pct, Some(15));
    }

    #[test]
    fn a_compaction_after_the_last_response_is_not_a_measurement() {
        let u = from_claude_lines(&[assistant("claude-opus-5-5", 900_000, 0), boundary(969_770)]);
        assert_eq!(u.state, UsageState::JustCompacted);
        assert_eq!((u.pct, u.compactions), (None, 1));
        assert_eq!(band(&u), Band::Unknown);
        // The next response measures again.
        let u = from_claude_lines(&[
            assistant("claude-opus-5-5", 900_000, 0),
            boundary(969_770),
            assistant("claude-opus-5-5", 18_000, 0),
        ]);
        assert_eq!((u.state, u.pct), (UsageState::Measured, Some(1)));
    }

    #[test]
    fn no_usage_is_unknown_not_zero() {
        let u = from_claude_lines(&[json!({"type": "user"})]);
        assert_eq!((u.state, u.pct), (UsageState::Unknown, None));
        assert_eq!(band(&u), Band::Unknown);
    }

    #[test]
    fn subagent_turns_do_not_count() {
        let mut side = assistant("claude-opus-5-5", 800_000, 0);
        side["isSidechain"] = json!(true);
        let u = from_claude_lines(&[assistant("claude-opus-5-5", 100_000, 0), side]);
        assert_eq!(u.pct, Some(10));
    }

    #[test]
    fn warn_and_compact_bands() {
        let at = |pct| ContextUsage {
            pct: Some(pct),
            state: UsageState::Measured,
            ..Default::default()
        };
        assert_eq!(band(&at(69)), Band::Ok);
        assert_eq!(band(&at(70)), Band::Warn);
        assert_eq!(band(&at(85)), Band::Compact);
    }

    /// The live view reads lean typed lines; it must reach the same figure as the `Value` read.
    #[test]
    fn the_typed_live_read_matches_the_value_read() {
        let mut side = assistant("claude-opus-5-5", 800_000, 0);
        side["isSidechain"] = json!(true);
        let cases: Vec<Vec<Value>> = vec![
            vec![assistant("claude-opus-5-5", 100_000, 500), side],
            vec![assistant("claude-x", 180_000, 0)],
            vec![assistant("claude-opus-5-5", 900_000, 0), boundary(969_770)],
            vec![boundary(50_000), assistant("claude-x", 40_000, 10)],
            vec![json!({"type": "system", "subtype": "compact_boundary",
                        "compactMetadata": "odd"})],
            vec![json!({"type": "assistant", "isSidechain": "yes",
                        "message": {"model": 7, "usage": {"output_tokens": 9}}})],
            vec![json!({"type": "user"})],
        ];
        for lines in cases {
            let typed: Vec<_> = lines
                .iter()
                .map(|v| crate::agent::live_line::parse_line(&v.to_string()).unwrap())
                .collect();
            assert_eq!(
                from_live_lines(&typed),
                from_claude_lines(&lines),
                "{lines:?}"
            );
        }
    }
}
