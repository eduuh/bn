//! Pre-grant an agent harness's *folder trust* so a freshly-created worktree boots without the
//! interactive "trust this folder?" prompt. This is the user's explicit opt-in (it rides on
//! `--yolo` / `bn trust-folder`) for worktrees they created from their own repos — the harnesses'
//! yolo flags (`--dangerously-skip-permissions`, `--allow-all-paths`) skip *tool/path* permissions
//! but NOT the first-run folder-trust gate, which is what actually blocks a dispatched agent.
//!
//! The mutating logic is pure (text in → text out) so it's unit-testable without touching the real
//! `~/.claude.json` / `~/.copilot/config.json`; the CLI wraps it with the file IO.

use serde_json::{json, Value};

/// Ensure `path` is trusted in Claude's `~/.claude.json`: `projects[path].hasTrustDialogAccepted =
/// true` (creating the `projects` map / project entry if absent). Returns the new file text.
/// `json_text` empty → start from `{}`.
pub fn claude_trust(json_text: &str, path: &str) -> Result<String, String> {
    let mut root: Value = if json_text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(json_text).map_err(|e| format!("~/.claude.json: {e}"))?
    };
    let obj = root
        .as_object_mut()
        .ok_or("~/.claude.json is not a JSON object")?;
    let projects = obj
        .entry("projects")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("~/.claude.json .projects is not an object")?;
    let entry = projects
        .entry(path.to_string())
        .or_insert_with(|| json!({}));
    if let Some(e) = entry.as_object_mut() {
        e.insert("hasTrustDialogAccepted".into(), json!(true));
    } else {
        *entry = json!({ "hasTrustDialogAccepted": true });
    }
    serde_json::to_string_pretty(&root).map_err(|e| e.to_string())
}

/// Ensure `path` is in Copilot's `~/.copilot/config.json` `trustedFolders` array. That file is
/// JSONC (leading `//` comment lines), so we split off the leading comment header, edit the JSON
/// body, and re-prepend the header. Idempotent: an already-trusted path is left unchanged. Returns
/// `(new_text, changed)`.
pub fn copilot_trust(jsonc_text: &str, path: &str) -> Result<(String, bool), String> {
    let (header, body) = split_jsonc_header(jsonc_text);
    let mut root: Value = if body.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&body).map_err(|e| format!("~/.copilot/config.json: {e}"))?
    };
    let obj = root
        .as_object_mut()
        .ok_or("~/.copilot/config.json is not a JSON object")?;
    let arr = obj
        .entry("trustedFolders")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("~/.copilot/config.json .trustedFolders is not an array")?;
    if arr.iter().any(|v| v.as_str() == Some(path)) {
        return Ok((jsonc_text.to_string(), false)); // already trusted — leave the file untouched
    }
    arr.push(json!(path));
    let json = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    let out = if header.is_empty() {
        json
    } else {
        format!("{header}\n{json}")
    };
    Ok((out, true))
}

/// Split a JSONC file into its leading `//`-comment header (verbatim) and the JSON body (from the
/// first line that starts the object). Blank lines before the object stay in the header.
fn split_jsonc_header(text: &str) -> (String, String) {
    let mut header = Vec::new();
    let mut body_start = 0;
    let lines: Vec<&str> = text.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if t.starts_with("//") || t.is_empty() {
            header.push(*l);
        } else {
            body_start = i;
            break;
        }
    }
    let header = header.join("\n");
    let body = lines[body_start..].join("\n");
    (header, body)
}


