//! `bn mcp init <claude|copilot>` — presentation layer over `bn_core::mcp::init`. The wiring
//! itself lives in bn-core so the `bn_mcp_init` MCP tool shares it verbatim.

use bn_core::mcp::{self, Harness};
use bn_core::render::Envelope;
use std::path::{Path, PathBuf};

/// Parse the positional harness arg, or `None` for an unknown value (the caller reports it).
pub fn parse_harness(s: &str) -> Option<Harness> {
    Harness::parse(s)
}

/// The `bn-mcp` binary name for the current platform.
fn bn_mcp_name() -> &'static str {
    if cfg!(windows) {
        "bn-mcp.exe"
    } else {
        "bn-mcp"
    }
}

/// Resolve the `bn-mcp` binary: prefer the sibling next to the running `bn` (same install dir, so a
/// from-source or release install runs its matching build), else fall back to the bare name on
/// `PATH`.
///
/// The **canonicalized** path is tried first, because bn is routinely launched through a symlink:
/// install.sh maintains `<config_dir>/bn -> <bin_dir>/bn` as the stable anchor, and the cron units
/// and MCP client registrations both spawn bn through it. `current_exe()` hands back that symlink
/// path, whose sibling directory holds no `bn-mcp` — so resolving without canonicalizing looked
/// beside the *anchor* instead of beside the real binary and died with "could not exec bn-mcp",
/// taking every MCP client down with it. Falling back to the uncanonicalized sibling keeps a
/// non-symlinked install working even if canonicalize() fails.
pub(crate) fn resolve_bn_mcp() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(real) = std::fs::canonicalize(&exe) {
            let sibling = real.with_file_name(bn_mcp_name());
            if sibling.is_file() {
                return sibling;
            }
        }
        let sibling = exe.with_file_name(bn_mcp_name());
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from(bn_mcp_name())
}

/// `bn mcp serve [--orchestrate]` — exec the sibling `bn-mcp` stdio launcher, pinned to the current
/// worktree. The child derives its `(repo, branch)` from this cwd, so an MCP client needs no
/// per-call scope headers. On unix we `exec()` so stdio/signals flow straight through (the client
/// speaks MCP over this process's stdin/stdout); elsewhere we spawn+wait and forward the exit code.
pub fn serve(orchestrate: bool) {
    let bin = resolve_bn_mcp();
    let args: &[&str] = if orchestrate { &["--orchestrate"] } else { &[] };

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // exec() only returns on failure.
        let err = std::process::Command::new(&bin).args(args).exec();
        crate::fail(&format!(
            "bn mcp serve: could not exec {} ({err}) — is bn-mcp installed beside bn? Try install.sh.",
            bin.display()
        ));
    }
    #[cfg(not(unix))]
    {
        match std::process::Command::new(&bin).args(args).status() {
            Ok(status) => std::process::exit(status.code().unwrap_or(0)),
            Err(e) => crate::fail(&format!(
                "bn mcp serve: could not run {} ({e}) — is bn-mcp installed beside bn?",
                bin.display()
            )),
        }
    }
}

pub fn init(harness: Harness, with_extras: bool, as_json: bool) {
    let base = std::env::current_dir().unwrap_or_default();
    // Extras are opt-in twice over: `[mcp] extras` in config.toml names a subset, and
    // `--with-extras` without config wires the full curated set.
    let mut extras = bn_core::resolver::mcp_extras();
    if with_extras && extras.is_empty() {
        extras = mcp::EXTRAS.iter().map(|e| e.name.to_string()).collect();
    }
    match mcp::init(&base, harness, &extras) {
        Ok(report) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success("mcp_init", report.to_json()).to_json()
                );
                return;
            }
            println!(
                "Wired {} for the bn MCP server in {}:",
                report.harness,
                report.repo_root.display()
            );
            for w in &report.files {
                println!("  {} {}", w.action, w.path.display());
            }
            for e in &report.extras {
                println!("  extra {} — {}", e.name, e.action);
            }
            for m in &report.missing_runners {
                println!("  ⚠ '{}' not on PATH — {}", m.runner, m.fix);
            }
            println!("Restart the editor (or reload the MCP servers) to pick up the bn tools.");
        }
        Err(msg) => {
            let msg = format!("bn mcp init: {msg}");
            if as_json {
                println!("{}", Envelope::failure("mcp_init", "io", &msg).to_json());
            } else {
                eprintln!("{msg}");
            }
            std::process::exit(1);
        }
    }
}

/// A [`SecretPrompter`] backed by the controlling terminal.
///
/// Credential provisioning is interactive on purpose: the value must not come from config.toml
/// (synced git), from a flag (shell history), or from the environment (inherited by every child).
/// Typing it once per machine is the narrowest path that leaves no copy behind.
struct TtyPrompter {
    /// Set once the operator declines or stdin ends, so the remaining fields of a multi-field
    /// secret are not asked for after the run is already doomed.
    aborted: bool,
}

impl bn_core::mcp_user::SecretPrompter for TtyPrompter {
    fn ask(&mut self, server: &str, path: &Path, field: &str, hidden: bool) -> Option<String> {
        use std::io::Write;
        if self.aborted {
            return None;
        }
        println!("  {server}: {} needs '{field}'", path.display());
        print!("    {field}{}: ", if hidden { " (hidden)" } else { "" });
        let _ = std::io::stdout().flush();

        let value = if hidden { read_hidden() } else { read_line() };
        match value {
            Some(v) if !v.is_empty() => Some(v),
            _ => {
                self.aborted = true;
                println!("    — skipped; '{server}' will not be registered until it is provided");
                None
            }
        }
    }
}

fn read_line() -> Option<String> {
    let mut s = String::new();
    match std::io::stdin().read_line(&mut s) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(s.trim().to_string()),
    }
}

/// Read one line with terminal echo disabled, so a pasted credential never reaches the scrollback
/// (or a screen share). Restores the original terminal settings on every exit path, including the
/// error paths — leaving a shell with echo off would be far worse than showing the secret.
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "disabling terminal echo requires the libc termios APIs"
)]
fn read_hidden() -> Option<String> {
    use std::os::unix::io::AsRawFd;
    let fd = std::io::stdin().as_raw_fd();
    // SAFETY: termios is a plain C struct fully initialised by tcgetattr before it is read.
    let mut orig: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut orig) } != 0 {
        // Not a tty (piped stdin, or no controlling terminal): there is no echo to disable.
        return read_line();
    }
    let mut quiet = orig;
    quiet.c_lflag &= !libc::ECHO;
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &quiet) } != 0 {
        return read_line();
    }
    let value = read_line();
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &orig) };
    println!();
    value
}

#[cfg(not(unix))]
fn read_hidden() -> Option<String> {
    read_line()
}

/// True when stdin is a terminal, i.e. there is someone to answer a credential prompt. A cron or
/// a piped install run gets `--no-prompt` behaviour automatically instead of blocking forever.
#[cfg(unix)]
#[allow(unsafe_code, reason = "isatty is the libc tty check")]
fn stdin_is_tty() -> bool {
    use std::os::unix::io::AsRawFd;
    unsafe { libc::isatty(std::io::stdin().as_raw_fd()) == 1 }
}

#[cfg(not(unix))]
fn stdin_is_tty() -> bool {
    false
}

/// `bn mcp sync-user` — write the `[mcp.servers.*]` declarations into every MCP client on this
/// machine. The user-scope counterpart to `bn mcp init`'s repo-scope wiring, and the reason a
/// personal server declared once in the synced config.toml exists on every machine.
pub fn sync_user(no_prompt: bool, as_json: bool) {
    let servers = bn_core::resolver::mcp_user_servers();
    if servers.is_empty() {
        let msg =
            "no user-scoped MCP servers declared — add a [mcp.servers.<name>] table to config.toml";
        if as_json {
            println!(
                "{}",
                Envelope::success("mcp_sync_user", serde_json::json!({ "servers": [] })).to_json()
            );
        } else {
            println!("{msg}");
        }
        return;
    }

    // Prompting needs both a terminal to ask at and permission to ask. Without either, a server
    // whose secret is missing is reported and skipped rather than silently registered broken.
    let mut tty = TtyPrompter { aborted: false };
    let prompter: Option<&mut dyn bn_core::mcp_user::SecretPrompter> =
        if no_prompt || as_json || !stdin_is_tty() {
            None
        } else {
            Some(&mut tty)
        };

    match bn_core::mcp_user::sync(&servers, prompter) {
        Ok(report) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success("mcp_sync_user", report.to_json()).to_json()
                );
                return;
            }
            println!("Synced user-scoped MCP servers:");
            for s in &report.secrets {
                println!("  secret {} {} — {}", s.server, s.path.display(), s.action);
            }
            for s in &report.servers {
                if s.clients.is_empty() {
                    println!("  {} — {}", s.name, s.action);
                } else {
                    println!("  {} — {} ({})", s.name, s.action, s.clients.join(", "));
                }
            }
            for w in &report.files {
                println!("  {} {}", w.action, w.path.display());
            }
            for c in &report.missing_commands {
                println!("  ⚠ '{c}' not on PATH — the server will fail to start until it is");
            }
            if report
                .servers
                .iter()
                .any(|s| s.action == "skipped-no-secret")
            {
                println!("Re-run `bn mcp sync-user` on a terminal to provide the missing secrets.");
            }
            println!("Restart your MCP clients to pick up the changes.");
        }
        Err(msg) => {
            let msg = format!("bn mcp sync-user: {msg}");
            if as_json {
                println!(
                    "{}",
                    Envelope::failure("mcp_sync_user", "io", &msg).to_json()
                );
            } else {
                eprintln!("{msg}");
            }
            std::process::exit(1);
        }
    }
}
