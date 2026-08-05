//! Cross-platform, best-effort desktop toasts + clipboard for the notification cron jobs
//! (`notify`, `standup-push`, `wrap-digest`). A direct port of the shell helpers those shims
//! carried: a toast backend chosen from `notify-send` / PowerShell BurntToast / `osascript`, and a
//! clipboard backend from `wl-copy` / `xclip` / `clip.exe` / `pbcopy`. Everything is best-effort —
//! a missing backend or a failed call is swallowed, exactly like the `… || true` shell idiom.

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
pub fn toast(title: &str, body: &str) -> bool {
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
