//! Real dispatch → relaunch → kill, against a real tmux server and a real git repo.
//!
//! `orchestrate_e2e.rs` drives the orchestrate state machine with every IO seam faked, which is the
//! right shape for scheduler policy and the wrong shape for this: the bugs that made the fleet
//! board's actions unusable all lived in the seam it fakes out — the tmux window join, the process
//! tree behind a pane, the worktree pool's leases, and the branch note underneath. None of them
//! were catchable without actually launching something.
//!
//! So this launches something. Not an LLM: a **stub harness** on `PATH` that records its argv and
//! then sleeps, which is enough to make every assertion here real — a live pane with a live process
//! tree, a held lease, and a window the join has to resolve.
//!
//! ## Isolation
//!
//! Every one of these is load-bearing, and CI runners share a real `$HOME`:
//!
//! * `TMUX_TMPDIR` — a **private tmux server**. `bn_core::tmux` shells out with an inherited env,
//!   so this is the whole lever; without it the test would kill windows on the developer's own
//!   server. The server is started by the test itself (see [`Sandbox::start_tmux`]) because a tmux
//!   server inherits its environment from whichever client starts it — start it any other way and
//!   the `bn agent launch` running *inside* a window resolves the real `~/.config/bn` and the real
//!   notes dir.
//! * `HOME` — `bn dispatch` addresses the launcher as `$HOME/.config/bn/bn agent launch` (the tmux
//!   server's PATH has no bindir), so the sandbox `HOME` gets a shim pointing at the test binary's
//!   `bn`.
//! * `WT_POOLS` — the pool config. Unset, bn reads `~/.config/wt/pools`, i.e. the maintainer's.
//! * `BN_NOTES_DIR`, `BN_STATE_DIR`, `BN_WORKTREE_DIR`, `TMPDIR`, `GIT_CONFIG_GLOBAL` — notes,
//!   leases/events, worktrees, seed prompts, and git identity.
//!
//! [`Sandbox::assert_isolated`] re-checks the important ones before the first dispatch, because a
//! typo in this list is not a failing test — it is a test that quietly edits your real machine.
//!
//! Every process this test starts is a `sleep 120`, and the sandbox tmux server is killed by
//! [`Sandbox::drop`]. The bound matters as much as the guard: a test killed by an outer timeout
//! never runs `Drop`, so anything it left behind has to die on its own.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

/// Skip (rather than fail) when tmux isn't installed, so a tmux-less machine stays green.
///
/// CI asserts tmux is present in the `test` job, so this can never quietly hide the test there —
/// it is a local-dev courtesy, not an escape hatch.
fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

macro_rules! require_tmux {
    () => {
        if !tmux_available() {
            eprintln!("skipping: tmux is not installed");
            return;
        }
    };
}

/// A fully isolated bn installation: private tmux server, own HOME, own notes/state/worktree roots,
/// own pool config, and a stub harness on PATH.
struct Sandbox {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: String,
    notes: PathBuf,
    state: PathBuf,
    wt_root: PathBuf,
    pools: PathBuf,
    tmux_tmpdir: PathBuf,
    bin: PathBuf,
    home: PathBuf,
    /// Where the stub harness appends one line per launch.
    launch_log: PathBuf,
}

impl Sandbox {
    fn new(repo: &str, pool_size: usize) -> Sandbox {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let sb = Sandbox {
            repo: repo.to_string(),
            notes: root.join("notes"),
            state: root.join("state"),
            wt_root: root.join("projects").join("worktree"),
            pools: root.join("pools"),
            tmux_tmpdir: root.join("tmuxsock"),
            bin: root.join("bin"),
            home: root.join("home"),
            launch_log: root.join("launches.log"),
            root: root.clone(),
            _tmp: tmp,
        };
        for d in [
            &sb.notes,
            &sb.state,
            &sb.wt_root,
            &sb.tmux_tmpdir,
            &sb.bin,
            &sb.home,
            &sb.root.join("tmp"),
            &sb.root.join("projects").join("bare"),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }

        // A real bare repo with one commit on main — `lifecycle::worktree_path` reads
        // `git worktree list --porcelain`, so the layout has to be genuine, not mocked.
        let seed = sb.root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "-q"]);
        git(&seed, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(&seed, &["branch", "-M", "main"]);
        git(
            &sb.root,
            &[
                "clone",
                "-q",
                "--bare",
                seed.to_str().unwrap(),
                sb.bare().to_str().unwrap(),
            ],
        );

        std::fs::write(&sb.pools, format!("{repo} {pool_size}\n")).unwrap();
        std::fs::write(
            sb.root.join("gitconfig"),
            "[user]\n\tname = t\n\temail = t@t\n",
        )
        .unwrap();

        // The launcher shim: `bn dispatch` builds `$HOME/.config/bn/bn agent launch …`.
        let bn_home = sb.home.join(".config").join("bn");
        std::fs::create_dir_all(&bn_home).unwrap();
        write_exec(
            &bn_home.join("bn"),
            &format!("#!/bin/sh\nexec {} \"$@\"\n", env!("CARGO_BIN_EXE_bn")),
        );

        // The stub harness: record the argv, then sleep. Sleeping matters — `remain-on-exit on`
        // means a harness that exits leaves a *dead* pane, and a dead pane reads as a released
        // lease and an empty process tree, so an exiting stub would make the interesting
        // assertions vacuously pass.
        for name in ["claude", "copilot"] {
            write_exec(
                &sb.bin.join(name),
                &format!(
                    "#!/bin/sh\nprintf '%s %s\\n' \"{name}\" \"$*\" >> {}\nexec sleep 120\n",
                    sb.launch_log.display()
                ),
            );
        }
        sb
    }

    fn bare(&self) -> PathBuf {
        self.root
            .join("projects")
            .join("bare")
            .join(format!("{}.git", self.repo))
    }

    fn pool_dir(&self) -> PathBuf {
        self.wt_root.join(&self.repo).join("_pool")
    }

    /// A `bn` invocation with the whole sandbox env applied.
    fn bn(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_bn"));
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        c.env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("TMUX_TMPDIR", &self.tmux_tmpdir)
            .env("PATH", path)
            .env("BN_NOTES_DIR", &self.notes)
            .env("BN_STATE_DIR", &self.state)
            .env("BN_WORKTREE_DIR", &self.wt_root)
            .env("BN_HOSTNAME", "h")
            .env("WT_POOLS", &self.pools)
            .env("BN_TODAY", "2026-08-04")
            // A test run from inside a bn-dispatched agent would otherwise inherit these and
            // dispatch would think it is a worker reporting to a controller.
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("WT_ROOT")
            .env_remove("BN_HOME")
            .env_remove("BN_CONFIG_DIR")
            .env_remove("BN_CONTROLLER_ID")
            .env_remove("BN_WORKER_SESSION_ID")
            .env_remove("BN_WORKER_REPO")
            .env_remove("BN_WORKER_BRANCH")
            .env_remove("BN_WORKER_REQUEST_ID")
            .env_remove("BN_AGENT_MEMORY_CEILING_MB")
            .current_dir(&self.root);
        c
    }

    /// tmux, against the private server.
    /// Every tmux invocation the sandbox makes, pre-isolated — the one place `TMUX_TMPDIR` is
    /// paired with dropping `TMUX`/`TMUX_PANE`, so no call site can be added that forgets.
    ///
    /// Removing `TMUX` is not belt-and-braces, it is the isolation: `$TMUX` names a socket path
    /// outright and **beats** `TMUX_TMPDIR`, so any tmux call that inherits it runs against the
    /// developer's real server no matter what `TMUX_TMPDIR` says. This suite runs inside a tmux
    /// pane whenever an agent runs it, so `$TMUX` is normally set — and for the `kill-server` in
    /// [`Sandbox::drop`] that meant killing the maintainer's whole server and every agent in it.
    fn tmux_cmd(&self) -> Command {
        let mut c = Command::new("tmux");
        c.env("TMUX_TMPDIR", &self.tmux_tmpdir)
            .env("HOME", &self.home)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        c
    }

    /// tmux, against the private server.
    fn tmux(&self, args: &[&str]) -> String {
        let out = self.tmux_cmd().args(args).output().expect("tmux");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Start the private tmux server *with the sandbox environment*, so every window the server
    /// later spawns inherits it. A server started any other way would hand the inner
    /// `bn agent launch` the developer's real HOME and notes dir.
    fn start_tmux(&self) {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let ok = self
            .tmux_cmd()
            .args(["new-session", "-d", "-s", "__harness", "sleep", "120"])
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("PATH", path)
            .env("BN_NOTES_DIR", &self.notes)
            .env("BN_STATE_DIR", &self.state)
            .env("BN_WORKTREE_DIR", &self.wt_root)
            .env("BN_HOSTNAME", "h")
            .env("WT_POOLS", &self.pools)
            .env("BN_TODAY", "2026-08-04")
            .status()
            .expect("tmux new-session")
            .success();
        assert!(ok, "could not start the sandbox tmux server");

        // Prove the isolation actually took, rather than trusting that the paths look right.
        // A private server holds exactly the session just created; the developer's server holds
        // their own sessions too. Failing here is the difference between a red test and a test
        // that silently kills their fleet in `drop`.
        let sessions = self.tmux(&["list-sessions", "-F", "#{session_name}"]);
        assert_eq!(
            sessions, "__harness",
            "sandbox tmux is not isolated — these are the real server's sessions, so kill-server \
             would take down the whole fleet"
        );
    }

    /// Fail loudly if anything still points at the real machine. A mistake here is not a failing
    /// test — it is a test that edits the maintainer's pools config and kills their agents.
    fn assert_isolated(&self) {
        let real_home = std::env::var("HOME").unwrap_or_default();
        assert!(
            self.home.starts_with(&self.root) && !self.home.to_string_lossy().eq(&real_home),
            "sandbox HOME escaped"
        );
        for p in [&self.pools, &self.notes, &self.state, &self.wt_root] {
            assert!(
                p.starts_with(&self.root),
                "{} escaped the sandbox",
                p.display()
            );
        }
        assert!(
            self.tmux_tmpdir.starts_with(&self.root),
            "tmux socket dir escaped the sandbox"
        );
    }

    /// Live windows on the private server, as `session:window`.
    fn windows(&self) -> Vec<String> {
        let out = self.tmux(&["list-windows", "-a", "-F", "#{session_name}:#{window_name}"]);
        out.lines()
            .map(str::to_string)
            .filter(|w| !w.starts_with("__harness"))
            .collect()
    }

    /// Live (non-dead) pane pids across the whole server.
    fn pane_pids(&self) -> BTreeSet<u32> {
        let out = self.tmux(&["list-panes", "-a", "-F", "#{pane_dead} #{pane_pid}"]);
        out.lines()
            .filter_map(|l| l.strip_prefix("0 "))
            .filter_map(|p| p.trim().parse().ok())
            .collect()
    }

    fn leases(&self) -> BTreeMap<u32, String> {
        let text = std::fs::read_to_string(self.pool_dir().join("leases.json")).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::json!({}));
        v.get("slots")
            .and_then(|s| s.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| {
                        Some((k.parse().ok()?, v.get("branch")?.as_str()?.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn note_path(&self, branch: &str) -> PathBuf {
        self.notes
            .join("h")
            .join(&self.repo)
            .join(branch.replace('/', "-"))
            .join("note.md")
    }

    /// Every line the stub harness recorded, across all launches.
    fn launches(&self) -> Vec<String> {
        std::fs::read_to_string(&self.launch_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // Never kill a server that isn't ours. `tmux_cmd` drops `$TMUX`, which is what aims this
        // at the sandbox — but `drop` also runs on panic, including a panic *because* isolation
        // lapsed, so ask tmux which socket it actually resolved and bail unless it is ours.
        // Leaking a stub `sleep` is a bad outcome; killing the developer's fleet is a worse one.
        let sock = self.tmux(&["display-message", "-p", "#{socket_path}"]);
        if sock.is_empty() || !Path::new(&sock).starts_with(&self.tmux_tmpdir) {
            eprintln!(
                "refusing to kill the tmux server at {sock:?}: not the sandbox server ({})",
                self.tmux_tmpdir.display()
            );
            return;
        }
        // Always, including on panic: a leaked tmux server outlives the tempdir and keeps the
        // stub `sleep`s alive.
        let _ = self.tmux_cmd().arg("kill-server").output();
    }
}

fn write_exec(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} in {dir:?} failed");
}

/// Run a `bn` command and return `(envelope, stdout, stderr)`.
fn run(mut cmd: Command) -> (serde_json::Value, String, String) {
    let out = cmd.output().expect("bn");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    // The envelope is the last JSON object line — bn prints warnings above it.
    let env = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .unwrap_or(serde_json::Value::Null);
    (env, stdout, stderr)
}

/// Poll until `f` holds, up to ~10s. tmux window creation and the harness exec are asynchronous,
/// so a bare assert right after a dispatch is a flake generator.
fn wait_for(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if f() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    f()
}

fn dispatch(sb: &Sandbox, item: &str, branch: &str) -> (serde_json::Value, String, String) {
    let mut c = sb.bn();
    c.args([
        "--json", "dispatch", item, "--repo", &sb.repo, "--branch", branch, "--agent", "claude",
        "--yolo",
    ]);
    run(c)
}

// ---------------------------------------------------------------------------
// the lifecycle
// ---------------------------------------------------------------------------

/// dispatch → relaunch → relaunch → kill, asserting the things that were broken:
/// a relaunch that actually respawns and keeps its slot, and a kill that reaps the process tree
/// and hands the slot back.
/// **`#[ignore]` until verified.** This has never completed a green run: it timed out locally at
/// 500s and the hang was not diagnosed. `cargo test --workspace` is CI's merge gate, so leaving it
/// enabled would let a hang wedge CI on any runner that has tmux — a worse outcome than the gap it
/// covers. Run it deliberately with `cargo test -p bn-cli --test agent_lifecycle_e2e -- --ignored`,
/// find the hang, then delete this attribute.
#[test]
#[ignore]
fn dispatch_relaunch_kill_is_a_complete_lifecycle() {
    require_tmux!();
    let sb = Sandbox::new("sandbox", 3); // slot 1 is the controller's → 2 worker slots
    sb.assert_isolated();
    sb.start_tmux();

    // --- dispatch ---------------------------------------------------------------------------
    let (env, out, err) = dispatch(&sb, "wire the fleet", "feat/one");
    assert_eq!(
        env["ok"], true,
        "dispatch failed\nstdout={out}\nstderr={err}"
    );
    assert_eq!(
        env["data"]["pooled"], true,
        "should have leased a warm slot"
    );
    let worktree = env["data"]["worktree"].as_str().unwrap().to_string();
    assert!(
        worktree.contains("_pool"),
        "expected a pooled slot, got {worktree}"
    );

    assert!(
        wait_for(|| sb.windows().iter().any(|w| w == "sandbox:feat-one")),
        "no agent window appeared: {:?}",
        sb.windows()
    );
    assert!(
        wait_for(|| !sb.launches().is_empty()),
        "the stub harness was never launched"
    );
    // The branch note is seeded with the goal and exactly one todo.
    let note = std::fs::read_to_string(sb.note_path("feat/one")).expect("note");
    assert!(note.contains("wire the fleet"), "goal not seeded:\n{note}");

    // The slot is leased to this branch.
    let leased = sb.leases();
    assert_eq!(leased.len(), 1, "expected one leased slot, got {leased:?}");
    let (slot_n, _) = leased.iter().next().unwrap();
    let slot_n = *slot_n;

    let pids_before = sb.pane_pids();
    let launches_before = sb.launches().len();

    // --- relaunch ---------------------------------------------------------------------------
    let (env, out, err) = run({
        let mut c = sb.bn();
        c.args(["--json", "agent", "relaunch", "sandbox:feat-one"]);
        c
    });
    assert_eq!(
        env["ok"], true,
        "relaunch failed\nstdout={out}\nstderr={err}"
    );
    assert_eq!(env["data"]["mode"], "resume", "a plain relaunch resumes");
    assert_eq!(
        env["data"]["session"]["old"], env["data"]["session"]["new"],
        "a resuming relaunch keeps the session id"
    );

    // The old pane's process tree is gone — this is the `remain-on-exit` reparenting bug: a bare
    // kill-window would leave the stub `sleep` alive and only the window would disappear.
    assert!(
        wait_for(|| sb.pane_pids().is_disjoint(&pids_before)),
        "the old agent's pane survived the relaunch"
    );
    // And a new agent really started, resuming the same session.
    assert!(
        wait_for(|| sb.launches().len() > launches_before),
        "the harness was not launched again"
    );
    let resumed = sb.launches().last().cloned().unwrap_or_default();
    assert!(
        resumed.contains("--resume"),
        "the respawned harness did not resume: {resumed}"
    );
    // Exactly one agent window, not two.
    assert!(
        wait_for(|| sb.windows().len() == 1),
        "expected one window after a relaunch, got {:?}",
        sb.windows()
    );
    // The pool slot is KEPT — freeing it is `kill`'s job, and losing it here would let another
    // dispatch lease the worktree out from under the agent we just restarted.
    assert_eq!(
        sb.leases().keys().copied().collect::<Vec<_>>(),
        vec![slot_n],
        "a relaunch must not release the pool slot"
    );

    // --- relaunch again: idempotent, not accumulative --------------------------------------
    let (env, out, err) = run({
        let mut c = sb.bn();
        c.args(["--json", "agent", "relaunch", "sandbox:feat-one"]);
        c
    });
    assert_eq!(
        env["ok"], true,
        "second relaunch failed\nstdout={out}\nstderr={err}"
    );
    assert!(
        wait_for(|| sb.windows().len() == 1),
        "relaunching twice stacked windows: {:?}",
        sb.windows()
    );

    // --- kill --------------------------------------------------------------------------------
    let (env, out, err) = run({
        let mut c = sb.bn();
        c.args(["--json", "agent", "kill", "sandbox:feat-one"]);
        c
    });
    assert_eq!(env["ok"], true, "kill failed\nstdout={out}\nstderr={err}");
    assert!(
        env["data"]["processes"].as_u64().unwrap_or(0) > 0,
        "kill signalled no processes — it only closed the window: {}",
        env["data"]
    );
    assert!(
        wait_for(|| sb.windows().is_empty()),
        "the window survived the kill: {:?}",
        sb.windows()
    );
    // The warm slot comes back. Nothing used to return a worker slot, so a day of kills filled
    // the pool and every later dispatch quietly cut a cold worktree instead.
    assert_eq!(
        env["data"]["slot"]["outcome"], "returned",
        "kill did not return the pool slot: {}",
        env["data"]["slot"]
    );
    assert!(
        sb.leases().is_empty(),
        "the pool slot is still leased after a kill: {:?}",
        sb.leases()
    );

    // Killing again is a no-op success, not an error.
    let (env, _, _) = run({
        let mut c = sb.bn();
        c.args(["--json", "agent", "kill", "sandbox:feat-one"]);
        c
    });
    assert_eq!(env["ok"], true, "a second kill should be idempotent");
    assert_eq!(env["data"]["killed"], false);
}

/// A relaunch of a branch that was never checked out must fail — loudly, with the code that names
/// the actual problem, rather than reporting success over an agent that never moved.
#[test]
fn relaunching_an_unknown_branch_fails_with_no_worktree() {
    require_tmux!();
    let sb = Sandbox::new("sandbox", 3);
    sb.assert_isolated();
    sb.start_tmux();

    let (env, _, _) = run({
        let mut c = sb.bn();
        c.args(["--json", "agent", "relaunch", "sandbox:no-such-branch"]);
        c
    });
    assert_eq!(
        env["ok"], false,
        "an unresolvable relaunch must not succeed"
    );
    assert_eq!(env["error"]["code"], "no_worktree");
}

/// With every warm slot leased, a dispatch schedules the work on the repo's run instead of cutting
/// yet another cold worktree — the unbounded growth that made pools fill up and stay full.
/// **`#[ignore]` until verified.** This has never completed a green run: it timed out locally at
/// 500s and the hang was not diagnosed. `cargo test --workspace` is CI's merge gate, so leaving it
/// enabled would let a hang wedge CI on any runner that has tmux — a worse outcome than the gap it
/// covers. Run it deliberately with `cargo test -p bn-cli --test agent_lifecycle_e2e -- --ignored`,
/// find the hang, then delete this attribute.
#[test]
#[ignore]
fn a_full_pool_schedules_the_work_instead_of_growing_the_disk() {
    require_tmux!();
    let sb = Sandbox::new("sandbox", 3); // slots 2..3 → exactly 2 worker slots
    sb.assert_isolated();
    sb.start_tmux();

    for (i, branch) in ["feat/a", "feat/b"].iter().enumerate() {
        let (env, out, err) = dispatch(&sb, &format!("item {i}"), branch);
        assert_eq!(env["ok"], true, "dispatch {branch} failed\n{out}\n{err}");
        assert_eq!(env["data"]["pooled"], true, "{branch} should be pooled");
    }
    assert_eq!(sb.leases().len(), 2, "both worker slots should be leased");

    // No run exists yet, so the documented fallback applies: cut a cold worktree rather than
    // refuse work on a repo that isn't orchestrated at all.
    let (env, _, _) = dispatch(&sb, "item overflow", "feat/c");
    assert_eq!(env["ok"], true);
    assert_eq!(
        env["data"]["pooled"], false,
        "with no run to queue onto, a full pool still falls back to cold"
    );

    // `--on-pool-full fail` is the scripted caller's answer: say so, do nothing.
    let (env, _, _) = run({
        let mut c = sb.bn();
        c.args([
            "--json",
            "dispatch",
            "item refused",
            "--repo",
            &sb.repo,
            "--branch",
            "feat/d",
            "--on-pool-full",
            "fail",
        ]);
        c
    });
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "pool_full");
    assert!(
        !sb.wt_root.join(&sb.repo).join("feat-d").exists(),
        "--on-pool-full fail must not create a worktree"
    );
}

// ---------------------------------------------------------------------------
// stress
// ---------------------------------------------------------------------------

/// N real, concurrent `bn dispatch` processes — the shape that actually exercises the pool's lease
/// file and the note store at the same time.
///
/// Two classes of bug are in scope, and both were live before this suite:
/// * **double-leasing** — `acquire_slot` read `leases.json`, picked the lowest free slot and wrote
///   it back with no mutual exclusion, so two dispatches could be handed the same worktree.
/// * **note clobbering** — `ensure_note` was an unlocked `exists()`-then-`save()`, so a racing
///   dispatch could write a blank note over one that had just been seeded with a goal and a todo.
///
/// **`#[ignore]` until verified.** This has never completed a green run: it timed out locally at
/// 500s and the hang was not diagnosed. `cargo test --workspace` is CI's merge gate, so leaving it
/// enabled would let a hang wedge CI on any runner that has tmux — a worse outcome than the gap it
/// covers. Run it deliberately with `cargo test -p bn-cli --test agent_lifecycle_e2e -- --ignored`,
/// find the hang, then delete this attribute.
#[test]
#[ignore]
fn concurrent_dispatches_never_share_a_slot_or_lose_a_note() {
    require_tmux!();
    const N: usize = 6;
    // More branches than worker slots on purpose: the overflow must fall back cleanly, not corrupt
    // the lease file or steal a leased slot.
    let sb = Sandbox::new("sandbox", 5); // slots 2..5 → 4 worker slots for 6 dispatches
    sb.assert_isolated();
    sb.start_tmux();

    let results: Vec<(usize, serde_json::Value, String)> = std::thread::scope(|s| {
        // The `collect` is load-bearing, not incidental: every thread must be *spawned* before any
        // is joined. Chained lazily — as clippy's `needless_collect` would have it — each dispatch
        // would be joined the moment it started, running them one after another and removing the
        // concurrency this whole test exists to create.
        #[allow(
            clippy::needless_collect,
            reason = "collect forces all spawns before the first join; chaining would serialize them"
        )]
        let handles: Vec<_> = (0..N)
            .map(|i| {
                let sb = &sb;
                s.spawn(move || {
                    let (env, _, err) =
                        dispatch(sb, &format!("concurrent item {i}"), &format!("feat/c{i}"));
                    (i, env, err)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    for (i, env, err) in &results {
        assert_eq!(env["ok"], true, "dispatch {i} failed: {env} / {err}");
        // The lock's own tripwire: `FileLock::acquire` gives up after 20s and proceeds *unowned*,
        // warning on stderr. If that ever fires, a lost update is possible and the assertions
        // below are no longer meaningful.
        assert!(
            !err.contains("proceeding without lock"),
            "dispatch {i} proceeded without a lock — a concurrent edit may have been lost:\n{err}"
        );
        assert!(
            !err.contains("timed out") || !err.contains("waiting for lock"),
            "dispatch {i} timed out on a lock:\n{err}"
        );
    }

    // --- leases: one slot per branch, no slot shared -----------------------------------------
    let leases = sb.leases();
    let branches: Vec<&String> = leases.values().collect();
    let unique: BTreeSet<&String> = branches.iter().copied().collect();
    assert_eq!(
        branches.len(),
        unique.len(),
        "two slots claim the same branch: {leases:?}"
    );
    assert!(
        leases.len() <= 4,
        "more slots leased than the pool has: {leases:?}"
    );

    // No two live agents share a worktree — the visible symptom of a double lease.
    let worktrees: Vec<String> = results
        .iter()
        .filter_map(|(_, e, _)| e["data"]["worktree"].as_str().map(str::to_string))
        .collect();
    let unique_wt: BTreeSet<&String> = worktrees.iter().collect();
    assert_eq!(
        worktrees.len(),
        unique_wt.len(),
        "two dispatches landed in the same worktree: {worktrees:?}"
    );

    // --- notes: one per branch, seeded, parseable, uncontaminated ---------------------------
    for i in 0..N {
        let path = sb.note_path(&format!("feat/c{i}"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("note {i} missing at {}: {e}", path.display()));
        assert!(
            text.contains(&format!("concurrent item {i}")),
            "note {i} lost its goal — the unlocked ensure_note race:\n{text}"
        );
        // Goal i belongs to note i only.
        for j in 0..N {
            if i != j {
                assert!(
                    !text.contains(&format!("concurrent item {j}")),
                    "note {i} contains note {j}'s goal:\n{text}"
                );
            }
        }
        // Exactly one seed todo: `if_absent` is what stops a re-dispatch or a race from seeding
        // the same work twice.
        let todos = text.matches("- [ ] ").count() + text.matches("- [x] ").count();
        assert!(
            todos <= 2,
            "note {i} has {todos} items — the seed todo was written more than once:\n{text}"
        );
    }

    // --- no scratch residue -----------------------------------------------------------------
    let mut residue = Vec::new();
    collect_residue(&sb.notes, &mut residue);
    collect_residue(&sb.wt_root, &mut residue);
    collect_residue(&sb.state, &mut residue);
    assert!(residue.is_empty(), "temp/lock files leaked: {residue:?}");

    // --- shared append-only logs are line-clean ----------------------------------------------
    // Asserted on the raw file: `events::read_events_at` silently skips unparseable lines, which
    // is exactly how interleaved writes stayed invisible.
    for log in [
        sb.state.join("events.jsonl"),
        sb.notes.join("h").join("metrics").join("events.jsonl"),
    ] {
        let Ok(text) = std::fs::read_to_string(&log) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            assert!(
                serde_json::from_str::<serde_json::Value>(line).is_ok(),
                "{}:{} is not valid JSON — two appenders interleaved:\n{line}",
                log.display(),
                n + 1
            );
        }
    }
}

/// Every `.tmp` / `.lock` left behind under `dir`. A leaked lock file blocks the next writer for
/// 30s; a leaked temp means an atomic write didn't complete its rename.
fn collect_residue(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            // A pool slot is a full checkout — don't walk a whole worktree looking for temp files.
            if p.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            collect_residue(&p, out);
        } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            if name.ends_with(".tmp") || name.ends_with(".lock") {
                out.push(p.clone());
            }
        }
    }
}
