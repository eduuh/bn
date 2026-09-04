//! Talking to a running editor over its `--listen` socket.
//!
//! Every call is bounded. `--remote-expr` is evaluated on nvim's **main loop**, so an editor
//! sitting on a modal prompt — `Press ENTER or type command to continue`, a `:confirm` dialog, an
//! error from a plugin's startup hook — never answers at all. That is not a hypothetical: a
//! plugin-manager error left a `Press ENTER` prompt during development and an unbounded call hung
//! for two minutes until it was killed by hand. tmux has no equivalent failure, because its server
//! answers regardless of what the pane's program is doing, so this is a hazard the socket
//! introduces and the timeout is what contains it.
//!
//! The bridge is deliberately expression-based rather than key-based: `--remote-send` feeds the
//! *keyboard*, so the user's own mappings apply — including terminal-mode `<Esc>` rebindings —
//! and it returns nothing. `--remote-expr` bypasses mappings and hands back a value we can act on.

use super::session;
use bn_core::runner::{CommandRunner, RealRunner};
use std::path::Path;
use std::time::Duration;

/// Landing runs Diffview and spawns a terminal; two seconds is generous for that and still short
/// enough that a wedged editor cannot hold up a keypress.
pub(crate) const LAND_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(crate) enum RpcError {
    /// Nothing is listening — no editor, or one that died without cleaning up.
    NoEditor,
    /// nvim is alive but did not answer inside the budget: almost always a modal prompt holding
    /// the main loop.
    Timeout(Duration),
    /// nvim answered, but with an error (a Lua exception, a missing module).
    Failed(String),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::NoEditor => write!(f, "no editor is listening"),
            RpcError::Timeout(d) => write!(
                f,
                "editor did not answer within {}s — it is probably waiting on a prompt",
                d.as_secs_f32()
            ),
            RpcError::Failed(m) => write!(f, "editor rejected the call: {m}"),
        }
    }
}

/// Evaluate a VimL expression in the editor at `sock` and return its printed result.
pub(crate) fn remote_expr(sock: &Path, expr: &str, timeout: Duration) -> Result<String, RpcError> {
    // A connect() first: it is a syscall rather than a process spawn, so the common "there is no
    // editor" case costs nothing and never has to wait out the timeout.
    if !session::nvim_alive(sock) {
        return Err(RpcError::NoEditor);
    }
    let sock = sock.display().to_string();
    let out =
        RealRunner.run_with_timeout("nvim", &["--server", &sock, "--remote-expr", expr], timeout);
    if out.success {
        return Ok(out.stdout.trim().to_string());
    }
    // `run_bounded` marks a timeout by appending this to stderr after killing the process group.
    if out.stderr.contains("command timed out after") {
        return Err(RpcError::Timeout(timeout));
    }
    let msg = out
        .stderr
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no output")
        .to_string();
    Err(RpcError::Failed(msg))
}

/// Escape a string for embedding inside a VimL double-quoted literal.
pub(crate) fn viml_str(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The expression that runs a landing spec and hands back its status as JSON.
///
/// The Lua half is written with **single** quotes so the whole thing survives one level of VimL
/// double-quoting without a thicket of backslashes, and `_A` carries the path rather than
/// interpolating the spec itself — so the warm path re-reads the exact bytes the cold path was
/// given, and the two are provably the same input.
pub(crate) fn land_expr(spec: &Path) -> String {
    format!(
        "luaeval(\"vim.json.encode(require('bn').land_file(_A))\", \"{}\")",
        viml_str(&spec.display().to_string())
    )
}

#[cfg(test)]
mod expr_tests {
    use super::*;

    #[test]
    fn the_landing_expression_quotes_cleanly() {
        let e = land_expr(Path::new("/run/user/1000/bn/edit/abc.land.json"));
        assert_eq!(
            e,
            "luaeval(\"vim.json.encode(require('bn').land_file(_A))\", \"/run/user/1000/bn/edit/abc.land.json\")"
        );
        // Single-quoted Lua inside double-quoted VimL: no escaping, so nothing to get wrong.
        assert!(!e.contains('\\'));
    }

    #[test]
    fn a_path_with_a_quote_is_escaped_rather_than_breaking_the_literal() {
        let e = land_expr(Path::new(r#"/tmp/we"ird\path.json"#));
        assert!(e.contains(r#"/tmp/we\"ird\\path.json"#));
    }
}
