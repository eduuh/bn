//! Queue and watchdog tools on the scoped server. The acting seat is always this server's own
//! `repo:branch`: a tool can open a row *for* another seat (`bn_queue_create`, a handoff's `to`),
//! but can only claim, close, park or annotate rows addressed to or owned by this seat.
//!
//! Each tool runs `bn --json queue …` with `--as <this seat>`, so the CLI's rules, note mirroring
//! and pane nudges apply exactly as they do from a terminal — one code path, not two.

use super::BnServer;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct QueueListArgs {
    /// Comma-separated states (pending, in-progress, blocked, done, failed, denied, canceled,
    /// handed-off), or "any". Default: every open state.
    #[serde(default)]
    state: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct QueueIdArgs {
    /// The row id, e.g. "q12".
    id: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct QueueCreateArgs {
    /// Who the work is for: "repo:branch" for an agent, "human:<name>" for a person.
    to: String,
    /// What needs doing.
    body: String,
    /// fast (30m) | routine (4h, default) | deep (24h) | critical (1h).
    #[serde(default)]
    tier: Option<String>,
    /// A link that goes with the work.
    #[serde(default, rename = "ref")]
    ref_: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct QueueHandoffArgs {
    /// The row to hand on.
    id: String,
    /// The seat that takes it next.
    to: String,
    /// What they are getting and why.
    note: String,
    /// True when your part is finished: close yours as done rather than handed-off.
    #[serde(default)]
    complete: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct QueueCloseArgs {
    id: String,
    /// done | failed | denied | canceled.
    outcome: String,
    /// Where the work went, or why it stopped. Required.
    note: String,
    #[serde(default, rename = "ref")]
    ref_: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct QueueBlockArgs {
    id: String,
    /// What it waits on: a seat, a row, or a few words.
    on: String,
    /// Wake it after this long ("30m", "2h").
    #[serde(default)]
    wake_after: Option<String>,
    /// For on="human:<name>": what they need to decide, in plain language.
    #[serde(default)]
    summary: Option<String>,
    /// For on="human:<name>": a link or path they can judge by.
    #[serde(default)]
    evidence: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct QueueNoteArgs {
    id: String,
    note: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckinArgs {
    /// Minutes from now.
    mins: u32,
    /// What to tell yourself then.
    note: String,
}

/// `--name=value` as one argv word, so a value that starts with `-` (a note like "- fixed it")
/// is never read as a flag.
fn flag(name: &str, value: &str) -> String {
    format!("--{name}={value}")
}

/// Run `bn --json <args>` and hand back its envelope's `data`, or the CLI's own error message.
fn run<S: AsRef<str>>(args: &[S]) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::structured(run_data(args)?))
}

fn run_data<S: AsRef<str>>(args: &[S]) -> Result<Value, ErrorData> {
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
    Ok(env.get("data").cloned().unwrap_or(Value::Null))
}

#[tool_router(router = queue_router, vis = "pub(super)")]
impl BnServer {
    /// This server's seat, `repo:branch`.
    fn seat(&self) -> Result<String, ErrorData> {
        let r = self.resolve()?;
        Ok(format!("{}:{}", r.repo, r.branch))
    }

    #[tool(
        name = "bn_queue_list",
        description = "Queue rows addressed to or owned by this branch's seat (repo:branch). A row is durable work handed between agents: it has an owner once claimed, a deadline, and a history. Default: every open row."
    )]
    async fn bn_queue_list(
        &self,
        Parameters(a): Parameters<QueueListArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let seat = self.seat()?;
        let mut args = vec!["queue".to_string(), "list".into(), flag("seat", &seat)];
        if let Some(s) = a.state.as_deref() {
            args.push(flag("state", s));
        }
        run(&args)
    }

    #[tool(
        name = "bn_queue_show",
        description = "One queue row in full, with its history."
    )]
    async fn bn_queue_show(
        &self,
        Parameters(a): Parameters<QueueIdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut row = run_data(&["queue", "show", &a.id])?;
        let history = run_data(&["queue", "transitions", &a.id])?;
        row["transitions"] = history.get("transitions").cloned().unwrap_or(Value::Null);
        Ok(CallToolResult::structured(row))
    }

    #[tool(
        name = "bn_queue_create",
        description = "Ask another seat for work: open a queue row for `to` (repo:branch or human:<name>). Unlike a message, the row is durable, owned once claimed, and cannot be closed without a note. The destination agent gets a [qN] todo in its note and a one-line nudge if it is live; a person gets an attention item."
    )]
    async fn bn_queue_create(
        &self,
        Parameters(a): Parameters<QueueCreateArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let seat = self.seat()?;
        let mut args = vec!["queue".to_string(), "create".into(), flag("as", &seat)];
        if let Some(t) = a.tier.as_deref() {
            args.push(flag("tier", t));
        }
        if let Some(r) = a.ref_.as_deref() {
            args.push(flag("ref", r));
        }
        // Positionals after `--`, so a body that starts with `-` stays a body.
        args.extend(["--".into(), a.to.clone(), a.body.clone()]);
        run(&args)
    }

    #[tool(
        name = "bn_queue_claim",
        description = "Claim a pending row addressed to this seat: pending → in-progress, with a deadline from its tier. Claim before you start, so the sender can see it was picked up."
    )]
    async fn bn_queue_claim(
        &self,
        Parameters(a): Parameters<QueueIdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let seat = self.seat()?;
        run(&[
            "queue".to_string(),
            "claim".into(),
            flag("as", &seat),
            "--".into(),
            a.id,
        ])
    }

    #[tool(
        name = "bn_queue_handoff",
        description = "Hand a row to the next seat: closes yours and opens theirs in one step, so nothing strands between seats. Closure records delivery, not acceptance — the next seat judges its own row. Set complete=true when your part is finished."
    )]
    async fn bn_queue_handoff(
        &self,
        Parameters(a): Parameters<QueueHandoffArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let seat = self.seat()?;
        let mut args = vec![
            "queue".to_string(),
            "handoff".into(),
            flag("to", &a.to),
            flag("note", &a.note),
            flag("as", &seat),
        ];
        if a.complete {
            args.push("--complete".into());
        }
        args.extend(["--".into(), a.id.clone()]);
        run(&args)
    }

    #[tool(
        name = "bn_queue_close",
        description = "Close a row as done, failed, denied or canceled. The note is required and must say where the work went or why it stopped — a seat cannot close work silently."
    )]
    async fn bn_queue_close(
        &self,
        Parameters(a): Parameters<QueueCloseArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let seat = self.seat()?;
        let verb = match a.outcome.as_str() {
            "done" => "done",
            "failed" | "fail" => "fail",
            "denied" | "deny" => "deny",
            "canceled" | "cancelled" | "cancel" => "cancel",
            other => {
                return Err(ErrorData::invalid_params(
                    format!("outcome '{other}' — use done, failed, denied or canceled"),
                    None,
                ))
            }
        };
        let mut args = vec![
            "queue".to_string(),
            verb.into(),
            flag("note", &a.note),
            flag("as", &seat),
        ];
        if let Some(r) = a.ref_.as_deref() {
            args.push(flag("ref", r));
        }
        args.extend(["--".into(), a.id.clone()]);
        run(&args)
    }

    #[tool(
        name = "bn_queue_block",
        description = "Park a row you hold, with what will wake it: wake_after (a timer), or for on=\"human:<name>\" a plain-language summary plus an evidence link the person can judge. A park with no wake trigger is refused — sitting on blocked work silently looks like a crash."
    )]
    async fn bn_queue_block(
        &self,
        Parameters(a): Parameters<QueueBlockArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let seat = self.seat()?;
        let mut args = vec![
            "queue".to_string(),
            "block".into(),
            flag("on", &a.on),
            flag("as", &seat),
        ];
        if let Some(w) = a.wake_after.as_deref() {
            args.push(flag("wake-after", w));
        }
        if let Some(s) = a.summary.as_deref() {
            args.push(flag("summary", s));
        }
        if let Some(e) = a.evidence.as_deref() {
            args.push(flag("evidence", e));
        }
        args.extend(["--".into(), a.id.clone()]);
        run(&args)
    }

    #[tool(
        name = "bn_queue_update",
        description = "Add a progress note to a row's history without changing its state."
    )]
    async fn bn_queue_update(
        &self,
        Parameters(a): Parameters<QueueNoteArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let seat = self.seat()?;
        run(&[
            "queue".to_string(),
            "update".into(),
            flag("note", &a.note),
            flag("as", &seat),
            "--".into(),
            a.id,
        ])
    }

    #[tool(
        name = "bn_watchdog_checkin",
        description = "Schedule a check-in to yourself: in `mins` minutes, `note` is typed into this branch's agent pane. Durable (survives a reboot) — use it instead of promising to come back later."
    )]
    async fn bn_watchdog_checkin(
        &self,
        Parameters(a): Parameters<CheckinArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let seat = self.seat()?;
        let mins = a.mins.to_string();
        run(&[
            "agent".to_string(),
            "checkin".into(),
            flag("target", &seat),
            "--".into(),
            mins,
            a.note,
        ])
    }
}

/// The `queue` block `bn_context` carries: this seat's open rows by state, and the pending ids to
/// claim. `None` when the seat has none (or the DB can't be read), so a quiet branch stays quiet.
pub(super) fn summary(ctx: &bn_core::resolver::Resolved) -> Option<Value> {
    use bn_core::coord::{CoordDb, ListFilter, QState};
    let seat = bn_core::coord::normalize_seat(&format!("{}:{}", ctx.repo, ctx.branch)).ok()?;
    let rows = CoordDb::open_default()
        .ok()?
        .list(&ListFilter {
            seat: Some(seat),
            ..Default::default()
        })
        .ok()?;
    if rows.is_empty() {
        return None;
    }
    let now = bn_core::util::now_epoch_secs();
    let count = |s: QState| rows.iter().filter(|r| r.qstate() == s).count();
    let pending: Vec<&str> = rows
        .iter()
        .filter(|r| r.qstate() == QState::Pending)
        .map(|r| r.id.as_str())
        .collect();
    let overdue: Vec<&str> = rows
        .iter()
        .filter(|r| r.is_overdue(now))
        .map(|r| r.id.as_str())
        .collect();
    Some(serde_json::json!({
        "pending": count(QState::Pending),
        "in_progress": count(QState::InProgress),
        "blocked": count(QState::Blocked),
        "overdue": overdue,
        "to_claim": pending,
        "hint": if pending.is_empty() {
            "bn_queue_list for detail"
        } else {
            "rows are waiting for this seat: bn_queue_claim before starting"
        },
    }))
}
