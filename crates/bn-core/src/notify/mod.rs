//! Cross-platform, best-effort desktop toasts + clipboard. A direct port of the shell helpers the
//! notification cron shims carried: a toast backend chosen from `notify-send` / PowerShell
//! BurntToast / `osascript`, and a clipboard backend from `wl-copy` / `xclip` / `clip.exe` /
//! `pbcopy`. Everything is best-effort — a missing backend or a failed call is swallowed, exactly
//! like the `… || true` shell idiom.
//!
//! The raw backend is **deliberately not public**. Every desktop toast in `bn` goes through
//! [`policy::deliver`], which decides whether the interruption is warranted (class bar, quiet
//! windows, dedup, cooldown, daily budget) before [`emit`] ever runs. Keeping [`emit`]
//! `pub(crate)` means `bn-cli` cannot reach around the gate: the chokepoint is enforced by
//! visibility, not by convention. See [`policy`] for the model.

pub mod policy;

use std::io::Write;
use std::process::{Command, Stdio};

/// Whether `bin` is found on `PATH` (cross-platform PATH scan, the `command -v` equivalent).
fn has(bin: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(bin).is_file())
}

/// The PowerShell toast script (BurntToast if installed, else a balloon tip), shared by all three
/// jobs. Reads title/body from the `BN_TOAST_*` env vars so we never have to escape into the
/// command line.
const PS_TOAST: &str = r#"
$t = $env:BN_TOAST_TITLE; $b = $env:BN_TOAST_BODY
if (Get-Module -ListAvailable -Name BurntToast) {
  Import-Module BurntToast; New-BurntToastNotification -Text $t, $b
} else {
  Add-Type -AssemblyName System.Windows.Forms
  $n = New-Object System.Windows.Forms.NotifyIcon
  $n.Icon = [System.Drawing.SystemIcons]::Information
  $n.Visible = $true
  $n.ShowBalloonTip(10000, $t, $b, [System.Windows.Forms.ToolTipIcon]::Info)
}"#;

/// The AppleScript toast, reading title/body from `BN_TOAST_*` via `system attribute`.
const OSA_TOAST: &str = r#"on run
  set t to (system attribute "BN_TOAST_TITLE")
  set b to (system attribute "BN_TOAST_BODY")
  display notification b with title t
end run"#;

/// Show a desktop toast. Tries each backend in turn; returns whether one ran.
///
/// `pub(crate)` on purpose — call [`policy::deliver`] instead. This is the unconditional backend:
/// it interrupts the user with no notion of why, so nothing outside the policy gate may call it.
pub(crate) fn emit(title: &str, body: &str) -> bool {
    if has("notify-send") {
        return ok(Command::new("notify-send").arg(title).arg(body));
    }
    if has("powershell.exe") {
        return ok(Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", PS_TOAST])
            .env("BN_TOAST_TITLE", title)
            .env("BN_TOAST_BODY", body));
    }
    if has("osascript") {
        return ok(Command::new("osascript")
            .args(["-e", OSA_TOAST])
            .env("BN_TOAST_TITLE", title)
            .env("BN_TOAST_BODY", body));
    }
    false
}

/// Last-resort delivery through tmux, for hosts where every OS toast backend is absent or broken.
///
/// This machine is the case in point: WSL has `powershell.exe` on PATH, so [`emit`] picks the
/// PowerShell toast and that call fails every time — the notify ledger showed `sent_today: 0`,
/// `last_sent: {}` and nineteen `no-backend` holds in a single day. Every one of those had already
/// passed the whole policy gate (class bar, quiet windows, cooldown, budget); they were interrupts
/// bn had decided were worth making, and they reached nobody.
///
/// `display-message` with no `-t` reaches every attached client, so it lands wherever the user
/// actually is. Gated on a client being attached, so a headless cron run reports failure honestly
/// and is recorded as `no-backend` rather than pretending it delivered to an empty screen.
///
/// Deliberately not a fourth arm inside [`emit`]: this is a *different channel* with a different
/// reach, and the caller records which one carried the message.
pub(crate) fn emit_tmux(title: &str, body: &str) -> bool {
    if crate::tmux::tmux_raw(&["list-clients", "-F", "#{client_name}"])
        .map(|o| o.trim().is_empty())
        .unwrap_or(true)
    {
        return false;
    }
    let text = if body.trim().is_empty() {
        title.to_string()
    } else {
        format!("{title} — {body}")
    };
    // One line, on screen for 5s. The burst batching in `deliver_batch_using` already collapses a
    // storm into a single call, so this cannot become a flood of its own.
    crate::tmux::tmux_run(&["display-message", "-d", "5000", &text])
}

/// Copy `text` to the system clipboard. Tries wl-copy / xclip / clip.exe / pbcopy; returns whether
/// one ran.
pub fn clipboard(text: &str) -> bool {
    let backends: [(&str, &[&str]); 4] = [
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("clip.exe", &[]),
        ("pbcopy", &[]),
    ];
    for (bin, args) in backends {
        if has(bin) {
            return pipe(bin, args, text);
        }
    }
    false
}

/// Run a command with stdio silenced, returning success.
fn ok(cmd: &mut Command) -> bool {
    cmd.stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Pipe `text` to a command's stdin, stdio otherwise silenced.
fn pipe(bin: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        // Best-effort: a broken pipe surfaces as a non-success exit from child.wait() below.
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}
