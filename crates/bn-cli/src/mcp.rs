//! `bn mcp init <claude|copilot>` — presentation layer over `bn_core::mcp::init`. The wiring
//! itself lives in bn-core so the `bn_mcp_init` MCP tool shares it verbatim.

use bn_core::mcp::{self, Harness};
use bn_core::render::Envelope;
use std::path::PathBuf;

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
/// `PATH`. Mirrors `bn serve`'s `resolve_bn_web` — `bn-mcp` sits beside `bn` exactly like `bn-web`.
fn resolve_bn_mcp() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
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
