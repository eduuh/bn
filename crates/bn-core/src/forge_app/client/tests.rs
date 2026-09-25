use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

#[test]
fn sse_events_join_data_lines_and_split_on_blank_lines() {
    let body = "event: message\r\ndata: {\"a\":\r\ndata: 1}\r\n\r\n: comment\ndata: two\n";
    assert_eq!(sse_data(body), vec!["{\"a\":\n1}", "two"]);
}

#[test]
fn rpc_reply_reads_json_and_sse_and_matches_the_id() {
    let json_body = r#"{"jsonrpc":"2.0","id":7,"result":{"ok":1}}"#;
    let v = parse_rpc_reply("application/json; charset=utf-8", json_body, 7, "x").unwrap();
    assert_eq!(v, json!({"ok": 1}));

    let sse =
        "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n\
               event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"x\":2}}\n\n";
    let v = parse_rpc_reply("text/event-stream", sse, 3, "x").unwrap();
    assert_eq!(v, json!({"x": 2}));

    let err = parse_rpc_reply("text/event-stream", sse, 4, "x").unwrap_err();
    assert!(err.to_string().contains("no reply to request 4"), "{err}");

    let rpc_err =
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"Unknown tool: 'nope'"}}"#;
    let err = parse_rpc_reply("application/json", rpc_err, 1, "nope").unwrap_err();
    assert!(err.to_string().contains("Unknown tool"), "{err}");
}

#[test]
fn tool_results_parse_text_json_and_surface_is_error() {
    let ok = json!({"content": [{"type": "text", "text": "{\"id\":\"abc\"}"}]});
    assert_eq!(tool_result("t", &ok).unwrap(), json!({"id": "abc"}));

    let plain = json!({"content": [{"type": "text", "text": "Deleted x."}]});
    assert_eq!(tool_result("t", &plain).unwrap(), json!("Deleted x."));

    let structured = json!({"content": [], "structuredContent": {"n": 1}});
    assert_eq!(tool_result("t", &structured).unwrap(), json!({"n": 1}));

    let bad =
        json!({"isError": true, "content": [{"type": "text", "text": "Activity 'x' not found."}]});
    let err = tool_result("get_activity", &bad).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Forge get_activity: Activity 'x' not found."
    );
}

#[test]
fn meta_and_claim_outcomes_decode_the_contract() {
    assert_eq!(
        decode_meta_outcome(&json!({"ok": true, "version": 4})).unwrap(),
        MetaOutcome::Ok { version: 4 }
    );
    assert_eq!(
        decode_meta_outcome(&json!({"ok": false, "reason": "version_conflict", "version": 9}))
            .unwrap(),
        MetaOutcome::VersionConflict { version: 9 }
    );
    assert!(decode_meta_outcome(&json!({"ok": false, "reason": "nope"})).is_err());
    match decode_claim_outcome(&json!({"claimed": true, "activity": {"id": "a1", "version": 2}}))
        .unwrap()
    {
        ClaimOutcome::Claimed(row) => assert_eq!((row.id.as_str(), row.version), ("a1", 2)),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        decode_claim_outcome(&json!({"claimed": false, "reason": "already_claimed"})).unwrap(),
        ClaimOutcome::Refused("already_claimed".into())
    );
}

#[test]
fn rows_decode_with_nulls_and_omitted_fields() {
    let v = json!([{
        "id": "u1", "goalId": "g1", "goalName": "bn", "goalRepoUrl": null,
        "title": "Do it", "priority": "High", "status": "Active", "isCompleted": false,
        "createdAt": "2026-09-01T10:00:00Z", "version": 3,
        "assignees": null, "agentMeta": {"id": "t4"}
    }]);
    let rows: Vec<BnTaskRow> = decode("list_bn_tasks", v).unwrap();
    assert_eq!(rows[0].agent_meta.get("id").map(String::as_str), Some("t4"));
    assert!(rows[0].assignees.is_empty());
    assert_eq!(rows[0].goal_repo_url, None);

    let members: Vec<FamilyMemberRow> = decode(
        "get_family_members",
        json!([{"userId": "u1", "displayName": "Agents", "role": "member", "isAgent": true}]),
    )
    .unwrap();
    assert!(members[0].is_agent);
}

/// One recorded HTTP request: its lowercased headers and body.
struct Seen {
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Seen {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut headers = Vec::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let len: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).unwrap();
    Seen {
        headers,
        body: String::from_utf8(body).unwrap(),
    }
}

fn respond(stream: &mut std::net::TcpStream, status: &str, extra: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).unwrap();
}

/// The whole handshake against a local stand-in server: initialize answered over SSE with a
/// session id, the initialized notification, then a tool call that must replay the session id and
/// the bearer key, answered as plain JSON.
#[test]
fn handshake_keeps_the_session_and_sends_the_key_only_as_a_header() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for n in 0..3 {
            let (mut s, _) = listener.accept().unwrap();
            let req = read_request(&mut s);
            let msg: Value = serde_json::from_str(&req.body).unwrap();
            match n {
                0 => {
                    let id = msg["id"].as_u64().unwrap();
                    let body = format!(
                        "event: message\ndata: {{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"protocolVersion\":\"2025-06-18\",\"serverInfo\":{{\"name\":\"forge\",\"version\":\"1.2\"}}}}}}\n\n"
                    );
                    respond(
                        &mut s,
                        "200 OK",
                        "Content-Type: text/event-stream\r\nMcp-Session-Id: sess-1\r\n",
                        &body,
                    );
                }
                1 => respond(&mut s, "202 Accepted", "", ""),
                _ => {
                    let id = msg["id"].as_u64().unwrap();
                    let text =
                        serde_json::to_string(&json!([{"id": "g1", "name": "Inbox"}])).unwrap();
                    let body = json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": text}]}});
                    respond(
                        &mut s,
                        "200 OK",
                        "Content-Type: application/json\r\n",
                        &body.to_string(),
                    );
                }
            }
            seen.push((req, msg));
        }
        seen
    });

    let api = HttpForgeApi::new(url, "fam_secret_key");
    let goals = api.get_goals().expect("get_goals");
    assert_eq!(goals.len(), 1);
    assert_eq!(goals[0].name, "Inbox");
    assert!(!format!("{api:?}").contains("fam_secret_key"));

    let seen = server.join().unwrap();
    assert_eq!(seen[0].1["method"], "initialize");
    assert_eq!(seen[0].0.header("mcp-session-id"), None);
    assert_eq!(seen[1].1["method"], "notifications/initialized");
    assert_eq!(seen[1].0.header("mcp-session-id"), Some("sess-1"));
    assert_eq!(seen[2].1["method"], "tools/call");
    assert_eq!(seen[2].1["params"]["name"], "get_goals");
    assert_eq!(seen[2].0.header("mcp-session-id"), Some("sess-1"));
    assert_eq!(seen[2].0.header("mcp-protocol-version"), Some("2025-06-18"));
    for (req, _) in &seen {
        assert_eq!(req.header("authorization"), Some("Bearer fam_secret_key"));
        assert!(req.header("accept").unwrap().contains("text/event-stream"));
        assert!(!req.body.contains("fam_secret_key"));
    }
}

#[test]
fn a_rejected_key_points_at_bn_forge_login() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let _ = read_request(&mut s);
        respond(&mut s, "401 Unauthorized", "", "");
    });
    let err = McpHttp::new(url, "fam_bad").initialize().unwrap_err();
    server.join().unwrap();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert!(err.to_string().contains("bn forge login"), "{err}");
    assert!(!err.to_string().contains("fam_bad"));
}

/// Two stores in one process share one MCP session: the second never re-initialises. A session
/// the server has dropped (HTTP 404) is opened again and the call retried.
#[test]
fn stores_share_one_session_per_endpoint_and_reopen_a_dropped_one() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut seen = Vec::new();
        let (mut sessions, mut calls) = (0, 0);
        while seen.len() < 8 {
            let (mut s, _) = listener.accept().unwrap();
            let req = read_request(&mut s);
            let msg: Value = serde_json::from_str(&req.body).unwrap();
            let method = msg["method"].as_str().unwrap_or_default().to_string();
            let id = msg["id"].as_u64().unwrap_or_default();
            let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
            match method.as_str() {
                "initialize" => {
                    sessions += 1;
                    let extra = format!(
                        "Content-Type: application/json\r\nMcp-Session-Id: sess-{sessions}\r\n"
                    );
                    let body = ok(json!({"protocolVersion": "2025-06-18"}));
                    respond(&mut s, "200 OK", &extra, &body.to_string());
                }
                "notifications/initialized" => respond(&mut s, "202 Accepted", "", ""),
                _ => {
                    calls += 1;
                    if calls == 3 {
                        respond(&mut s, "404 Not Found", "", "");
                    } else {
                        let body = ok(json!({"content": [{"type": "text", "text": "[]"}]}));
                        let extra = "Content-Type: application/json\r\n";
                        respond(&mut s, "200 OK", extra, &body.to_string());
                    }
                }
            }
            let session = req.header("mcp-session-id").unwrap_or("-").to_string();
            seen.push(format!("{method} {session}"));
        }
        seen
    });

    let first = crate::forge_app::session::api_for(&url, "fam_shared_key");
    let second = crate::forge_app::session::api_for(&url, "fam_shared_key");
    first.get_goals().expect("first store");
    second.get_goals().expect("second store, same session");
    second
        .get_goals()
        .expect("after the server dropped the session");

    assert_eq!(
        server.join().unwrap(),
        [
            "initialize -",
            "notifications/initialized sess-1",
            "tools/call sess-1",
            "tools/call sess-1",
            "tools/call sess-1",
            "initialize -",
            "notifications/initialized sess-2",
            "tools/call sess-2",
        ]
    );
}
