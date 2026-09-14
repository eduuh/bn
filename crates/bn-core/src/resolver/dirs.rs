//! Filesystem anchors for bn tooling: the `~/.bn` home, config/bin/jobs dirs, the config layer
//! paths in the notes tree, and the state / managed-repos roots.

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

/// The config dir holding the files bn writes for itself — the machine pin, the web token,
/// `approvals.toml` — and the install anchors (`jobs`, `agents`, `repo`). Settings do not live
/// here; see [`config_layer_paths`].
///
/// `$BN_CONFIG_DIR` wins; else `<bn_home>/config`. Native Windows keeps its platform config
/// directory (`%APPDATA%\bn`) when that is where bn was installed and `~/.bn` was never created.
pub fn config_dir() -> PathBuf {
    if let Some(d) = non_empty_env("BN_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    let home_cfg = bn_home().join("config");
    match dirs::config_dir().map(|p| p.join("bn")) {
        Some(platform)
            if cfg!(windows)
                && non_empty_env("BN_HOME").is_none()
                && !home_cfg.is_dir()
                && platform.is_dir() =>
        {
            platform
        }
        _ => home_cfg,
    }
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

/// The config files bn reads, lowest precedence first; a later one wins key by key.
///
/// `$BN_CONFIG` names one file and is the whole answer when set: a test sandbox saying exactly what
/// to read. Otherwise the shared `<notes_root>/config.toml` every machine syncs, then this
/// machine's own `<notes_root>/<machine>/config.toml`.
pub fn config_layer_paths() -> Vec<PathBuf> {
    match non_empty_env("BN_CONFIG") {
        Some(path) => vec![PathBuf::from(path)],
        None => vec![shared_config_path(), machine_config_path()],
    }
}

/// `<notes_root>/config.toml`: the settings every machine shares.
pub fn shared_config_path() -> PathBuf {
    non_empty_env("BN_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| config_root().join("config.toml"))
}

/// `<notes_root>/<machine>/config.toml`: what this machine does differently, and where bn writes
/// the choices it records for this machine. `$BN_CONFIG`
/// when set, so a sandbox's writes land in the one file it reads.
pub fn machine_config_path() -> PathBuf {
    non_empty_env("BN_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| config_root().join(machine_name()).join("config.toml"))
}

/// The notes root that holds the config: the personal tree, or the work tree on a machine that has
/// only that one.
pub fn config_root() -> PathBuf {
    let (personal, work) = notes_roots();
    if !personal.is_dir() && work.is_dir() {
        work
    } else {
        personal
    }
}

/// `(personal_root, work_root)`, from the path anchors and the defaults alone. No setting can move
/// them, because the settings live inside one of them. `$BN_NOTES_DIR` collapses both into one
/// root for test sandboxes, and `$BN_NOTES_DIR_WORK` splits the work root back out.
pub fn notes_roots() -> (PathBuf, PathBuf) {
    if let Some(dir) = non_empty_env("BN_NOTES_DIR") {
        let work = non_empty_env("BN_NOTES_DIR_WORK")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&dir));
        return (PathBuf::from(dir), work);
    }
    let home = home();
    let work_primary = PathBuf::from(format!("{home}/projects/notes/bn"));
    let work = if work_primary.is_dir() {
        work_primary
    } else {
        PathBuf::from(format!("{home}/projects/branch-notes-work/bn"))
    };
    (PathBuf::from(format!("{home}/projects/branch-notes")), work)
}

/// This machine's name: `$BN_HOSTNAME`, else the pinned identity. Never empty, so a machine whose
/// hostname cannot be read still gets a subtree of its own rather than writing into the notes root.
pub fn machine_name() -> String {
    Some(non_empty_env("BN_HOSTNAME").unwrap_or_else(super::util::stable_machine))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string())
}

/// Path to the pinned machine identity — one line naming this host, written once and reused
/// forever after. `$BN_MACHINE_FILE` wins, else `<config_dir>/machine`.
///
/// Its own file rather than a setting because bn *writes* it, and because the machine's own config
/// file is found by this name: it cannot also be a key inside that file.
pub fn machine_file_path() -> PathBuf {
    if let Some(p) = non_empty_env("BN_MACHINE_FILE") {
        return PathBuf::from(p);
    }
    config_dir().join("machine")
}

/// Path to `approvals.toml` — the agent auto-approve policy (`bn agent answer`'s unattended half).
/// `$BN_APPROVALS` wins, else `<config_dir>/approvals.toml`.
///
/// Deliberately its own document rather than an `[approve]` block in `config.toml`: it is a
/// security-sensitive allowlist that decides which commands an agent may run without a human, it is
/// list-shaped and edited far more often than the scalar settings around it, and keeping it
/// separate means "what may bn approve on my behalf?" is answered by one file you can read top to
/// bottom. Same person-scoped, own-file pattern as `references/` and `jobs/`.
pub fn approvals_file_path() -> PathBuf {
    if let Some(p) = non_empty_env("BN_APPROVALS") {
        return PathBuf::from(p);
    }
    config_dir().join("approvals.toml")
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
/// `<state_dir>/logs` unless `[log].dir` overrides it (resolved in [`Config::log`]).
/// Machine-scoped like the state dir, so logs never git-collide across hosts.
pub fn log_dir() -> PathBuf {
    state_dir().join("logs")
}

/// The reusable references/standards directory (`bn ref`, #623). `$BN_REFERENCES_DIR` wins
/// (tests, explicit override); otherwise it resolves to `<work_root>/references` inside the
/// synced notes repo via [`Config::references_dir`]. Person-scoped and cross-machine (standards
/// apply on every host), so it is NOT keyed by `<machine>`.
pub fn references_dir() -> PathBuf {
    if let Some(d) = non_empty_env("BN_REFERENCES_DIR") {
        return PathBuf::from(d);
    }
    Config::load().references_dir()
}

/// Where a `bn links` tag vocabulary came from, so `bn links tags` can say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagSource {
    /// `[links] tags` was set in `config.toml`.
    Config,
    /// The key was absent, so [`crate::links::DEFAULT_TAGS`] applies.
    Default,
}

/// The curated tag vocabulary for `bn links` (`[links].tags` in `config.toml`), and where it
/// came from.
///
/// Loaded on call rather than cached on [`Config`]: the vocabulary is read once per `bn links
/// add`, and a process-lifetime cache would be invisible to tests that swap the config file.
///
/// An absent key falls back to the shipped default, so a machine whose `config.toml` predates
/// the setting still gets a working vocabulary. An explicit `tags = []` is honoured as an empty
/// vocabulary — every tag is then unlisted, which warns but never blocks.
pub fn link_tags_with_source() -> (Vec<String>, TagSource) {
    match super::config_file::ConfigFile::load().and_then(|c| c.links_tags()) {
        Some(tags) => (tags, TagSource::Config),
        None => (
            crate::links::DEFAULT_TAGS
                .iter()
                .map(|t| t.to_string())
                .collect(),
            TagSource::Default,
        ),
    }
}

/// The tag vocabulary alone, for callers that only need to validate against it.
pub fn link_tags() -> Vec<String> {
    link_tags_with_source().0
}

/// `<worktree_dir>`: `$BN_WORKTREE_DIR`, else `~/projects/worktree`. Not a setting, for the same
/// reason as the notes roots: nothing in a config file may move where bn looks for itself.
pub fn worktree_dir() -> PathBuf {
    non_empty_env("BN_WORKTREE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{}/projects/worktree", home())))
}

/// The bn checkout whose `config/` ships bn's own prompts, agents, jobs and cron manifest.
///
/// `<worktree_dir>/bn/main` when it holds one — every machine running bn keeps it, because
/// self-update installs from it — so what a machine serves follows main, not whichever checkout
/// last ran install.sh. That anchor used to be a leasable pool slot, and a dispatch that leased the
/// slot swapped the whole machine's prompts for its branch's (#1291). Else the `<config_dir>/repo`
/// anchor install.sh leaves. A sandbox that pins `$BN_HOME` or `$BN_CONFIG_DIR` never reads the
/// real checkout: its anchors are its whole universe.
pub fn shipped_root() -> Option<PathBuf> {
    let sandboxed = non_empty_env("BN_HOME").is_some() || non_empty_env("BN_CONFIG_DIR").is_some();
    let main = worktree_dir().join("bn").join("main");
    if !sandboxed && main.join("config").is_dir() {
        return Some(main);
    }
    let anchor = config_dir().join("repo");
    anchor.is_dir().then_some(anchor)
}

/// One shipped item, `config/<relative>`, when it exists: from `<worktree_dir>/bn/main` first
/// (outside a sandbox, see [`shipped_root`]), then the per-item anchor install.sh links
/// (`<config_dir>/<relative>`, such as `<config_dir>/crons.toml`), then the `<config_dir>/repo`
/// checkout anchor. The per-item anchor wins over the generic one because it is the more specific
/// statement of what this install serves.
pub fn shipped_config(relative: &str) -> Option<PathBuf> {
    let sandboxed = non_empty_env("BN_HOME").is_some() || non_empty_env("BN_CONFIG_DIR").is_some();
    let main = worktree_dir()
        .join("bn")
        .join("main")
        .join("config")
        .join(relative);
    if !sandboxed && main.exists() {
        return Some(main);
    }
    [
        config_dir().join(relative),
        config_dir().join("repo").join("config").join(relative),
    ]
    .into_iter()
    .find(|path| path.exists())
}

/// Directory of version-controlled cron job scripts. `$BN_JOBS_DIR` wins, else the shipped
/// `config/jobs` (see [`shipped_config`]), else `<config_dir>/jobs`. Jobs are scripts persisted in
/// the repo — never inline crontab commands.
pub fn jobs_dir() -> PathBuf {
    if let Some(d) = non_empty_env("BN_JOBS_DIR") {
        return PathBuf::from(d);
    }
    shipped_config("jobs").unwrap_or_else(|| config_dir().join("jobs"))
}

/// Directory of self-describing agent definitions (`agents/<name>.md`). `$BN_AGENTS_DIR` wins,
/// else the shipped `config/agents` (see [`shipped_root`]), else `<config_dir>/agents` — the agent
/// analogue of [`jobs_dir`].
pub fn agents_dir() -> PathBuf {
    if let Some(d) = non_empty_env("BN_AGENTS_DIR") {
        return PathBuf::from(d);
    }
    shipped_config("agents").unwrap_or_else(|| config_dir().join("agents"))
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

#[cfg(test)]
mod shipped_tests {
    use super::*;
    use crate::test_support::{env_lock, EnvGuard};

    /// Shipped content follows the bn/main checkout, so a pool slot leased to another branch can no
    /// longer decide what the whole machine serves (#1291).
    #[test]
    fn shipped_content_comes_from_bn_main_before_any_anchor() {
        let _lock = env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let main = temp.path().join("worktree").join("bn").join("main");
        std::fs::create_dir_all(main.join("config").join("prompts")).expect("main checkout");
        let _worktrees = EnvGuard::set("BN_WORKTREE_DIR", temp.path().join("worktree"));
        let _home = EnvGuard::remove("BN_HOME");
        let _config_dir = EnvGuard::remove("BN_CONFIG_DIR");

        assert_eq!(shipped_root(), Some(main.clone()));
        assert_eq!(
            shipped_config("prompts"),
            Some(main.join("config").join("prompts"))
        );
    }

    /// A sandbox's anchors are its whole universe: it never reads the real bn/main checkout.
    #[test]
    fn a_sandbox_reads_only_its_own_anchor() {
        let _lock = env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(temp.path().join("worktree/bn/main/config")).expect("main");
        let _worktrees = EnvGuard::set("BN_WORKTREE_DIR", temp.path().join("worktree"));
        let _config_dir = EnvGuard::set("BN_CONFIG_DIR", temp.path().join("cfg"));

        assert_eq!(shipped_root(), None);
        std::fs::create_dir_all(temp.path().join("cfg").join("repo")).expect("anchor");
        assert_eq!(shipped_root(), Some(temp.path().join("cfg").join("repo")));
    }
}
