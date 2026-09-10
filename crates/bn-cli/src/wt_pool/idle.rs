//! What a pool slot sheds once a reset leaves it idle again: Rust incremental caches under
//! `target/`, and the dirty mark that kept warm away from it.
//!
//! Split out of `wt_pool.rs` under the 1000-line ratchet.
use super::*;

/// Finish a reset: drop the slot's incremental caches, then clear its dirty mark.
pub(super) fn settle(slot: &Path) {
    trim_idle_incremental_dirs(slot);
    if let Some((_, n, pdir)) = slot_parts(slot) {
        clear_dirty_mark(&pdir, n);
    }
}

/// Trim Rust incremental caches from an idle slot without touching the rest of its ignored build
/// output. The pool's warm promise is "deps and build outputs are already there", and warm itself
/// builds with `CARGO_INCREMENTAL=0`, so `target/**/incremental` is the one ignored subtree a
/// reset can safely reclaim.
fn trim_idle_incremental_dirs(slot: &Path) {
    trim_incremental_dirs_under(&slot.join("target"));
}

fn trim_incremental_dirs_under(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        let path = entry.path();
        if entry.file_name().to_str() == Some("incremental") {
            if let Err(error) = std::fs::remove_dir_all(&path) {
                ceprintln!(
                    "{YELLOW}Could not trim idle incremental cache {}: {error}{NC}",
                    path.display()
                );
            }
            continue;
        }
        trim_incremental_dirs_under(&path);
    }
}
