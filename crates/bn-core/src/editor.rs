//! The per-worktree nvim: where its socket lives, and the tmux option names that find its pane.
//!
//! bn drives one long-lived nvim per worktree — `bn edit` creates it, parks it, restores it, and
//! (via `--server <sock> --remote-expr`) tells it to open a diff or land a session. Two rules make
//! the rest of that machinery simple:
//!
//! **Identity is the canonical absolute worktree path, never `repo/branch`.** Two repos can hold
//! the same branch name, and a pool slot or a branch deleted underneath a worktree is *detached
//! with no branch at all* — the failure `wt::window` documents at length, where an empty branch
//! matched every plain window on the server. A path cannot be empty and cannot collide.
//!
//! **The socket path is derived, never recorded.** Any process anywhere inside the worktree finds
//! its editor from the path alone, with zero inherited state — which is what lets an agent launched
//! in that worktree reach the nvim hosting it. Nothing has to be kept in sync, so nothing can go
//! stale: liveness is one `connect()`, and a refused connect *is* the answer.

use crate::hash::fnv1a64;
use std::path::{Path, PathBuf};

/// The detached tmux session that holds parked editor panes. Leading `_` marks it as bn-internal
/// so the pickers skip it (see `bn tat`'s orphan rows and `choose-tree`'s filter).
pub const HOLD_SESSION: &str = "_bn_edit";

/// **Pane** option: the absolute worktree path this nvim serves.
///
/// Pane options inherit from window options, so this must only ever be set with `set-option -p`.
/// Set at window scope it would make every pane in that window claim to be the editor — the same
/// trap `wt::window` documents for `@bn_branch`.
pub const PANE_OPT: &str = "@bn_edit";

/// **Pane** option: the `@<window_id>` a parked editor belongs back in. Pane-scope only, as above.
pub const PANE_HOME_OPT: &str = "@bn_edit_home";

/// **Window** option: the `%<pane_id>` currently hosting this window's editor, unset while parked.
/// Deliberately a *different* name from [`PANE_OPT`] so pane→window inheritance can never leak a
/// window's value into a pane lookup.
pub const WINDOW_OPT: &str = "@bn_editor";

/// **Window** option: `shell` | `agent` | `editor` — which shape this worktree window is, so
/// `bn edit hide` knows whether there is a guest pane to move at all.
pub const SHAPE_OPT: &str = "@bn_shape";

/// Canonicalize a worktree path for identity purposes, falling back to the path as given when it
/// cannot be resolved (a worktree removed underneath us still needs a stable key to clean up by).
pub fn canonical(worktree: &Path) -> PathBuf {
    worktree
        .canonicalize()
        .unwrap_or_else(|_| worktree.to_path_buf())
}

/// The 16-hex identity of a worktree — a hash, not the path itself.
///
/// `AF_UNIX` `sun_path` is 108 bytes; a real worktree path under a runtime dir already crowds that,
/// and a work repo with a long branch name would blow it outright. Hashing also sidesteps `/` in
/// branch names without a lossy sanitize step.
pub fn worktree_key(worktree: &Path) -> String {
    let canon = canonical(worktree);
    format!("{:016x}", fnv1a64(canon.as_os_str().as_encoded_bytes()))
}

/// Where per-boot editor runtime files live: `$XDG_RUNTIME_DIR/bn/edit`, else `<tmp>/bn-<uid>/edit`.
///
/// Deliberately **not** under [`crate::resolver::state_dir`], which is documented as living in this
/// machine's subtree of the *synced notes repo*. Sockets and lockfiles die with the boot; putting
/// them there would commit them.
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir).join("bn").join("edit");
    }
    std::env::temp_dir()
        .join(format!("bn-{}", fallback_uid()))
        .join("edit")
}

/// The invoking user's uid, read from the home directory's owner. Avoids `libc::getuid` (the
/// workspace denies `unsafe_code`); `0` when it cannot be determined, which only ever widens a
/// fallback path that the 0700 mode on [`ensure_runtime_dir`] still protects.
fn fallback_uid() -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Some(home) = dirs::home_dir() {
            if let Ok(meta) = std::fs::metadata(home) {
                return meta.uid();
            }
        }
    }
    0
}

/// Create [`runtime_dir`] if absent, owner-only. Returns the directory either way so callers can
/// build paths under it even when creation failed (the subsequent open reports the real error).
pub fn ensure_runtime_dir() -> PathBuf {
    let dir = runtime_dir();
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir
}

/// This worktree's nvim `--listen` socket.
pub fn socket_for(worktree: &Path) -> PathBuf {
    runtime_dir().join(format!("{}.sock", worktree_key(worktree)))
}

/// This worktree's landing spec — the JSON `require('bn').land_file()` reads.
///
/// A file rather than inline JSON on the command line: it survives tmux's parsing and the shell
/// with a single quote, hits no argv length limit, and the warm (`--remote-expr`) path re-reads
/// *the same bytes* the cold (`nvim -c`) path was given, so the two are provably the same input.
pub fn spec_file_for(worktree: &Path) -> PathBuf {
    runtime_dir().join(format!("{}.land.json", worktree_key(worktree)))
}

/// The advisory lock guarding create/hide/restore for one worktree, so two clients racing
/// `bn edit toggle` cannot both decide the socket is stale and unlink each other's nvim.
pub fn lock_file_for(worktree: &Path) -> PathBuf {
    runtime_dir().join(format!("{}.lock", worktree_key(worktree)))
}
