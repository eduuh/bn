//! Reading a running agent: its live tmux pane, and its session transcript.
//!
//! The read-only half of supervision, and the layer `bn agent prompt` sits on. Split out of
//! `cmd/agent.rs` to keep it under the file-size ratchet.

use crate::*;

/// Capture the last `lines` lines of a running agent's tmux pane (read-only). Powers the web
/// dashboard's live-pane view: the target is addressed exactly (`=repo:branch`) so a capture never
/// resolves a prefix/fnmatch neighbour, tmux's bottom padding of blank lines is trimmed, and the
/// trailing `lines` are returned. Emits a `render::Envelope` so the web API / MCP / CLI share one
/// shape; `--json` carries `{target, lines:[…], count}`. With `ansi`, `capture-pane -e` keeps the
/// pane's SGR escape sequences so a terminal renderer (the web live terminal) shows real colours.
pub(crate) fn emit_agent_pane(
    cfg: &Config,
    target: &str,
    lines: usize,
    ansi: bool,
    as_json: bool,
) {
    let target = target.trim();
    // Guard the shape (`repo:branch`) so a malformed/empty input can't widen the tmux target. The
    // arg is passed to tmux as a distinct argv element (never a shell), so there's no injection.
    if target.is_empty() || !target.contains(':') {
        let msg = format!("invalid agent target '{target}' (expected repo:branch)");
        if as_json {
            println!(
                "{}",
                Envelope::failure("agent_pane", "invalid_target", &msg).to_json()
            );
        } else {
            eprintln!("bn agent pane: {msg}");
        }
        return;
    }

    let exact = resolve_tmux_target(cfg, target);
    // `-e` includes SGR (colour/attribute) escapes for a real-terminal render; without it the tail
    // is plain text. `-e` covers only text attributes, not cursor motion, so the frame stays
    // line-oriented (safe to re-render in a resized xterm).
    // `-J` joins lines tmux soft-wrapped at the pane width, so a long line arrives as the one
    // logical line the program printed rather than split at whatever column the pane happened to
    // be. Display-only paths only — the pane CLASSIFIERS (transport.rs, reap.rs, agent_live.rs)
    // deliberately still capture unjoined: `is_footer_hint` caps a hint at 60 chars and
    // `has_bare_prompt` compares a trimmed line for exact equality, so joining would silently
    // change what they detect. See the classifier fixtures todo before flipping those.
    let mut cap_args: Vec<&str> = vec!["capture-pane", "-p", "-J"];
    if ansi {
        cap_args.push("-e");
    }
    cap_args.extend(["-t", &exact]);
    let Some(raw) = crate::proc::tmux_raw(&cap_args) else {
        let msg = "tmux unreachable or no such window";
        if as_json {
            println!(
                "{}",
                Envelope::failure("agent_pane", "capture_failed", msg).to_json()
            );
        } else {
            eprintln!("bn agent pane: {target}: {msg}");
        }
        return;
    };

    let tail = pane_tail(&raw, lines);

    if as_json {
        let data = serde_json::json!({
            "target": target,
            "lines": tail,
            "count": tail.len(),
        });
        println!("{}", Envelope::success("agent_pane", data).to_json());
    } else {
        for l in &tail {
            println!("{l}");
        }
    }
}

/// `bn agent activity <session>` — the session's transcript as a readable timeline (prompts, agent
/// prose, collapsed tool runs). The companion to `pane` for the web dashboard: `pane` mirrors a
/// live tmux window, this reads the transcript, so it still works once the window has closed. A
/// session id with no transcript under either harness root is a `no_transcript` failure envelope
/// rather than an empty success — "we can't find this session" and "this session did nothing" are
/// different answers.
pub(crate) fn emit_agent_activity(session: &str, limit: usize, as_json: bool) {
    let session = session.trim();
    let Some(activity) = bn_core::agent::activity_for(session, limit) else {
        let msg = format!("no transcript for session '{session}' under either harness");
        if as_json {
            println!(
                "{}",
                Envelope::failure("agent_activity", "no_transcript", &msg).to_json()
            );
        } else {
            eprintln!("bn agent activity: {msg}");
        }
        return;
    };

    if as_json {
        let data = serde_json::json!({
            "session_id": activity.session_id,
            "harness": activity.harness,
            "entries": activity.entries,
            "total": activity.total,
            "count": activity.entries.len(),
        });
        println!("{}", Envelope::success("agent_activity", data).to_json());
        return;
    }

    for e in &activity.entries {
        match e.kind {
            // Speaker labels are structure, not severity, so they take the bold head and the one
            // non-severity tint rather than borrowing a colour that elsewhere means "on fire".
            bn_core::agent::EntryKind::User => println!("\n{}\n{}", style::head("you"), e.text),
            bn_core::agent::EntryKind::Agent => {
                println!("\n{}\n{}", style::accent("agent"), e.text)
            }
            bn_core::agent::EntryKind::Tools => {
                println!("\n{}", style::meta(&format!("· {}", e.summary)))
            }
        }
    }
    println!(
        "\n{}",
        style::meta(&format!(
            "{} of {} entries · {}",
            activity.entries.len(),
            activity.total,
            activity.harness
        ))
    );
}

/// The last `n` lines of a captured pane: right-trim each line, drop tmux's bottom padding of blank
/// lines, then keep the trailing `n`. Interior blank lines are preserved so the pane's layout
/// survives; only the padded tail is dropped. Pure, so it's unit-testable without tmux.
pub(crate) fn pane_tail(raw: &str, n: usize) -> Vec<String> {
    bn_core::agent::tail(raw, n)
}
