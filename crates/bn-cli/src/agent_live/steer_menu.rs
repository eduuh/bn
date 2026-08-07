//! `bn fleet steer` — the quick-steer menus (#633 P3).
//!
//! Steer an agent from a fleet row *without* navigating into its pane: a native `display-menu` of
//! the live fleet, each row opening a single-key action submenu. The mobile / one-handed
//! counterpart to hand-navigating a tmux window and typing.
//!
//! Split out of `agent_live.rs` so the file-size ratchet leaves room for the submenu to be built
//! from the *parsed* dialog rather than a hardcoded pair of numeric guesses. Re-exported by
//! `agent_live.rs`, so `agent_live::run_steer_menu` / `run_steer_target` resolve unchanged.

use super::{
    build_menu_entries, build_pick_rows, collect, read_pick_windows, run_board, MenuEntry,
};
use bn_core::resolver::Config;

/// A quick-steer action (#633 P3): steer an agent from a fleet row *without* navigating into its
/// pane. `Continue` nudges a paused agent onward; `SendText` prompts for a free-text steer;
/// `Interrupt` sends Ctrl-C; `Kill` terminates the window.
///
/// There is deliberately no generic "Approve" here any more. The menu used to open with
/// `Approve (1)` / `Approve (2)`, which sent a bare `1` or `2` at whatever was on screen — on
/// Claude's permission dialog, option 3 is "No, and tell Claude what to do differently", so being
/// one off turned an approval into a rejection into a composer. When the agent is on a dialog, the
/// approve entries are now generated from the dialog itself ([`prompt_entries`]); when it is not,
/// there is nothing to approve and offering it would be a lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SteerAction {
    Continue,
    SendText,
    Interrupt,
    Kill,
}

/// The always-available steer actions, in menu order. Any dialog-specific options are prepended.
const STEER_ACTIONS: [SteerAction; 4] = [
    SteerAction::Continue,
    SteerAction::SendText,
    SteerAction::Interrupt,
    SteerAction::Kill,
];

impl SteerAction {
    /// The single-key accelerator for this action's `display-menu` entry. Mnemonic letters only —
    /// the digits are reserved for the dialog's own options, so `1` always means "the option the
    /// harness numbered 1" and never a guess. `q` is avoided (tmux reserves it to cancel).
    fn key(self) -> char {
        match self {
            SteerAction::Continue => 'c',
            SteerAction::SendText => 's',
            SteerAction::Interrupt => 'i',
            SteerAction::Kill => 'k',
        }
    }

    /// The plain menu label shown for this action.
    fn label(self) -> &'static str {
        match self {
            SteerAction::Continue => "Continue",
            SteerAction::SendText => "Send text…",
            SteerAction::Interrupt => "Interrupt (C-c)",
            SteerAction::Kill => "Kill window",
        }
    }
}

/// The dialog-specific menu entries for `target`, when it is blocked on one: `(label, key, command)`
/// per option the harness actually offered, each answering through `bn agent answer --expect`.
///
/// The `--expect` fingerprint is what makes a menu safe to leave open. A tmux `display-menu` can sit
/// on screen indefinitely while the agent moves on to a different dialog; without the fingerprint,
/// pressing `2` a minute later would answer whatever is on screen *now*. With it, the answer is
/// refused and nothing is sent.
fn prompt_entries(cfg: &Config, target: &str, bn: &str) -> Vec<(String, char, String)> {
    let Some(prompt) =
        crate::cmd::agent::read_prompt(cfg, target, bn_core::agent::PROMPT_SCAN_LINES)
    else {
        return Vec::new();
    };
    let fp = prompt.fingerprint;
    prompt
        .options
        .iter()
        .filter_map(|o| {
            let key = char::from_digit(u32::from(o.number), 10)?;
            let label = format!("{}. {}  [{}]", o.number, o.label, o.semantic.label());
            let cmd = format!(
                "run-shell -b \"{bn} agent answer {} --option {} --expect {fp}\"",
                crate::shq(target),
                o.number
            );
            Some((label, key, cmd))
        })
        .collect()
}

/// Resolve a picker row's `window_id` to the exact tmux target the steer commands act on. The
/// server-global window id (`@N`) is rename- and renumber-proof, so keys never land in a neighbor's
/// pane; [`tmux_exact_target`](crate::tmux_exact_target) passes ids through untouched and `=`-guards
/// a bare name. Pure over its input → unit-testable.
fn steer_target(window_id: &str) -> String {
    crate::tmux_exact_target(window_id)
}

/// Assemble the `display-menu` entry command that performs `action` against pane `target` (#633
/// P3). The three text steers reuse the reliable `bn agent send … --keys` channel (paste + raw-CR
/// submit + confirm) via a backgrounded `run-shell`, so the menu closes immediately; `SendText`
/// first prompts with `command-prompt` (`%%` is the typed text). `Interrupt` is a raw `send-keys
/// C-c`; `Kill` is a `confirm-before`-guarded `kill-window`. `bn` is the shell-quoted path to this
/// executable. Pure over its inputs → unit-testable without a tmux server.
fn steer_action_command(action: SteerAction, target: &str, bn: &str) -> String {
    match action {
        SteerAction::Continue => {
            format!("run-shell -b \"{bn} agent send {target} continue --keys\"")
        }
        SteerAction::SendText => format!(
            "command-prompt -p \"steer {target}:\" \"run-shell -b \\\"{bn} agent send {target} '%%' --keys\\\"\""
        ),
        SteerAction::Interrupt => format!("send-keys -t {target} C-c"),
        SteerAction::Kill => {
            format!("confirm-before -p \"kill {target}? (y/n)\" \"kill-window -t {target}\"")
        }
    }
}

/// Build the `tmux display-menu` argv for the quick-steer actions against `target` (#633 P3).
///
/// When the agent is blocked on a dialog, that dialog's own options lead the menu — real numbers,
/// the harness's real labels, and what each one means — followed by a separator and the generic
/// actions. `prompt_opts` is `(label, key, command)` per option, from [`prompt_entries`]; it is
/// empty when nothing is pending, and the menu is then just the generic actions. `title_extra` puts
/// the question in the menu's title, so you read what you are answering before you press a key.
/// Pure over its inputs → testable without a tmux server.
fn steer_menu_args(
    target: &str,
    name: &str,
    bn: &str,
    prompt_opts: &[(String, char, String)],
    title_extra: Option<&str>,
) -> Vec<String> {
    let title = match title_extra {
        Some(q) => format!("steer · {name} · {q}"),
        None => format!("steer · {name}"),
    };
    let mut args: Vec<String> = vec![
        "display-menu".into(),
        "-T".into(),
        title,
        "-x".into(),
        "C".into(),
        "-y".into(),
        "C".into(),
    ];
    for (label, key, cmd) in prompt_opts {
        args.push(label.clone());
        args.push(key.to_string());
        args.push(cmd.clone());
    }
    if !prompt_opts.is_empty() {
        // A tmux menu separator: an empty name with an empty key and command.
        args.push("".into());
        args.push("".into());
        args.push("".into());
    }
    for a in STEER_ACTIONS {
        args.push(a.label().into());
        args.push(a.key().to_string());
        args.push(steer_action_command(a, target, bn));
    }
    args
}

/// Build the `display-menu` argv for the fleet-row picker whose entries open the quick-steer submenu
/// (#633 P3) instead of jumping into the pane. Reuses [`build_menu_entries`] for the ordered rows +
/// accelerators (needs-you first), then maps each to a backgrounded `run-shell` that re-invokes `bn
/// fleet steer <window_id>` — which raises the action submenu for that row. `bn` is the shell-quoted
/// path to this executable. Pure over its inputs → unit-testable.
fn steer_row_menu_args(entries: &[MenuEntry], title: &str, bn: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "display-menu".into(),
        "-T".into(),
        title.into(),
        "-x".into(),
        "C".into(),
        "-y".into(),
        "C".into(),
    ];
    for e in entries {
        let cmd = format!("run-shell -b \"{bn} fleet steer {}\"", e.window_id);
        args.push(e.name.clone());
        args.push(e.key.to_string());
        args.push(cmd);
    }
    args
}

/// `bn fleet steer` — the quick-steer row picker (#633 P3): a native `display-menu` of the live
/// fleet (one entry per agent window + orchestrator cockpit, needs-you first), each opening a
/// single-key action submenu (approve/continue/send/interrupt/kill) that steers the agent *without*
/// navigating into its pane. The mobile/one-handed counterpart to hand-navigating and typing. Falls
/// back to the read board when there's no tmux client to raise the menu in.
pub fn run_steer_menu(cfg: &Config, idle: i64, stuck: i64) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let views = collect(cfg, idle, stuck);
    let rows = build_pick_rows(
        &views,
        &read_pick_windows(),
        now,
        crate::wt::orchestrator_session(),
    );
    if rows.is_empty() {
        println!("No live agents or orchestrator windows to steer.");
        return;
    }
    if std::env::var_os("TMUX").is_none() {
        println!("\x1b[2m(no tmux client — attach first; showing the board)\x1b[0m");
        run_board(cfg, idle, stuck, None, false);
        return;
    }
    let entries = build_menu_entries(&rows);
    let title = format!("steer · {} · pick an agent", entries.len());
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());
    crate::proc::tmux_run(&steer_row_menu_args(&entries, &title, &bn));
}

/// `bn fleet steer <target>` — raise the quick-steer action submenu for one window (#633 P3). The
/// row picker's entries call this per row; passing a `window_id` (or `repo:branch`) jumps straight
/// to the action menu for that pane. Reuses the same exact target resolution as the steer commands.
/// Falls back to a note when there's no tmux client.
///
/// The pane is read *as the menu is raised*, so a blocked agent's menu is that agent's actual
/// dialog: its options, its wording, its numbers.
pub fn run_steer_target(cfg: &Config, target: &str) {
    if std::env::var_os("TMUX").is_none() {
        println!("bn fleet steer needs a tmux client — attach first.");
        return;
    }
    let resolved = steer_target(target);
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());
    let prompt = crate::cmd::agent::read_prompt(cfg, target, bn_core::agent::PROMPT_SCAN_LINES);
    let opts = prompt_entries(cfg, target, &bn);
    let question = prompt.as_ref().map(|p| p.question.clone());
    crate::proc::tmux_run(&steer_menu_args(
        &resolved,
        target,
        &bn,
        &opts,
        question.as_deref(),
    ));
}
