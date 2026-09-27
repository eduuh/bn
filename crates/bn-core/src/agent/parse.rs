//! Harness session-file parsers: completed Copilot and Claude sessions on disk,
//! plus the low-level file/JSON helpers every scanner shares.

use super::session::AgentSession;
use crate::cost::{TokenUsage, WorkClass};
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
    let (mut input, mut output, mut cache_read, mut cache_write) = (0i64, 0i64, 0i64, 0i64);
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
                cache_write += u
                    .get("cacheWriteTokens")
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
        // Copilot reports what it wrote but never at which TTL, so it all lands in the 5-minute
        // bucket — the providers' default, and the cheaper of the two, so an unknown TTL rounds
        // the benchmark down rather than inventing spend.
        cache_write_5m_tokens: cache_write,
        cache_write_1h_tokens: 0,
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
    let mut usage = TokenUsage::default();
    let (mut turns, mut tools) = (0i64, 0i64);
    let mut counted: HashSet<String> = HashSet::new();
    for v in &lines {
        if typ(v) != "assistant" {
            continue;
        }
        let msg = v.get("message").cloned().unwrap_or(Value::Null);
        if let Some(m) = msg.get("model").and_then(|x| x.as_str()) {
            // Claude stamps "<synthetic>" on placeholder assistant lines — not a real model.
            if !m.is_empty() && m != "<synthetic>" && !models.iter().any(|e| e == m) {
                models.push(m.to_string());
            }
        }
        // One API response is written as *several* transcript lines — one per content block —
        // and every one of them repeats the whole response's `usage`. Summing per line therefore
        // multiplies a session's tokens by its average blocks-per-response (measured at 2.37× on
        // a real transcript), so usage and turns are counted once per `message.id`.
        if first_line_of_message(&msg, &mut counted) {
            turns += 1;
            if let Some(u) = msg.get("usage") {
                usage += claude_usage(u);
            }
        }
        // Tool calls are the opposite case: each line carries its *own* block, so they are summed
        // across every line. Deduping these would undercount by the same factor usage overcounts.
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
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_5m_tokens: usage.cache_write_5m_tokens,
        cache_write_1h_tokens: usage.cache_write_1h_tokens,
        cost_nanoaiu: None,
        lines_added,
        lines_removed,
        files_modified,
        outcome: "quiet".into(),
        work_class: WorkClass::Unattributed,
    })
}

/// Whether this transcript line is the first one belonging to its API response — the guard that
/// keeps a multi-block response's repeated `usage` from being counted once per block.
///
/// Claude writes one line per content block (thinking, text, and each `tool_use` land separately),
/// and every line repeats the parent response's full `message.usage`. `message.id` is the response
/// identity, so the first line bearing an id owns that response's usage and turn; later lines
/// contribute only their own content. A line with **no** id can't be attributed to a response, so
/// it counts — under-reporting a session is the worse failure, and synthetic lines carry no usage
/// to inflate anyway.
pub(crate) fn first_line_of_message(msg: &Value, counted: &mut HashSet<String>) -> bool {
    first_line_of_id(msg.get("id").and_then(|x| x.as_str()), counted)
}

/// [`first_line_of_message`] for a message whose `id` has already been read out.
pub(crate) fn first_line_of_id(id: Option<&str>, counted: &mut HashSet<String>) -> bool {
    match id.filter(|s| !s.is_empty()) {
        Some(id) => counted.insert(id.to_string()),
        None => true,
    }
}

/// The token buckets one assistant turn's `usage` object contributes, in the shape the price table
/// bills them ([`crate::cost::cost_usd`]).
///
/// Claude reports the three prompt figures **disjointly** — `input_tokens` is only the *uncached*
/// remainder, with cache reads and cache writes in fields of their own — so they sum to the real
/// prompt instead of overlapping. On a Claude Code turn the uncached remainder is often single
/// digits while the cache fields run to five, which is why reading only `input_tokens` (as this
/// parser once did) makes a session look nearly free.
///
/// The cache-write **TTL** decides the rate (1.25× input at 5 minutes, 2× at an hour) and lives in
/// a nested `cache_creation` object. A transcript written before that object existed carries only
/// the `cache_creation_input_tokens` total; that falls to the 5-minute bucket — the API default and
/// the cheaper rate, so an unknown TTL rounds the benchmark down rather than inventing spend.
fn claude_usage(u: &Value) -> TokenUsage {
    let n = |k: &str| u.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    let ttl = |k: &str| {
        u.get("cache_creation")
            .and_then(|c| c.get(k))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
    };
    let (write_5m, write_1h) = (
        ttl("ephemeral_5m_input_tokens"),
        ttl("ephemeral_1h_input_tokens"),
    );
    let total_writes = n("cache_creation_input_tokens");
    TokenUsage {
        input_tokens: n("input_tokens"),
        cache_read_tokens: n("cache_read_input_tokens"),
        // Trust the total over the split: the split is the newer field, and if the two ever
        // disagree the untracked remainder is still real spend that has to be billed somewhere.
        cache_write_5m_tokens: write_5m + (total_writes - write_5m - write_1h).max(0),
        cache_write_1h_tokens: write_1h,
        output_tokens: n("output_tokens"),
    }
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
pub(crate) fn parse_iso(s: &str) -> Option<i64> {
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
    use serde_json::json;

    /// The `usage` object of a real mid-session Claude Code turn: two fresh input tokens against
    /// 56_575 cache reads and 1_192 tokens written at the 1-hour TTL.
    fn cached_turn() -> Value {
        json!({
            "input_tokens": 2,
            "cache_creation_input_tokens": 1192,
            "cache_read_input_tokens": 56575,
            "output_tokens": 197,
            "cache_creation": {
                "ephemeral_1h_input_tokens": 1192,
                "ephemeral_5m_input_tokens": 0
            }
        })
    }

    #[test]
    fn a_claude_turns_cache_writes_are_captured_at_their_ttl() {
        let u = claude_usage(&cached_turn());
        assert_eq!(u.input_tokens, 2);
        assert_eq!(u.cache_read_tokens, 56_575);
        assert_eq!(u.cache_write_1h_tokens, 1_192);
        assert_eq!(u.cache_write_5m_tokens, 0);
        assert_eq!(u.output_tokens, 197);
    }

    #[test]
    fn the_prompt_is_far_larger_than_the_input_tokens_field_alone() {
        // The reason cache fields cannot be ignored: `input_tokens` accounts for 2 tokens of a
        // ~58k prompt, so a fold that reads only it prices the session at effectively zero.
        let u = claude_usage(&cached_turn());
        assert_eq!(u.prompt_tokens(), 57_769);
        assert!(u.prompt_tokens() > u.input_tokens * 1000);
    }

    #[test]
    fn a_transcript_without_the_ttl_split_falls_back_to_the_cheaper_bucket() {
        // Older Claude Code wrote the total with no `cache_creation` object. Attributing it to the
        // 5-minute rate under-prices rather than inventing spend — and, crucially, still counts it.
        let u = claude_usage(&json!({
            "input_tokens": 10,
            "cache_creation_input_tokens": 4096,
            "cache_read_input_tokens": 100,
            "output_tokens": 50
        }));
        assert_eq!(u.cache_write_5m_tokens, 4096);
        assert_eq!(u.cache_write_1h_tokens, 0);
        assert_eq!(u.cache_write_tokens(), 4096);
    }

    #[test]
    fn a_split_that_undershoots_its_total_still_bills_the_remainder() {
        // If a future TTL bucket appears that this parser doesn't know to name, the tokens are
        // still real spend: the unaccounted remainder falls to the 5m bucket rather than vanishing.
        let u = claude_usage(&json!({
            "cache_creation_input_tokens": 1000,
            "cache_creation": { "ephemeral_1h_input_tokens": 600 }
        }));
        assert_eq!(u.cache_write_1h_tokens, 600);
        assert_eq!(u.cache_write_5m_tokens, 400);
        assert_eq!(u.cache_write_tokens(), 1000);
    }

    #[test]
    fn a_usage_object_missing_every_field_reads_as_zero_rather_than_panicking() {
        let u = claude_usage(&json!({}));
        assert_eq!(u, TokenUsage::default());
    }

    #[test]
    fn per_turn_usage_accumulates_bucket_by_bucket_across_a_session() {
        let mut total = TokenUsage::default();
        for _ in 0..3 {
            total += claude_usage(&cached_turn());
        }
        assert_eq!(total.input_tokens, 6);
        assert_eq!(total.cache_read_tokens, 169_725);
        assert_eq!(total.cache_write_1h_tokens, 3_576);
        assert_eq!(total.output_tokens, 591);
    }
    /// Claude writes one line per content block, each repeating the parent response's `usage`.
    /// These two lines are one API response: a `thinking` block and a `tool_use` block.
    fn split_response() -> (Value, Value) {
        let usage = json!({
            "input_tokens": 3,
            "cache_creation_input_tokens": 1000,
            "cache_read_input_tokens": 50_000,
            "output_tokens": 400,
            "cache_creation": { "ephemeral_1h_input_tokens": 1000 }
        });
        (
            json!({ "id": "msg_01", "usage": usage, "content": [{"type": "thinking"}] }),
            json!({ "id": "msg_01", "usage": usage, "content": [{"type": "tool_use"}] }),
        )
    }

    #[test]
    fn a_response_split_across_lines_is_counted_once() {
        // The 2.37x inflation this guards: both lines carry the same aggregate usage.
        let (a, b) = split_response();
        let mut counted = HashSet::new();
        assert!(first_line_of_message(&a, &mut counted));
        assert!(!first_line_of_message(&b, &mut counted));

        let mut usage = TokenUsage::default();
        let mut counted = HashSet::new();
        for line in [&a, &b] {
            if first_line_of_message(line, &mut counted) {
                usage += claude_usage(line.get("usage").expect("usage"));
            }
        }
        assert_eq!(usage.output_tokens, 400);
        assert_eq!(usage.cache_write_1h_tokens, 1_000);
        assert_eq!(usage.cache_read_tokens, 50_000);
    }

    #[test]
    fn separate_responses_each_count() {
        let mut counted = HashSet::new();
        assert!(first_line_of_message(
            &json!({ "id": "msg_01" }),
            &mut counted
        ));
        assert!(first_line_of_message(
            &json!({ "id": "msg_02" }),
            &mut counted
        ));
    }

    #[test]
    fn a_line_with_no_message_id_counts_rather_than_being_dropped() {
        // Unattributable to a response, so counting it risks a small overcount while dropping it
        // risks losing a whole response. Synthetic lines carry no usage to inflate anyway.
        let mut counted = HashSet::new();
        assert!(first_line_of_message(&json!({}), &mut counted));
        assert!(first_line_of_message(&json!({ "id": "" }), &mut counted));
        assert!(first_line_of_message(&json!({ "id": "" }), &mut counted));
    }
}
