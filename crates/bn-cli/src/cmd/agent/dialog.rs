//! `bn agent prompt` / `bn agent answer` — read the dialog a blocked agent is on, and answer it.
//!
//! The pair replaces guessing. Before this, the only way to clear a permission prompt from outside
//! the pane was to send a bare digit — the old quick-steer menu offered "Approve (1)" and
//! "Approve (2)" and
//! fired that number blind. On Claude's dialog, option 2 is "Yes, and don't ask again" but option 3
//! is "No, and tell Claude what to do differently"; one keystroke off and an approval becomes a
//! rejection into a composer nobody is going to fill in.
//!
//! So answering here is *resolved and verified*, in this order:
//!
//!   1. read the pane and parse the dialog ([`bn_core::agent::parse_pending_prompt`]),
//!   2. refuse if the caller's `--expect` fingerprint no longer matches — the pane moved, and the
//!      option numbers computed against the old dialog would land on a different question,
//!   3. resolve the requested semantic against the options *actually offered*, refusing if the
//!      harness never offered it,
//!   4. send the digit to the exact window, then re-read and confirm the dialog is **gone**.
//!
//! Every refusal sends nothing at all and exits non-zero.

use super::{resolve_tmux_target, tmux_exact_target};
use crate::*;
use bn_core::agent::AgentTransport as _;
use bn_core::agent::{PendingPrompt, PromptSemantic, PROMPT_SCAN_LINES};

/// How long to wait for the pane to repaint after a digit, before re-reading it. A TUI redraws in
/// well under this; the retry loop covers the slow case.
const ANSWER_SETTLE_MS: u64 = 250;

/// How many times to re-send the submitting return before giving up. A first return is often eaten
/// dismissing an autocomplete popup — the same behaviour `bn agent send --keys` compensates for.
const ANSWER_ATTEMPTS: usize = 3;

/// Validate the `repo:branch` shape. The value reaches tmux as a distinct argv element (never a
/// shell), so this is not injection defence — it is "don't widen the target and steer the wrong
/// agent" defence, which matters more when the next thing we do is press a key in someone's pane.
fn check_target(cmd: &str, target: &str, as_json: bool) -> bool {
    if !target.is_empty() && target.contains(':') {
        return true;
    }
    let msg = format!("invalid agent target '{target}' (expected repo:branch)");
    emit_failure(cmd, "invalid_target", &msg, as_json);
    false
}

fn emit_failure(cmd: &str, code: &str, msg: &str, as_json: bool) {
    if as_json {
        println!("{}", Envelope::failure(cmd, code, msg).to_json());
    } else {
        eprintln!("bn agent {}: {msg}", cmd.trim_start_matches("agent_"));
    }
    std::process::exit(1);
}

/// Capture a pane tail deep enough to hold a whole dialog box, plus the window's Plane-A state.
fn read_pane(cfg: &Config, target: &str, lines: usize) -> Option<(Vec<String>, String)> {
    let exact = resolve_tmux_target(cfg, target);
    // The screen comes through the transport; Plane A does not. `@bn_agent` is a *window* option
    // the agent sets from inside itself, so it is transport-independent by design and stays a
    // direct read.
    let screen = bn_core::agent::TmuxTransport::new(&exact).read_screen(lines)?;
    let plane_a = crate::proc::tmux_raw(&["show-options", "-w", "-v", "-t", &exact, "@bn_agent"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    Some((screen, plane_a))
}

/// Parse the dialog on `target`'s pane, or `None` when it is not blocked on one.
pub(crate) fn read_prompt(cfg: &Config, target: &str, lines: usize) -> Option<PendingPrompt> {
    let (tail, plane_a) = read_pane(cfg, target, lines)?;
    bn_core::agent::parse_pending_prompt(&tail, &plane_a, true, false)
}

/// Classify `target`'s pane, scanning deeply enough to actually find a spinner.
///
/// The fleet board's own glance uses a 6-line tail, which is fine for a board and wrong for a
/// decision: a harness prints its spinner above the input box and the mode line, so on a wide
/// session it sits well outside six lines. A steer that mistook a working agent for an idle one
/// escalated to a *forked* `-p --resume` turn — duplicate delivery, and a block for the length of
/// a whole agent turn. Hence [`PROMPT_SCAN_LINES`], the same depth the dialog reader uses.
pub(crate) fn pane_state(cfg: &Config, target: &str) -> Option<bn_core::orchestrate::PaneState> {
    let (tail, plane_a) = read_pane(cfg, target, PROMPT_SCAN_LINES)?;
    Some(bn_core::orchestrate::classify(
        &bn_core::orchestrate::PaneObservation {
            tail: &tail,
            has_window: true,
            plane_a: &plane_a,
            has_wip: false,
        },
    ))
}

/// `bn agent prompt <target>` — the read half of supervision.
pub(crate) fn emit_prompt(cfg: &Config, target: &str, lines: usize, as_json: bool) {
    let cmd = "agent_prompt";
    let target = target.trim();
    if !check_target(cmd, target, as_json) {
        return;
    }
    let Some((tail, plane_a)) = read_pane(cfg, target, lines) else {
        emit_failure(cmd, "capture_failed", "tmux unreachable or no such window", as_json);
        return;
    };
    let Some(prompt) = bn_core::agent::parse_pending_prompt(&tail, &plane_a, true, false) else {
        // Not an error state so much as an answer: "nothing is waiting on you". Still non-zero, so
        // a script can gate on it without parsing output.
        emit_failure(cmd, "no_pending_prompt", &format!("{target} is not waiting on a dialog"), as_json);
        return;
    };
    if as_json {
        let data = serde_json::json!({ "target": target, "prompt": prompt });
        println!("{}", Envelope::success(cmd, data).to_json());
    } else {
        print_prompt(target, &prompt);
    }
}

fn print_prompt(target: &str, p: &PendingPrompt) {
    println!("\x1b[1m{target}\x1b[0m — \x1b[33m{}\x1b[0m", p.kind.label());
    if let Some(subject) = &p.subject {
        println!("  {subject}");
    }
    println!("  \x1b[1m{}\x1b[0m", p.question);
    for o in &p.options {
        let cursor = if o.selected { "\u{276f}" } else { " " };
        println!("  {cursor} {}. {}  \x1b[2m[{}]\x1b[0m", o.number, o.label, o.semantic.label());
    }
    println!("  \x1b[2mfingerprint {} — pass as `--expect` to bn agent answer\x1b[0m", p.fingerprint);
}

/// `bn agent answer <target>` — the write half.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_answer(
    cfg: &Config,
    target: &str,
    option: Option<u8>,
    approve: bool,
    approve_session: bool,
    reject: bool,
    expect: Option<u64>,
    as_json: bool,
) {
    let cmd = "agent_answer";
    let target = target.trim();
    if !check_target(cmd, target, as_json) {
        return;
    }

    let semantic = match (approve, approve_session, reject) {
        (true, _, _) => Some(PromptSemantic::Approve),
        (_, true, _) => Some(PromptSemantic::ApproveSession),
        (_, _, true) => Some(PromptSemantic::Reject),
        _ => None,
    };
    if option.is_none() && semantic.is_none() {
        emit_failure(
            cmd,
            "no_choice",
            "say which option: --option N, --approve, --approve-session or --reject",
            as_json,
        );
        return;
    }

    let Some(prompt) = read_prompt(cfg, target, PROMPT_SCAN_LINES) else {
        emit_failure(
            cmd,
            "no_pending_prompt",
            &format!("{target} is not waiting on a dialog — nothing to answer"),
            as_json,
        );
        return;
    };

    // The pane moved between the read that produced `expect` and now. The option numbers the
    // caller reasoned about belong to a dialog that is no longer on screen, so answering would
    // approve something nobody read. Refuse and report the current dialog instead.
    if let Some(expected) = expect {
        if expected != prompt.fingerprint {
            let msg = format!(
                "the pane moved since you read it (expected fingerprint {expected}, now {}) — re-read with `bn agent prompt {target}`",
                prompt.fingerprint
            );
            if as_json {
                // Carry the dialog that *is* on screen in the failure envelope, so a client can
                // re-render and let the human answer the real question in one round-trip.
                let mut env = Envelope::failure(cmd, "prompt_moved", &msg);
                env.data = serde_json::json!({
                    "target": target, "answered": false, "prompt": prompt,
                });
                println!("{}", env.to_json());
                std::process::exit(1);
            }
            emit_failure(cmd, "prompt_moved", &msg, as_json);
            return;
        }
    }

    let chosen = match (option, semantic) {
        (Some(n), _) => {
            if !prompt.options.iter().any(|o| o.number == n) {
                emit_failure(cmd, "option_unavailable", &offered(&prompt, &format!("option {n}")), as_json);
                return;
            }
            n
        }
        (_, Some(s)) => match prompt.resolve(s) {
            Some(n) => n,
            None => {
                emit_failure(cmd, "option_unavailable", &offered(&prompt, s.label()), as_json);
                return;
            }
        },
        _ => unreachable!("guarded above"),
    };
    let picked = prompt.options.iter().find(|o| o.number == chosen);
    let picked_label = picked.map(|o| o.label.clone()).unwrap_or_default();
    let picked_semantic = picked.map(|o| o.semantic).unwrap_or(PromptSemantic::Other);

    let (verified, after) = send_answer(cfg, target, chosen);

    if as_json {
        let data = serde_json::json!({
            "target": target,
            "answered": true,
            "option": chosen,
            "label": picked_label,
            "semantic": picked_semantic.label(),
            "verified": verified,
            "prompt_after": after,
        });
        println!("{}", Envelope::success(cmd, data).to_json());
    } else if verified {
        println!("\u{2192} {target}: answered {chosen}. {picked_label} [{}]", picked_semantic.label());
    } else {
        // Deliberately not a hard failure: the digit was sent and may well have landed — a dialog
        // that redraws into *another* dialog (a second permission in the same turn) is the common
        // case. Say what is true rather than claiming success.
        eprintln!(
            "bn agent answer: sent {chosen} to {target} but the dialog is still up — re-read with `bn agent prompt {target}`"
        );
        std::process::exit(1);
    }
}

/// The "you asked for X, the harness offered Y" message. Naming the real options is the difference
/// between a caller that can retry correctly and one that guesses again.
fn offered(prompt: &PendingPrompt, wanted: &str) -> String {
    let list = prompt
        .options
        .iter()
        .map(|o| format!("{}. {} [{}]", o.number, o.label, o.semantic.label()))
        .collect::<Vec<_>>()
        .join("; ");
    format!("this dialog does not offer {wanted}. It offers: {list}")
}

/// Answer `target`'s dialog with `option`, verifying it cleared. The primitive the auto-approve
/// sweep uses — it has already decided, so it needs the send, not the resolution.
pub(crate) fn answer_option(cfg: &Config, target: &str, option: u8) -> bool {
    send_answer(cfg, target, option).0
}

/// Send the chosen digit and confirm the dialog cleared. Returns `(verified, prompt_after)`.
///
/// Verification is "the dialog is gone", not "a spinner appeared": a harness that redraws the same
/// menu because the keystroke was swallowed also shows no spinner, and reporting that as delivered
/// is exactly the lie this command exists to stop telling.
fn send_answer(cfg: &Config, target: &str, option: u8) -> (bool, Option<PendingPrompt>) {
    let exact = tmux_exact_target(&resolve_tmux_target(cfg, target));
    let digit = option.to_string();
    let mut last: Option<PendingPrompt> = None;
    for attempt in 0..ANSWER_ATTEMPTS {
        // The digit alone selects on most harness menus; the return commits it. Sent as separate
        // key events so a menu that acts on the digit immediately is not also given a stray return.
        if !crate::proc::tmux_ok(&["send-keys", "-t", &exact, &digit]) {
            return (false, None);
        }
        crate::proc::tmux_ok(&["send-keys", "-t", &exact, "-H", "0d"]);
        std::thread::sleep(std::time::Duration::from_millis(
            ANSWER_SETTLE_MS * (attempt as u64 + 1),
        ));
        last = read_prompt(cfg, target, PROMPT_SCAN_LINES);
        if last.is_none() {
            return (true, None);
        }
    }
    (false, last)
}
