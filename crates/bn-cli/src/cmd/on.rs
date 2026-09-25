//! `bn on` — the CLI over [`bn_core::reactor`]: manage rules, and run the reactor pass.
use crate::*;
use bn_core::reactor::{self, Firing, Rule};
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

const RED: &str = style::Tone::Danger.sgr();
const GREEN: &str = style::Tone::Ok.sgr();
const DIM: &str = style::Tone::Meta.sgr();
const NC: &str = style::RESET;

pub(crate) fn emit_on(cfg: &Config, action: Option<OnCmd>, json: bool) -> i32 {
    match action {
        None => list(json),
        Some(OnCmd::Add { kind, fields, run }) => add(&kind, &fields, run, json),
        Some(OnCmd::Rm { id }) => remove(&id, json),
        Some(OnCmd::Disable { id }) => set_enabled(&id, false, json),
        Some(OnCmd::Enable { id }) => set_enabled(&id, true, json),
        Some(OnCmd::Tick { dry_run, replay }) => tick(cfg, dry_run, replay, json),
        Some(OnCmd::Test { kind, fields }) => test(&kind, &fields, json),
    }
}

/// Every event a rule may react to.
///
/// Two logs, because bn writes to two: `bn event` and the job scripts append to the **synced
/// metrics** log (one per machine — a rule should fire on a PR merged from the laptop too), while
/// cron runs land in this machine's **state** log. Reacting to only one of them silently drops
/// half the kinds a person would reasonably write a rule for.
fn all_events(cfg: &Config) -> Vec<bn_core::events::Event> {
    let mut paths = cfg.metrics_logs_all_machines();
    paths.push(bn_core::events::log_path());
    bn_core::events::read_events_across(&paths)
}

fn rule_json(r: &Rule) -> serde_json::Value {
    serde_json::json!({
        "id": r.id,
        "kind": r.kind,
        "run": r.run,
        "fields": r.fields,
        "created": r.created,
        "enabled": r.enabled,
    })
}

/// `--field k=v` pairs into a map, or an error naming the one that is not a pair.
fn parse_fields(raw: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for item in raw {
        let Some((k, v)) = item.split_once('=') else {
            return Err(format!("--field expects K=V, got '{item}'"));
        };
        out.insert(k.trim().to_string(), v.to_string());
    }
    Ok(out)
}

fn list(json: bool) -> i32 {
    let rules = reactor::read_rules().rules;
    if json {
        json_ok(
            "on",
            serde_json::json!({
                "rules": rules.iter().map(rule_json).collect::<Vec<_>>(),
                "cursor": reactor::read_cursor(),
                "path": reactor::rules_path().to_string_lossy(),
            }),
        );
        return 0;
    }
    if rules.is_empty() {
        println!("No reactions. `bn on add <kind> -- <command>` adds one.");
        return 0;
    }
    for r in &rules {
        let state = if r.enabled { "" } else { " (disabled)" };
        let filters: Vec<String> = r.fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let filters = if filters.is_empty() {
            String::new()
        } else {
            format!(" [{}]", filters.join(" "))
        };
        println!("{:<10} {}{filters}{state}", r.id, r.kind);
        cprintln!("           {DIM}{}{NC}", r.run.join(" "));
    }
    0
}

fn add(kind: &str, raw_fields: &[String], run: Vec<String>, json: bool) -> i32 {
    let fields = match parse_fields(raw_fields) {
        Ok(f) => f,
        Err(e) => fail_json(json, "on", "bad_field", &e),
    };
    let mut rules = reactor::read_rules();
    let rule = Rule {
        id: reactor::mint_id(kind, &rules.rules),
        kind: kind.to_string(),
        run,
        fields,
        // From now: adding a rule says what should happen next, not replay the log.
        created: bn_core::util::now_epoch_secs(),
        enabled: true,
    };
    rules.rules.push(rule.clone());
    if let Err(e) = reactor::write_rules(&rules) {
        fail_json(json, "on", "write_failed", &format!("{e}"));
    }
    if json {
        json_ok("on-add", rule_json(&rule));
    } else {
        println!("Added [{}] on {} → {}", rule.id, rule.kind, rule.run.join(" "));
    }
    0
}

fn remove(id: &str, json: bool) -> i32 {
    let mut rules = reactor::read_rules();
    let before = rules.rules.len();
    rules.rules.retain(|r| r.id != id);
    if rules.rules.len() == before {
        fail_json(json, "on", "no_such_rule", &format!("no rule '{id}'"));
    }
    if let Err(e) = reactor::write_rules(&rules) {
        fail_json(json, "on", "write_failed", &format!("{e}"));
    }
    if json {
        json_ok("on-rm", serde_json::json!({ "id": id }));
    } else {
        println!("Removed [{id}]");
    }
    0
}

fn set_enabled(id: &str, enabled: bool, json: bool) -> i32 {
    let mut rules = reactor::read_rules();
    let Some(rule) = rules.rules.iter_mut().find(|r| r.id == id) else {
        fail_json(json, "on", "no_such_rule", &format!("no rule '{id}'"));
    };
    rule.enabled = enabled;
    let snapshot = rule.clone();
    if let Err(e) = reactor::write_rules(&rules) {
        fail_json(json, "on", "write_failed", &format!("{e}"));
    }
    if json {
        json_ok("on-set", rule_json(&snapshot));
    } else {
        println!(
            "[{id}] {}",
            if enabled { "enabled" } else { "disabled" }
        );
    }
    0
}

/// Run one firing. The event reaches the command as `BN_EVENT_*` variables and as JSON on stdin.
fn run_firing(f: &Firing, event_json: &str) -> (i32, String) {
    let Some((program, rest)) = f.argv.split_first() else {
        return (1, "rule has no command".to_string());
    };
    let child = Command::new(program)
        .args(rest)
        .envs(&f.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return (127, format!("{program}: {e}")),
    };
    if let Some(mut stdin) = child.stdin.take() {
        // A command that ignores stdin closes it early; that is a broken pipe, not a failure.
        let _ = stdin.write_all(event_json.as_bytes());
    }
    match child.wait_with_output() {
        Ok(out) => {
            let tail = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
            let err = String::from_utf8_lossy(&out.stderr).trim_end().to_string();
            let msg = if err.is_empty() { tail } else { format!("{tail}{err}") };
            (out.status.code().unwrap_or(1), msg)
        }
        Err(e) => (1, format!("{e}")),
    }
}

fn tick(cfg: &Config, dry_run: bool, replay: bool, json: bool) -> i32 {
    let rules = reactor::read_rules().rules;
    let events = all_events(cfg);
    let since = if replay { 0 } else { reactor::read_cursor() };
    let firings = reactor::plan(&rules, &events, since);
    // The high-water mark advances over every event this pass considered, not only the ones that
    // matched — otherwise an unmatched event is re-examined forever.
    let high = events.iter().map(|e| e.ts).max().unwrap_or(since).max(since);
    let by_ts: BTreeMap<i64, String> = events
        .iter()
        .map(|e| (e.ts, reactor::event_json(e)))
        .collect();

    let mut results = Vec::new();
    let mut failed = 0;
    for f in &firings {
        if dry_run {
            results.push(serde_json::json!({
                "rule": f.rule_id, "kind": f.kind, "ts": f.ts,
                "run": f.argv, "would_run": true,
            }));
            continue;
        }
        let payload = by_ts.get(&f.ts).cloned().unwrap_or_else(|| "{}".to_string());
        let (code, output) = run_firing(f, &payload);
        if code != 0 {
            failed += 1;
        }
        results.push(serde_json::json!({
            "rule": f.rule_id, "kind": f.kind, "ts": f.ts,
            "run": f.argv, "exit": code, "ok": code == 0, "output": output,
        }));
    }
    // Only a real pass moves the cursor; a dry run must leave the next pass exactly as it was.
    if !dry_run && !replay {
        bn_core::util::best_effort(reactor::write_cursor(high), "on: write cursor");
    }
    if json {
        json_ok(
            "on-tick",
            serde_json::json!({
                "fired": results, "failed": failed,
                "cursor": if dry_run || replay { since } else { high },
                "dry_run": dry_run,
            }),
        );
        return i32::from(failed > 0);
    }
    if firings.is_empty() {
        println!("Nothing to react to.");
        return 0;
    }
    for r in &results {
        let rule = r["rule"].as_str().unwrap_or("?");
        let kind = r["kind"].as_str().unwrap_or("?");
        if dry_run {
            println!("would fire [{rule}] on {kind}");
        } else if r["ok"].as_bool().unwrap_or(false) {
            cprintln!("{GREEN}[{rule}] {kind} ok{NC}");
        } else {
            cprintln!(
                "{RED}[{rule}] {kind} exit {}{NC} {}",
                r["exit"].as_i64().unwrap_or(1),
                r["output"].as_str().unwrap_or("")
            );
        }
    }
    i32::from(failed > 0)
}

fn test(kind: &str, raw_fields: &[String], json: bool) -> i32 {
    let fields = match parse_fields(raw_fields) {
        Ok(f) => f,
        Err(e) => fail_json(json, "on", "bad_field", &e),
    };
    let mut map = serde_json::Map::new();
    for (k, v) in &fields {
        map.insert(k.clone(), serde_json::Value::String(v.clone()));
    }
    // Stamped now so it is newer than every rule; `bn on test` asks "does this rule match this
    // shape", not "would it have matched last week".
    let event = bn_core::events::Event {
        kind: kind.to_string(),
        ts: bn_core::util::now_epoch_secs(),
        fields: map,
    };
    let rules = reactor::read_rules().rules;
    let matched: Vec<&Rule> = rules.iter().filter(|r| reactor::matches(r, &event)).collect();
    if json {
        json_ok(
            "on-test",
            serde_json::json!({
                "event": serde_json::from_str::<serde_json::Value>(&reactor::event_json(&event))
                    .unwrap_or(serde_json::Value::Null),
                "matched": matched.iter().map(|r| rule_json(r)).collect::<Vec<_>>(),
            }),
        );
        return 0;
    }
    if matched.is_empty() {
        println!("No rule matches a '{kind}' event with those fields.");
        return 0;
    }
    for r in matched {
        println!("[{}] would run: {}", r.id, r.run.join(" "));
    }
    0
}

/// `reactor` — one cron-driven pass (`bn on tick`).
///
/// A failing reaction is the *rule's* failure, not the job's: the tick reports which fired and
/// how, and returning non-zero here would mark the whole cron unhealthy every time one user
/// script exits 1, burying the signal that the reactor itself broke.
pub(crate) fn job_reactor(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    tick(cfg, false, false, false);
    0
}
