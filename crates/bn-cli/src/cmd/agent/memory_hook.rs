//! `bn agent memory-hook`, the Claude Code SessionStart hook. Split out of `agent.rs` under the
//! never-grow ratchet.
use crate::*;

/// `bn agent memory-hook` — the Claude Code SessionStart hook, ported from `scripts/wt-memory.sh`.
/// Points this worktree's auto-memory at `<root>/.claude/memory` (so it travels with the worktree)
/// by writing `autoMemoryDirectory` into the gitignored `<root>/.claude/settings.local.json` — the
/// checked-in `settings.json` is ignored for this key by a Claude Code security rule. Idempotent:
/// only rewrites when the value differs. Then prints the branch's handoff packet to orient the
/// session. Best-effort throughout: never fails the hook, and stays quiet when there's no note.
pub(crate) fn emit_memory_hook(cfg: &Config) {
    let root = std::env::var_os("CLAUDE_PROJECT_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            crate::git::git_capture(&["rev-parse", "--show-toplevel"], None, true)
                .map(std::path::PathBuf::from)
        })
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();

    let mem = root.join(".claude").join("memory");
    let settings = root.join(".claude").join("settings.local.json");
    let _ = std::fs::create_dir_all(&mem);
    let mem_str = mem.to_string_lossy().to_string();

    // Merge autoMemoryDirectory into the local settings, preserving any existing keys. Only write
    // when the value actually changes, so we don't churn the file on every session.
    let current = std::fs::read_to_string(&settings).ok();
    if let Some(text) = merge_auto_memory_dir(current.as_deref(), &mem_str) {
        let _ = std::fs::write(&settings, text);
    }

    // After a compaction the session needs the audit, not the usual orientation. A session
    // `bn agent launch` started has its own `compact` hook running the audit (it sets
    // `BN_COMPACT_HOOKS`), so stay quiet there rather than print it twice; any other session
    // gets the audit from here.
    if hook_source().as_deref() == Some("compact") {
        if std::env::var("BN_COMPACT_HOOKS").is_ok_and(|v| v == "1") {
            return;
        }
        if let Ok(ctx) = resolver::resolve(cfg, &root) {
            crate::cmd::compact::audit_quietly(cfg, &ctx);
        }
        return;
    }

    // Orient the session: print the structured resume packet for this worktree's branch, quietly.
    if let Ok(ctx) = resolver::resolve(cfg, &root) {
        let handoff = bn_core::handoff::read(cfg, &ctx).or_else(|| {
            bn_core::store::read(&cfg.note_file(&ctx))
                .ok()
                .map(|note| bn_core::handoff::compose(&note, &today(), &Default::default()))
        });
        if let Some(h) = handoff {
            print!("{}", bn_core::handoff::render_orientation(&h));
        }
    }
}

/// Merge `autoMemoryDirectory = <mem>` into the `settings.local.json` body, preserving every other
/// key. Returns the new file text to write, or `None` when the value is already set (so the caller
/// skips the write and doesn't churn the file). Unparseable/absent input starts from `{}`.
fn merge_auto_memory_dir(current: Option<&str>, mem: &str) -> Option<String> {
    let parsed = current.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
    if parsed
        .as_ref()
        .and_then(|v| v.get("autoMemoryDirectory"))
        .and_then(|v| v.as_str())
        == Some(mem)
    {
        return None;
    }
    let mut obj = match parsed {
        Some(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    obj.insert(
        "autoMemoryDirectory".to_string(),
        serde_json::Value::String(mem.to_string()),
    );
    serde_json::to_string_pretty(&serde_json::Value::Object(obj))
        .ok()
        .map(|t| format!("{t}\n"))
}


/// The `source` a SessionStart hook passes on stdin (`startup`, `resume`, `clear`, `compact`), or
/// `None` when run by hand or nothing arrives.
///
/// The read runs on its own thread with a short deadline: a harness hook pipes its JSON and closes
/// stdin at once, but a script or parent that leaves a pipe open would otherwise hang the hook —
/// and everything waiting on it — forever.
fn hook_source() -> Option<String> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf);
        let _ = tx.send(buf);
    });
    let buf = rx.recv_timeout(std::time::Duration::from_millis(500)).ok()?;
    let v: serde_json::Value = serde_json::from_str(buf.trim()).ok()?;
    v.get("source")?.as_str().map(str::to_string)
}
