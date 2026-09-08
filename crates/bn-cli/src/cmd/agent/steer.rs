//! `bn agent send` — the steer channels and their delivery logic.
//!
//! Split out of `cmd/agent.rs` so the file-size ratchet leaves room for the supervision commands
//! and so the two delivery channels live together:
//!
//!   * **resume** — one `-p --resume` turn in the agent's worktree. Reliable and it returns the
//!     agent's reply, but it is a *separate* headless turn, not the live session you are watching.
//!   * **keys** — paste into the live tmux window and submit with a verified raw CR. This is the
//!     channel that actually steers the session on screen.
//!
//! Re-exported by `cmd/agent.rs`, so every `cmd::agent::emit_agent_send`, … path keeps resolving
//! unchanged.

use super::{
    resolve_agent_session, resolve_agent_transport, resolve_tmux_target, tmux_exact_target,
};
use crate::*;
use bn_core::agent::{AgentTransport as _, Key};

/// Build the `-p --resume` command for a harness (shared by the sync + detached steer paths).
/// `None` for an unknown harness.
pub(crate) fn resume_command(
    harness: &str,
    id: &str,
    wt: &std::path::Path,
    msg: &str,
) -> Option<std::process::Command> {
    let mut c = match harness {
        "copilot" => {
            let mut c = std::process::Command::new("copilot");
            c.args(["-p", msg, &format!("--resume={id}")]);
            c.args(bn_core::mcp::Harness::Copilot.autonomy_args());
            c
        }
        "claude" => {
            let mut c = std::process::Command::new("claude");
            c.args(["-p", msg, "--resume", id, "--output-format", "json"]);
            c.args(bn_core::mcp::Harness::Claude.autonomy_args());
            c
        }
        _ => return None,
    };
    c.current_dir(wt);
    Some(c)
}

/// Steer an agent by running one reliable `-p --resume` turn in its worktree and returning the
/// agent's reply. Works for both harnesses and even against a live-held session (verified), unlike
/// send-keys. `None` if the harness is unknown or the process failed. **Blocks** for the whole turn.
pub(crate) fn steer_via_resume(
    harness: &str,
    id: &str,
    wt: &std::path::Path,
    msg: &str,
) -> Option<String> {
    let out = resume_command(harness, id, wt, msg)?.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if harness == "claude" {
        // Structured: pull `.result`; fall back to raw stdout if it isn't the JSON envelope.
        return serde_json::from_str::<serde_json::Value>(&stdout)
            .ok()
            .and_then(|v| v.get("result").and_then(|r| r.as_str()).map(String::from))
            .or(Some(stdout));
    }
    Some(stdout) // copilot: stdout carries the reply (stats go to stderr)
}

/// Fire a `-p --resume` turn **detached** and return immediately (its log path on success). The
/// child runs in its OWN process group (so a killed caller — e.g. a timed-out shell — doesn't take
/// it down) with output redirected to a log file. For long ship tasks: the agent works in the
/// background and reports back via `bn agent done`. `None` for an unknown harness / spawn failure.
pub(crate) fn steer_detached(
    harness: &str,
    id: &str,
    wt: &std::path::Path,
    msg: &str,
) -> Option<std::path::PathBuf> {
    let mut cmd = resume_command(harness, id, wt, msg)?;
    let log = bn_core::resolver::state_dir().join(format!(
        "steer-{}-{}.log",
        harness,
        bn_core::util::now_epoch_secs()
    ));
    if let Some(p) = log.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let out = std::fs::File::create(&log).ok()?;
    let err = out.try_clone().ok()?;
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(out))
        .stderr(std::process::Stdio::from(err));
    // Own process group so the detached turn survives the caller's group being killed.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().ok().map(|_| log)
}

/// Short pause after Escape (popup dismiss) before the submitting carriage return, giving the TUI a
/// moment to close any autocomplete popup so the CR lands on the message line rather than the popup.
pub(crate) const KEYS_SUBMIT_DELAY_MS: u64 = 120;

/// Print the outcome of a synchronous `-p --resume` steer turn — shared by the primary resume path
/// and the keys-fallback escalation, so both report identically.
pub(crate) fn report_resume(target: &str, harness: &str, msg: &str, reply: &str, as_json: bool) {
    if as_json {
        let data = serde_json::json!({
            "delivered": true, "via": "resume", "target": target,
            "harness": harness, "msg": msg, "reply": reply,
        });
        println!("{}", Envelope::success("agent_send", data).to_json());
    } else {
        println!("→ {target} ({harness}, resume): {msg}");
        if !reply.is_empty() {
            println!("← {reply}");
        }
    }
}

/// Which channel a steer is delivered over.
///
/// The two are not interchangeable, and treating them as if they were is what made steering
/// unreliable. `Resume` runs a **separate headless turn** against the same transcript: excellent
/// for asking a question and getting an answer back, useless for unblocking the session you are
/// watching, because the live pane never sees it. `Keys` types into that live pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SteerChannel {
    /// Pick from what the pane is doing — see [`choose_channel`].
    Auto,
    /// Force the live tmux pane.
    Keys,
    /// Force a `-p --resume` turn.
    Resume,
}

impl SteerChannel {
    pub(crate) fn parse(s: Option<&str>, keys_flag: bool) -> SteerChannel {
        if keys_flag {
            return SteerChannel::Keys;
        }
        match s.map(str::trim) {
            Some("keys") => SteerChannel::Keys,
            Some("resume") => SteerChannel::Resume,
            _ => SteerChannel::Auto,
        }
    }
}

/// Resolve `Auto` against the live pane.
///
/// A target with a live window is a session someone is watching, so a steer belongs *in* it — the
/// old default sent every message down `--resume` instead, which forks a turn the pane never
/// shows. Only a target with no window falls back to resume, where it is the sole option rather
/// than a silent second session.
fn choose_channel(cfg: &Config, target: &str, requested: SteerChannel) -> SteerChannel {
    match requested {
        SteerChannel::Auto if has_live_window(cfg, target) => SteerChannel::Keys,
        SteerChannel::Auto => SteerChannel::Resume,
        explicit => explicit,
    }
}

fn has_live_window(cfg: &Config, target: &str) -> bool {
    let exact = tmux_exact_target(&resolve_tmux_target(cfg, target));
    bn_core::agent::TmuxTransport::new(&exact).liveness() == bn_core::agent::Liveness::Live
}

/// Deliver `text` to `target` over the auto-selected channel, quietly. The shared primitive behind
/// `bn agent instruct`'s live half. Returns whether it landed.
pub(crate) fn deliver_instruction(cfg: &Config, target: &str, text: &str) -> bool {
    match choose_channel(cfg, target, SteerChannel::Auto) {
        SteerChannel::Keys => deliver_via_keys(cfg, target, text).0,
        _ => match resolve_agent_session(cfg, target) {
            Some((wt, harness, id)) => steer_via_resume(&harness, &id, &wt, text).is_some(),
            None => false,
        },
    }
}

/// Paste `msg` into `target`'s live pane and submit it, verifying the pane took it.
///
/// We *paste* via a tmux buffer rather than `send-keys <msg>` char-by-char — an autocomplete /
/// autosuggest TUI mangles a typed message and opens an `@files`/`#issues` popup that eats an
/// immediate Enter. After the paste we submit with a raw CR and confirm a working spinner rather
/// than firing blind, retrying the CR (a first CR often only dismisses the popup).
///
/// Returns `(confirmed, pasted)` so a caller can tell "no such window" from "typed it but the TUI
/// swallowed the Enter" — two failures with different fixes.
fn deliver_via_keys(cfg: &Config, target: &str, msg: &str) -> (bool, bool) {
    // Resolve both layers once: the window we verify against, and the transport that owns the
    // actual write.
    let window = resolve_tmux_target(cfg, target);
    let transport = resolve_agent_transport(cfg, target);
    let pasted =
        transport.write_keys(&[Key::Text(msg.to_string())]) && transport.write_keys(&[Key::Escape]);
    let mut confirmed = false;
    if pasted {
        std::thread::sleep(std::time::Duration::from_millis(KEYS_SUBMIT_DELAY_MS));
        for _ in 0..agent_live::SUBMIT_BACKOFF.submit_attempts() {
            if !transport.write_keys(&[Key::Submit]) {
                break;
            }
            if agent_live::steer_landed(&window) {
                confirmed = true;
                break;
            }
        }
    }
    (confirmed, pasted)
}

/// Refuse to type at an agent sitting on a **dialog**, and exit.
///
/// Pasting prose at a numbered menu is how a stray character becomes a menu selection nobody chose —
/// and the message would be swallowed by the prompt rather than read. The refusal names the options
/// and points at `bn agent answer`. `verb` is the `bn agent <verb>` being refused; it names both the
/// human prefix and the `agent_<verb>` JSON envelope so `send` and `msg` report identically.
fn refuse_if_dialog(cfg: &Config, verb: &str, target: &str, as_json: bool) {
    let Some(prompt) = super::read_prompt(cfg, target, bn_core::agent::PROMPT_SCAN_LINES) else {
        return;
    };
    let opts = prompt
        .options
        .iter()
        .map(|o| format!("{}. {}", o.number, o.label))
        .collect::<Vec<_>>()
        .join("; ");
    let msg = format!(
        "{target} is waiting on a dialog ({}) — answer it with `bn agent answer {target}`, \
         not a message. {} Options: {opts}",
        prompt.kind.label(),
        prompt.question
    );
    if as_json {
        let mut env = Envelope::failure(format!("agent_{verb}"), "prompt_pending", &msg);
        env.data = serde_json::json!({ "target": target, "prompt": prompt });
        println!("{}", env.to_json());
    } else {
        eprintln!("bn agent {verb}: {msg}");
    }
    std::process::exit(1);
}

/// Send a message to a running agent.
///
/// `--channel auto` (the default) picks from the pane: a target with a live tmux window is steered
/// **in that window** — the transport writes a whole-text key, then a raw carriage return, and the
/// caller verifies the pane took it, retrying the CR and escalating to `-p --resume` if it still
/// can't confirm (#306). A target with no window falls back to `-p --resume`, which returns the
/// agent's actual reply.
///
/// An agent sitting on a **dialog** is refused outright. Pasting prose at a numbered menu is how a
/// stray character becomes a menu selection nobody chose — and the message would be swallowed by
/// the prompt rather than read. The refusal names the options and points at `bn agent answer`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_agent_send(
    cfg: &Config,
    target: &str,
    msg: &str,
    channel: SteerChannel,
    detach: bool,
    as_json: bool,
) {
    // Guard the dialog case before anything is sent. `--channel resume` is exempt: a resume turn is
    // a separate session that never touches the blocked pane, so it cannot mis-answer the menu.
    if channel != SteerChannel::Resume {
        refuse_if_dialog(cfg, "send", target, as_json);
    }

    let keys = choose_channel(cfg, target, channel) == SteerChannel::Keys;
    let session = resolve_agent_session(cfg, target);

    // Was the agent already mid-turn before we typed anything? This decides how a keys steer is
    // *verified*, and it is not a detail: the verification is "a spinner appeared", which proves
    // nothing when a spinner was already there. On a working pane the harness queues the paste and
    // reads it when the turn ends — the correct outcome — so an unconfirmed submit must NOT
    // escalate to a `-p --resume` turn. Escalating there forks a second session against the same
    // transcript (delivering the message twice) and blocks the caller for a whole agent turn.
    let was_working =
        keys && super::pane_state(cfg, target) == Some(bn_core::orchestrate::PaneState::Working);

    // Reliable path: run a resume turn against the recorded session, unless --keys was asked. Track
    // a failure so a keys fallback that also can't confirm doesn't re-try a resume that already died.
    let mut resume_failed = false;
    if !keys {
        if let Some((wt, harness, id)) = &session {
            // Async: fire the turn detached and return now (for long ship tasks — the agent reports
            // back via `bn agent done`). It survives the caller exiting.
            if detach {
                let log = steer_detached(harness, id, wt, msg);
                let ok = log.is_some();
                if as_json {
                    let data = serde_json::json!({
                        "delivered": ok, "via": "resume-detached", "target": target,
                        "harness": harness, "msg": msg,
                        "log": log.as_ref().map(|p| p.to_string_lossy().into_owned()),
                    });
                    println!("{}", Envelope::success("agent_send", data).to_json());
                } else if ok {
                    println!("→ {target} ({harness}, detached): {msg}");
                    println!(
                        "  running in background — the agent will `bn agent done` when finished"
                    );
                } else {
                    fail(&format!(
                        "bn agent send: could not spawn detached turn for {target}"
                    ));
                }
                return;
            }
            match steer_via_resume(harness, id, wt, msg) {
                Some(reply) => {
                    report_resume(target, harness, msg, &reply, as_json);
                    return;
                }
                None => {
                    // Resume failed (session gone / harness error) — fall through to send-keys.
                    resume_failed = true;
                    eprintln!("bn agent send: resume steer failed for {target}, trying send-keys");
                }
            }
        }
    }

    // Fallback / --keys: deliver into the live tmux window. We *paste* the message via a tmux buffer
    // rather than `send-keys <msg>` char-by-char — an autocomplete/autosuggest TUI (copilot) mangles
    // a typed message and opens an @files/#issues popup that eats an immediate Enter. After the paste
    // we submit with a raw CR and CONFIRM the pane picked it up (a working spinner) rather than
    // firing blind: retry the CR (a first CR often only dismisses the popup), and if we still can't
    // confirm, escalate to the reliable `-p --resume` channel when a session id is known (#306).
    let (confirmed, pasted) = deliver_via_keys(cfg, target, msg);

    // Unconfirmed submit + a known session we haven't already exhausted → escalate to the reliable
    // resume channel. Guarded so the default path (which tried resume first) never re-tries it, and
    // skipped entirely for a pane that was already working: there the paste is *queued*, not lost,
    // and a resume turn would both duplicate it and block for the length of that turn.
    if !confirmed && !resume_failed && !was_working {
        if let Some((wt, harness, id)) = &session {
            if let Some(reply) = steer_via_resume(harness, id, wt, msg) {
                report_resume(target, harness, msg, &reply, as_json);
                return;
            }
        }
    }

    // On a pane that was mid-turn, "a spinner is up" is not evidence of anything, so delivery is
    // reported by what is actually known: the paste landed, and the harness will read it when the
    // current turn ends.
    let queued = was_working && pasted;

    if as_json {
        let data = serde_json::json!({
            "delivered": confirmed || queued, "via": "keys", "target": target, "msg": msg,
            "pasted": pasted, "confirmed": confirmed, "queued": queued,
        });
        let env = if confirmed || queued {
            Envelope::success("agent_send", data)
        } else if !pasted {
            Envelope::failure(
                "agent_send",
                "send_failed",
                "no such tmux window, or tmux absent",
            )
        } else {
            Envelope::failure(
                "agent_send",
                "unconfirmed_submit",
                "pasted into the input box but could not confirm submission (the TUI may have eaten the Enter); no session id recorded to escalate to --resume",
            )
        };
        println!("{}", env.to_json());
    } else if queued {
        println!("→ {target} (keys, queued): {msg}");
        println!("  the agent is mid-turn — it will read this when the turn ends");
    } else if confirmed {
        println!("→ {target} (keys): {msg}");
    } else if !pasted {
        fail(&format!(
            "bn agent send: couldn't deliver to '{target}' (no recorded session and no tmux window)"
        ));
    } else {
        fail(&format!(
            "bn agent send: pasted into '{target}' but could not confirm submission (the TUI may have eaten the Enter); no session id recorded to escalate to --resume"
        ));
    }
}

/// The line a peer message is delivered as. One line on purpose: no steer has ever pasted an
/// embedded newline, so a multi-line paste is unverified against either TUI. The sender's
/// `repo:branch` is stated up front and the reply command is spelled out, so the receiving agent
/// can answer without any store telling it who asked.
pub(crate) fn peer_envelope(sender: &str, text: &str) -> String {
    format!(
        "[bn msg from {sender}] {} — reply: bn agent msg {sender} \"<your answer>\"",
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    )
}

/// `repo:branch` as the tiered join compares it: repo case-folded, branch slashes dashed — the same
/// normalisation the resolver applies, so `bn:feat/x` and `bn:feat-x` name one agent.
fn normalize_target(target: &str) -> String {
    match target.split_once(':') {
        Some((repo, branch)) => {
            format!("{}:{}", repo.to_ascii_lowercase(), branch.replace('/', "-"))
        }
        None => target.to_ascii_lowercase(),
    }
}

/// The validation `bn agent msg` runs before touching tmux. Pure so the refusals are testable:
/// `Err((code, message))` is the failure envelope, `Ok(sender)` the resolved sender target.
fn validate_msg(target: &str, sender: Option<&str>) -> Result<String, (&'static str, String)> {
    if !target.contains(':') || target.starts_with(':') || target.ends_with(':') {
        return Err((
            "invalid_target",
            format!("'{target}' is not a repo:branch target (see `bn agent live`)"),
        ));
    }
    let sender = sender.ok_or_else(|| {
        (
            "no_sender_context",
            "not inside a git worktree — pass --from <repo:branch>".to_string(),
        )
    })?;
    if !sender.contains(':') || sender.starts_with(':') || sender.ends_with(':') {
        return Err((
            "invalid_sender",
            format!("--from '{sender}' is not a repo:branch target"),
        ));
    }
    if normalize_target(target) == normalize_target(sender) {
        return Err((
            "self_target",
            format!("{target} is this agent — a message to yourself would land as your own next prompt"),
        ));
    }
    Ok(sender.to_string())
}

fn fail_msg(code: &str, msg: &str, as_json: bool) -> ! {
    if as_json {
        println!("{}", Envelope::failure("agent_msg", code, msg).to_json());
    } else {
        eprintln!("bn agent msg: {msg}");
    }
    std::process::exit(1);
}

/// Message another live agent, peer to peer: `bn agent msg <repo:branch> "<text>"`.
///
/// Where `bn agent send` is a human (or the dashboard) steering an agent, this is one agent asking
/// another for something — so the sender's own `repo:branch` rides in the delivered line, and the
/// reply is just another `msg` back. The text lands in the target's live pane as its next prompt;
/// a target mid-turn queues it and reads it when the turn ends.
///
/// Live-only, by design. There is no `-p --resume` fallback: that forks a headless turn the session
/// on screen never sees, which for two agents talking is a phantom conversation. A target with no
/// live window is refused instead, and `bn agent live` says who can be reached.
pub(crate) fn emit_agent_msg(
    cfg: &Config,
    target: &str,
    text: &str,
    from: Option<&str>,
    as_json: bool,
) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let resolved = bn_core::resolver::resolve(cfg, &cwd)
        .ok()
        .map(|ctx| format!("{}:{}", ctx.repo, ctx.branch));
    let sender = match validate_msg(target, from.or(resolved.as_deref())) {
        Ok(sender) => sender,
        Err((code, msg)) => fail_msg(code, &msg, as_json),
    };
    refuse_if_dialog(cfg, "msg", target, as_json);
    if !has_live_window(cfg, target) {
        fail_msg(
            "no_live_agent",
            &format!("{target} has no live agent window — `bn agent live` lists who can be reached"),
            as_json,
        );
    }

    // Same verification caveat as `send`: on a pane that was already working, "a spinner is up"
    // proves nothing, so an unconfirmed submit there is a *queued* message, not a lost one.
    let was_working =
        super::pane_state(cfg, target) == Some(bn_core::orchestrate::PaneState::Working);
    let (confirmed, pasted) = deliver_via_keys(cfg, target, &peer_envelope(&sender, text));
    let queued = was_working && pasted;
    let delivered = confirmed || queued;

    let mut fields = serde_json::Map::new();
    fields.insert("from".into(), sender.clone().into());
    fields.insert("to".into(), target.into());
    fields.insert("text".into(), text.into());
    fields.insert("delivered".into(), delivered.into());
    fields.insert("confirmed".into(), confirmed.into());
    fields.insert("queued".into(), queued.into());
    let _ = bn_core::events::append("agent_msg", fields);

    if as_json {
        let data = serde_json::json!({
            "from": sender, "to": target, "text": text,
            "delivered": delivered, "pasted": pasted, "confirmed": confirmed, "queued": queued,
        });
        let env = if delivered {
            Envelope::success("agent_msg", data)
        } else if !pasted {
            Envelope::failure("agent_msg", "send_failed", "no such tmux window, or tmux absent")
        } else {
            Envelope::failure(
                "agent_msg",
                "unconfirmed_submit",
                "pasted into the input box but could not confirm submission (the TUI may have eaten the Enter)",
            )
        };
        println!("{}", env.to_json());
    } else if queued {
        println!("→ {target} (queued): {text}");
        println!("  the agent is mid-turn — it will read this when the turn ends");
    } else if confirmed {
        println!("→ {target}: {text}");
    } else if !pasted {
        fail_msg("send_failed", &format!("couldn't paste into '{target}' (no tmux window)"), as_json);
    } else {
        fail_msg(
            "unconfirmed_submit",
            &format!("pasted into '{target}' but could not confirm submission (the TUI may have eaten the Enter)"),
            as_json,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_names_sender_and_reply_command_on_one_line() {
        let line = peer_envelope("bn:feat-a", "please bump the schema");
        assert_eq!(
            line,
            "[bn msg from bn:feat-a] please bump the schema — reply: bn agent msg bn:feat-a \"<your answer>\""
        );
        assert!(!line.contains('\n'));
    }

    #[test]
    fn envelope_flattens_multiline_text() {
        let line = peer_envelope("bn:a", "first line\n  second line\n");
        assert!(!line.contains('\n'));
        assert!(line.contains("first line second line — reply:"));
    }

    #[test]
    fn validate_accepts_a_distinct_peer() {
        assert_eq!(validate_msg("bn:feat-b", Some("bn:feat-a")).unwrap(), "bn:feat-a");
    }

    #[test]
    fn validate_refuses_self_after_normalisation() {
        let err = validate_msg("bn:feat-a", Some("BN:feat/a")).unwrap_err();
        assert_eq!(err.0, "self_target");
    }

    #[test]
    fn validate_refuses_missing_sender_and_bad_shapes() {
        assert_eq!(validate_msg("bn:feat-b", None).unwrap_err().0, "no_sender_context");
        assert_eq!(validate_msg("feat-b", Some("bn:a")).unwrap_err().0, "invalid_target");
        assert_eq!(validate_msg("bn:", Some("bn:a")).unwrap_err().0, "invalid_target");
        assert_eq!(validate_msg("bn:b", Some("nocolon")).unwrap_err().0, "invalid_sender");
    }
}
