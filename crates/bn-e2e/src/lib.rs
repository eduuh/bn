//! A hermetic sandbox for end-to-end testing the `bn` binary.
//!
//! Every test gets its own bn universe — its own `HOME`, config, state, notes, worktree pool,
//! dashboard port and **tmux server** — built from one contract ([`env::contract`]) instead of the
//! per-script hand-rolled subsets that leaked fixtures into the real notes twice already.
//!
//! The tmux half is the part that needs care. bn drives the *default* tmux socket across
//! `agent_lease`, `reap`, `agent_live`, `jump` and `connect`, and it calls `kill-window` on windows
//! it found by enumerating the whole server. A test suite that reaches the developer's live server
//! does not fail — it closes their windows. So the harness proves isolation before it hands out a
//! sandbox ([`proof::assert_isolated`]) and re-checks the host at teardown
//! ([`proof::assert_host_untouched`]) rather than trusting the environment to have taken effect.
//!
//! ```no_run
//! # fn main() -> anyhow::Result<()> {
//! use bn_e2e::{Sandbox, shims::Behavior};
//! let sb = Sandbox::new()?;
//! sb.fake_agent("claude", &Behavior::Hang)?;
//! sb.bn(["note", "goal", "ship it"])?.expect_success()?;
//! assert!(sb.tmux_sessions()?.is_empty());
//! # Ok(())
//! # }
//! ```
//!
//! Set `BN_E2E_KEEP=1` to leave the sandbox root on disk after a run; the path is printed.

pub mod env;
pub mod proof;
pub mod shims;

use anyhow::{anyhow, bail, Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A frozen clock for every sandbox: 2026-01-15T12:00:00Z. Deterministic dates in output.
const FROZEN_NOW: i64 = 1_768_478_400;
const FROZEN_TODAY: &str = "2026-01-15";

/// One isolated bn universe. Dropping it tears down the sandbox tmux server, any bn-web child,
/// and the temporary root.
pub struct Sandbox {
    root: PathBuf,
    env: BTreeMap<String, String>,
    port: u16,
    bn_bin: PathBuf,
    calls: PathBuf,
    host_sessions_before: Option<Vec<String>>,
    keep: bool,
    /// Whether [`proof::assert_isolated`] has *succeeded* for this sandbox's environment.
    ///
    /// Teardown runs `tmux kill-server` with `self.env`, and that is only safe if the environment
    /// genuinely reaches the sandbox's own server. Until the canary has proven it does, the
    /// environment is untrusted and the kill must not happen: a contract that fails to strip
    /// `$TMUX` sends `kill-server` to the developer's live server and destroys every session on
    /// the machine. This flag is the difference between a guard that refuses and a guard that
    /// refuses *and then does the damage anyway* on its way out.
    isolation_proven: bool,
}

impl Sandbox {
    /// Build a sandbox, prove tmux isolation, and seed a config plus an empty git repo.
    pub fn new() -> Result<Self> {
        let root = tempfile::Builder::new()
            .prefix("bn-e2e-")
            .tempdir()
            .context("create sandbox root")?
            // Into the raw path: teardown is ordered by hand in `Drop` (tmux server first, then
            // the directory), which a TempDir's own drop cannot express.
            .keep();

        let port = free_port().context("allocate a dashboard port for this sandbox")?;
        let mut env = env::contract(&root, port, FROZEN_NOW, FROZEN_TODAY);

        let shim_dir = root.join("shims");
        let calls = root.join("calls.log");
        let inherited_path = std::env::var("PATH").unwrap_or_default();
        env.insert(
            "PATH".into(),
            format!("{}:{inherited_path}", shim_dir.display()),
        );

        for dir in [
            "home/.bn/config",
            "home/.bn/bin",
            "home/.bn/repos",
            "state",
            "notes",
            "worktrees",
            "tmux",
            "shims",
            "repo",
        ] {
            std::fs::create_dir_all(root.join(dir))
                .with_context(|| format!("create sandbox dir {dir}"))?;
        }

        shims::install_defaults(&shim_dir, &calls)?;
        std::fs::write(
            root.join("home/.bn/config/config.toml"),
            format!(
                "[personal]\nnotes_root = \"{}\"\n\n[web]\nport = {port}\nbind = \"127.0.0.1\"\n\n[machines.e2ehost]\nrepos = [\"repo\"]\nqueue = true\ncron = true\nweb = true\n",
                root.join("notes").display()
            ),
        )
        .context("seed config.toml")?;
        std::fs::write(
            root.join("gitconfig"),
            "[user]\n\tname = bn-e2e\n\temail = e2e@example.invalid\n[init]\n\tdefaultBranch = main\n",
        )
        .context("seed sandbox gitconfig")?;

        // The stable installed-bn anchor. `dispatch::agent_launcher_bin` builds the tmux command
        // line as `$HOME/.config/bn/bn agent launch …` — the same path install.sh creates with
        // `ln -sfn "$bindir/bn" "$cfgdir/bn"` plus the `~/.config/bn` compat link. Without it the
        // dispatched window opens but the agent never starts, because the launcher path does not
        // exist. Seeding it here keeps the sandbox faithful to an installed machine.
        let bn_bin = locate_bn()?;
        let anchor_dir = root.join("home/.config/bn");
        std::fs::create_dir_all(&anchor_dir).context("create the installed-bn anchor dir")?;
        symlink_binary(&bn_bin, &anchor_dir.join("bn"))?;

        // The shipped-content anchor, install.sh's `ln -sfn "$repo" "$cfgdir/repo"` (line 699).
        // `skills::shared_skills_dir` resolves the shipped skills through
        // `<config_dir>/repo/config/prompts`, and `bn dispatch --agent` refuses to launch without
        // the skill its stage requires ("required skill 'investigate' not found"). Pointing the
        // anchor at this checkout means the sandbox exercises the skills actually in the tree.
        symlink_binary(&workspace_root()?, &config_dir_of(&root).join("repo"))?;

        let host_sessions_before = proof::host_sessions();

        // Prove isolation BEFORE constructing the Sandbox, not after.
        //
        // This ordering is load-bearing and was learned the hard way. Building the struct first
        // and checking second means a failed check returns through `?`, which drops the
        // half-built sandbox, which runs `Drop` — and `Drop` called `tmux kill-server` with the
        // very environment the check had just rejected. A contract that failed to strip `$TMUX`
        // therefore refused the run and then killed every tmux session on the developer's
        // machine on its way out. The guard has to hold before anything exists that can tear
        // itself down.
        let mut isolation_proven = false;
        if proof::tmux_available() {
            // A unique canary name so concurrent sandboxes cannot confuse each other.
            let canary = format!(
                "bn-e2e-canary-{}-{}",
                std::process::id(),
                root.file_name().unwrap_or_default().to_string_lossy()
            );
            proof::assert_isolated(&env, &canary).inspect_err(|_| {
                // The sandbox root is all we may clean up here: the tmux server is off limits
                // precisely because we could not prove which one it is.
                let _ = std::fs::remove_dir_all(&root);
            })?;
            isolation_proven = true;
        }

        let sandbox = Self {
            bn_bin,
            calls,
            env,
            isolation_proven,
            keep: std::env::var("BN_E2E_KEEP").is_ok_and(|v| v == "1"),
            host_sessions_before,
            port,
            root,
        };

        sandbox.git(["init", "-q", "."])?;
        sandbox
            .git(["commit", "-q", "--allow-empty", "-m", "init"])?
            .expect_success()?;

        Ok(sandbox)
    }

    /// The sandbox root. Everything this universe touches lives under here.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A path inside the sandbox.
    pub fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.join(relative)
    }

    /// The git repo every `bn` invocation runs in by default.
    pub fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    /// The dashboard port this sandbox owns — unique per sandbox, so instances run concurrently.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Run `bn` inside the sandbox, from the sandbox repo.
    pub fn bn<I, S>(&self, args: I) -> Result<Run>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut cmd = self.command(&self.bn_bin.clone());
        cmd.args(args);
        self.finish(cmd, "bn")
    }

    /// Run `git` inside the sandbox repo, with the sandbox's pinned global config.
    pub fn git<I, S>(&self, args: I) -> Result<Run>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut cmd = self.command(Path::new("git"));
        cmd.args(args);
        self.finish(cmd, "git")
    }

    /// Seed the bare+worktree layout `bn dispatch` requires, returning the bare repo's path.
    ///
    /// `bn dispatch` refuses to run without `<bare_dir>/<repo>.git` ("dispatch needs the
    /// bare+worktree layout"), so any test of the tmux workflow has to build one first. The seeded
    /// repo has one commit on `main`, which is what `dispatch` branches from.
    pub fn seed_bare_repo(&self, name: &str) -> Result<PathBuf> {
        let bare = self.path("projects/bare").join(format!("{name}.git"));
        std::fs::create_dir_all(self.path("projects/bare"))?;
        std::fs::create_dir_all(self.path("projects/worktree"))?;

        // Clone from the sandbox's seeded repo so the bare has a real commit and a `main` head;
        // an empty `git init --bare` has no branch for dispatch to base a worktree on.
        self.git_in(
            &self.root,
            [
                "clone",
                "-q",
                "--bare",
                &self.repo().to_string_lossy(),
                &bare.to_string_lossy(),
            ],
        )?
        .expect_success()?;
        Ok(bare)
    }

    /// Run git with an explicit working directory (the sandbox repo may not exist yet).
    pub fn git_in<I, S>(&self, dir: &Path, args: I) -> Result<Run>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut cmd = self.command(Path::new("git"));
        cmd.current_dir(dir);
        cmd.args(args);
        self.finish(cmd, "git")
    }

    /// Run `bn` from an explicit working directory inside the sandbox.
    pub fn bn_in<I, S>(&self, dir: &Path, args: I) -> Result<Run>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut cmd = self.command(&self.bn_bin.clone());
        cmd.current_dir(dir);
        cmd.args(args);
        self.finish(cmd, "bn")
    }

    /// Replace a PATH shim with a scripted behaviour.
    pub fn fake_agent(&self, name: &str, behavior: &shims::Behavior) -> Result<()> {
        shims::write(&self.root.join("shims"), &self.calls, name, behavior)
    }

    /// Every shim invocation so far, as `(binary, cwd, args)`.
    pub fn shim_calls(&self) -> Vec<(String, String, String)> {
        shims::calls(&self.calls)
    }

    /// Run tmux against the sandbox's own server.
    pub fn tmux(&self, args: &[&str]) -> Result<Output> {
        proof::sandbox_tmux(&self.env, args)
    }

    /// Session names on the sandbox tmux server (empty when no server is running).
    pub fn tmux_sessions(&self) -> Result<Vec<String>> {
        self.tmux_list(&["list-sessions", "-F", "#{session_name}"])
    }

    /// Window names on the sandbox tmux server, across all sessions.
    pub fn tmux_windows(&self) -> Result<Vec<String>> {
        self.tmux_list(&["list-windows", "-a", "-F", "#{session_name}:#{window_name}"])
    }

    /// The visible text of a pane — for asserting an agent actually started.
    pub fn capture_pane(&self, target: &str) -> Result<String> {
        let out = self.tmux(&["capture-pane", "-p", "-t", target])?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Poll `condition` until it holds or `timeout` elapses.
    ///
    /// Dispatch returns as soon as the tmux window exists; the agent inside the pane starts a
    /// moment later. Asserting immediately would be a race, and a fixed `sleep` would be both
    /// slower and still racy on a loaded machine.
    pub fn wait_for(
        &self,
        what: &str,
        timeout: std::time::Duration,
        mut condition: impl FnMut(&Self) -> bool,
    ) -> Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if condition(self) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                bail!("timed out after {timeout:?} waiting for {what}");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Confirm the developer's tmux server is exactly as it was. Call at the end of a tmux test;
    /// `Drop` also checks, but an explicit call attributes the failure to the right test.
    pub fn assert_host_untouched(&self) -> Result<()> {
        proof::assert_host_untouched(&self.host_sessions_before)
    }

    fn tmux_list(&self, args: &[&str]) -> Result<Vec<String>> {
        let out = self.tmux(args)?;
        if !out.status.success() {
            // "no server running" is the ordinary empty case, not a failure.
            return Ok(Vec::new());
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect())
    }

    fn command(&self, program: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(self.repo());
        for key in env::REMOVED {
            cmd.env_remove(key);
        }
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        cmd
    }

    fn finish(&self, mut cmd: Command, label: &str) -> Result<Run> {
        let out = cmd
            .output()
            .with_context(|| format!("spawn {label} in sandbox"))?;
        Ok(Run {
            label: label.to_string(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            code: out.status.code(),
        })
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // Kill the sandbox tmux server first: its panes hold the sandbox root open, and on some
        // filesystems removing the tree under a live process leaves the server writing into a
        // deleted directory.
        //
        // Gated on the canary having passed. `kill-server` is the single most destructive thing
        // this harness does, and it is aimed by an environment variable — so it may only fire at
        // an environment that has been *proven* to reach the sandbox's own server. Without the
        // gate, a contract that fails to strip `$TMUX` turns teardown into "destroy every tmux
        // session on this machine", which is precisely the outcome the harness exists to prevent.
        // Leaking a sandbox server is a nuisance; killing the developer's is not recoverable.
        if self.isolation_proven {
            if let Err(e) = proof::kill_server(&self.env) {
                eprintln!("bn-e2e: leaving the sandbox tmux server running — {e}");
            }
        }

        // Never panic while already unwinding — that aborts the process and hides the real
        // failure. A leak during a passing run is worth failing on; during a failing one, the
        // original assertion is the more useful message.
        if !std::thread::panicking() {
            if let Err(e) = proof::assert_host_untouched(&self.host_sessions_before) {
                panic!("{e}");
            }
        }

        if self.keep {
            eprintln!("bn-e2e: keeping sandbox at {}", self.root.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

/// The result of one sandboxed command.
#[derive(Debug, Clone)]
pub struct Run {
    pub label: String,
    pub stdout: String,
    pub stderr: String,
    pub code: Option<i32>,
}

impl Run {
    /// Fail with the captured output when the command did not exit 0.
    pub fn expect_success(&self) -> Result<&Self> {
        if self.code == Some(0) {
            return Ok(self);
        }
        bail!(
            "{} exited {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.label,
            self.code,
            self.stdout.trim(),
            self.stderr.trim()
        )
    }

    /// stdout and stderr together — bn writes user-facing text to both.
    pub fn output(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// Find the `bn` binary: `$BN_E2E_BIN`, else the workspace's debug build.
fn locate_bn() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("BN_E2E_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        bail!(
            "BN_E2E_BIN points at {}, which is not a file",
            path.display()
        );
    }
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| anyhow!("cannot locate the workspace root from CARGO_MANIFEST_DIR"))?;
    let debug = workspace.join("target/debug/bn");
    if debug.is_file() {
        return Ok(debug);
    }
    bail!(
        "no bn binary at {} — run `cargo build --workspace` first, or set BN_E2E_BIN",
        debug.display()
    )
}

/// A sandbox's config dir, matching [`env::contract`].
fn config_dir_of(root: &Path) -> PathBuf {
    root.join("home/.bn/config")
}

/// The bn checkout this harness was compiled from.
fn workspace_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("cannot locate the workspace root from CARGO_MANIFEST_DIR"))
}

/// Link `target` at `link`, mirroring install.sh's `ln -sfn`. Copies where symlinks are
/// unavailable, so the anchor exists either way.
fn symlink_binary(target: &Path, link: &Path) -> Result<()> {
    if link.exists() || link.symlink_metadata().is_ok() {
        let _ = std::fs::remove_file(link);
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
            .with_context(|| format!("link {} -> {}", link.display(), target.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(target, link)?;
    }
    Ok(())
}

/// An unused loopback port, found by binding :0 and reading back the assignment.
///
/// There is a window between releasing this and bn-web binding it. bn-web rejects `--port 0`
/// (`bn-web/src/main.rs:96`), so it cannot report a kernel-assigned port itself; teaching it to
/// would close the window entirely.
fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}
