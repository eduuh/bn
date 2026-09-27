//! `bn-mcp --forge-call`: one MCP tool call against a remote Streamable-HTTP server (the Forge app),
//! for the `bn forge-agent` loop.
//!
//! The `bn` binary carries no async runtime or HTTP client, and pulling reqwest + TLS into it for a
//! five-minute poll is the wrong trade. `bn-mcp` already links tokio and rmcp, so the client lives
//! here and `bn` shells out to it the way it shells out to `gh`.
//!
//! The request arrives on **stdin**, not argv, so the bearer key never shows up in `ps`:
//! `{"url", "token", "tool", "arguments"}`. The reply on stdout is always one JSON line:
//! `{"ok": true, "result": <tool result>}` or `{"ok": false, "error": "..."}`. Exit status is 0 for
//! both, so the caller reads one shape instead of guessing from the exit code.

use anyhow::{anyhow, bail, Context, Result};
use rmcp::model::CallToolRequestParams;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::Read;
use std::time::Duration;

/// A whole call, handshake included, gets this long. A hung server must not wedge a tick.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct Request {
    url: String,
    token: String,
    tool: String,
    #[serde(default)]
    arguments: serde_json::Map<String, Value>,
}

/// Read the request from stdin, make the call, print the reply. Never returns an error: failures
/// become `{"ok": false}` so the caller has one shape to parse.
pub async fn run() -> Result<()> {
    let reply = match tokio::time::timeout(CALL_TIMEOUT, call_from_stdin()).await {
        Ok(Ok(result)) => json!({ "ok": true, "result": result }),
        Ok(Err(e)) => json!({ "ok": false, "error": format!("{e:#}") }),
        Err(_) => {
            json!({ "ok": false, "error": format!("timed out after {}s", CALL_TIMEOUT.as_secs()) })
        }
    };
    println!("{reply}");
    Ok(())
}

async fn call_from_stdin() -> Result<Value> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("reading request from stdin")?;
    let req: Request = serde_json::from_str(&input).context("parsing request")?;
    let token = req.token.trim();
    // rmcp adds the `Bearer ` prefix itself; tolerate a caller that passed the full header value.
    let token = token.strip_prefix("Bearer ").unwrap_or(token).to_string();

    let config = StreamableHttpClientTransportConfig::with_uri(req.url.as_str()).auth_header(token);
    let transport = StreamableHttpClientTransport::from_config(config);
    let client = ().serve(transport).await.with_context(|| format!("connecting to {}", req.url))?;
    let outcome = client
        .call_tool(CallToolRequestParams::new(req.tool.clone()).with_arguments(req.arguments))
        .await
        .with_context(|| format!("calling {}", req.tool));
    let _ = client.cancel().await;
    tool_value(serde_json::to_value(outcome?)?)
}

/// The tool's JSON result: `structuredContent` when the server sent it, else the first text block
/// parsed as JSON (or kept as a string). A result flagged `isError` becomes an error.
fn tool_value(result: Value) -> Result<Value> {
    let text = result["content"]
        .as_array()
        .and_then(|blocks| blocks.iter().find_map(|b| b["text"].as_str()))
        .map(str::to_string);
    if result["isError"].as_bool() == Some(true) {
        bail!(
            "{}",
            text.unwrap_or_else(|| "tool returned an error".into())
        );
    }
    if let Some(structured) = result.get("structuredContent").filter(|v| !v.is_null()) {
        return Ok(structured.clone());
    }
    let text = text.ok_or_else(|| anyhow!("tool returned no content"))?;
    Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_structured_content() {
        let v = tool_value(json!({
            "content": [{"type": "text", "text": "{\"claimed\":false}"}],
            "structuredContent": {"claimed": true}
        }))
        .unwrap();
        assert_eq!(v, json!({"claimed": true}));
    }

    #[test]
    fn falls_back_to_text_json() {
        let v =
            tool_value(json!({"content": [{"type": "text", "text": "{\"ok\":true}"}]})).unwrap();
        assert_eq!(v, json!({"ok": true}));
    }

    #[test]
    fn is_error_becomes_err() {
        let e = tool_value(json!({
            "content": [{"type": "text", "text": "Activity not found."}],
            "isError": true
        }))
        .unwrap_err();
        assert!(e.to_string().contains("not found"));
    }
}
