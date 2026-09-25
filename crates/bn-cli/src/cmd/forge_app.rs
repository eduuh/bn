//! `bn forge login`, `bn forge status` and `bn forge sweep`.
//!
//! The Forge client and the credential rules live in [`bn_core::forge_app`]. This file is the
//! command surface: the paste path, the status report, and the hand-off to [`login`], which drives
//! the browser.

mod login;
mod playwright;
pub(crate) mod sweep;

use crate::*;
use bn_core::forge_app::{self, client::ForgeApi};
use std::io::{BufRead, IsTerminal, Read};

pub(crate) fn run(cfg: &Config, action: ForgeAppCmd, json: bool) -> i32 {
    match action {
        ForgeAppCmd::Login {
            paste: true,
            ..
        } => login_paste(json),
        ForgeAppCmd::Login {
            web_url,
            api_url,
            family,
            agent_name,
            browser,
            timeout,
            yes,
            ..
        } => {
            let opts = login::Options {
                web_url,
                api_url: api_url.unwrap_or_else(|| api_origin(&cfg.tasks_forge_url)),
                family,
                agent_name,
                key_name: format!("bn@{}", cfg.hostname),
                timeout: std::time::Duration::from_secs(timeout),
                yes,
            };
            let mut page = playwright::PlaywrightCli::new(&browser);
            let outcome = login::run(&mut page, &opts, &mut login::StdPrompt);
            page.finish();
            match outcome.and_then(|minted| save_key(&minted.key).map(|path| (minted, path))) {
                Ok((minted, path)) => {
                    report_saved(
                        json,
                        &path,
                        serde_json::json!({
                            "source": "browser",
                            "family": minted.family,
                            "agent": minted.agent,
                            "agent_created": minted.agent_created,
                        }),
                    );
                    0
                }
                Err(e) => report_error(json, "forge login", "login_failed", &e),
            }
        }
        ForgeAppCmd::Status => status(cfg, json),
        ForgeAppCmd::Sweep { max } => sweep::run(cfg, max, json),
        ForgeAppCmd::List { agent, task, limit } => {
            crate::cmd::forge::run(cfg, ForgeCmd::List { agent, task, limit }, json);
            0
        }
        ForgeAppCmd::Claim {
            agent,
            task,
            lease_seconds,
        } => {
            crate::cmd::forge::run(
                cfg,
                ForgeCmd::Claim {
                    agent,
                    task,
                    lease_seconds,
                },
                json,
            );
            0
        }
        ForgeAppCmd::Dispatch {
            agent,
            task,
            repo,
            agent_harness,
            base,
            skill,
            name,
            verify,
            lease_seconds,
        } => {
            crate::cmd::forge::run(
                cfg,
                ForgeCmd::Dispatch {
                    agent,
                    task,
                    repo,
                    agent_harness,
                    base,
                    skill,
                    name,
                    verify,
                    lease_seconds,
                },
                json,
            );
            0
        }
        ForgeAppCmd::Tick {
            agent,
            repo,
            agent_harness,
            base,
            skill,
            max_active,
            dry_run,
            lease_seconds,
        } => {
            crate::cmd::forge::run(
                cfg,
                ForgeCmd::Tick {
                    agent,
                    repo,
                    agent_harness,
                    base,
                    skill,
                    max_active,
                    dry_run,
                    lease_seconds,
                },
                json,
            );
            0
        }
    }
}

/// `https://api.wira360.com/mcp` -> `https://api.wira360.com`: the GraphQL API shares the MCP
/// endpoint's origin.
pub(crate) fn api_origin(forge_url: &str) -> String {
    let u = forge_url.trim().trim_end_matches('/');
    u.strip_suffix("/mcp").unwrap_or(u).to_string()
}

fn save_key(key: &str) -> Result<std::path::PathBuf, String> {
    if !forge_app::looks_like_key(key) {
        return Err("that is not a Forge family key (expected fam_...); nothing was saved".into());
    }
    forge_app::write_token(key).map_err(|e| format!("could not save the Forge key: {e}"))
}

fn report_saved(json: bool, path: &std::path::Path, mut data: serde_json::Value) {
    data["path"] = serde_json::json!(path.display().to_string());
    if json {
        println!("{}", Envelope::success("forge login", data).to_json());
    } else {
        println!("Saved the Forge key to {}", path.display());
        if let (Some(agent), Some(family)) = (data["agent"].as_str(), data["family"].as_str()) {
            println!("It acts as {agent} in {family}.");
        }
        if std::env::var_os(forge_app::TOKEN_ENV).is_some() {
            println!(
                "Note: {} is set and wins over the file.",
                forge_app::TOKEN_ENV
            );
        }
    }
}

fn report_error(json: bool, command: &str, code: &str, message: &str) -> i32 {
    if json {
        println!(
            "{}",
            Envelope::failure(command, code, message).to_json()
        );
    } else {
        eprintln!("bn {command}: {message}");
    }
    1
}

/// Read one key from stdin without echoing it: piped input is read whole, and a terminal has
/// echo switched off (`stty -echo`) for the one line.
fn login_paste(json: bool) -> i32 {
    let stdin = std::io::stdin();
    let key = if stdin.is_terminal() {
        eprint!("Paste the Forge key (fam_...), then press Enter: ");
        let echo_off = set_echo(false);
        let mut line = String::new();
        let read = stdin.lock().read_line(&mut line);
        if echo_off {
            set_echo(true);
        }
        eprintln!();
        match read {
            Ok(_) => line,
            Err(e) => return report_error(json, "forge login", "read_failed", &e.to_string()),
        }
    } else {
        let mut all = String::new();
        if let Err(e) = stdin.lock().read_to_string(&mut all) {
            return report_error(json, "forge login", "read_failed", &e.to_string());
        }
        all
    };
    match save_key(key.trim()) {
        Ok(path) => {
            report_saved(json, &path, serde_json::json!({ "source": "paste" }));
            0
        }
        Err(e) => report_error(json, "forge login", "bad_key", &e),
    }
}

/// Toggle terminal echo through `stty`, which reads the terminal from its inherited stdin. `true`
/// when the change took.
fn set_echo(on: bool) -> bool {
    if cfg!(windows) {
        return false;
    }
    std::process::Command::new("stty")
        .arg(if on { "echo" } else { "-echo" })
        .stdin(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// What `bn forge status` found, as data first so the human and JSON renderings agree.
#[derive(Debug, Default, serde::Serialize)]
struct StatusReport {
    provider: String,
    endpoint: String,
    token_source: Option<String>,
    session: Option<String>,
    session_error: Option<String>,
    projects: Vec<serde_json::Value>,
    goals_error: Option<String>,
    agents: Vec<String>,
    members_error: Option<String>,
    open_agent_tasks: Option<usize>,
    claimed_agent_tasks: Option<usize>,
    tasks_error: Option<String>,
}

fn status(cfg: &Config, json: bool) -> i32 {
    let mut report = StatusReport {
        provider: cfg.tasks_provider.label().to_string(),
        endpoint: cfg.tasks_forge_url.clone(),
        ..StatusReport::default()
    };
    let Some((token, source)) = forge_app::resolve_token() else {
        let msg = forge_app::missing_token_error().to_string();
        if json {
            println!("{}", Envelope::failure("forge status", "no_token", msg).to_json());
        } else {
            print_status(&report);
            eprintln!("bn forge status: {msg}");
        }
        return 1;
    };
    report.token_source = Some(source.describe());
    let api = forge_app::client::HttpForgeApi::new(cfg.tasks_forge_url.clone(), token);
    match api.mcp().initialize() {
        Ok(info) => {
            report.session = Some(format!(
                "{} {} (MCP {})",
                info.name, info.version, info.protocol_version
            ))
        }
        Err(e) => report.session_error = Some(e.to_string()),
    }
    if report.session.is_some() {
        fill_from_forge(&api, &mut report);
    }
    let ok = report.session.is_some();
    if json {
        let data = serde_json::to_value(&report).unwrap_or_default();
        let env = if ok {
            Envelope::success("forge status", data)
        } else {
            Envelope::failure(
                "forge status",
                "unreachable",
                report.session_error.clone().unwrap_or_default(),
            )
        };
        println!("{}", env.to_json());
    } else {
        print_status(&report);
    }
    i32::from(!ok)
}

fn fill_from_forge(api: &dyn ForgeApi, report: &mut StatusReport) {
    match api.get_goals() {
        Ok(goals) => {
            report.projects = goals
                .iter()
                .filter_map(|g| {
                    let url = g.repo_url.as_deref().filter(|u| !u.trim().is_empty())?;
                    Some(serde_json::json!({
                        "goal": g.name,
                        "repo_url": url,
                        "repo": forge_app::repo_slug(url),
                    }))
                })
                .collect()
        }
        Err(e) => report.goals_error = Some(e.to_string()),
    }
    match api.get_family_members() {
        Ok(members) => {
            report.agents = members
                .into_iter()
                .filter(|m| m.is_agent)
                .map(|m| {
                    if m.display_name.is_empty() {
                        m.user_id
                    } else {
                        m.display_name
                    }
                })
                .collect()
        }
        Err(e) => report.members_error = Some(e.to_string()),
    }
    match api.list_bn_tasks(Some(forge_app::store::COMPLETED_WINDOW_DAYS)) {
        Ok(rows) => {
            let open: Vec<_> = rows
                .iter()
                .filter(|r| {
                    !r.is_completed
                        && !r.status.eq_ignore_ascii_case("abandoned")
                        && r.assignees.iter().any(|a| a.is_agent)
                })
                .collect();
            report.claimed_agent_tasks = Some(
                open.iter()
                    .filter(|r| r.started_at.is_some() || r.agent_meta.contains_key("claimer"))
                    .count(),
            );
            report.open_agent_tasks = Some(open.len());
        }
        Err(e) => report.tasks_error = Some(e.to_string()),
    }
}

fn print_status(r: &StatusReport) {
    let dash = || "-".to_string();
    println!("provider   {}", r.provider);
    println!("endpoint   {}", r.endpoint);
    println!(
        "key        {}",
        r.token_source.clone().unwrap_or_else(|| "none".into())
    );
    if r.token_source.is_none() {
        return;
    }
    match (&r.session, &r.session_error) {
        (Some(s), _) => println!("session    ok: {s}"),
        (None, Some(e)) => println!("session    failed: {e}"),
        (None, None) => println!("session    {}", dash()),
    }
    if r.session.is_none() {
        return;
    }
    if let Some(e) = &r.goals_error {
        println!("projects   error: {e}");
    } else if r.projects.is_empty() {
        println!("projects   none (no goal has a repository)");
    } else {
        for (i, p) in r.projects.iter().enumerate() {
            let label = if i == 0 { "projects  " } else { "          " };
            println!(
                "{label} {} -> {}",
                p["goal"].as_str().unwrap_or_default(),
                p["repo"].as_str().or(p["repo_url"].as_str()).unwrap_or_default()
            );
        }
    }
    match (&r.members_error, r.agents.is_empty()) {
        (Some(e), _) => println!("agent      error: {e}"),
        (None, true) => println!("agent      none: add one with `bn forge login` or Forge Settings"),
        (None, false) => println!("agent      {}", r.agents.join(", ")),
    }
    if let Some(e) = &r.tasks_error {
        println!("tasks      error: {e}");
        return;
    }
    println!(
        "tasks      {} open for agents, {} claimed",
        r.open_agent_tasks.map_or_else(dash, |n| n.to_string()),
        r.claimed_agent_tasks.map_or_else(dash, |n| n.to_string())
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_origin_drops_the_mcp_path() {
        assert_eq!(
            api_origin("https://api.wira360.com/mcp"),
            "https://api.wira360.com"
        );
        assert_eq!(api_origin("http://localhost:5000/mcp/"), "http://localhost:5000");
        assert_eq!(api_origin("https://x.test"), "https://x.test");
    }

    #[test]
    fn a_non_key_is_refused_before_anything_is_written() {
        let err = save_key("hello").unwrap_err();
        assert!(err.contains("nothing was saved"), "{err}");
        assert!(!err.contains("hello"), "the input is never echoed");
    }
}
