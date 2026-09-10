//! Low-level parsing, env, and git helpers shared by the resolver submodules.

use crate::runner::{CommandRunner, RealRunner};
use std::env;
use std::path::Path;

pub(super) fn non_empty_env(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

/// Parse a weekday into `0=Sun .. 6=Sat`: a name (`sun`/`sunday`/… case-insensitive,
/// any prefix ≥3 chars) or a bare number `0..6`. `None` if unrecognized.
pub(super) fn parse_weekday(s: &str) -> Option<u32> {
    let s = s.trim().to_lowercase();
    if let Ok(n) = s.parse::<u32>() {
        return (n <= 6).then_some(n);
    }
    const NAMES: [&str; 7] = [
        "sunday",
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
    ];
    NAMES
        .iter()
        .position(|name| name.starts_with(&s) && s.len() >= 3)
        .map(|i| i as u32)
}

/// Cross-platform home dir: `$HOME` on unix, `%USERPROFILE%` on Windows (via `dirs`).
pub(super) fn home() -> String {
    dirs::home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Machine hostname, cross-platform: `gethostname` reads it directly (no `hostname` binary,
/// which native Windows lacks). `$BN_HOSTNAME` overrides upstream in [`Config::resolve`].
pub(super) fn read_hostname() -> String {
    gethostname::gethostname()
        .to_string_lossy()
        .trim()
        .to_string()
}

/// This machine's identity, pinned on first use so it cannot drift.
///
/// Everything machine-scoped hangs off this name — the state dir holding the event log, the queue,
/// and metrics — so it has to answer "which machine am I?" the same way every time. A raw
/// [`read_hostname`] does not: macOS hands out a DHCP name (`mac.lan`) or a Bonjour one
/// (`vivs-MacBook-Pro.local`) depending on the network, and a host that answers differently after
/// lunch silently splits its own history into two directories that each look complete.
///
/// So the first resolution wins and is written down. Later runs read the pin and ignore whatever
/// the network currently claims. Writing is best-effort and only into a config dir that already
/// exists: an unwritable or absent one degrades to today's behaviour rather than failing a command
/// that had nothing to do with identity.
pub(super) fn stable_machine() -> String {
    let path = super::dirs::machine_file_path();
    if let Some(pinned) = std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return pinned;
    }
    let live = read_hostname();
    if !live.is_empty() && path.parent().is_some_and(Path::is_dir) {
        let _ = std::fs::write(&path, format!("{live}\n"));
    }
    live
}

pub(super) fn git_current_branch(dir: &Path) -> Option<String> {
    git(dir, &["branch", "--show-current"])
}

pub(super) fn git_toplevel(dir: &Path) -> Option<String> {
    git(dir, &["rev-parse", "--show-toplevel"])
}

/// The git *common* dir for `dir` (`git rev-parse --git-common-dir`): the bare repo shared by
/// all linked worktrees, or `.git`/`.bare` for an in-place layout. Authoritative repo identity
/// — unlike the worktree directory path, it's the same wherever the worktree lives. `None`
/// outside a git work tree.
pub(super) fn git_common_dir(dir: &Path) -> Option<String> {
    git(dir, &["rev-parse", "--git-common-dir"])
}

/// Run `git -C <dir> <args>`, returning trimmed stdout if the command succeeds and is
/// non-empty. Any failure (not a repo, git missing) ⇒ None. Routes through the shared
/// [`crate::runner`] seam so it's the same testable path the feature reads use.
pub(super) fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let dir_s = dir.to_string_lossy();
    let mut full: Vec<&str> = Vec::with_capacity(args.len() + 2);
    full.push("-C");
    full.push(dir_s.as_ref());
    full.extend_from_slice(args);
    RealRunner.git(&full).ok_stdout()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{env_lock, EnvGuard};

    #[test]
    fn a_pinned_identity_wins_over_whatever_the_network_calls_the_host() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pin = dir.path().join("machine");
        std::fs::write(&pin, "mac.lan\n").expect("write pin");
        let _guard = EnvGuard::set("BN_MACHINE_FILE", &pin);

        // The live hostname is whatever this test host happens to be; the pin must win regardless.
        assert_eq!(stable_machine(), "mac.lan");
    }

    #[test]
    fn the_first_resolution_pins_the_identity_so_later_ones_cannot_drift() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pin = dir.path().join("machine");
        let _guard = EnvGuard::set("BN_MACHINE_FILE", &pin);

        let first = stable_machine();
        assert!(pin.is_file(), "first resolution must write the pin");
        assert_eq!(
            std::fs::read_to_string(&pin).expect("read pin").trim(),
            first
        );
        // Simulate the host renaming itself: the pin, not the new name, is what answers.
        std::fs::write(&pin, "renamed-by-dhcp\n").expect("rewrite pin");
        assert_eq!(stable_machine(), "renamed-by-dhcp");
    }

    #[test]
    fn a_blank_pin_falls_back_instead_of_yielding_an_empty_machine() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pin = dir.path().join("machine");
        std::fs::write(&pin, "   \n").expect("write blank pin");
        let _guard = EnvGuard::set("BN_MACHINE_FILE", &pin);

        // An empty name would resolve every machine-scoped path to the same directory.
        assert_eq!(stable_machine(), read_hostname());
    }

    #[test]
    fn an_absent_config_dir_degrades_rather_than_creating_one() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let pin = dir.path().join("no-such-dir").join("machine");
        let _guard = EnvGuard::set("BN_MACHINE_FILE", &pin);

        assert_eq!(stable_machine(), read_hostname());
        assert!(!pin.exists(), "must not conjure a config dir to pin into");
    }
}
