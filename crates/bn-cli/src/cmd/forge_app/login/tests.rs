use super::*;
use std::collections::VecDeque;

/// A page that answers each eval from a script, keyed by the operation name in the request.
#[derive(Default)]
struct FakePage {
    opened: Vec<String>,
    /// (operation name, reply JSON the in-page function would return)
    replies: VecDeque<(&'static str, Value)>,
    requests: Vec<Value>,
    pauses: usize,
}

impl FakePage {
    fn reply(mut self, op: &'static str, v: Value) -> FakePage {
        self.replies.push_back((op, v));
        self
    }
}

impl Page for FakePage {
    fn open(&mut self, url: &str) -> Result<(), String> {
        self.opened.push(url.to_string());
        Ok(())
    }

    fn eval(&mut self, js: &str) -> Result<String, String> {
        // Pull the embedded body literal back out: `body: "<json string>" }`.
        let start = js.find("body: \"").ok_or("no body")? + "body: ".len();
        let end = js[start..].find(" })").ok_or("no end")? + start;
        let body: String = serde_json::from_str(&js[start..end]).map_err(|e| e.to_string())?;
        let request: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        let query = request["query"].as_str().unwrap_or_default().to_string();
        self.requests.push(request);
        let (op, reply) = self.replies.pop_front().ok_or("unscripted request")?;
        if !query.contains(op) {
            return Err(format!("expected {op}, got {query}"));
        }
        // playwright-cli --raw prints a returned string as a JSON string literal.
        Ok(Value::String(reply.to_string()).to_string())
    }

    fn pause(&mut self, _d: Duration) {
        self.pauses += 1;
    }
}

struct Answer(Option<bool>, usize);

impl Prompt for Answer {
    fn confirm(&mut self, _q: &str) -> Option<bool> {
        self.1 += 1;
        self.0
    }
}

fn opts(yes: bool) -> Options {
    Options {
        web_url: "https://forge.test".into(),
        api_url: "https://api.test/".into(),
        family: None,
        agent_name: "Agents".into(),
        key_name: "bn@mac".into(),
        timeout: Duration::from_secs(60),
        yes,
    }
}

/// `unwrap_err` without asking `Minted` for `Debug`, which it deliberately lacks.
fn expect_err(r: Result<Minted, String>) -> String {
    match r {
        Ok(_) => panic!("expected the login to fail"),
        Err(e) => e,
    }
}

fn ok(data: Value) -> Value {
    json!({ "status": 200, "body": { "data": data } })
}

fn signed_out() -> Value {
    json!({ "status": 200, "body": { "errors": [{ "message": "The current user is not authorized" }] } })
}

fn member(user: &str, role: &str, agent: bool, id: &str) -> Value {
    json!({ "id": id, "userId": user, "role": role, "isAgent": agent, "user": { "displayName": if agent { "Agents" } else { "Edd" } } })
}

fn family(id: &str, name: &str, members: Vec<Value>) -> Value {
    json!({ "id": id, "uuid": format!("uuid-{id}"), "name": name, "isPersonal": false, "members": members })
}

fn me() -> Value {
    ok(json!({ "me": { "id": "U-1", "email": "e@x" } }))
}

#[test]
fn waits_for_sign_in_then_mints_for_the_existing_agent() {
    let fam = family("F1", "Home", vec![member("u-1", "owner", false, "m1"), member("u-a", "member", true, "m-agent")]);
    let mut page = FakePage::default()
        .reply("BnForgeLoginMe", signed_out())
        .reply("BnForgeLoginMe", json!({ "status": 401, "body": null }))
        .reply("BnForgeLoginMe", me())
        .reply("BnForgeLoginFamilies", ok(json!({ "families": [fam] })))
        .reply("BnForgeLoginMe", me())
        .reply(
            "BnForgeLoginCreateKey",
            ok(json!({ "createAgentApiKey": { "key": "fam_minted_123", "errors": null } })),
        );
    let mut prompt = Answer(None, 0);
    let minted = run(&mut page, &opts(false), &mut prompt).expect("login");
    assert_eq!(minted.key, "fam_minted_123");
    assert_eq!(minted.family, "Home");
    assert_eq!(minted.agent, "Agents");
    assert!(!minted.agent_created);
    assert_eq!(page.opened, ["https://forge.test"]);
    assert_eq!(page.pauses, 2);
    assert_eq!(prompt.1, 0, "no question when the agent exists");
    let last = page.requests.last().unwrap();
    assert_eq!(
        last["variables"],
        json!({ "input": { "agentMemberId": "m-agent", "name": "bn@mac" } })
    );
}

#[test]
fn creates_the_agent_only_with_consent() {
    let fam = family("F1", "Home", vec![member("U-1", "admin", false, "m1")]);
    let script = || {
        FakePage::default()
            .reply("BnForgeLoginMe", me())
            .reply("BnForgeLoginFamilies", ok(json!({ "families": [fam.clone()] })))
            .reply("BnForgeLoginMe", me())
    };

    let mut page = script();
    let err = expect_err(run(&mut page, &opts(false), &mut Answer(None, 0)));
    assert!(err.contains("--yes"), "{err}");

    let mut page = script();
    let err = expect_err(run(&mut page, &opts(false), &mut Answer(Some(false), 0)));
    assert!(err.contains("declined"), "{err}");

    let mut page = script()
        .reply(
            "BnForgeLoginAddAgent",
            ok(json!({ "addAgentMember": { "member": { "id": "m-new", "isAgent": true }, "errors": null } })),
        )
        .reply(
            "BnForgeLoginCreateKey",
            ok(json!({ "createAgentApiKey": { "key": "fam_k", "errors": null } })),
        );
    let minted = run(&mut page, &opts(true), &mut Answer(None, 0)).expect("login");
    assert!(minted.agent_created);
    assert_eq!(page.requests[3]["variables"], json!({ "input": { "familyId": "F1", "name": "Agents" } }));
    assert_eq!(page.requests[4]["variables"]["input"]["agentMemberId"], "m-new");
}

#[test]
fn mutation_errors_surface_and_no_key_is_returned() {
    let fam = family("F1", "Home", vec![member("U-1", "owner", false, "m1"), member("a", "member", true, "m-a")]);
    let mut page = FakePage::default()
        .reply("BnForgeLoginMe", me())
        .reply("BnForgeLoginFamilies", ok(json!({ "families": [fam] })))
        .reply("BnForgeLoginMe", me())
        .reply(
            "BnForgeLoginCreateKey",
            ok(json!({ "createAgentApiKey": { "key": null, "errors": [{ "message": "Agent member not found, or admin role required", "code": "FORBIDDEN" }] } })),
        );
    let err = expect_err(run(&mut page, &opts(false), &mut Answer(None, 0)));
    assert!(err.contains("admin role required"), "{err}");
}

#[test]
fn sign_in_times_out_with_a_way_forward() {
    let mut page = FakePage::default().reply("BnForgeLoginMe", signed_out());
    let mut o = opts(false);
    o.timeout = Duration::ZERO;
    let err = expect_err(run(&mut page, &o, &mut Answer(None, 0)));
    assert!(err.contains("--paste"), "{err}");
}

#[test]
fn family_choice_prefers_the_named_then_the_one_with_an_agent_then_the_only_admin() {
    let home = family("F1", "Home", vec![member("u", "owner", false, "1"), member("a", "member", true, "2")]);
    let work = family("F2", "Work", vec![member("u", "admin", false, "3")]);
    let other = family("F3", "Other", vec![member("u", "member", false, "4")]);
    let all = vec![home, work.clone(), other.clone()];
    let pick = |fams: &[Value], want: Option<&str>| choose_family(fams, "U", want).map(|f| f["id"].as_str().unwrap().to_string());

    assert_eq!(pick(&all, None).unwrap(), "F1");
    assert_eq!(pick(&all, Some("work")).unwrap(), "F2");
    assert!(pick(&all, Some("Other")).unwrap_err().contains("do not administer"));
    assert_eq!(pick(&[work.clone(), other.clone()], None).unwrap(), "F2");
    let no_agent_two = family("F4", "Two", vec![member("u", "owner", false, "5")]);
    assert!(pick(&[work, no_agent_two], None).unwrap_err().contains("--family"));
    assert!(pick(&[other], None).unwrap_err().contains("not an admin"));
}

#[test]
fn eval_output_accepts_a_value_or_a_string_holding_one() {
    assert_eq!(parse_eval_output("{\"a\":1}\n").unwrap(), json!({"a": 1}));
    assert_eq!(parse_eval_output("\"{\\\"a\\\":1}\"").unwrap(), json!({"a": 1}));
    assert!(parse_eval_output("TypeError: boom").is_err());
}

#[test]
fn the_page_function_embeds_its_inputs_as_string_literals() {
    let js = fetch_js("https://api.test/graphql/forge", r#"{"query":"q \"x\" </script>"}"#);
    assert!(js.starts_with("async () =>"));
    assert!(js.contains(r#"fetch("https://api.test/graphql/forge""#));
    assert!(js.contains(r#"body: "{\"query\":\"q \\\"x\\\" </script>\"}""#), "{js}");
    assert!(js.contains("credentials: 'include'"));
    assert!(js.contains("X-CSRF-Token"));
}
