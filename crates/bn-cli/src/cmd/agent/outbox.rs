//! The message outbox: `bn agent msg` never loses a message.
//!
//! A message bn cannot confirm in the target's pane (the target is not live yet, the machine it
//! runs on is unreachable, it sits on a dialog, or the TUI ate the Enter) is parked here, on the
//! machine that sent it, instead of failing. `bn agent outbox --flush` (run by the tunnel watchdog
//! every minute) sends each one again through the full path, relay included, and drops it only
//! once delivery is confirmed. Nothing expires: `bn agent outbox` lists what is still waiting and
//! why.
//!
//! Delivery is at-least-once. A submit bn could not confirm may have landed after all, so a retry
//! can repeat a message; a repeat is the price of never dropping one.

use std::io::Write;
use std::path::PathBuf;

/// Set on the `bn agent msg` a flush runs, so a failed retry is reported to the flush instead of
/// being parked a second time.
pub(crate) const RETRY_ENV: &str = "BN_MSG_RETRY";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Parked {
    pub id: String,
    pub to: String,
    pub from: String,
    pub text: String,
    /// Unix seconds when it was first parked.
    pub at: i64,
    pub tries: u32,
    /// Why the last attempt did not land.
    pub why: String,
}

pub(crate) fn path() -> PathBuf {
    bn_core::resolver::state_dir().join("msg-outbox.jsonl")
}

/// Hold `<outbox>.<name>.lock` while `f` runs. `wait`: retry for up to 5s, else give up at once
/// (`None`). A lock older than 10 minutes is from a process that died, and is taken over.
fn locked<T>(name: &str, wait: bool, f: impl FnOnce() -> T) -> Option<T> {
    let lock = path().with_extension(format!("{name}.lock"));
    if let Some(dir) = lock.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if std::fs::OpenOptions::new().write(true).create_new(true).open(&lock).is_ok() {
            break;
        }
        let stale = std::fs::metadata(&lock)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|e| e.as_secs() > 600));
        if stale {
            let _ = std::fs::remove_file(&lock);
            continue;
        }
        if !wait || std::time::Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let out = f();
    let _ = std::fs::remove_file(&lock);
    Some(out)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub(crate) fn read() -> Vec<Parked> {
    std::fs::read_to_string(path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn write(items: &[Parked]) -> std::io::Result<()> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("jsonl.tmp");
    let mut body = String::new();
    for it in items {
        body.push_str(&serde_json::to_string(it).map_err(std::io::Error::other)?);
        body.push('\n');
    }
    std::fs::write(&tmp, body)?;
    std::fs::rename(tmp, p)
}

/// Park a message; returns its id. Appended, so a crash mid-write loses at most this line.
pub(crate) fn park(to: &str, from: &str, text: &str, why: &str) -> std::io::Result<String> {
    let at = now();
    let id = format!("m{at}-{}", std::process::id());
    let item = Parked {
        id: id.clone(),
        to: to.into(),
        from: from.into(),
        text: text.into(),
        at,
        tries: 1,
        why: why.into(),
    };
    let line = serde_json::to_string(&item).map_err(std::io::Error::other)?;
    locked("file", true, || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path())?;
        writeln!(f, "{line}")
    })
    .unwrap_or_else(|| Err(std::io::Error::other("outbox is locked")))?;
    let _ = bn_core::events::append(
        "agent_msg_parked",
        serde_json::json!({ "id": id, "to": to, "from": from, "why": why })
            .as_object()
            .cloned()
            .unwrap_or_default(),
    );
    Ok(id)
}

/// The outcome of one retry: the `bn agent msg` JSON envelope's `ok`, and its error message.
fn retry(item: &Parked) -> (bool, String) {
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let out = std::process::Command::new(exe)
        .args(["agent", "msg", &item.to, "--from", &item.from, "--json", &item.text])
        .env(RETRY_ENV, "1")
        .stdin(std::process::Stdio::null())
        .output();
    let Ok(out) = out else {
        return (false, "could not run bn".into());
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    // A relayed call prints the remote envelope; the last line is the one that decides.
    let env = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok());
    match env {
        Some(v) if v["ok"].as_bool() == Some(true) => (true, String::new()),
        Some(v) => (false, v["error"]["message"].as_str().unwrap_or("not delivered").to_string()),
        None if out.status.success() => (true, String::new()),
        None => (false, String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

/// Send every parked message again, oldest first; keep the ones that still did not land.
/// Returns (delivered, still waiting).
pub(crate) fn flush() -> (usize, usize) {
    // One flush at a time, or two would send the same message twice. A busy one is skipped.
    locked("flush", false, flush_locked).unwrap_or((0, read().len()))
}

fn flush_locked() -> (usize, usize) {
    let items = read();
    if items.is_empty() {
        return (0, 0);
    }
    let seen: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
    let mut keep = Vec::new();
    let mut sent = 0;
    for mut it in items {
        let (ok, why) = retry(&it);
        if ok {
            sent += 1;
            let _ = bn_core::events::append(
                "agent_msg_unparked",
                serde_json::json!({ "id": it.id, "to": it.to, "tries": it.tries + 1 })
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            );
        } else {
            it.tries += 1;
            it.why = why;
            keep.push(it);
        }
    }
    let waiting = keep.len();
    // Messages parked while this flush ran were appended after it read the file; keep them too.
    locked("file", true, || {
        keep.extend(read().into_iter().filter(|p| !seen.contains(&p.id)));
        let _ = write(&keep);
    });
    (sent, waiting)
}

/// `bn agent outbox [--flush] [--json]`.
pub(crate) fn emit(flush_now: bool, drop: Option<&str>, as_json: bool) {
    if let Some(id) = drop {
        let removed = locked("file", true, || {
            let mut items = read();
            let before = items.len();
            items.retain(|p| p.id != id);
            let removed = items.len() < before;
            if removed {
                let _ = write(&items);
            }
            removed
        })
        .unwrap_or(false);
        if !removed {
            eprintln!("bn agent outbox: no parked message {id}");
            std::process::exit(1);
        }
        let _ = bn_core::events::append(
            "agent_msg_dropped",
            serde_json::json!({ "id": id }).as_object().cloned().unwrap_or_default(),
        );
        println!("dropped {id}");
        return;
    }
    let (sent, _) = if flush_now { flush() } else { (0, 0) };
    let items = read();
    if as_json {
        let data = serde_json::json!({ "delivered_now": sent, "waiting": items });
        println!("{}", crate::Envelope::success("agent_outbox", data).to_json());
        return;
    }
    if flush_now && sent > 0 {
        println!("delivered {sent} parked message(s)");
    }
    if items.is_empty() {
        if !flush_now {
            println!("outbox empty: every message was delivered");
        }
        return;
    }
    println!("{} message(s) waiting for delivery (retried every minute by the tunnel watchdog):", items.len());
    for it in &items {
        let age = (now() - it.at) / 60;
        println!("  {}  → {}  ({} tries, {age}m)  {}", it.id, it.to, it.tries, it.why);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parked_message_round_trips_through_the_file_format() {
        let it = Parked {
            id: "m1-2".into(),
            to: "byte_s:deps-1".into(),
            from: "bn:coord".into(),
            text: "it's \"quoted\"\nand multi-line".into(),
            at: 5,
            tries: 2,
            why: "unconfirmed".into(),
        };
        let line = serde_json::to_string(&it).unwrap();
        assert!(!line.contains('\n'), "one message, one line");
        assert_eq!(serde_json::from_str::<Parked>(&line).unwrap(), it);
    }
}
