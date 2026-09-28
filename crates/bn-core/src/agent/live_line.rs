//! A Claude transcript line read for the live fleet view, keeping only the fields
//! [`super::supervise`] uses.
//!
//! The live view reads every still-running transcript on every call, and a long session's
//! transcript runs to tens of megabytes, almost all of it message text, tool inputs and tool
//! results the view never looks at. Parsing each line into a `serde_json::Value` built that whole
//! tree just to read a handful of keys. These types skip everything else without building it.
//!
//! Every field is lenient, so a line reads exactly as the `Value` accessors read it: a key whose
//! value has an unexpected type reads as absent (`as_str()` / `as_i64()` returning `None`) instead
//! of failing the line.

use serde::de::{Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use std::fmt;

/// One transcript line: `type`, `sessionId`, `gitBranch`, `cwd`, `timestamp` and `message`. A line
/// that is valid JSON but not an object reads as all-absent, and a repeated key keeps its last
/// value, both as with a `Value`.
#[derive(Debug, Default)]
pub(crate) struct LiveLine {
    pub typ: LenientStr,
    pub session_id: LenientStr,
    pub git_branch: LenientStr,
    pub cwd: LenientStr,
    pub timestamp: LenientStr,
    pub message: MessageField,
    /// `subtype` (`compact_boundary` marks a compaction) — for the context-usage read.
    pub subtype: LenientStr,
    /// `isSidechain`, when it is a boolean.
    pub is_sidechain: LenientBool,
    /// `compactMetadata.preTokens`, when it is an integer.
    pub compact_pre_tokens: Option<i64>,
}

/// A string value, or `None` for any other JSON type — `Value::as_str`.
#[derive(Debug, Default)]
pub(crate) struct LenientStr(pub Option<String>);

/// A boolean value, or `None` for any other JSON type — `Value::as_bool`.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct LenientBool(pub Option<bool>);

/// An integer value, or `None` for any other JSON type (floats included) — `Value::as_i64`.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct LenientI64(pub Option<i64>);

/// The `message` key. `present` is true whenever the key exists, whatever its value (`null`
/// included), which is what `v.get("message").is_some()` tested. The rest is read only from an
/// object.
#[derive(Debug, Default)]
pub(crate) struct MessageField {
    pub present: bool,
    pub id: Option<String>,
    /// `model`, when it is a string.
    pub model: Option<String>,
    /// `Some` when the message object has a `usage` key, of any type.
    pub usage: Option<Usage>,
    /// Count of `tool_use` blocks when `content` is an array; `None` otherwise.
    pub tool_uses: Option<i64>,
}

/// The four token counters of a `usage` object; each `0` when missing or not an integer.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Usage {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

/// Parse one line; `None` when it is not valid JSON (the line is skipped, as before).
pub(crate) fn parse_line(line: &str) -> Option<LiveLine> {
    serde_json::from_str(line).ok()
}

/// Every parseable line of `path`, in file order.
pub(crate) fn read_lines(path: &std::path::Path) -> Vec<LiveLine> {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| parse_line(&l))
        .collect()
}

/// Implements the scalar `visit_*` methods of a lenient visitor: each one maps to `$none`, and
/// compound values are drained without being built.
macro_rules! ignore_other_types {
    ($none:expr) => {
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok($none)
        }
        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok($none)
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok($none)
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok($none)
        }
        fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            d.deserialize_any(self)
        }
    };
}

/// Drain a sequence without building it.
fn skip_seq<'de, A: SeqAccess<'de>>(mut seq: A) -> Result<(), A::Error> {
    while seq.next_element::<IgnoredAny>()?.is_some() {}
    Ok(())
}

/// Drain a map without building it.
fn skip_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<(), A::Error> {
    while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
    Ok(())
}

impl<'de> Deserialize<'de> for LenientStr {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientStr;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E> {
                Ok(LenientStr(Some(s.to_string())))
            }
            fn visit_string<E>(self, s: String) -> Result<Self::Value, E> {
                Ok(LenientStr(Some(s)))
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientStr(None))
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientStr(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                skip_seq(seq).map(|_| LenientStr(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                skip_map(map).map(|_| LenientStr(None))
            }
            ignore_other_types!(LenientStr(None));
        }
        d.deserialize_any(V)
    }
}

impl<'de> Deserialize<'de> for LenientI64 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientI64;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_i64<E>(self, n: i64) -> Result<Self::Value, E> {
                Ok(LenientI64(Some(n)))
            }
            fn visit_u64<E>(self, n: u64) -> Result<Self::Value, E> {
                Ok(LenientI64(i64::try_from(n).ok()))
            }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientI64(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                skip_seq(seq).map(|_| LenientI64(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                skip_map(map).map(|_| LenientI64(None))
            }
            ignore_other_types!(LenientI64(None));
        }
        d.deserialize_any(V)
    }
}

impl<'de> Deserialize<'de> for LenientBool {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientBool;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_bool<E>(self, b: bool) -> Result<Self::Value, E> {
                Ok(LenientBool(Some(b)))
            }
            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientBool(None))
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(LenientBool(None))
            }
            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(LenientBool(None))
            }
            fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
                d.deserialize_any(self)
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientBool(None))
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientBool(None))
            }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientBool(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                skip_seq(seq).map(|_| LenientBool(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                skip_map(map).map(|_| LenientBool(None))
            }
        }
        d.deserialize_any(V)
    }
}

/// `compactMetadata`: its `preTokens` when it is an object with an integer there.
struct PreTokens(Option<i64>);

impl<'de> Deserialize<'de> for PreTokens {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = PreTokens;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut pre = None;
                while let Some(key) = map.next_key::<LenientKey>()? {
                    if key.0 == "preTokens" {
                        pre = map.next_value::<LenientI64>()?.0;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(PreTokens(pre))
            }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(PreTokens(None))
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(PreTokens(None))
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(PreTokens(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                skip_seq(seq).map(|_| PreTokens(None))
            }
            ignore_other_types!(PreTokens(None));
        }
        d.deserialize_any(V)
    }
}

impl<'de> Deserialize<'de> for Usage {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Usage;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                // Last occurrence of a key wins, as it does when serde_json builds a `Value`.
                let mut u = Usage::default();
                while let Some(key) = map.next_key::<LenientKey>()? {
                    let slot = match key.0.as_str() {
                        "input_tokens" => &mut u.input,
                        "output_tokens" => &mut u.output,
                        "cache_read_input_tokens" => &mut u.cache_read,
                        "cache_creation_input_tokens" => &mut u.cache_write,
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                            continue;
                        }
                    };
                    *slot = map.next_value::<LenientI64>()?.0.unwrap_or(0);
                }
                Ok(u)
            }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(Usage::default())
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(Usage::default())
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(Usage::default())
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                skip_seq(seq).map(|_| Usage::default())
            }
            ignore_other_types!(Usage::default());
        }
        d.deserialize_any(V)
    }
}

/// An object key, owned (keys with escapes cannot be borrowed).
struct LenientKey(String);

impl<'de> Deserialize<'de> for LenientKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(LenientKey)
    }
}

impl<'de> Deserialize<'de> for LiveLine {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LiveLine;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut l = LiveLine::default();
                while let Some(key) = map.next_key::<LenientKey>()? {
                    match key.0.as_str() {
                        "type" => l.typ = map.next_value()?,
                        "sessionId" => l.session_id = map.next_value()?,
                        "gitBranch" => l.git_branch = map.next_value()?,
                        "cwd" => l.cwd = map.next_value()?,
                        "timestamp" => l.timestamp = map.next_value()?,
                        "message" => l.message = map.next_value()?,
                        "subtype" => l.subtype = map.next_value()?,
                        "isSidechain" => l.is_sidechain = map.next_value()?,
                        "compactMetadata" => {
                            l.compact_pre_tokens = map.next_value::<PreTokens>()?.0;
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(l)
            }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LiveLine::default())
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LiveLine::default())
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LiveLine::default())
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                skip_seq(seq).map(|_| LiveLine::default())
            }
            ignore_other_types!(LiveLine::default());
        }
        d.deserialize_any(V)
    }
}

/// The `type` of one content block: counts as a tool use only when the block is an object whose
/// `type` is the string `tool_use`.
struct IsToolUse(bool);

impl<'de> Deserialize<'de> for IsToolUse {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = IsToolUse;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut is = false;
                while let Some(key) = map.next_key::<LenientKey>()? {
                    if key.0 == "type" {
                        is = map.next_value::<LenientStr>()?.0.as_deref() == Some("tool_use");
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(IsToolUse(is))
            }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(IsToolUse(false))
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(IsToolUse(false))
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(IsToolUse(false))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                skip_seq(seq).map(|_| IsToolUse(false))
            }
            ignore_other_types!(IsToolUse(false));
        }
        d.deserialize_any(V)
    }
}

/// `content`: the number of `tool_use` blocks when it is an array, `None` for anything else.
struct ToolUses(Option<i64>);

impl<'de> Deserialize<'de> for ToolUses {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = ToolUses;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut n = 0;
                while let Some(block) = seq.next_element::<IsToolUse>()? {
                    n += i64::from(block.0);
                }
                Ok(ToolUses(Some(n)))
            }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(ToolUses(None))
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(ToolUses(None))
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(ToolUses(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                skip_map(map).map(|_| ToolUses(None))
            }
            ignore_other_types!(ToolUses(None));
        }
        d.deserialize_any(V)
    }
}

impl<'de> Deserialize<'de> for MessageField {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = MessageField;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut m = MessageField {
                    present: true,
                    ..MessageField::default()
                };
                while let Some(key) = map.next_key::<LenientKey>()? {
                    match key.0.as_str() {
                        "id" => m.id = map.next_value::<LenientStr>()?.0,
                        "model" => m.model = map.next_value::<LenientStr>()?.0,
                        "usage" => m.usage = Some(map.next_value::<Usage>()?),
                        "content" => m.tool_uses = map.next_value::<ToolUses>()?.0,
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(m)
            }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(present_only())
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(present_only())
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(present_only())
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                skip_seq(seq).map(|_| present_only())
            }
            ignore_other_types!(present_only());
        }
        d.deserialize_any(V)
    }
}

fn present_only() -> MessageField {
    MessageField {
        present: true,
        ..MessageField::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// `(type, gitBranch, message present, message id, usage input_tokens, tool_use blocks)`.
    type Read = (
        Option<String>,
        Option<String>,
        bool,
        Option<String>,
        i64,
        i64,
    );

    /// The same line read through `Value` accessors, the way the live view used to.
    fn via_value(line: &str) -> Read {
        let v: Value = serde_json::from_str(line).unwrap();
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
        let msg = v.get("message");
        let id = msg
            .and_then(|m| m.get("id"))
            .and_then(|x| x.as_str())
            .map(String::from);
        let input = msg
            .and_then(|m| m.get("usage"))
            .and_then(|u| u.get("input_tokens"))
            .and_then(|x| x.as_i64())
            .unwrap_or(0);
        let tools = msg
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
            .map(|c| {
                c.iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
                    .count() as i64
            })
            .unwrap_or(0);
        (s("type"), s("gitBranch"), msg.is_some(), id, input, tools)
    }

    fn via_typed(line: &str) -> Read {
        let l = parse_line(line).unwrap();
        let m = l.message;
        (
            l.typ.0,
            l.git_branch.0,
            m.present,
            m.id,
            m.usage.map_or(0, |u| u.input),
            m.tool_uses.unwrap_or(0),
        )
    }

    #[test]
    fn reads_every_line_shape_as_the_value_accessors_did() {
        let lines = [
            r#"{"type":"assistant","sessionId":"s","gitBranch":"main","cwd":"/w","timestamp":"2026-09-27T10:00:00Z","message":{"id":"m1","usage":{"input_tokens":5,"output_tokens":7},"content":[{"type":"text","text":"hi"},{"type":"tool_use","input":{"a":[1,2,{"b":null}]}},{"type":"tool_use"}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":"plain string content"}}"#,
            r#"{"type":"assistant","message":null}"#,
            r#"{"type":"assistant","message":"not an object"}"#,
            r#"{"type":"assistant","message":{"id":7,"usage":"odd","content":{"type":"tool_use"}}}"#,
            r#"{"type":"assistant","message":{"id":"","usage":{"input_tokens":1.5},"content":[1,"x",null,{"type":7}]}}"#,
            r#"{"type":5,"gitBranch":null,"cwd":{"x":1}}"#,
            r#"{"type":"assistant","type":"user"}"#,
            r#"{"message":{"id":"a","id":"b","usage":{"input_tokens":1,"input_tokens":2}}}"#,
            r#"{"type":"assistant","message":{"usage":{"input_tokens":18446744073709551615}}}"#,
            r#"[1,2,3]"#,
            r#""just a string""#,
            r#"{}"#,
        ];
        for line in lines {
            assert_eq!(via_typed(line), via_value(line), "{line}");
        }
    }

    #[test]
    fn a_line_that_is_not_json_is_skipped() {
        assert!(parse_line("{not json").is_none());
        assert!(parse_line("").is_none());
    }
}
