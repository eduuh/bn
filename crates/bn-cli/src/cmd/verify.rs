//! `bn verify`/`bn gate` — run the repo verification gate in a snapshot.
use crate::*;

/// One step in the repo's verification gate (#454, #758).
#[derive(Debug)]
pub(crate) enum VerifyTarget {
    /// `[repo.<name>].verify`: a native command line, run with `sh -c <cmd>`. Not snapshotted —
    /// there's no shared `jobs/` file to race.
    Command { label: String, cmd: String },
    /// A bn job script (`jobs/<name>.sh`), run with `sh <path>` after a copy-on-run snapshot.
    Script {
        label: String,
        path: std::path::PathBuf,
    },
}

impl VerifyTarget {
    /// The step label shown while the gate runs.
    pub(crate) fn label(&self) -> &str {
        match self {
            VerifyTarget::Command { label, .. } | VerifyTarget::Script { label, .. } => label,
        }
    }
}

/// The ordered, fail-fast verification gate shared by `bn verify` and pre-land `bn gate`.
#[derive(Debug)]
pub(crate) struct VerifyGate {
    targets: Vec<VerifyTarget>,
}

impl VerifyGate {
    /// The label recorded in note progress / JSON / gate verdicts.
    pub(crate) fn label(&self) -> String {
        self.targets
            .iter()
            .map(VerifyTarget::label)
            .collect::<Vec<_>>()
            .join("+")
    }

    
}

/// Resolve what the verify gate runs for `ctx` (#454, #758), shared by `bn verify`
/// ([`emit_verify`]) and the pre-land gate ([`crate::wt::merge::gate_head`]): an explicit `--job`
/// (a bn job that must exist), else the repo's configured native `[repo.<name>].verify` command,
/// else every available conventional bn job in build → test → lint order. Explicit and configured
/// gates remain authoritative single-target overrides.
pub(crate) fn resolve_verify_target(
    cfg: &Config,
    ctx: &Resolved,
    job: Option<&str>,
) -> Result<Option<VerifyGate>, String> {
    // An explicit --job forces a bn job script (never the native command) — and must resolve.
    if let Some(j) = job {
        return match resolver::job_script(cfg, Some(ctx), j) {
            Some((_, path)) => Ok(Some(VerifyGate {
                targets: vec![VerifyTarget::Script {
                    label: j.to_string(),
                    path,
                }],
            })),
            None => Err(format!(
                "no '{j}' job for {} — create {}/{j}.sh",
                ctx.repo,
                cfg.notes_dir_for(&ctx.repo)
                    .join("jobs")
                    .join(&ctx.repo)
                    .display()
            )),
        };
    }
    // The repo's native verify command, when configured — the repo-agnostic gate.
    if let Some(cmd) = cfg.repo_verify(&ctx.repo) {
        return Ok(Some(VerifyGate {
            targets: vec![VerifyTarget::Command {
                label: cmd.to_string(),
                cmd: cmd.to_string(),
            }],
        }));
    }
    let targets = ["build", "test", "lint"]
        .into_iter()
        .filter_map(|label| {
            resolver::job_script(cfg, Some(ctx), label).map(|(_, path)| VerifyTarget::Script {
                label: label.to_string(),
                path,
            })
        })
        .collect::<Vec<_>>();
    Ok((!targets.is_empty()).then_some(VerifyGate { targets }))
}

/// Copy-on-run snapshot for the verify gate. `script` is a SHARED job file (`jobs/<name>.sh`)
/// resolved across branch/repo/global tiers, so a background job-script/symlink refresh
/// (refresh-all's git fast-forward, install.sh re-linking `jobs/`) can rewrite it mid-run. Copy it
/// to a private, per-process temp (`<dir>/bn-verify-<pid>.sh`), mark it executable, and return that
/// path so the gate runs a stable file no concurrent writer can truncate. The caller removes it
/// after the run.
pub(crate) fn snapshot_gate_script(
    script: &std::path::Path,
    dir: &std::path::Path,
) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let snap = dir.join(format!("bn-verify-{}.sh", std::process::id()));
    std::fs::copy(script, &snap)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&snap, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(snap)
}

/// Remove a copy-on-run snapshot after the gate has run. No-op when `run_path` is the shared
/// script itself (the snapshot fell back), so we never delete a real `jobs/<name>.sh`.
pub(crate) fn cleanup_snapshot(run_path: &std::path::Path, original: &std::path::Path) {
    if run_path != original {
        let _ = std::fs::remove_file(run_path);
    }
}

/// How long a build waits for a pool slot before running anyway.
///
/// Generous, because a real workspace build is minutes and three waiting builds drain well inside
/// this. Bounded at all, because the alternative to running ungated is not running — and a merge
/// that never lands is a worse failure than a machine that is briefly too busy. See
/// [`bn_core::build_slots`] for why this fails open.
const BUILD_WAIT_MAX_SECS: u64 = 900;

/// How often to re-ask for a slot while waiting. A build is minutes long, so half a second is far
/// below the noise floor and costs one lock acquisition.
const BUILD_POLL_MS: u64 = 500;

/// Take a slot from the machine's build pool, waiting for one if every slot is busy.
///
/// The choke point for *all* heavy verification: both `bn verify` and the `bn wt merge` pre-land
/// gate reach a build through this function, so neither has to remember to throttle and neither can
/// be changed into forgetting. A finishing wave — several agents landing at once, each running a
/// full build while other agents are mid-turn — is the case this exists for.
///
/// Returns `None` when the wait budget ran out, meaning *run anyway*: the caller proceeds exactly as
/// it did before this pool existed.
fn build_slot(label: &str, cwd: &std::path::Path) -> Option<bn_core::build_slots::Ticket> {
    use bn_core::build_slots::{self as build, Admission};
    let repo = repo_root(cwd)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cap = build::observe_slots();
    let alive = build::pid_alive;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(BUILD_WAIT_MAX_SECS);
    let mut announced = false;
    loop {
        match build::try_acquire(label, &repo, cap, bn_core::util::now_epoch_secs(), &alive) {
            // An unreadable or unwritable pool must not stop a build; that would make a throttle
            // into a single point of failure for landing anything.
            Err(_) => return None,
            Ok(Admission::Granted(t)) => return Some(t),
            Ok(Admission::Full(held)) => {
                if !announced {
                    announced = true;
                    let who = held
                        .iter()
                        .map(|h| format!("{}/{}", h.repo, h.label))
                        .collect::<Vec<_>>()
                        .join(", ");
                    eprintln!(
                        "→ waiting for a build slot ({}/{cap} busy: {who})",
                        held.len()
                    );
                }
                if std::time::Instant::now() >= deadline {
                    eprintln!(
                        "→ build slot never freed after {BUILD_WAIT_MAX_SECS}s — running anyway"
                    );
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(BUILD_POLL_MS));
            }
        }
    }
}

/// Execute an ordered verification gate, stopping at the first failing step. Job scripts are
/// snapshotted independently immediately before execution; configured commands run via `sh -c`.
/// Captured mode echoes child output to stderr so JSON stdout remains clean.
///
/// Holds a build-pool slot for the whole run — see [`build_slot`].
pub(crate) fn run_verify_gate(
    gate: &VerifyGate,
    cwd: &std::path::Path,
    stream: bool,
    cargo_target_dir: Option<&std::path::Path>,
) -> Result<i32, String> {
    run_verify_gate_to(gate, cwd, stream, cargo_target_dir, None)
}

/// A shared, bounded copy of a gate's output, kept to attach as proof.
pub(crate) type GateLog = std::sync::Arc<std::sync::Mutex<bn_core::proof::tee::CappedLog>>;

/// [`run_verify_gate`], also teeing every step's output into `log` when given. The terminal sees
/// exactly what it did before; the log gets a bounded copy.
pub(crate) fn run_verify_gate_to(
    gate: &VerifyGate,
    cwd: &std::path::Path,
    stream: bool,
    cargo_target_dir: Option<&std::path::Path>,
    log: Option<&GateLog>,
) -> Result<i32, String> {
    let _slot = build_slot(&gate.label(), cwd);
    let gate_path = crate::cmd::cron::job_path();
    let aggregate = gate.targets.len() > 1;

    for target in &gate.targets {
        if aggregate {
            eprintln!("→ verify {}", target.label());
        }
        if let Some(log) = log {
            let line = format!("$ {} ({})\n", target.label(), cwd.display());
            log.lock().unwrap_or_else(|e| e.into_inner()).push(line.as_bytes());
        }
        let mut cmd = std::process::Command::new("/bin/sh");
        let snapshot = match target {
            VerifyTarget::Command {
                cmd: configured, ..
            } => {
                cmd.arg("-c").arg(configured);
                None
            }
            VerifyTarget::Script { path: script, .. } => {
                let run_path = snapshot_gate_script(script, &std::env::temp_dir())
                    .unwrap_or_else(|_| script.clone());
                cmd.arg(&run_path);
                Some((run_path, script.clone()))
            }
        };
        cmd.current_dir(cwd).env("PATH", &gate_path);
        if let Some(target_dir) = cargo_target_dir {
            cmd.env("CARGO_TARGET_DIR", target_dir);
        }
        let result = if let Some(log) = log {
            run_teed(&mut cmd, stream, log)
        } else if stream {
            cmd.status().map(|status| status.code().unwrap_or(1))
        } else {
            cmd.output().map(|output| {
                use std::io::Write;
                let _ = std::io::stderr().write_all(&output.stdout);
                let _ = std::io::stderr().write_all(&output.stderr);
                output.status.code().unwrap_or(1)
            })
        };
        if let Some((run_path, original)) = &snapshot {
            cleanup_snapshot(run_path, original);
        }
        let code = result.map_err(|e| format!("failed to run {}: {e}", target.label()))?;
        if code != 0 {
            return Ok(code);
        }
    }
    Ok(0)
}

/// Run `cmd` with its stdout and stderr piped through to the terminal (stdout to stdout when
/// `stream`, else to stderr so JSON stdout stays clean) and into `log`.
fn run_teed(cmd: &mut std::process::Command, stream: bool, log: &GateLog) -> std::io::Result<i32> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let pump = |mut from: Box<dyn Read + Send>, to_stdout: bool, log: GateLog| {
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = from.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let chunk = &buf[..n];
                if to_stdout {
                    let _ = std::io::stdout().write_all(chunk);
                } else {
                    let _ = std::io::stderr().write_all(chunk);
                }
                log.lock().unwrap_or_else(|e| e.into_inner()).push(chunk);
            }
        })
    };
    let out = child.stdout.take().map(|s| pump(Box::new(s), stream, log.clone()));
    let err = child.stderr.take().map(|s| pump(Box::new(s), false, log.clone()));
    let status = child.wait()?;
    for t in [out, err].into_iter().flatten() {
        let _ = t.join();
    }
    Ok(status.code().unwrap_or(1))
}

/// The branch's changed files (repo-relative), mirroring the `test` job's rustfmt-on-changed set:
/// committed vs `origin/main` (three-dot), plus unstaged and staged working-tree edits, deduped.
/// Run with `-C dir` so it works from a subdir or a disposable gate worktree. `git diff --name-only`
/// emits repo-root-relative paths regardless of cwd, so the result feeds
/// [`bn_core::verify_ui::is_ui_source`] directly. Fail-open (empty on any git error) — detection
/// must never break the Rust gate.
pub(crate) fn changed_files(dir: &std::path::Path) -> Vec<String> {
    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for args in [
        &["diff", "--name-only", "origin/main...HEAD"][..],
        &["diff", "--name-only"][..],
        &["diff", "--name-only", "--cached"][..],
    ] {
        if let Some(out) = crate::git::git_capture(args, Some(dir), true) {
            for line in out.lines() {
                let f = line.trim();
                if !f.is_empty() {
                    set.insert(f.to_string());
                }
            }
        }
    }
    set.into_iter().collect()
}

/// Repo root for `dir` (`git rev-parse --show-toplevel`) so the UI gate's `npm --prefix` and the
/// changed-file diff both resolve from the worktree root even when `bn verify` is invoked from a
/// subdirectory. Falls back to `dir` if git can't answer.
fn repo_root(dir: &std::path::Path) -> std::path::PathBuf {
    crate::git::git_capture(&["rev-parse", "--show-toplevel"], Some(dir), true)
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dir.to_path_buf())
}

/// The bn-web UI gate (#505): when the branch changed the SPA (`crates/bn-web/ui/**`), additionally
/// `npm ci` + build (tsc+vite) + test (vitest) from the repo root, so a broken SPA — a `tsc` error,
/// a missing dep, a failing vitest — fails `bn verify`/`bn gate`. Returns `0` when nothing to gate
/// (pure-Rust change → the "skip when unchanged" fast path) or the whole gate passes; the first
/// non-zero npm exit otherwise. `stream` mirrors the Rust gate: live output for humans, captured
/// (echoed to stderr) for `--json`.
pub(crate) fn run_ui_gate_if_changed(dir: &std::path::Path, stream: bool) -> i32 {
    let root = repo_root(dir);
    if !bn_core::verify_ui::ui_gate_needed(changed_files(&root)) {
        return 0;
    }
    run_ui_gate(&root, stream)
}

/// Execute the ordered [`bn_core::verify_ui::ui_gate_steps`] from `root`, stopping at the first
/// failure. Kept separate from detection so the decision stays a pure, unit-tested bn-core policy
/// and this only does the spawn.
///
/// Takes its own build slot: `npm ci` + vite + vitest is as heavy as the Rust gate, and it runs
/// *after* [`run_verify_gate`] has released, so it is a second build rather than a nested one.
fn run_ui_gate(root: &std::path::Path, stream: bool) -> i32 {
    let _slot = build_slot("ui", root);
    let full_path = crate::cmd::cron::job_path();
    eprintln!("→ bn-web UI gate (crates/bn-web/ui changed): npm ci → build → vitest [#505]");
    for step in bn_core::verify_ui::ui_gate_steps() {
        eprintln!("  $ npm {}", step.args.join(" "));
        let mut cmd = std::process::Command::new("npm");
        cmd.args(&step.args)
            .current_dir(root)
            .env("PATH", &full_path)
            .env("CI", "1");
        let code = if stream {
            match cmd.status() {
                Ok(s) => s.code().unwrap_or(1),
                Err(e) => return spawn_failed(step.label, e),
            }
        } else {
            match cmd.output() {
                Ok(o) => {
                    use std::io::Write;
                    let _ = std::io::stderr().write_all(&o.stdout);
                    let _ = std::io::stderr().write_all(&o.stderr);
                    o.status.code().unwrap_or(1)
                }
                Err(e) => return spawn_failed(step.label, e),
            }
        };
        if code != 0 {
            eprintln!(
                "  ✗ bn-web UI gate: `npm {}` FAILED (exit {code})",
                step.args.join(" ")
            );
            return code;
        }
    }
    eprintln!("  ✓ bn-web UI gate: PASS");
    0
}

/// Report a failure to even spawn `npm` (usually not installed) as a hard, fail-closed gate error:
/// a SPA change we cannot validate must not pass. The message points at the `--no-ui` escape hatch.
fn spawn_failed(label: &str, e: std::io::Error) -> i32 {
    eprintln!(
        "  ✗ bn-web UI gate: could not run `{label}` ({e}) — install Node/npm, or re-run with --no-ui to skip"
    );
    1
}

/// `bn verify` — run the worktree's verification gate and record the PASS/FAIL to the branch
/// note's Progress (bn-native — same append `bn log-progress` uses). Exits with the first failing
/// step's status so an agent (or a caller) can gate "done" on a green run.
/// When the branch changed the bn-web SPA (`crates/bn-web/ui/**`) the UI gate (npm ci + build +
/// vitest) also runs after a green Rust gate (#505); `skip_ui` (`--no-ui`) opts out.
pub(crate) fn emit_verify(
    cfg: &Config,
    job: Option<String>,
    as_json: bool,
    skip_ui: bool,
    for_: Vec<String>,
) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let Ok(ctx) = resolver::resolve(cfg, &cwd) else {
        fail("bn verify: not in a git repo");
    };
    // Resolve what to gate on (#454, #758): an explicit --job, else the repo's native
    // `[repo].verify` command, else every available build/test/lint job.
    let target = match resolve_verify_target(cfg, &ctx, job.as_deref()) {
        Ok(Some(t)) => t,
        Ok(None) => fail(&format!(
            "bn verify: no build, test, or lint job for {} — create one under {}",
            ctx.repo,
            cfg.notes_dir_for(&ctx.repo)
                .join("jobs")
                .join(&ctx.repo)
                .display()
        )),
        Err(msg) => fail(&format!("bn verify: {msg}")),
    };
    let job_name = target.label();
    let log = GateLog::default();
    let code = run_verify_gate_to(&target, &cwd, !as_json, None, Some(&log))
        .unwrap_or_else(|e| fail(&format!("bn verify: {e}")));
    // #505: additionally gate the bn-web SPA when the branch touched it. Only after a green Rust
    // gate — a red gate already blocks, so we don't pay the npm cost — and never for a pure-Rust
    // change (`run_ui_gate_if_changed` returns 0 when nothing under `crates/bn-web/ui/**` changed).
    let code = if code == 0 && !skip_ui {
        run_ui_gate_if_changed(&cwd, !as_json)
    } else {
        code
    };
    let pass = code == 0;

    // Keep the run as proof, then record the outcome in the note's Progress pointing at it.
    let head = bn_core::proof::head(cfg, &ctx);
    let proof = crate::cmd::proof::record_run(
        cfg,
        &ctx,
        crate::cmd::proof::Run {
            kind: bn_core::proof::Kind::Verify,
            job: &job_name,
            code,
            log: &log,
            sha: head.sha,
            dirty: head.dirty,
            for_: &for_,
        },
    );
    let bullet = format!(
        "{} — verify {job_name}: {}{}",
        bn_core::util::now_stamp(),
        if pass { "PASS ✓" } else { "FAIL ✗" },
        proof.as_deref().map(|p| format!(" → proof {p}")).unwrap_or_default()
    );
    let _ = commands::append_body(cfg, &ctx, "progress", &bullet, &today());

    if as_json {
        // The command succeeded in *determining* the result; the gate outcome is `pass` (and the
        // process exit code below), so the envelope always carries the data — never drops `pass`.
        let data = serde_json::json!({ "job": job_name, "pass": pass, "code": code, "proof": proof });
        println!("{}", Envelope::success("verify", data).to_json());
    } else if pass {
        println!("verify {job_name}: PASS ✓ (recorded to note)");
    } else {
        println!("verify {job_name}: FAIL ✗ (recorded to note)");
    }
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A teed run keeps both streams and the step header in the log, and still reports the
    /// step's own exit code.
    #[test]
    fn a_teed_gate_captures_both_streams_and_the_exit_code() {
        let _lock = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", temp.path().join("state"));
        let gate = VerifyGate {
            targets: vec![VerifyTarget::Command {
                label: "noisy".to_string(),
                cmd: "echo to-out; echo to-err >&2; exit 3".to_string(),
            }],
        };
        let log = GateLog::default();
        let code = run_verify_gate_to(&gate, temp.path(), false, None, Some(&log)).expect("runs");
        assert_eq!(code, 3);
        let (bytes, truncated) = log.lock().unwrap().finish();
        let text = String::from_utf8(bytes).unwrap();
        assert!(!truncated);
        assert!(text.starts_with("$ noisy"), "{text}");
        assert!(text.contains("to-out") && text.contains("to-err"), "{text}");
    }

    /// A verify gate runs the user's own script, which routinely shells out to `bn`. It must get
    /// the same PATH every other job process gets — the one whose leading entry is bn's own
    /// directory. Re-deriving `~/.local/bin:~/.bin:$PATH` here is what left staff gathers exiting
    /// 127 under the scheduler, so assert the shared prelude rather than an equivalent-looking
    /// string.
    #[test]
    fn the_verify_gate_runs_under_the_shared_job_path() {
        let _lock = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", temp.path().join("state"));
        let observed = temp.path().join("path.txt");
        let gate = VerifyGate {
            targets: vec![VerifyTarget::Command {
                label: "echo-path".to_string(),
                cmd: format!("printf %s \"$PATH\" > {}", observed.display()),
            }],
        };

        let code = run_verify_gate(&gate, temp.path(), false, None).expect("gate runs");

        assert_eq!(code, 0, "the echo-path gate should succeed");
        let path = std::fs::read_to_string(&observed).expect("gate wrote its PATH");
        assert_eq!(path, crate::cmd::cron::job_path());
        let exe_dir = std::env::current_exe()
            .expect("current exe")
            .parent()
            .expect("exe dir")
            .to_string_lossy()
            .into_owned();
        assert_eq!(path.split(':').next(), Some(exe_dir.as_str()));
    }
}
