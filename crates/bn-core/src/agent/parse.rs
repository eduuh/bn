//! Harness session-file parsers: completed Copilot and Claude sessions on disk,
//! plus the low-level file/JSON helpers every scanner shares.

use super::session::AgentSession;
use crate::cost::WorkClass;
use crate::resolver::{self, Config};
use crate::util::now_epoch_secs;
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

/// A Claude transcript untouched for this long is treated as finished (mirrors `FLOW_GAP_SECS`).
/// Copilot sessions are detected as done by the presence of a `session.shutdown` event instead.
pub(crate) const QUIET_SECS: i64 = 1800;

// --- harness scanners -----------------------------------------------------------------------

/// Parse every completed Copilot session under `root` (`~/.copilot/session-state`).
pub(crate) fn scan_copilot(cfg: &Config, root: &Path) -> Vec<AgentSession> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| parse_copilot_session(cfg, &e.path()))
        .collect()
}

fn parse_copilot_session(cfg: &Config, dir: &Path) -> Option<AgentSession> {
    let evs = events_json(&dir.join("events.jsonl"));
    if evs.is_empty() {
        return None;
    }
    let typ = |v: &Value| {
        v.get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()
    };
    // Completed iff the stream carries a shutdown record (which also holds the aggregate).
    let shutdown = evs.iter().find(|v| typ(v) == "session.shutdown")?;
    let start = evs.iter().find(|v| typ(v) == "session.start");

    let sd = shutdown.get("data").cloned().unwrap_or(Value::Null);
    let ctx = start
        .and_then(|s| s.get("data"))
        .and_then(|d| d.get("context"))
        .cloned()
        .unwrap_or(Value::Null);

    // Session identity + attribution: workspace.yaml is authoritative for cwd/branch; the
    // session.start context is the fallback.
    let ws = read_yaml(&dir.join("workspace.yaml"));
    let session_id =
        ws_str(&ws, "id").or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))?;
    let cwd = ws_str(&ws, "cwd")
        .or_else(|| ctx.get("cwd").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_default();
    let branch_hint = ws_str(&ws, "branch")
        .or_else(|| ctx.get("branch").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_default();
    let repository = ws_str(&ws, "repository").or_else(|| {
        ctx.get("repository")
            .and_then(|v| v.as_str())
            .map(String::from)
    });
    let (repo, branch) = note_key(cfg, &cwd, &branch_hint, repository.as_deref())?;

    let started_ts = start
        .and_then(|s| s.get("data"))
        .and_then(|d| d.get("startTime"))
        .and_then(|v| v.as_str())
        .and_then(parse_iso)
        .or_else(|| evs.first().and_then(ts_of))
        .unwrap_or(0);
    let ended_ts = ts_of(shutdown).unwrap_or(started_ts);

    let count = |t: &str| evs.iter().filter(|v| typ(v) == t).count() as i64;

    // Token + cost aggregate from modelMetrics (per-model usage), plus model list.
    let mut models = Vec::new();
    let (mut input, mut output, mut cache_read) = (0i64, 0i64, 0i64);
    if let Some(mm) = sd.get("modelMetrics").and_then(|v| v.as_object()) {
        for (model, m) in mm {
            models.push(model.clone());
            if let Some(u) = m.get("usage") {
                input += u.get("inputTokens").and_then(|v| v.as_i64()).unwrap_or(0);
                output += u.get("outputTokens").and_then(|v| v.as_i64()).unwrap_or(0);
                cache_read += u
                    .get("cacheReadTokens")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
            }
        }
    }
    let cc = sd.get("codeChanges").cloned().unwrap_or(Value::Null);

    Some(AgentSession {
        session_id,
        harness: "copilot".into(),
        repo,
        branch,
        started_ts,
        ended_ts,
        turns: count("assistant.turn_start"),
        tool_calls: count("tool.execution_start"),
        permission_prompts: count("permission.requested"),
        models,
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cost_nanoaiu: sd.get("totalNanoAiu").and_then(|v| v.as_i64()),
        lines_added: cc.get("linesAdded").and_then(|v| v.as_i64()).unwrap_or(0),
        lines_removed: cc.get("linesRemoved").and_then(|v| v.as_i64()).unwrap_or(0),
        files_modified: cc
            .get("filesModified")
            .and_then(|v| v.as_array())
            .map(|a| a.len() as i64)
            .unwrap_or(0),
        outcome: "completed".into(),
        work_class: WorkClass::Unattributed,
    })
}

/// Parse every idle (finished) Claude transcript under `root` (`~/.claude/projects`).
pub(crate) fn scan_claude(cfg: &Config, root: &Path) -> Vec<AgentSession> {
    let Ok(projects) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let now = now_epoch_secs();
    let mut out = Vec::new();
    for proj in projects.flatten() {
        let Ok(files) = std::fs::read_dir(proj.path()) else {
            continue;
        };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if !is_quiet(&path, now) {
                continue; // still active — wait until it goes idle so we ingest it once, whole.
            }
            if let Some(s) = parse_claude_session(cfg, &path) {
                out.push(s);
            }
        }
    }
    out
}

fn parse_claude_session(cfg: &Config, path: &Path) -> Option<AgentSession> {
    let lines = events_json(path);
    if lines.is_empty() {
        return None;
    }
    let typ = |v: &Value| {
        v.get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()
    };

    let session_id = lines
        .iter()
        .find_map(|v| {
            v.get("sessionId")
                .and_then(|s| s.as_str())
                .map(String::from)
        })
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))?;

    // Latest line carrying a branch wins (the user may switch branches mid-session); its cwd
    // attributes the repo. Both fields must come from the *same* line — advancing the branch
    // off one line while keeping a stale cwd from another would mis-key the session.
    let (mut cwd, mut branch_hint) = (String::new(), String::new());
    for v in &lines {
        let b = v.get("gitBranch").and_then(|x| x.as_str()).unwrap_or("");
        let c = v.get("cwd").and_then(|x| x.as_str()).unwrap_or("");
        if !b.is_empty() && !c.is_empty() {
            branch_hint = b.to_string();
            cwd = c.to_string();
        }
    }
    let (repo, branch) = note_key(cfg, &cwd, &branch_hint, None)?;

    let stamps: Vec<i64> = lines.iter().filter_map(ts_of).collect();
    let started_ts = stamps.iter().copied().min().unwrap_or(0);
    let ended_ts = stamps.iter().copied().max().unwrap_or(started_ts);

    let mut models: Vec<String> = Vec::new();
    let (mut input, mut output, mut cache_read, mut turns, mut tools) =
        (0i64, 0i64, 0i64, 0i64, 0i64);
    for v in &lines {
        if typ(v) != "assistant" {
            continue;
        }
        turns += 1;
        let msg = v.get("message").cloned().unwrap_or(Value::Null);
        if let Some(m) = msg.get("model").and_then(|x| x.as_str()) {
            // Claude stamps "<synthetic>" on placeholder assistant lines — not a real model.
            if !m.is_empty() && m != "<synthetic>" && !models.iter().any(|e| e == m) {
                models.push(m.to_string());
            }
        }
        if let Some(u) = msg.get("usage") {
            input += u.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
            output += u.get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
            cache_read += u
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
        }
        if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
            tools += content
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
                .count() as i64;
        }
    }

    let (lines_added, lines_removed, files_modified) = claude_code_changes(&lines);

    Some(AgentSession {
        session_id,
        harness: "claude".into(),
        repo,
        branch,
        started_ts,
        ended_ts,
        turns,
        tool_calls: tools,
        permission_prompts: 0, // not recorded in the transcript
        models,
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cost_nanoaiu: None,
        lines_added,
        lines_removed,
        files_modified,
        outcome: "quiet".into(),
        work_class: WorkClass::Unattributed,
    })
}

/// Sum the code changes a Claude transcript records: each Edit/Write lands a `toolUseResult`
/// carrying `structuredPatch` (unified hunks whose `lines` are `+`/`-`/context prefixed) and a
/// `filePath`; a file creation has an empty patch, so its whole `content` counts as added.
/// Returns `(lines_added, lines_removed, files_modified)` — the Claude counterpart of Copilot's
/// `codeChanges` shutdown aggregate (#213: Claude sessions read as zero lines without this).
fn claude_code_changes(lines: &[Value]) -> (i64, i64, i64) {
    let (mut added, mut removed) = (0i64, 0i64);
    let mut files: HashSet<String> = HashSet::new();
    for v in lines {
        let Some(r) = v.get("toolUseResult").and_then(|r| r.as_object()) else {
            continue;
        };
        let Some(path) = r.get("filePath").and_then(|p| p.as_str()) else {
            continue;
        };
        let patch = r.get("structuredPatch").and_then(|p| p.as_array());
        let hunk_lines: i64 = patch
            .map(|hunks| {
                hunks
                    .iter()
                    .filter_map(|h| h.get("lines").and_then(|l| l.as_array()))
                    .flatten()
                    .filter_map(|l| l.as_str())
                    .map(|l| {
                        if l.starts_with('+') {
                            added += 1;
                            1
                        } else if l.starts_with('-') {
                            removed += 1;
                            1
                        } else {
                            0
                        }
                    })
                    .sum()
            })
            .unwrap_or(0);
        // A creation ("type":"create") has no patch — the written content is all new lines.
        if hunk_lines == 0 && r.get("type").and_then(|t| t.as_str()) == Some("create") {
            if let Some(content) = r.get("content").and_then(|c| c.as_str()) {
                added += content.lines().count() as i64;
            }
        }
        files.insert(path.to_string());
    }
    (added, removed, files.len() as i64)
}

// --- shared helpers -------------------------------------------------------------------------

/// Map a session's cwd + harness branch hint onto a bn note key. Repo comes from the cwd's first
/// segment under the worktree root (matching `resolver::resolve` Case A), falling back to the
/// harness `repository` (`owner/repo` → `repo`). Returns `None` when neither yields a repo or the
/// branch is unknown — an unattributable session is dropped rather than mis-filed.
pub(crate) fn note_key(
    cfg: &Config,
    cwd: &str,
    branch_hint: &str,
    repository: Option<&str>,
) -> Option<(String, String)> {
    if branch_hint.is_empty() {
        return None;
    }
    let repo = Path::new(cwd)
        .strip_prefix(&cfg.worktree_dir)
        .ok()
        .and_then(|rel| rel.components().next())
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .filter(|r| !r.is_empty())
        .or_else(|| repository.map(|r| r.rsplit('/').next().unwrap_or(r).to_string()))
        .filter(|r| !r.is_empty())?;
    let r = resolver::resolve_target(
        cfg,
        Path::new(""),
        Some(repo),
        Some(branch_hint.to_string()),
    )
    .ok()?;
    Some((r.repo, r.branch))
}

/// Read a JSONL file into one `Value` per parseable line (tolerant of torn/garbage lines).
pub(crate) fn events_json(path: &Path) -> Vec<Value> {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
        .collect()
}

/// A file untouched for at least `QUIET_SECS` (so an in-progress session isn't ingested mid-flight).
pub(crate) fn is_quiet(path: &Path, now: i64) -> bool {
    let modified = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    match modified {
        Some(m) => now - m >= QUIET_SECS,
        None => false,
    }
}

/// Top-level `.timestamp` (ISO8601) of an event line → epoch secs.
pub(crate) fn ts_of(v: &Value) -> Option<i64> {
    v.get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(parse_iso)
}

/// Parse an ISO8601 / RFC3339 timestamp to epoch seconds.
fn parse_iso(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

/// Read a small YAML file (Copilot's `workspace.yaml`) into a `Value`; empty on any failure.
pub(crate) fn read_yaml(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_yaml::from_str::<Value>(&t).ok())
        .unwrap_or(Value::Null)
}

pub(crate) fn ws_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{test_cfg, write};
    use serde_json::json;

    #[test]
    fn copilot_session_parses_and_attributes() {
        let tmp = std::env::temp_dir().join(format!("bn-agent-copilot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cfg = test_cfg(&tmp);
        let cwd = cfg.worktree_dir.join("bn").join("working-copilot");
        let sid = "11111111-1111-1111-1111-111111111111";
        let copilot = tmp.join("copilot").join(sid);
        write(
            &copilot.join("workspace.yaml"),
            &format!(
                "id: {sid}\ncwd: {}\nbranch: working-copilot\nrepository: eduuh/bn\n",
                cwd.display()
            ),
        );
        write(
            &copilot.join("events.jsonl"),
            &[
                json!({"type":"session.start","timestamp":"2026-06-26T12:00:00.000Z","data":{"startTime":"2026-06-26T12:00:00.000Z","context":{"cwd":cwd,"branch":"working-copilot","repository":"eduuh/bn"}}}).to_string(),
                json!({"type":"assistant.turn_start","timestamp":"2026-06-26T12:00:01.000Z","data":{}}).to_string(),
                json!({"type":"tool.execution_start","timestamp":"2026-06-26T12:00:02.000Z","data":{}}).to_string(),
                json!({"type":"session.shutdown","timestamp":"2026-06-26T12:05:00.000Z","data":{"totalNanoAiu":14970775000i64,"codeChanges":{"linesAdded":10,"linesRemoved":3,"filesModified":["a.rs","b.rs"]},"modelMetrics":{"claude-opus-4.8":{"usage":{"inputTokens":78139,"outputTokens":1408,"cacheReadTokens":65018}}}}}).to_string(),
            ].join("\n"),
        );

        let sessions = scan_copilot(&cfg, &tmp.join("copilot"));
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.session_id, sid);
        assert_eq!(
            (s.repo.as_str(), s.branch.as_str()),
            ("bn", "working-copilot")
        );
        assert_eq!(s.turns, 1);
        assert_eq!(s.tool_calls, 1);
        assert_eq!(s.duration_secs(), 300);
        assert_eq!(s.models, vec!["claude-opus-4.8"]);
        assert_eq!(s.input_tokens, 78139);
        assert_eq!(s.cost_nanoaiu, Some(14970775000));
        assert_eq!(
            (s.lines_added, s.lines_removed, s.files_modified),
            (10, 3, 2)
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn claude_branch_only_line_does_not_desync_cwd() {
        let tmp = std::env::temp_dir().join(format!("bn-agent-desync-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cfg = test_cfg(&tmp);
        let cwd = cfg.worktree_dir.join("bn").join("feat-x");
        let cwd_s = cwd.to_string_lossy().into_owned();
        let proj = tmp.join("claude").join("projects").join("proj");
        let body = [
            json!({"type":"assistant","sessionId":"sid-desync","timestamp":"2026-06-26T12:00:00.000Z","gitBranch":"feat/x","cwd":cwd_s,"message":{"model":"claude-opus-4-8"}}).to_string(),
            // A later line names a different branch but carries NO cwd — it must not advance the
            // branch while cwd still points at the previous line's repo/branch.
            json!({"type":"user","timestamp":"2026-06-26T12:01:00.000Z","gitBranch":"feat/y"}).to_string(),
        ]
        .join("\n");
        write(&proj.join("x.jsonl"), &body);

        let s = parse_claude_session(&cfg, &proj.join("x.jsonl")).expect("session parses");
        // Buggy code would advance branch_hint to "feat/y" (→ "feat-y"); the fix keeps the last
        // line that had *both* fields, so attribution stays "feat-x".
        assert_eq!(
            (s.repo.as_str(), s.branch.as_str()),
            ("bn", "feat-x"),
            "a branch-only line without cwd must not desync attribution"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn claude_session_counts_lines_from_tool_results() {
        let tmp = std::env::temp_dir().join(format!("bn-agent-lines-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cfg = test_cfg(&tmp);
        let cwd = cfg.worktree_dir.join("bn").join("feat-x");
        let cwd_s = cwd.to_string_lossy().into_owned();
        let proj = tmp.join("claude").join("projects").join("proj");
        let body = [
            json!({"type":"assistant","sessionId":"sid-lines","timestamp":"2026-06-26T12:00:00.000Z","gitBranch":"feat/x","cwd":cwd_s,"message":{"model":"claude-opus-4-8"}}).to_string(),
            // An Edit: structuredPatch with 2 added, 1 removed, 1 context line.
            json!({"type":"user","timestamp":"2026-06-26T12:01:00.000Z","toolUseResult":{
                "type":"update","filePath":"/w/a.rs","oldString":"x","newString":"y",
                "structuredPatch":[{"oldStart":1,"oldLines":2,"newStart":1,"newLines":3,
                    "lines":[" fn main() {","-    old();","+    new();","+    extra();"]}]
            }}).to_string(),
            // A Write creating a file: empty patch, 3 content lines → 3 added.
            json!({"type":"user","timestamp":"2026-06-26T12:02:00.000Z","toolUseResult":{
                "type":"create","filePath":"/w/b.rs","structuredPatch":[],
                "content":"line1\nline2\nline3"
            }}).to_string(),
            // Second edit to the same file — more lines, but not another distinct file.
            json!({"type":"user","timestamp":"2026-06-26T12:03:00.000Z","toolUseResult":{
                "type":"update","filePath":"/w/a.rs",
                "structuredPatch":[{"lines":["+one_more();"]}]
            }}).to_string(),
            // A Bash result (no filePath) must not count.
            json!({"type":"user","timestamp":"2026-06-26T12:04:00.000Z","toolUseResult":{
                "stdout":"+++ not a diff","stderr":"","interrupted":false,"isImage":false
            }}).to_string(),
        ]
        .join("\n");
        write(&proj.join("s.jsonl"), &body);

        let s = parse_claude_session(&cfg, &proj.join("s.jsonl")).expect("session parses");
        assert_eq!(
            (s.lines_added, s.lines_removed, s.files_modified),
            (6, 1, 2),
            "2+3+1 added (edit + create + edit), 1 removed, 2 distinct files"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // --- digest tests -----------------------------------------------------------------------
}
