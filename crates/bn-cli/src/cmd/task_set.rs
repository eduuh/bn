//! `bn task set <id> key:value…` — edit an existing task's scheduling metadata.
//!
//! The dashboard's task PATCH route was the only writer of `day:`, `est:`, `priority` and `repo:`
//! on an existing task; #1479 removed it with the web server, which left the task-enhance and
//! task-triage agents told to stamp fields nothing could stamp. This is that write, on the CLI,
//! with the same validation the route applied.
//!
//! The keys are an allowlist on purpose. `smart:` is a gate verdict and `uid:`/`mod:` are
//! bookkeeping, so a generic `key:value` setter would let any caller forge them.
use crate::*;

/// The keys `bn task set` accepts, in the order the help text lists them.
pub(crate) const SETTABLE: &[&str] = &["day", "due", "est", "priority", "q", "repo"];

/// One validated edit: a key and its new value, `None` to clear it.
#[derive(Debug, PartialEq)]
pub(crate) struct Edit {
    pub key: String,
    pub value: Option<String>,
}

/// Parse and validate `key:value` pairs. An empty value (`day:`) clears the key. Every pair is
/// checked before anything is written, so one bad pair rejects the whole command.
pub(crate) fn parse_edits(pairs: &[String]) -> Result<Vec<Edit>, String> {
    if pairs.is_empty() {
        return Err(format!("nothing to set: pass key:value ({})", SETTABLE.join(", ")));
    }
    pairs.iter().map(|p| parse_edit(p)).collect()
}

fn parse_edit(pair: &str) -> Result<Edit, String> {
    let (key, value) = pair
        .split_once(':')
        .ok_or_else(|| format!("'{pair}' is not key:value"))?;
    let key = key.trim().to_ascii_lowercase();
    let value = value.trim();
    if !SETTABLE.contains(&key.as_str()) {
        return Err(format!("cannot set '{key}': settable keys are {}", SETTABLE.join(", ")));
    }
    if value.is_empty() {
        return Ok(Edit { key, value: None });
    }
    let ok = match key.as_str() {
        "day" | "due" => bn_core::util::epoch_day(value).is_some(),
        "est" => bn_core::smart::parse_est_minutes(value).is_some(),
        "priority" => matches!(value, "A" | "B" | "C" | "a" | "b" | "c"),
        // `q:` is a quarter goal key (`bn quarter`); same charset as a repo name.
        "repo" | "q" => valid_repo(value),
        _ => unreachable!("checked against SETTABLE"),
    };
    if !ok {
        let want = match key.as_str() {
            "day" | "due" => "YYYY-MM-DD",
            "est" => "like 90m, 2h or 1h30m",
            "priority" => "A, B or C",
            _ => "letters, digits, '.', '_' or '-'",
        };
        return Err(format!("{key} must be {want}, got '{value}'"));
    }
    let value = if key == "priority" { value.to_ascii_uppercase() } else { value.to_string() };
    Ok(Edit { key, value: Some(value) })
}

/// A repo name: the charset bn uses for state paths and argv slots.
fn valid_repo(s: &str) -> bool {
    s.len() <= 100
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Apply validated edits to one task. `priority` is the todo.txt `(A)` token, not a meta.
pub(crate) fn apply(task: &mut bn_core::todo::Task, edits: &[Edit]) {
    for e in edits {
        match (e.key.as_str(), e.value.as_deref()) {
            ("priority", v) => {
                task.priority = v.and_then(|v| v.chars().next());
                task.touch();
            }
            (k, Some(v)) => task.set_meta(k, v),
            (k, None) => {
                task.remove_meta(k);
            }
        }
    }
}

pub(crate) fn emit_task_set(cfg: &Config, id: &str, pairs: &[String], as_json: bool) {
    let edits = parse_edits(pairs).unwrap_or_else(|e| fail(&format!("bn task set: {e}")));
    let store = crate::cmd::task::task_store(cfg);
    // Serialize the load→mutate→save against a concurrent append add (held to fn end).
    let _lock = store.lock();
    let mut file = store
        .load()
        .unwrap_or_else(|e| fail(&format!("bn task set: {e}")));
    let task = file
        .task_mut(id)
        .unwrap_or_else(|| fail(&format!("bn task set: no task {id}")));
    apply(task, &edits);
    store
        .save(&file)
        .unwrap_or_else(|e| fail(&format!("bn task set: {e}")));
    if as_json {
        let set: serde_json::Map<String, serde_json::Value> = edits
            .iter()
            .map(|e| (e.key.clone(), e.value.clone().into()))
            .collect();
        println!(
            "{}",
            Envelope::success("task", serde_json::json!({ "id": id, "set": set })).to_json()
        );
    } else {
        let shown: Vec<String> = edits
            .iter()
            .map(|e| match &e.value {
                Some(v) => format!("{}:{v}", e.key),
                None => format!("{} cleared", e.key),
            })
            .collect();
        println!("Set {id} {}", shown.join(" "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edits(pairs: &[&str]) -> Result<Vec<Edit>, String> {
        parse_edits(&pairs.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn accepts_each_settable_key_and_normalises_priority() {
        let got = edits(&["day:2026-10-09", "est:1h30m", "priority:b", "repo:bn", "due:2026-11-01", "q:ship-v1"])
            .unwrap();
        let vals: Vec<_> = got.iter().map(|e| (e.key.as_str(), e.value.as_deref())).collect();
        assert_eq!(
            vals,
            [
                ("day", Some("2026-10-09")),
                ("est", Some("1h30m")),
                ("priority", Some("B")),
                ("repo", Some("bn")),
                ("due", Some("2026-11-01")),
                ("q", Some("ship-v1")),
            ]
        );
    }

    #[test]
    fn empty_value_clears() {
        assert_eq!(edits(&["day:"]).unwrap(), [Edit { key: "day".into(), value: None }]);
    }

    /// `smart:` is the SMART gate's verdict; letting `set` write it would let any caller wave a
    /// task through the gate.
    #[test]
    fn rejects_keys_outside_the_allowlist() {
        for bad in ["smart:ok", "uid:x", "mod:1", "branch:main"] {
            assert!(edits(&[bad]).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn rejects_malformed_values_and_pairs() {
        for bad in ["day:tomorrow", "est:soon", "priority:D", "repo:a b", "day", ""] {
            assert!(edits(&[bad]).is_err(), "'{bad}' should be rejected");
        }
        assert!(edits(&[]).is_err());
    }

    /// One bad pair must reject the whole command before anything is written.
    #[test]
    fn one_bad_pair_rejects_all() {
        assert!(edits(&["day:2026-10-09", "est:soon"]).is_err());
    }

    #[test]
    fn apply_sets_meta_priority_and_clears() {
        let mut file = bn_core::todo::TodoFile::default();
        let id = file.add("write it", Some('C'), "", "manual", "2026-10-08");
        let task = file.task_mut(&id).unwrap();
        apply(task, &edits(&["day:2026-10-09", "priority:A", "est:2h"]).unwrap());
        assert_eq!(task.meta("day"), Some("2026-10-09"));
        assert_eq!(task.meta("est"), Some("2h"));
        assert_eq!(task.priority, Some('A'));
        apply(task, &edits(&["day:", "priority:"]).unwrap());
        assert_eq!(task.meta("day"), None);
        assert_eq!(task.priority, None);
        assert_eq!(task.meta("est"), Some("2h"));
    }
}
