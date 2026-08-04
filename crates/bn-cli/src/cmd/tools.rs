//! `bn tools` (#653) — thin CLI over the `bn_core::tools` managed-services subsystem. Declares,
//! deploys, reports health for, reconciles, and tears down docker-backed developer tools. Built-in
//! tools (pilot: n8n) are opt-in via `[tools].builtins` / `$BN_TOOLS_BUILTINS`; user `tools.toml`
//! entries are always active. All lifecycle actions run through the real [`CommandRunner`], so
//! the heavy logic stays unit-tested in bn-core.

use crate::*;
use bn_core::runner::RealRunner;
use bn_core::tools::{self, DeployOutcome, ToolSpec, ToolStatus};

/// Dispatch a `bn tools <sub>` command. Returns the process exit code.
pub(crate) fn emit_tools(action: ToolsCmd, json: bool) -> i32 {
    match action {
        ToolsCmd::List => list(json),
        ToolsCmd::Status { name } => status(name.as_deref(), json),
        ToolsCmd::Deploy { name } => deploy(&name, json),
        ToolsCmd::Reconcile { name } => reconcile(name.as_deref(), json),
        ToolsCmd::Down { name } => down(&name, json),
    }
}

/// Resolve one spec by name, or print an error and return `None`.
fn spec_or_report(name: &str, json: bool) -> Option<ToolSpec> {
    match tools::find_spec(name) {
        Some(spec) => Some(spec),
        None => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "ok": false, "error": "unknown_tool", "name": name })
                );
            } else {
                eprintln!("bn tools: unknown tool '{name}' (see `bn tools list`)");
            }
            None
        }
    }
}

fn list(json: bool) -> i32 {
    let specs = tools::all_specs();
    if json {
        println!("{}", serde_json::json!({ "tools": specs }));
        return 0;
    }
    if specs.is_empty() {
        println!("No tools declared.");
        return 0;
    }
    println!("Declared tools:");
    for s in &specs {
        let ports: Vec<String> = s
            .ports
            .iter()
            .map(|p| format!("{}:{}", p.host, p.container))
            .collect();
        let ports = if ports.is_empty() {
            String::new()
        } else {
            format!("  [{}]", ports.join(", "))
        };
        println!("  {:<12} {}{}", s.name, s.image, ports);
    }
    0
}

fn status_one(spec: &ToolSpec) -> Result<ToolStatus, String> {
    tools::status(&RealRunner, spec)
}

fn print_status_line(st: &ToolStatus) {
    println!(
        "  {:<12} {:<8} {}",
        st.name,
        st.state.label(),
        st.health.label()
    );
}

fn status(name: Option<&str>, json: bool) -> i32 {
    let specs = match name {
        Some(n) => match spec_or_report(n, json) {
            Some(s) => vec![s],
            None => return 1,
        },
        None => tools::all_specs(),
    };
    if specs.is_empty() {
        if json {
            println!("{}", serde_json::json!({ "status": [] }));
        } else {
            println!("No tools declared.");
        }
        return 0;
    }
    let mut statuses = Vec::new();
    let mut errors = Vec::new();
    for spec in &specs {
        match status_one(spec) {
            Ok(status) => statuses.push(status),
            Err(error) => errors.push(serde_json::json!({
                "name": spec.name,
                "error": error,
            })),
        }
    }
    if json {
        println!(
            "{}",
            serde_json::json!({ "status": statuses, "errors": errors })
        );
        return i32::from(!errors.is_empty());
    }
    if !statuses.is_empty() {
        println!("{:<14} {:<8} HEALTH", "TOOL", "STATE");
        for st in &statuses {
            print_status_line(st);
        }
    }
    for error in &errors {
        eprintln!(
            "bn tools: failed to inspect '{}': {}",
            error["name"].as_str().unwrap_or("unknown"),
            error["error"].as_str().unwrap_or("command failed")
        );
    }
    i32::from(!errors.is_empty())
}

fn deploy(name: &str, json: bool) -> i32 {
    let Some(spec) = spec_or_report(name, json) else {
        return 1;
    };
    report_deploy(&spec, tools::deploy(&RealRunner, &spec), json)
}

fn reconcile(name: Option<&str>, json: bool) -> i32 {
    let specs = match name {
        Some(n) => match spec_or_report(n, json) {
            Some(s) => vec![s],
            None => return 1,
        },
        None => tools::all_specs(),
    };
    let mut rc = 0;
    let mut results = Vec::new();
    for spec in &specs {
        let outcome = tools::reconcile(&RealRunner, spec);
        if matches!(outcome, DeployOutcome::Failed(_)) {
            rc = 1;
        }
        if json {
            results.push(deploy_json(spec, &outcome));
        } else {
            println!("{}: {}", spec.name, outcome_label(&outcome));
        }
    }
    if json {
        println!("{}", serde_json::json!({ "reconcile": results }));
    }
    rc
}

fn down(name: &str, json: bool) -> i32 {
    let Some(spec) = spec_or_report(name, json) else {
        return 1;
    };
    match tools::down(&RealRunner, &spec) {
        Ok(()) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "ok": true, "name": spec.name, "action": "down" })
                );
            } else {
                println!("{}: down", spec.name);
            }
            0
        }
        Err(e) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "ok": false, "name": spec.name, "error": e })
                );
            } else {
                eprintln!("bn tools: failed to tear down '{}': {e}", spec.name);
            }
            1
        }
    }
}

fn outcome_label(outcome: &DeployOutcome) -> String {
    match outcome {
        DeployOutcome::Created => "created".to_string(),
        DeployOutcome::Started => "started".to_string(),
        DeployOutcome::AlreadyRunning => "already running".to_string(),
        DeployOutcome::Failed(e) => format!("FAILED — {e}"),
    }
}

fn deploy_json(spec: &ToolSpec, outcome: &DeployOutcome) -> serde_json::Value {
    match outcome {
        DeployOutcome::Failed(e) => {
            serde_json::json!({ "name": spec.name, "ok": false, "error": e })
        }
        other => serde_json::json!({
            "name": spec.name,
            "ok": true,
            "outcome": outcome_label(other),
        }),
    }
}

fn report_deploy(spec: &ToolSpec, outcome: DeployOutcome, json: bool) -> i32 {
    let ok = !matches!(outcome, DeployOutcome::Failed(_));
    if json {
        println!("{}", deploy_json(spec, &outcome));
    } else if ok {
        println!("{}: {}", spec.name, outcome_label(&outcome));
    } else {
        eprintln!("bn tools: {} deploy {}", spec.name, outcome_label(&outcome));
    }
    i32::from(!ok)
}


