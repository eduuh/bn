//! Shared command context, input validation and error reporting.
use crate::*;

/// Resolve git context or exit 1 with the oracle's "Not in a git repo" message.
pub(crate) fn resolve_or_exit(cfg: &Config) -> Resolved {
    let cwd = std::env::current_dir().unwrap_or_default();
    match resolver::resolve(cfg, &cwd) {
        Ok(ctx) => ctx,
        Err(_) => fail("Not in a git repo"),
    }
}

/// Validate a `--est` duration against the `bn focus start` grammar, returning the trimmed
/// string to persist. Exits with a usage error on a malformed/empty value so a bad estimate
/// never lands on an item. `None` passes through untouched (no estimate given).
pub(crate) fn parse_est_or_exit(cmd: &str, est: Option<&str>) -> Option<String> {
    let raw = est?.trim();
    if bn_core::focus::parse_duration(raw).is_none() {
        fail(&format!(
            "{cmd}: invalid --est '{raw}' (expected a duration like 90m, 2h, 1h30m)"
        ));
    }
    Some(raw.to_string())
}

pub(crate) fn emit_err(command: &str, e: &CmdError, as_json: bool) {
    if as_json {
        let env = Envelope::failure(command, e.code(), e.message());
        println!("{}", env.to_json());
    } else {
        eprintln!("{}", e.message());
    }
    std::process::exit(1);
}
