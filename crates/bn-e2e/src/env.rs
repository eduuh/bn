//! The isolation contract: every environment variable that must be redirected (or unset) before
//! a `bn` subprocess may run against a sandbox.
//!
//! This is the *single* place the contract is written down. Two sandbox scripts once hand-rolled a
//! subset, and each forgot `BN_NOTES_DIR`, which silently appended test fixtures to the real
//! `tasks.txt`. A forgotten variable is not a hypothetical failure mode here; it is the observed
//! one, twice.
//!
//! Two entries below are load-bearing in a way that is not obvious, and both were verified
//! experimentally rather than assumed:
//!
//! * **`HOME`** — every resolver in `bn_core::resolver::dirs` ultimately falls through to
//!   `home()`. Pinning `HOME` is what turns "the harness forgot a `BN_*` variable" from data
//!   corruption into a harmless write inside the sandbox. It also stops a sandbox tmux server
//!   from sourcing the developer's real `~/.tmux.conf` — a config that commonly creates sessions
//!   on server start, which would otherwise appear in the sandbox and break session assertions.
//!
//! * **`TMUX` / `TMUX_PANE`** — see [`super::proof`]. `TMUX_TMPDIR` alone does NOT isolate tmux:
//!   when `$TMUX` is set, tmux reads the socket path out of it and ignores `TMUX_TMPDIR`
//!   entirely. Since most of bn's tmux surface runs on the *default* socket (and calls
//!   `kill-window`), a suite run from inside a real tmux session would drive the developer's live
//!   server. Removing these two variables is what makes the sandbox safe.

use std::collections::BTreeMap;
use std::path::Path;

/// Variables that must be *removed* from a sandboxed child's environment.
pub const REMOVED: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    // Belt-and-braces against a developer's shell that already points bn somewhere: any of these
    // set in the ambient environment would otherwise win over the sandbox's own values only if we
    // failed to set them below. Removing first means the map built here is the whole truth.
    "BN_CONFIG",
    "BN_LOG_DIR",
    "BN_LOG",
];

/// The full sandbox environment for a bn universe rooted at `root`.
pub fn contract(root: &Path, now: i64, today: &str) -> BTreeMap<String, String> {
    let home = root.join("home");
    let bn_home = home.join(".bn");
    let config = bn_home.join("config");
    let notes = root.join("notes");

    let mut env = BTreeMap::new();
    let mut set = |key: &str, value: &Path| {
        env.insert(key.to_string(), value.to_string_lossy().into_owned());
    };

    // The backstop. Everything below is a redirect that a bug could miss; this is the net.
    set("HOME", &home);
    set("XDG_CONFIG_HOME", &home.join(".config"));
    set("XDG_DATA_HOME", &home.join(".local/share"));
    set("XDG_CACHE_HOME", &home.join(".cache"));

    // tmux: an isolated socket directory. Only effective with TMUX/TMUX_PANE removed.
    set("TMUX_TMPDIR", &root.join("tmux"));

    // bn's own anchors, mirroring `bn_core::resolver::dirs`.
    set("BN_HOME", &bn_home);
    set("BN_CONFIG_DIR", &config);
    set("BN_CONFIG", &config.join("config.toml"));
    set("BN_BIN_DIR", &bn_home.join("bin"));
    set("BN_STATE_DIR", &root.join("state"));
    set("BN_NOTES_DIR", &notes);
    set("BN_REFERENCES_DIR", &notes.join("references"));
    set("BN_JOBS_DIR", &config.join("jobs"));
    set("BN_AGENTS_DIR", &config.join("agents"));
    set("BN_MACHINE_FILE", &config.join("machine"));
    set("BN_MANAGED_REPOS_DIR", &bn_home.join("repos"));

    // Worktree roots. `BN_WORKTREE_DIR` is the one `bn dispatch` resolves the bare+worktree layout
    // under; without it dispatch falls back to `$HOME/projects` — contained by the pinned HOME,
    // but pointing at a directory the sandbox never seeded, so dispatch fails on a missing bare
    // repo rather than exercising the workflow. `WT_ROOT` is the `wtw` wrapper's separate override
    // consulted by the pool.
    // Note the two have *different* shapes, and mixing them up silently splits the layout in
    // half: `WT_ROOT` is the parent holding `bare/` and `worktree/` (wt::layout, pool::
    // worktree_root), whereas `BN_WORKTREE_DIR` is the worktree dir itself, with the bare dir
    // derived as its sibling (Config::bare_dir). Set consistently, both resolve to the same pair.
    set("BN_WORKTREE_DIR", &root.join("projects/worktree"));
    set("WT_ROOT", &root.join("projects"));

    // git: a sandbox-owned global config, so commits work without the developer's identity and
    // nothing reads (or writes) their real ~/.gitconfig. Mirrors what ci.yml does per job.
    set("GIT_CONFIG_GLOBAL", &root.join("gitconfig"));
    set("GIT_CONFIG_SYSTEM", &root.join("gitconfig-system"));

    env.insert("BN_HOSTNAME".into(), "e2ehost".into());

    // Determinism: a frozen clock.
    env.insert("BN_NOW".into(), now.to_string());
    env.insert("BN_TODAY".into(), today.into());

    env
}
