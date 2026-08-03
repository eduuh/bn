//! Filesystem anchors for bn tooling: the `~/.bn` home, config/bin/jobs dirs, the
//! `config.toml` path, and the state / managed-repos roots.

use super::util::{home, non_empty_env};
use super::Config;
use std::path::PathBuf;

/// The bn home root: `$BN_HOME`, else `~/.bn`. The single hub for bn's *tooling* — the source
/// checkout (`repo/`), binaries (`bin/`), config (`config/`), the jobs symlink (`jobs/`), and
/// bn-managed repos (`repos/`) all default under here. NOTE: notes / metrics / the event log are
/// deliberately NOT here — they live in the synced notes repo (see [`Config`]) so they cross
/// machines via git; `~/.bn` is local tooling state.
pub fn bn_home() -> PathBuf {
    if let Some(d) = non_empty_env("BN_HOME") {
        return PathBuf::from(d);
    }
    PathBuf::from(format!("{}/.bn", home()))
}

/// The config dir holding `config.toml` and the legacy `work-repos` plaintext file.
/// Resolution, high → low: `$BN_CONFIG_DIR` wins; else `$BN_HOME/config`
/// (the new `~/.bn` layout) when `BN_HOME` is set or that dir already exists; else the legacy
/// `~/.config/bn` while it still exists (back-compat — pre-migration machines keep working);
/// else default to the new `~/.bn/config` on a fresh machine.
pub fn config_dir() -> PathBuf {
    if let Some(d) = non_empty_env("BN_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    let home_cfg = bn_home().join("config");
    if non_empty_env("BN_HOME").is_some() || home_cfg.is_dir() {
        return home_cfg;
    }
    // `~/.config/bn` on unix — macOS included: install.sh anchors everything (the `jobs` symlink,
    // the `repo` anchor, the `bn` binary link) at `~/.config/bn`, so the binary must read the same
    // place. `dirs::config_dir()` would pick `~/Library/Application Support` on macOS, splitting
    // the brain: the manifest lookup came up empty ("0 declared") while install kept wiring
    // `~/.config/bn`. Windows keeps `%APPDATA%\bn` (via dirs).
    #[cfg(windows)]
    let legacy = dirs::config_dir()
        .map(|p| p.join("bn"))
        .unwrap_or_else(|| PathBuf::from(format!("{}/.config/bn", home())));
    #[cfg(not(windows))]
    let legacy = PathBuf::from(format!("{}/.config/bn", home()));
    if legacy.is_dir() {
        return legacy;
    }
    home_cfg
}

/// The persisted bn-web token, when one has already been generated. Reads `<config_dir>/web-token`
/// (the file `bn serve`'s `auth::ensure_token` writes) and returns its trimmed, non-empty contents.
/// Dependency-free and read-only: bn-core never *generates* the token (that stays in bn-web, which
/// owns `rand`), so callers that need one when none exists must fall back gracefully. `bn mcp init`
/// uses this to add an `Authorization` header to the emitted HTTP MCP entry only when a token
/// already exists — the `/mcp` endpoint is untokened (#523), so url-only still works.
pub fn web_token() -> Option<String> {
    let raw = std::fs::read_to_string(config_dir().join("web-token")).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Directory bn's binaries (`bn`, `bn-mcp`, the workflow tools) live in. Resolution,
/// high → low: `$BN_BIN_DIR` wins; else `<bn_home>/bin` (the new `~/.bn` layout) when `BN_HOME`
/// is set or that dir already exists; else the legacy `~/.bin` while it still exists; else
/// default to `<bn_home>/bin`. install.sh keeps these in step with [`config_dir`].
pub fn bin_dir() -> PathBuf {
    if let Some(d) = non_empty_env("BN_BIN_DIR") {
        return PathBuf::from(d);
    }
    let home_bin = bn_home().join("bin");
    if non_empty_env("BN_HOME").is_some() || home_bin.is_dir() {
        return home_bin;
    }
    let legacy = PathBuf::from(format!("{}/.bin", home()));
    if legacy.is_dir() {
        return legacy;
    }
    home_bin
}

/// Path to `config.toml`: `$BN_CONFIG` override, else `<config_dir>/config.toml`.
pub fn config_file_path() -> PathBuf {
    if let Some(d) = non_empty_env("BN_CONFIG") {
        return PathBuf::from(d);
    }
    config_dir().join("config.toml")
}

/// The state directory for append-only logs (the event log) and caches. `$BN_STATE_DIR` wins
/// (tests, explicit override); otherwise it resolves to `<personal_root>/<machine>/state`
/// inside the synced notes repo via [`Config::state_dir`]. Machine-scoped so it syncs across
/// hosts without git-colliding.
pub fn state_dir() -> PathBuf {
    if let Some(d) = non_empty_env("BN_STATE_DIR") {
        return PathBuf::from(d);
    }
    Config::load().state_dir()
}

/// The default directory for the opt-in verbose/debug action log (`bn-YYYY-MM-DD.jsonl` files).
/// `<state_dir>/logs` unless `[log].dir` / `$BN_LOG_DIR` overrides it (resolved in
/// [`Config::log`]). Machine-scoped like the state dir, so logs never git-collide across hosts.
pub fn log_dir() -> PathBuf {
    state_dir().join("logs")
}

/// The reusable references/standards directory (`bn ref`, #623). `$BN_REFERENCES_DIR` wins
/// (tests, explicit override); otherwise it resolves to `<personal_root>/references` inside the
/// synced notes repo via [`Config::references_dir`]. Person-scoped and cross-machine (standards
/// apply on every host), so it is NOT keyed by `<machine>`.
pub fn references_dir() -> PathBuf {
    if let Some(d) = non_empty_env("BN_REFERENCES_DIR") {
        return PathBuf::from(d);
    }
    Config::load().references_dir()
}

/// Directory of version-controlled cron job scripts. `$BN_JOBS_DIR` wins, else
/// `<config_dir>/jobs` (which `install.sh` symlinks onto the repo's `jobs/` dir, so the
/// jobs cron runs are the same scripts tracked in git). Jobs are scripts persisted in the
/// repo — never inline crontab commands.
pub fn jobs_dir() -> PathBuf {
    if let Some(d) = non_empty_env("BN_JOBS_DIR") {
        return PathBuf::from(d);
    }
    config_dir().join("jobs")
}

/// Directory of self-describing agent definitions (`agents/<name>.md`). `$BN_AGENTS_DIR` wins,
/// else `<config_dir>/agents` (which `install.sh` symlinks onto the repo's `agents/` dir, so the
/// shipped agents are the same markdown tracked in git — the agent analogue of [`jobs_dir`]).
pub fn agents_dir() -> PathBuf {
    if let Some(d) = non_empty_env("BN_AGENTS_DIR") {
        return PathBuf::from(d);
    }
    config_dir().join("agents")
}

/// Root for bn-managed repos: `$BN_MANAGED_REPOS_DIR`, else `<bn_home>/repos`. Cloned bare +
/// worktree under here (via `bn wt` with `WT_ROOT` pinned to this dir), so the layout matches
/// every other bn worktree: `<dir>/bare/<repo>.git` + `<dir>/worktree/<repo>/<branch>`.
pub fn managed_repos_dir() -> PathBuf {
    if let Some(d) = non_empty_env("BN_MANAGED_REPOS_DIR") {
        return PathBuf::from(d);
    }
    bn_home().join("repos")
}
