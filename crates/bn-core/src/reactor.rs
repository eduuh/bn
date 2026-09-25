//! `bn on` — user-scriptable reactions to the event log.
//!
//! bn already writes every interesting thing that happens to `events.jsonl`, and already reacts to
//! some of it: `bn watch` toasts agents that need you, `bn doctor` surfaces failing crons. But
//! those reactions are compiled in. Wanting a different one — "when a PR merges in any repo, run
//! this" — meant editing Rust. This module is the write-your-own half: a rule matches events, and
//! a command runs for each match.
//!
//! Two decisions worth stating, because both are about not surprising you:
//!
//! * **A rule fires at most once per event.** The reactor keeps a cursor (the timestamp of the
//!   last event it considered) and only looks past it. Without that, every pass would re-run every
//!   matching command in the log's history — the first tick of a rule added on a year-old log
//!   would fire thousands of processes.
//! * **A brand-new rule starts from now, not from the beginning of the log.** Adding a rule is a
//!   statement about what should happen *next*, not a request to replay the past. `--replay` asks
//!   for the other thing explicitly.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// One reaction: which events, and what to run for each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Stable short id, used by `bn on rm`.
    pub id: String,
    /// The event `kind` to match (e.g. `pr_merged`). `*` matches every kind.
    pub kind: String,
    /// The command to run, as argv. Run directly — no shell — so nothing is word-split or
    /// glob-expanded behind your back; use `sh -c '…'` when you want shell syntax.
    pub run: Vec<String>,
    /// Extra equality filters on the event's fields, all of which must match.
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
    /// Unix seconds when the rule was created — a rule never fires on events older than this
    /// unless it is replayed.
    pub created: i64,
    /// Set false to keep a rule on file without running it.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// The rule file, `<config_dir>/hooks.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Rules {
    #[serde(default, rename = "on")]
    pub rules: Vec<Rule>,
}

pub fn rules_path() -> PathBuf {
    crate::resolver::config_dir().join("hooks.toml")
}

/// The reactor's cursor: the newest event timestamp already considered.
pub fn cursor_path() -> PathBuf {
    crate::resolver::state_dir().join("reactor-cursor")
}

pub fn read_rules() -> Rules {
    let Ok(raw) = std::fs::read_to_string(rules_path()) else {
        return Rules::default();
    };
    toml::from_str(&raw).unwrap_or_default()
}

pub fn write_rules(rules: &Rules) -> std::io::Result<()> {
    let path = rules_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = toml::to_string_pretty(rules)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, body)
}

pub fn read_cursor() -> i64 {
    std::fs::read_to_string(cursor_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

pub fn write_cursor(ts: i64) -> std::io::Result<()> {
    let path = cursor_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, ts.to_string())
}

/// A short id that does not collide with `existing`.
pub fn mint_id(kind: &str, existing: &[Rule]) -> String {
    let stem: String = kind
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(6)
        .collect();
    let stem = if stem.is_empty() {
        "rule".to_string()
    } else {
        stem
    };
    for n in 1..1000 {
        let id = format!("{stem}{n}");
        if !existing.iter().any(|r| r.id == id) {
            return id;
        }
    }
    format!("{stem}-{}", crate::util::now_epoch_secs())
}

/// Whether `rule` matches `event`, ignoring timing.
///
/// A field filter compares against the event field's *display* form, so `--field number=52`
/// matches whether the log stored `52` or `"52"` — `bn event` turns all-digit values into numbers,
/// and a rule written by hand should not have to know that.
pub fn matches(rule: &Rule, event: &crate::events::Event) -> bool {
    if !rule.enabled {
        return false;
    }
    if rule.kind != "*" && rule.kind != event.kind {
        return false;
    }
    rule.fields.iter().all(|(k, want)| {
        event
            .fields
            .get(k)
            .is_some_and(|got| field_string(got) == *want)
    })
}

/// A field value as a rule would write it: strings unquoted, everything else as JSON.
pub fn field_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// One (rule, event) pair the reactor decided to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firing {
    pub rule_id: String,
    pub kind: String,
    pub ts: i64,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// Decide what should fire. Pure: no clock, no filesystem, no processes — `since` and the event
/// list are the whole input, which is what makes the cursor and once-only rules testable.
///
/// An event fires a rule when it is newer than the cursor AND not older than the rule itself, so
/// adding a rule never replays history. Results are ordered oldest-first, so a run that dies
/// part-way leaves the cursor on the last event it actually handled.
pub fn plan(rules: &[Rule], events: &[crate::events::Event], since: i64) -> Vec<Firing> {
    let mut out = Vec::new();
    let mut fresh: Vec<&crate::events::Event> = events.iter().filter(|e| e.ts > since).collect();
    fresh.sort_by_key(|e| e.ts);
    for event in fresh {
        for rule in rules {
            if event.ts < rule.created || !matches(rule, event) {
                continue;
            }
            out.push(Firing {
                rule_id: rule.id.clone(),
                kind: event.kind.clone(),
                ts: event.ts,
                argv: rule.run.clone(),
                env: event_env(event),
            });
        }
    }
    out
}

/// The environment one firing hands its command: the event's kind and timestamp, plus every field
/// as `BN_EVENT_<FIELD>`. The whole event also goes in on stdin as JSON, for anything that needs
/// more structure than a string.
pub fn event_env(event: &crate::events::Event) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("BN_EVENT_KIND".to_string(), event.kind.clone());
    env.insert("BN_EVENT_TS".to_string(), event.ts.to_string());
    for (k, v) in &event.fields {
        let name: String = k
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();
        env.insert(format!("BN_EVENT_{name}"), field_string(v));
    }
    env
}

/// The event as the JSON a firing receives on stdin.
pub fn event_json(event: &crate::events::Event) -> String {
    let mut map = Map::new();
    map.insert("kind".to_string(), Value::String(event.kind.clone()));
    map.insert("ts".to_string(), Value::from(event.ts));
    for (k, v) in &event.fields {
        map.insert(k.clone(), v.clone());
    }
    Value::Object(map).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Event;

    fn ev(kind: &str, ts: i64, fields: &[(&str, Value)]) -> Event {
        let mut map = Map::new();
        for (k, v) in fields {
            map.insert((*k).to_string(), v.clone());
        }
        Event {
            kind: kind.to_string(),
            ts,
            fields: map,
        }
    }

    fn rule(id: &str, kind: &str, created: i64) -> Rule {
        Rule {
            id: id.to_string(),
            kind: kind.to_string(),
            run: vec!["true".to_string()],
            fields: BTreeMap::new(),
            created,
            enabled: true,
        }
    }

    #[test]
    fn a_matching_event_newer_than_the_cursor_fires() {
        let fired = plan(
            &[rule("r1", "pr_merged", 0)],
            &[ev("pr_merged", 10, &[])],
            5,
        );
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].rule_id, "r1");
    }

    /// The cursor is what stops a pass re-running the whole log every tick.
    #[test]
    fn an_event_at_or_before_the_cursor_never_fires() {
        let rules = [rule("r1", "pr_merged", 0)];
        let events = [ev("pr_merged", 5, &[]), ev("pr_merged", 3, &[])];
        assert!(plan(&rules, &events, 5).is_empty());
    }

    /// Adding a rule is a statement about what happens next, not a request to replay history.
    #[test]
    fn a_rule_does_not_fire_on_events_older_than_itself() {
        let fired = plan(
            &[rule("r1", "pr_merged", 100)],
            &[ev("pr_merged", 50, &[])],
            0,
        );
        assert!(fired.is_empty());
    }

    #[test]
    fn a_star_rule_matches_every_kind() {
        let fired = plan(
            &[rule("r1", "*", 0)],
            &[ev("pr_merged", 10, &[]), ev("deploy", 11, &[])],
            0,
        );
        assert_eq!(fired.len(), 2);
    }

    #[test]
    fn a_disabled_rule_never_fires() {
        let mut r = rule("r1", "pr_merged", 0);
        r.enabled = false;
        assert!(plan(&[r], &[ev("pr_merged", 10, &[])], 0).is_empty());
    }

    /// `bn event` stores all-digit values as numbers, so a hand-written filter must match both.
    #[test]
    fn a_field_filter_matches_a_number_stored_unquoted() {
        let mut r = rule("r1", "pr_merged", 0);
        r.fields.insert("number".to_string(), "52".to_string());
        let numeric = ev("pr_merged", 10, &[("number", Value::from(52))]);
        let stringy = ev("pr_merged", 11, &[("number", Value::from("52"))]);
        assert_eq!(plan(&[r], &[numeric, stringy], 0).len(), 2);
    }

    #[test]
    fn a_field_filter_that_disagrees_blocks_the_firing() {
        let mut r = rule("r1", "pr_merged", 0);
        r.fields.insert("repo".to_string(), "bn".to_string());
        let other = ev("pr_merged", 10, &[("repo", Value::from("wira360"))]);
        let missing = ev("pr_merged", 11, &[]);
        assert!(plan(std::slice::from_ref(&r), &[other], 0).is_empty());
        assert!(plan(&[r], &[missing], 0).is_empty());
    }

    /// Oldest-first, so a crash mid-run leaves the cursor on the last event actually handled.
    #[test]
    fn firings_come_back_oldest_first() {
        let fired = plan(
            &[rule("r1", "*", 0)],
            &[ev("a", 30, &[]), ev("b", 10, &[]), ev("c", 20, &[])],
            0,
        );
        let ts: Vec<i64> = fired.iter().map(|f| f.ts).collect();
        assert_eq!(ts, [10, 20, 30]);
    }

    #[test]
    fn fields_reach_the_command_as_bn_event_variables() {
        let env = event_env(&ev(
            "pr_merged",
            10,
            &[("number", Value::from(52)), ("work-item", Value::from("x"))],
        ));
        assert_eq!(
            env.get("BN_EVENT_KIND").map(String::as_str),
            Some("pr_merged")
        );
        assert_eq!(env.get("BN_EVENT_NUMBER").map(String::as_str), Some("52"));
        // A field name that is not a legal variable name is sanitized, not dropped.
        assert_eq!(env.get("BN_EVENT_WORK_ITEM").map(String::as_str), Some("x"));
    }

    #[test]
    fn an_id_never_collides_with_one_already_on_file() {
        let existing = vec![rule("prmerg1", "pr_merged", 0)];
        assert_eq!(mint_id("pr_merged", &existing), "prmerg2");
        assert_eq!(mint_id("pr_merged", &[]), "prmerg1");
    }
}
