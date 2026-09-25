//! The browser half of `bn forge login`, as decisions over a [`Page`].
//!
//! Everything the page does is one GraphQL request made from inside it with `fetch`, so the
//! signed-in session's cookie (plus the `forge-csrf` double-submit header forge-web itself sends)
//! authorises it and bn never sees a password or a session token. What to ask for, and what to
//! make of the answers, is decided here in Rust against the [`Page`] trait, so the flow is tested
//! with a scripted fake and the browser driver ([`super::playwright`]) stays a thin, swappable
//! shell-out.

use serde_json::{json, Value};
use std::io::{BufRead, IsTerminal, Write};
use std::time::{Duration, Instant};

/// A browser tab bn can steer: open a URL and evaluate a JavaScript function in the page.
pub(crate) trait Page {
    fn open(&mut self, url: &str) -> Result<(), String>;
    /// Evaluate `js` (a function expression) in the page and return what it printed.
    fn eval(&mut self, js: &str) -> Result<String, String>;
    /// Wait between sign-in probes. A method so tests do not sleep.
    fn pause(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// How a yes/no question reaches the person.
pub(crate) trait Prompt {
    fn confirm(&mut self, question: &str) -> Option<bool>;
}

/// Ask on stderr and read stdin; `None` when stdin is not a terminal.
pub(crate) struct StdPrompt;

impl Prompt for StdPrompt {
    fn confirm(&mut self, question: &str) -> Option<bool> {
        let stdin = std::io::stdin();
        if !stdin.is_terminal() {
            return None;
        }
        eprint!("{question} [y/N] ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        stdin.lock().read_line(&mut line).ok()?;
        Some(matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
    }
}

pub(crate) struct Options {
    pub web_url: String,
    /// The API origin; GraphQL is `<api_url>/graphql/forge`.
    pub api_url: String,
    pub family: Option<String>,
    pub agent_name: String,
    /// The name the minted key carries in Forge, so a person can tell machines apart and revoke one.
    pub key_name: String,
    pub timeout: Duration,
    pub yes: bool,
}

/// A minted key and what it acts as. `Debug` is deliberately not derived: the key must never end
/// up in a `{:?}`.
pub(crate) struct Minted {
    pub key: String,
    pub family: String,
    pub agent: String,
    pub agent_created: bool,
}

const PROBE_EVERY: Duration = Duration::from_secs(3);

const ME: &str = "query BnForgeLoginMe { me { id email } }";
const FAMILIES: &str = "query BnForgeLoginFamilies { families { id uuid name isPersonal members { id userId role isAgent user { displayName } } } }";
const ADD_AGENT: &str = "mutation BnForgeLoginAddAgent($input: AddAgentMemberInput!) { addAgentMember(input: $input) { member { id isAgent user { displayName } } errors { message code } } }";
const CREATE_KEY: &str = "mutation BnForgeLoginCreateKey($input: CreateAgentApiKeyInput!) { createAgentApiKey(input: $input) { key errors { message code } } }";

/// Open Forge, wait for sign-in, and mint a key for the agent member.
pub(crate) fn run(
    page: &mut dyn Page,
    opts: &Options,
    prompt: &mut dyn Prompt,
) -> Result<Minted, String> {
    let endpoint = format!("{}/graphql/forge", opts.api_url.trim_end_matches('/'));
    page.open(&opts.web_url)?;
    wait_for_sign_in(page, &endpoint, opts)?;

    let families = graphql(page, &endpoint, FAMILIES, json!({}))?;
    let me = graphql(page, &endpoint, ME, json!({}))?;
    let me_id = me["me"]["id"].as_str().unwrap_or_default().to_string();
    let family = choose_family(families["families"].as_array().map_or(&[], Vec::as_slice), &me_id, opts.family.as_deref())?;
    let family_name = family["name"].as_str().unwrap_or_default().to_string();

    let (agent_id, agent_name, agent_created) = match agent_member(family) {
        Some((id, name)) => (id, name, false),
        None => {
            let question = format!(
                "Family \"{family_name}\" has no agent member. Create one named \"{}\"?",
                opts.agent_name
            );
            let go = opts.yes || prompt.confirm(&question).ok_or_else(|| {
                format!("family \"{family_name}\" has no agent member; rerun with --yes to create \"{}\"", opts.agent_name)
            })?;
            if !go {
                return Err("no agent member, and creating one was declined".into());
            }
            let input = json!({ "input": { "familyId": family["id"], "name": opts.agent_name } });
            let data = graphql(page, &endpoint, ADD_AGENT, input)?;
            let payload = &data["addAgentMember"];
            payload_errors("addAgentMember", payload)?;
            let id = payload["member"]["id"]
                .as_str()
                .ok_or("addAgentMember returned no member")?
                .to_string();
            (id, opts.agent_name.clone(), true)
        }
    };

    let input = json!({ "input": { "agentMemberId": agent_id, "name": opts.key_name } });
    let data = graphql(page, &endpoint, CREATE_KEY, input)?;
    let payload = &data["createAgentApiKey"];
    payload_errors("createAgentApiKey", payload)?;
    let key = payload["key"]
        .as_str()
        .filter(|k| !k.is_empty())
        .ok_or("createAgentApiKey returned no key")?
        .to_string();
    Ok(Minted {
        key,
        family: family_name,
        agent: agent_name,
        agent_created,
    })
}

fn wait_for_sign_in(page: &mut dyn Page, endpoint: &str, opts: &Options) -> Result<(), String> {
    let started = Instant::now();
    let mut told = false;
    loop {
        let reply = request(page, endpoint, ME, &json!({}))?;
        if reply["body"]["data"]["me"]["id"].as_str().is_some() {
            return Ok(());
        }
        if started.elapsed() >= opts.timeout {
            return Err(format!(
                "not signed in to Forge after {}s; sign in in the browser window and run `bn forge login` again, or use `bn forge login --paste`",
                opts.timeout.as_secs()
            ));
        }
        if !told {
            eprintln!(
                "Sign in to Forge in the browser window bn opened. Waiting up to {}s...",
                opts.timeout.as_secs()
            );
            told = true;
        }
        page.pause(PROBE_EVERY);
    }
}

/// One GraphQL call's `data`, or the reason there is none.
fn graphql(page: &mut dyn Page, endpoint: &str, query: &str, vars: Value) -> Result<Value, String> {
    let reply = request(page, endpoint, query, &vars)?;
    let status = reply["status"].as_u64().unwrap_or(0);
    if let Some(e) = reply["error"].as_str() {
        return Err(format!("Forge GraphQL request failed in the page: {e}"));
    }
    if let Some(msg) = reply["body"]["errors"][0]["message"].as_str() {
        return Err(format!("Forge GraphQL: {msg}"));
    }
    if !(200..300).contains(&status) {
        return Err(format!("Forge GraphQL: HTTP {status}"));
    }
    match &reply["body"]["data"] {
        Value::Null => Err("Forge GraphQL: no data in the reply".into()),
        data => Ok(data.clone()),
    }
}

/// Run the in-page fetch and decode `{status, body}` or `{status: 0, error}`.
fn request(page: &mut dyn Page, endpoint: &str, query: &str, vars: &Value) -> Result<Value, String> {
    let js = fetch_js(endpoint, &json!({ "query": query, "variables": vars }).to_string());
    parse_eval_output(&page.eval(&js)?)
}

/// The in-page function: POST `body` to `endpoint` with the session cookie and the CSRF header,
/// and hand back the status and parsed body as a JSON string. Both values are embedded as JSON
/// string literals, which are valid JavaScript.
pub(crate) fn fetch_js(endpoint: &str, body: &str) -> String {
    let endpoint = Value::String(endpoint.to_string()).to_string();
    let body = Value::String(body.to_string()).to_string();
    format!(
        "async () => {{ const m = document.cookie.match(/(?:^|;\\s*)forge-csrf=([^;]+)/); \
         const headers = {{ 'Content-Type': 'application/json' }}; \
         if (m) headers['X-CSRF-Token'] = decodeURIComponent(m[1]); \
         try {{ const r = await fetch({endpoint}, {{ method: 'POST', credentials: 'include', headers, body: {body} }}); \
         const text = await r.text(); let parsed = null; try {{ parsed = JSON.parse(text); }} catch (e) {{}} \
         return JSON.stringify({{ status: r.status, body: parsed }}); \
         }} catch (e) {{ return JSON.stringify({{ status: 0, error: String(e) }}); }} }}"
    )
}

/// What `eval` printed, as JSON: a JSON value, or a JSON string holding one.
pub(crate) fn parse_eval_output(out: &str) -> Result<Value, String> {
    let trimmed = out.trim();
    let first: Value = serde_json::from_str(trimmed)
        .map_err(|_| "the browser returned something that is not JSON".to_string())?;
    match first {
        Value::String(inner) => serde_json::from_str(&inner)
            .map_err(|_| "the browser returned something that is not JSON".to_string()),
        v => Ok(v),
    }
}

fn payload_errors(what: &str, payload: &Value) -> Result<(), String> {
    match payload["errors"][0]["message"].as_str() {
        Some(msg) => Err(format!("{what}: {msg}")),
        None if payload.is_null() => Err(format!("{what}: no payload")),
        None => Ok(()),
    }
}

fn is_admin(family: &Value, me_id: &str) -> bool {
    family["members"].as_array().is_some_and(|ms| {
        ms.iter().any(|m| {
            m["userId"]
                .as_str()
                .is_some_and(|u| !me_id.is_empty() && u.eq_ignore_ascii_case(me_id))
                && matches!(m["role"].as_str(), Some("owner" | "admin"))
        })
    })
}

fn agent_member(family: &Value) -> Option<(String, String)> {
    family["members"].as_array()?.iter().find_map(|m| {
        (m["isAgent"].as_bool() == Some(true)).then(|| {
            let id = m["id"].as_str().unwrap_or_default().to_string();
            let name = m["user"]["displayName"]
                .as_str()
                .unwrap_or("agent")
                .to_string();
            (id, name)
        })
    })
}

/// The family to mint in: the one named by `--family`, else the only family you administer that
/// has an agent member, else the only family you administer.
pub(crate) fn choose_family<'a>(
    families: &'a [Value],
    me_id: &str,
    wanted: Option<&str>,
) -> Result<&'a Value, String> {
    let admin: Vec<&Value> = families.iter().filter(|f| is_admin(f, me_id)).collect();
    let names = |fs: &[&Value]| {
        fs.iter()
            .filter_map(|f| f["name"].as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if let Some(want) = wanted {
        return admin
            .iter()
            .copied()
            .find(|f| f["name"].as_str().is_some_and(|n| n.trim().eq_ignore_ascii_case(want.trim())))
            .ok_or_else(|| format!("you do not administer a family named \"{want}\" (you administer: {})", names(&admin)));
    }
    let with_agent: Vec<&Value> = admin.iter().copied().filter(|f| agent_member(f).is_some()).collect();
    match (with_agent.as_slice(), admin.as_slice()) {
        ([only], _) => Ok(only),
        ([], [only]) => Ok(only),
        (_, []) => Err("you are not an admin of any Forge family, and minting an agent key needs one".into()),
        (_, many) => Err(format!("you administer several families; pick one with --family ({})", names(many))),
    }
}

#[cfg(test)]
mod tests;
