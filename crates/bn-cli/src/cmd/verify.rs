//! `bn verify`/`bn gate` — run the repo verification gate in a snapshot.
use crate::*;

/// One step in the repo's verification gate (#454, #758).
#[derive(Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum VerifyTarget {
    /// `[repo.<name>].verify`: a native command line, run with `sh -c <cmd>`. Not snapshotted —
    /// there's no shared `jobs/` file to race.
    Command {
        label: String,
        #[serde(rename = "command")]
        cmd: String,
    },
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

    fn emit_preview(&self, cwd: &std::path::Path, as_json: bool) {
        if as_json {
            let data = serde_json::json!({
                "dry_run": true,
                "job": self.label(),
                "cwd": cwd,
                "steps": self.targets,
            });
            println!("{}", Envelope::success("verify", data).to_json());
        } else {
            println!("verify {}: DRY RUN (not executed)", self.label());
            println!("working directory: {}", cwd.display());
            for (index, target) in self.targets.iter().enumerate() {
                match target {
                    VerifyTarget::Command { cmd, .. } => {
                        println!("{}. command: {cmd}", index + 1);
                    }
                    VerifyTarget::Script { label, path } => {
                        println!("{}. {label}: {}", index + 1, path.display());
                    }
                }
            }
        }
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
            log.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(line.as_bytes());
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
    let out = child
        .stdout
        .take()
        .map(|s| pump(Box::new(s), stream, log.clone()));
    let err = child
        .stderr
        .take()
        .map(|s| pump(Box::new(s), false, log.clone()));
    let status = child.wait()?;
    for t in [out, err].into_iter().flatten() {
        let _ = t.join();
    }
    Ok(status.code().unwrap_or(1))
}

/// Resolve the worktree root even when verification starts in a subdirectory.
fn repo_root(dir: &std::path::Path) -> std::path::PathBuf {
    crate::git::git_capture(&["rev-parse", "--show-toplevel"], Some(dir), true)
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dir.to_path_buf())
}

/// Run the verification gate and record its result, or preview it without execution or writes.
/// Executed runs exit with the first failing step's status so an agent can gate "done" on them.
pub(crate) fn emit_verify(cfg: &Config, args: VerifyCommandArgs, as_json: bool) {
    let VerifyCommandArgs {
        gate: VerifyArgs { job, for_ },
        dry_run,
    } = args;
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
    if dry_run {
        target.emit_preview(&cwd, as_json);
        return;
    }
    let job_name = target.label();
    let log = GateLog::default();
    let code = run_verify_gate_to(&target, &cwd, !as_json, None, Some(&log))
        .unwrap_or_else(|e| fail(&format!("bn verify: {e}")));
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
        proof
            .as_deref()
            .map(|p| format!(" → proof {p}"))
            .unwrap_or_default()
    );
    let _ = commands::append_body(cfg, &ctx, "progress", &bullet, &today());

    if as_json {
        // The command succeeded in *determining* the result; the gate outcome is `pass` (and the
        // process exit code below), so the envelope always carries the data — never drops `pass`.
        let data =
            serde_json::json!({ "job": job_name, "pass": pass, "code": code, "proof": proof });
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

    #[test]
    fn preview_targets_expose_the_command_or_resolved_script_path() {
        let command = VerifyTarget::Command {
            label: "test".to_string(),
            cmd: "cargo test && cargo clippy".to_string(),
        };
        let script = VerifyTarget::Script {
            label: "lint".to_string(),
            path: std::path::PathBuf::from("/jobs/lint.sh"),
        };

        assert_eq!(
            serde_json::to_value(command).unwrap(),
            serde_json::json!({
                "kind": "command", "label": "test", "command": "cargo test && cargo clippy"
            })
        );
        assert_eq!(
            serde_json::to_value(script).unwrap(),
            serde_json::json!({"kind": "script", "label": "lint", "path": "/jobs/lint.sh"})
        );
    }

    /// A teed run keeps both streams and the step header in the log, and still reports the
    /// step's own exit code.
    #[test]
    fn a_teed_gate_captures_both_streams_and_the_exit_code() {
        let _lock = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _state =
            bn_core::test_support::EnvGuard::set("BN_STATE_DIR", temp.path().join("state"));
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
        let _state =
            bn_core::test_support::EnvGuard::set("BN_STATE_DIR", temp.path().join("state"));
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
