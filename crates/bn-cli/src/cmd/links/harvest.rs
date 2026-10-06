//! `bn links harvest`: the agent Stop hook that saves the PRs and work items a session
//! mentioned, for Claude (`transcript_path` on stdin) and Copilot (`sessionId` on stdin, read
//! from `events.jsonl`). Every failure is swallowed, so it can never break a turn.

use super::*;

/// `bn links harvest` — save what an agent session mentioned. Every failure is swallowed: this
/// runs at the end of every turn, and a bookmark list is never worth a broken one.
pub(super) fn harvest(
    cfg: &Config,
    transcript: Option<std::path::PathBuf>,
    copilot_session: Option<String>,
    dry_run: bool,
) {
    let hook = if transcript.is_none() && copilot_session.is_none() {
        read_hook_stdin()
    } else {
        serde_json::Value::Null
    };
    let field = |k: &str| hook.get(k).and_then(serde_json::Value::as_str).map(str::to_string);
    // Claude names its transcript; Copilot only names its session, whose events live under
    // `$COPILOT_HOME/session-state/<id>/events.jsonl`.
    let path = transcript
        .or_else(|| field("transcript_path").map(Into::into))
        .or_else(|| {
            copilot_session
                .or_else(|| field("sessionId"))
                .and_then(|id| copilot_events(&id))
        });
    let Some(path) = path else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let urls = links::notable_urls(&spoken_text(&text));
    let stored = links::read(cfg);
    let urls: Vec<String> = urls
        .into_iter()
        .filter(|u| !stored.iter().any(|l| &l.url == u))
        .collect();
    if dry_run {
        for u in &urls {
            println!("{u}");
        }
        return;
    }
    if urls.is_empty() {
        return;
    }
    let cwd = field("cwd")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    let ctx = cwd.and_then(|d| resolver::resolve(cfg, &d).ok());
    let _ = links::harvest(
        cfg,
        &urls,
        ctx.as_ref().map(|c| c.repo.as_str()),
        ctx.as_ref().map(|c| c.branch.as_str()),
        &today(),
    );
}

/// Where Copilot keeps a session's events. `None` for an id that could climb out of the
/// session-state directory.
pub(super) fn copilot_events(session: &str) -> Option<std::path::PathBuf> {
    let safe = !session.is_empty()
        && session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !safe {
        return None;
    }
    let home = std::env::var_os("COPILOT_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".copilot")))?;
    Some(home.join("session-state").join(session).join("events.jsonl"))
}

/// The hook's stdin JSON, read with a bound: a harness closes stdin at once, and a hook must
/// never hang a turn on a pipe that stays open.
pub(super) fn read_hook_stdin() -> serde_json::Value {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf);
        let _ = tx.send(buf);
    });
    let buf = rx
        .recv_timeout(std::time::Duration::from_millis(500))
        .unwrap_or_default();
    serde_json::from_str(buf.trim()).unwrap_or(serde_json::Value::Null)
}

/// What was said in a session: the user's messages and the agent's replies, never tool output.
/// A tool result (a changelog, `git log`) is full of URLs nobody chose; a URL the agent or user
/// actually wrote down is one the session cared about.
///
/// Reads both transcript shapes line by line: Claude's `user`/`assistant` lines with
/// `message.content` (a string or `text` items), and Copilot's `user.message`/`assistant.message`
/// events with a `data.content` string. Every other line is skipped, so an unknown event in
/// either format is never an error.
pub(super) fn spoken_text(jsonl: &str) -> String {
    let mut out = String::new();
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let content = match v.get("type").and_then(serde_json::Value::as_str) {
            Some("user" | "assistant") => v.pointer("/message/content"),
            Some("user.message" | "assistant.message") => v.pointer("/data/content"),
            _ => continue,
        };
        match content {
            Some(serde_json::Value::String(t)) => out.push_str(t),
            Some(serde_json::Value::Array(items)) => {
                for item in items {
                    if item.get("type").and_then(serde_json::Value::as_str) == Some("text") {
                        if let Some(t) = item.get("text").and_then(serde_json::Value::as_str) {
                            out.push_str(t);
                            out.push('\n');
                        }
                    }
                }
            }
            _ => {}
        }
        out.push('\n');
    }
    out
}
