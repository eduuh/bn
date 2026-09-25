//! Shared test-support surface.
//!
//! This module is the *single* place both `bn-core` and `bn-cli` reach for when a test needs to
//! mutate process-global env (`BN_STATE_DIR`, `BN_NOTES_DIR`, `BN_JOBS_DIR`, `BN_NOW`, …). Cargo
//! runs a crate's tests as parallel threads of one process, so an unlocked `env::set_var` in one
//! test bleeds into another mid-flight — a classic source of order-dependent flakes. Everything
//! here funnels through [`env_lock`] (a process-wide mutex) and [`EnvGuard`] (RAII set/restore).
//!
//! It compiles under ` the only
//! sanctioned raw calls live in [`EnvGuard`] below.

use crate::resolver::Config;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

pub mod forge;

pub fn config(root: &Path) -> Config {
    config_with_hostname(root, "testhost")
}

pub fn config_with_hostname(root: &Path, hostname: &str) -> Config {
    config_with_roots(
        root.to_path_buf(),
        root.to_path_buf(),
        PathBuf::from("/nonexistent"),
        hostname,
    )
}

pub fn config_with_roots(
    personal_root: impl Into<PathBuf>,
    work_root: impl Into<PathBuf>,
    worktree_dir: impl Into<PathBuf>,
    hostname: &str,
) -> Config {
    Config {
        personal_root: personal_root.into(),
        work_root: work_root.into(),
        worktree_dir: worktree_dir.into(),
        hostname: hostname.into(),
        ..Default::default()
    }
}

pub fn with_state_dir<T>(f: impl FnOnce() -> T) -> T {
    with_state_dir_path(|_| f())
}

#[allow(
    clippy::panic,
    reason = "test setup must fail loudly when its isolated temporary directory cannot be created"
)]
pub fn with_state_dir_path<T>(f: impl FnOnce(&Path) -> T) -> T {
    let _g = env_lock();
    let dir =
        tempfile::tempdir().unwrap_or_else(|error| panic!("create test state directory: {error}"));
    let _state = EnvGuard::set("BN_STATE_DIR", dir.path());
    f(dir.path())
}

pub fn with_testhost_state_and_notes_dir<T>(root: &Path, f: impl FnOnce() -> T) -> T {
    with_state_and_notes_dir(root, "testhost", f)
}

#[allow(
    clippy::panic,
    reason = "test setup must fail loudly when its isolated temporary directory cannot be created"
)]
pub fn with_state_and_notes_dir<T>(root: &Path, hostname: &str, f: impl FnOnce() -> T) -> T {
    let _g = env_lock();
    let dir =
        tempfile::tempdir().unwrap_or_else(|error| panic!("create test state directory: {error}"));
    let _state = EnvGuard::set("BN_STATE_DIR", dir.path());
    let _notes = EnvGuard::set("BN_NOTES_DIR", root);
    let _host = EnvGuard::set("BN_HOSTNAME", hostname);
    // Isolate the shipped-jobs global tier too. Without this, `jobs_dir()` falls through to the
    // real `~/.config/bn/jobs` on an installed machine, so `crons::/staff::discover` leak the
    // host's live `crons.toml`/`config/agents/*.md` (e.g. `staff-chief-*` jobs) into tests and make
    // hermetic counts non-deterministic. Point it at an isolated, empty path so only the
    // test-populated tiers under `root` are read — matching CI, where `~/.config/bn/jobs` is absent.
    let _jobs = EnvGuard::set("BN_JOBS_DIR", dir.path().join("global-jobs"));
    // Isolate the agent-definition tier too. `staff::discover` reads `agents_dir()`, which falls
    // through to the real `<config_dir>/agents` (an installed machine's live `config/agents/*.md`)
    // when `BN_AGENTS_DIR` is unset — so `doctor`'s staff findings would otherwise leak the host's
    // real staff roster (`staff-chief-*`, `sentinel`, …) into a test as spurious missed-run rows.
    // Point it at an isolated, empty path so only test-declared agents are discovered, matching CI
    // where the host agent tier is absent. A test that needs its own roster sets `BN_AGENTS_DIR`
    // after this (its guard shadows for the test body).
    let _agents = EnvGuard::set("BN_AGENTS_DIR", dir.path().join("global-agents"));
    // Isolate the pinned machine identity. `Config::resolve` *writes* this file on first use, so
    // an unisolated test would pin (or read) the developer's real `<config_dir>/machine` — a test
    // deciding a live machine's identity is exactly the surprise the pin exists to prevent.
    // `BN_HOSTNAME` above already wins for these tests; this just keeps the fallback hermetic.
    let _machine = EnvGuard::set("BN_MACHINE_FILE", dir.path().join("machine"));
    f()
}

/// Process-wide lock for tests that mutate global env vars (`BN_STATE_DIR`, `BN_JOBS_DIR`, …).
/// Cargo runs a crate's tests in parallel threads of one process, so any test touching env must
/// serialize against every other — a per-module lock isn't enough. Poison-tolerant. Hold the
/// returned guard for the whole span in which the env var is set *and* read.
pub fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::Mutex;
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// RAII guard that sets (or removes) one env var and restores its prior value on drop — so a
/// mutation can't leak into a later test even in a single-threaded run. Always pair it with a held
/// [`env_lock`] guard for the duration the var is live.
pub struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

#[allow(clippy::disallowed_methods)] // this IS the sanctioned wrapper around env mutation
impl EnvGuard {
    /// Set `key` to `value`, remembering the prior value to restore on drop.
    pub fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }

    /// Remove `key`, remembering the prior value to restore on drop. Use when a test needs a var
    /// *absent* (e.g. forcing a fallback path) without clobbering the developer's real environment.
    pub fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, previous }
    }
}

#[allow(clippy::disallowed_methods)] // restoring the saved value is the guard's whole job
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}
