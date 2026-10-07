//! Watchdog tools on the scoped server. The target seat is always this server's own
//! `repo:branch`.
//!
//! Each tool runs `bn --json …`, so the CLI's rules apply exactly as they do from a terminal —
//! one code path, not two.

use super::BnServer;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckinArgs {
    /// Minutes from now.
    mins: u32,
    /// What to tell yourself then.
    note: String,
}

/// `--name=value` as one argv word, so a value that starts with `-` is never read as a flag.
fn flag(name: &str, value: &str) -> String {
    format!("--{name}={value}")
}

/// Run `bn --json <args>` and hand back its envelope's `data`, or the CLI's own error message.
fn run<S: AsRef<str>>(args: &[S]) -> Result<CallToolResult, ErrorData> {
    let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    let out = bn_core::bn_subprocess::run(&args, None)
        .map_err(|e| ErrorData::internal_error(format!("failed to run bn: {e}"), None))?;
    let env = out.json.unwrap_or(Value::Null);
    if !out.success || env.get("ok") == Some(&Value::Bool(false)) {
        let msg = env
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| out.stderr.trim().to_string());
        return Err(ErrorData::invalid_params(msg, None));
    }
    Ok(CallToolResult::structured(
        env.get("data").cloned().unwrap_or(Value::Null),
    ))
}

#[tool_router(router = watchdog_router, vis = "pub(super)")]
impl BnServer {
    #[tool(
        name = "bn_watchdog_checkin",
        description = "Schedule a check-in to yourself: in `mins` minutes, `note` is typed into this branch's agent pane. Durable (survives a reboot) — use it instead of promising to come back later."
    )]
    async fn bn_watchdog_checkin(
        &self,
        Parameters(a): Parameters<CheckinArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let r = self.resolve()?;
        let seat = format!("{}:{}", r.repo, r.branch);
        run(&[
            "agent".to_string(),
            "checkin".into(),
            flag("target", &seat),
            "--".into(),
            a.mins.to_string(),
            a.note,
        ])
    }
}
