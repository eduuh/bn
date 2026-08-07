//! `bn agent send` — the steer channels, and the tmux step plans they are built from.
//!
//! Split out of `cmd/agent.rs` so the file-size ratchet leaves room for the supervision commands
//! and so the two delivery channels live together:
//!
//!   * **resume** — one `-p --resume` turn in the agent's worktree. Reliable and it returns the
//!     agent's reply, but it is a *separate* headless turn, not the live session you are watching.
//!   * **keys** — paste into the live tmux window and submit with a verified raw CR. This is the
//!     channel that actually steers the session on screen.
//!
//! Re-exported by `cmd/agent.rs`, so every `cmd::agent::emit_agent_send`, `cmd::agent::TmuxStep`,
//! … path keeps resolving unchanged.

use super::{resolve_agent_session, resolve_tmux_target, tmux_exact_target};
use crate::*;

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
            c.args(bn_core::mcp::Harness::Copilot.yolo_args());
            c
        }
        "claude" => {
            let mut c = std::process::Command::new("claude");
            c.args(["-p", msg, "--resume", id, "--output-format", "json"]);
            c.args(bn_core::mcp::Harness::Claude.yolo_args());
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

/// One step of the `--keys` steer sequence: either a `tmux` invocation (the argv *after* `tmux`) or
/// a pause. Split out so [`keys_paste_steps`] / [`keys_submit_steps`] can be pure, unit-testable
/// plans while the executor in [`emit_agent_send`] just runs each step (tmux I/O isn't cleanly
/// testable).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TmuxStep {
    /// Run `tmux <args...>`.
    Cmd(Vec<String>),
    /// Sleep this many milliseconds.
    DelayMs(u64),
}

/// Build the tmux step plan that primes pane `target`'s input box with `msg` via a paste-buffer
/// named `buf`. Mirrors the seed launcher (`wt::launch_agent_in_window`): stage the message into a
/// buffer with `set-buffer` (a `--` guard so a leading `-` in `msg` isn't parsed as a flag),
/// `paste-buffer -d` it into the pane (one block, so an autosuggest/highlight TUI can't mangle a
/// char-by-char type; `-d` auto-deletes the buffer). Then dismiss any autocomplete popup the pasted
/// `@`/`#` may have opened with `Escape` and pause [`KEYS_SUBMIT_DELAY_MS`]. The submit is a
/// **separate** step ([`keys_submit_steps`]) so the executor can re-send and verify it. The target
/// is normalized to an exact-match name ([`tmux_exact_target`]). Pure: no tmux I/O.
pub(crate) fn keys_paste_steps(target: &str, buf: &str, msg: &str) -> Vec<TmuxStep> {
    let target = &tmux_exact_target(target);
    let cmd = |args: &[&str]| TmuxStep::Cmd(args.iter().map(|s| s.to_string()).collect());
    vec![
        cmd(&["set-buffer", "-b", buf, "--", msg]),
        cmd(&["paste-buffer", "-d", "-b", buf, "-t", target]),
        cmd(&["send-keys", "-t", target, "Escape"]),
        TmuxStep::DelayMs(KEYS_SUBMIT_DELAY_MS),
    ]
}

/// The submit step for a `--keys` steer: a **raw carriage return** via `send-keys -H 0d` (the literal
/// CR byte, 0x0d) rather than the named `Enter` key. Copilot's compose/autocomplete TUI was observed
/// to swallow the named `Enter` — leaving the message unsent in the box — while the raw CR submits
/// (#306). Kept separate + pure so the executor can re-send it under pane verification. The target is
/// normalized to an exact-match name ([`tmux_exact_target`]) so the CR lands in the same pane the
/// paste primed, never a prefix/fnmatch-resolved neighbor.
pub(crate) fn keys_submit_steps(target: &str) -> Vec<TmuxStep> {
    let target = tmux_exact_target(target);
    vec![TmuxStep::Cmd(
        ["send-keys", "-t", &target, "-H", "0d"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    )]
}

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
    crate::proc::tmux_ok(&["display-message", "-p", "-t", &exact, "#{window_id}"])
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
    let run_steps = |steps: Vec<TmuxStep>| -> bool {
        for step in steps {
            match step {
                TmuxStep::Cmd(args) => {
                    if !proc::tmux_ok(&args) {
                        return false;
                    }
                }
                TmuxStep::DelayMs(ms) => {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                }
            }
        }
        true
    };
    // Resolve the identity to the window that actually hosts it once, and steer that — the window
    // is rarely named `repo:branch` (pool slots, reused directories), and every step must address
    // the same pane.
    let window = resolve_tmux_target(cfg, target);
    let buf = format!("bn-steer-{}", bn_core::util::now_epoch_secs());
    let pasted = run_steps(keys_paste_steps(&window, &buf, msg));
    let mut confirmed = false;
    if pasted {
        for _ in 0..agent_live::SUBMIT_BACKOFF.submit_attempts() {
            if !run_steps(keys_submit_steps(&window)) {
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

/// Send a message to a running agent.
///
/// `--channel auto` (the default) picks from the pane: a target with a live tmux window is steered
/// **in that window** — paste via a tmux buffer ([`keys_paste_steps`]), submit with a raw carriage
/// return ([`keys_submit_steps`]), and verify the pane took it, retrying the CR and escalating to
/// `-p --resume` if it still can't confirm (#306). A target with no window falls back to `-p
/// --resume`, which returns the agent's actual reply.
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
        if let Some(prompt) = super::read_prompt(cfg, target, bn_core::agent::PROMPT_SCAN_LINES) {
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
                let mut env = Envelope::failure("agent_send", "prompt_pending", &msg);
                env.data = serde_json::json!({ "target": target, "prompt": prompt });
                println!("{}", env.to_json());
            } else {
                eprintln!("bn agent send: {msg}");
            }
            std::process::exit(1);
        }
    }

    let keys = choose_channel(cfg, target, channel) == SteerChannel::Keys;
    let session = resolve_agent_session(cfg, target);

    // Was the agent already mid-turn before we typed anything? This decides how a keys steer is
    // *verified*, and it is not a detail: the verification is "a spinner appeared", which proves
    // nothing when a spinner was already there. On a working pane the harness queues the paste and
    // reads it when the turn ends — the correct outcome — so an unconfirmed submit must NOT
    // escalate to a `-p --resume` turn. Escalating there forks a second session against the same
    // transcript (delivering the message twice) and blocks the caller for a whole agent turn.
    let was_working = keys
        && super::pane_state(cfg, target) == Some(bn_core::orchestrate::PaneState::Working);

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
