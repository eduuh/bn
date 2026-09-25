//! `bn dispatch` — fork a worktree for a todo and launch an agent in it.
use crate::*;

mod seed;
pub(crate) use seed::*;

#[cfg(test)]
mod tests;

mod authorization;
mod base;
pub(crate) use base::{dispatch_base_bootstrap_message, ensure_dispatch_base};
use base::bare_git_args;
pub(crate) mod branch;
pub(crate) mod invocation;
mod item;
pub(crate) mod policy;
mod slot_reuse;
use slot_reuse::may_reuse_occupied_worktree;
use branch::dispatch_branch_occupied;
pub(crate) use invocation::DispatchInvocation;
pub(crate) use item::resolve_dispatch_item;
use policy as dp;
pub(crate) use policy::{
    effective_launch_harness, normalize_agent, plan_gate_enabled, routes_to_pipeline_investigate,
    PlanGate,
};

/// Slugify a title into a branch-safe segment: lowercase, runs of non-alphanumerics → one '-',
/// trimmed, capped so branch/dir names stay sane.
pub(crate) fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let capped: String = out.chars().take(40).collect();
    capped.trim_matches('-').to_string()
}

/// Queue `item` on `repo`'s orchestrate run because the warm pool has no free slot, then exit.
///
/// The alternative — what dispatch did for years — is to cut a cold worktree, which succeeds every
/// time and is exactly the problem: nothing reclaims those directories, so a pool that fills up
/// once quietly grows the disk from then on. Queueing says the true thing (there is no capacity
/// right now) and keeps the work: `RunState::extend` dedupes by derived id, so re-dispatching the
/// same item is idempotent rather than a second queue entry.
///
/// **Returns** only when there is no live run to queue onto — the caller then falls back to cold,
/// since refusing to start work on a repo that isn't orchestrated at all would be worse than a
/// directory. Otherwise it prints the envelope and exits 0: the dispatch was *handled*, not failed.
fn schedule_on_pool_full(repo: &str, item: &str, json: bool) {
    let path = bn_core::orchestrate::state_path_for(repo);
    let now = bn_core::util::now_epoch_secs();
    let queued = match bn_core::orchestrate::mutate(&path, |st| st.extend(&[item.to_string()], now))
    {
        Ok(Some(n)) => n,
        // No run for this repo — nothing to schedule onto.
        Ok(None) => return,
        Err(e) => {
            eprintln!("bn dispatch: could not queue on {repo}'s run ({e})");
            return;
        }
    };
    if json {
        let data = serde_json::json!({
            "repo": repo,
            "item": item,
            "dispatched": false,
            "scheduled": true,
            "reason": "pool_full",
            "queued": queued,
        });
        println!("{}", Envelope::success("dispatch", data).to_json());
    } else {
        println!(
            "bn dispatch: every warm slot for '{repo}' is leased — scheduled on the run instead \
             ({queued} newly queued). It starts as soon as a slot frees."
        );
    }
    std::process::exit(0);
}

/// A repo's primary branch that must never host a dispatched agent (its worktree would be the
/// main checkout, which reap/cleanup guards also protect). Canonical definition lives in
/// [`bn_core::agent::is_reserved_branch`]; kept here as a thin alias so dispatch, the lease layer,
/// reap.rs and wt/cleanup.rs never diverge on what "main" means.
pub(crate) fn is_reserved_branch(branch: &str) -> bool {
    bn_core::agent::is_reserved_branch(branch)
}

/// Is `p` an executable file? (On non-unix, just a file.)
pub(crate) fn is_executable(p: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// Create a worktree for <branch> at <wt_path> using <bare>. Returns Ok(true) when it created
/// one, Ok(false) when the path already existed. Mirrors the shell primitive: an existing branch
/// is checked out; otherwise a new branch is cut from main.
pub(crate) fn create_worktree(
    bare: &std::path::Path,
    wt_path: &std::path::Path,
    branch: &str,
    base: &str,
) -> Result<bool, String> {
    if wt_path.exists() {
        return Ok(false);
    }
    if let Some(parent) = wt_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bare_s = bare.to_string_lossy().to_string();
    let wt_s = wt_path.to_string_lossy().to_string();
    let bare_git = |tail: &[&str]| crate::git::git_command(&bare_git_args(&bare_s, tail)).output();
    let branch_exists = bare_git(&[
        "show-ref",
        "--verify",
        "--quiet",
        &format!("refs/heads/{branch}"),
    ])
    .map(|o| o.status.success())
    .unwrap_or(false);
    let out = if branch_exists {
        // Re-dispatch of an existing branch: check it out as-is — no base fork involved.
        bare_git(&["worktree", "add", &wt_s, branch])
    } else {
        // #304: fork the new branch from an *up-to-date* base. A stale local base (not updated
        // after sibling PRs merged on the remote) makes every dispatched branch miss those
        // commits, so its PR comes up CONFLICTING. Fetch `origin/<base>` best-effort (offline is
        // fine — we fall back to the local ref) and prefer forking from `origin/<base>` so the
        // branch starts from current remote base. `-c credential.useHttpPath` mirrors the wt.rs
        // fetch helper so ADO/GCM fetches can derive the org from the URL path.
        let origin_ref = format!("refs/remotes/origin/{base}");
        let _ = bare_git(&[
            "-c",
            "credential.useHttpPath=true",
            "fetch",
            "origin",
            &format!("refs/heads/{base}:{origin_ref}"),
        ]);
        let origin_exists = bare_git(&["show-ref", "--verify", "--quiet", &origin_ref])
            .map(|o| o.status.success())
            .unwrap_or(false);
        let count = |range: &str| -> u32 {
            bare_git(&["rev-list", "--count", range])
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .trim()
                        .parse::<u32>()
                        .ok()
                })
                .unwrap_or(0)
        };
        let (behind, ahead) = if origin_exists {
            (
                count(&format!("refs/heads/{base}..{origin_ref}")),
                count(&format!("{origin_ref}..refs/heads/{base}")),
            )
        } else {
            (0, 0)
        };
        let (start, warn) = dispatch_fork_ref(base, origin_exists, behind, ahead);
        if let Some(w) = warn {
            eprintln!("bn dispatch: {w}");
        }
        bare_git(&["worktree", "add", "-b", branch, &wt_s, &start])
    }
    .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(true)
}

/// Decide the start ref for a freshly-forked dispatch branch, plus any advisory warning, from the
/// observed relationship between the local base and `origin/<base>` (#304, #456). `origin_exists`
/// is whether the `origin/<base>` remote-tracking ref resolved (false when offline or the base is
/// local-only). `behind` = commits `origin/<base>` has that the local base lacks; `ahead` =
/// commits the local base has that `origin/<base>` lacks.
///
/// A fresh dispatch branch should start from the repo's *current* remote base whenever origin is
/// reachable — a stale or **diverged** local base is exactly what makes the branch miss merged
/// sibling work and its PR come up CONFLICTING. So fork from `origin/<base>` unless:
///   - origin is unavailable (offline / local-only base) — fall back to the local base; or
///   - the local base is *purely ahead* (`ahead > 0 && behind == 0`) — it's a clean superset of
///     `origin/<base>` (contains it plus unpushed commits), so keep it rather than silently drop
///     that local work (#304).
///
/// The diverged case (`behind > 0 && ahead > 0`) — a squash-merge or a rewritten remote leaves the
/// local base with both missing and extra commits — now forks from `origin/<base>` too (#456): the
/// extra local-base commits are drift on a mirror's local `main`, and carrying them is what forced
/// the "its PR may need a rebase" fallback on a rarely-updated second repo.
pub(crate) fn dispatch_fork_ref(
    base: &str,
    origin_exists: bool,
    behind: u32,
    ahead: u32,
) -> (String, Option<String>) {
    // Offline / local-only base: nothing fresher to fork from.
    if !origin_exists {
        return (base.to_string(), None);
    }
    // Purely ahead: the local base already contains origin/<base> plus unpushed commits, so it's a
    // clean superset — keep it rather than drop that local work. Not stale, so no warning.
    if ahead > 0 && behind == 0 {
        return (base.to_string(), None);
    }
    // Otherwise fork from the CURRENT remote base.
    let warn = match (behind, ahead) {
        (0, _) => None,
        (b, 0) => Some(format!(
            "local {base} was {b} commit(s) behind origin/{base} — forking from origin/{base} \
             so this branch starts from current {base}"
        )),
        (b, a) => Some(format!(
            "local {base} had diverged from origin/{base} ({b} behind, {a} ahead) — forking from \
             origin/{base} so this branch starts from current {base} ({a} local-only commit(s) \
             on {base} left behind)"
        )),
    };
    (format!("origin/{base}"), warn)
}

/// Run the post-create lifecycle hook (mirrors lib/tmux-lib.zsh): `~/.config/bn/hooks/post-create`
/// then `<wt>/.bn/hooks/post-create`, both if present, with cwd = the worktree and BN_WORKTREE_PATH
/// / BN_BRANCH / BN_REPO / BN_MAIN_PATH set. A non-zero hook is reported but never fails dispatch.
pub(crate) fn run_post_create_hook(
    cfg: &Config,
    wt_path: &std::path::Path,
    repo: &str,
    branch: &str,
    base: &str,
) {
    let main_path = cfg
        .worktree_dir
        .join(repo)
        .join(crate::wt::sanitize_branch(base));
    let global = std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".config/bn/hooks/post-create"));
    let candidates = [global, Some(wt_path.join(".bn/hooks/post-create"))];
    for hook in candidates.into_iter().flatten() {
        if !is_executable(&hook) {
            continue;
        }
        println!("Running post-create hook: {}", hook.display());
        let ok = std::process::Command::new(&hook)
            .current_dir(wt_path)
            .env("BN_WORKTREE_PATH", wt_path)
            .env("BN_BRANCH", branch)
            .env("BN_REPO", repo)
            .env("BN_MAIN_PATH", &main_path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            println!("  post-create hook exited non-zero (continuing)");
        }
    }
}


/// The references section folded into a dispatched ship agent's seed (#623): the selected coding
/// standards by name, with a hint to run `bn ref <name>` for each one's full text — so the seed
/// stays small but the agent knows to consult them and write to your standards. Selection: an
/// empty `refs` attaches ALL references; explicit names (repeatable or comma-separated) pin a
/// subset; a literal `-` (or empty value) attaches none. Comma-splits and resolves each name
/// against the store (fuzzy), dedups, and preserves order. `None` when nothing is attached (no
/// references exist, or `-` was passed) so the caller appends nothing.
pub(crate) fn references_section(refs: &[String]) -> Option<String> {
    // Flatten comma-separated values; a `-` or empty token means "attach none".
    let mut tokens: Vec<String> = Vec::new();
    for r in refs {
        for part in r.split(',') {
            let t = part.trim();
            if t == "-" {
                return None;
            }
            if !t.is_empty() {
                tokens.push(t.to_string());
            }
        }
    }
    let names: Vec<String> = if tokens.is_empty() {
        // Default: every reference in the store.
        bn_core::references::names()
    } else {
        // Explicit subset: resolve each (fuzzy) to its canonical name, dedup, keep order.
        let mut out: Vec<String> = Vec::new();
        for t in &tokens {
            if let Some(r) = bn_core::references::resolve(t) {
                if !out.contains(&r.name) {
                    out.push(r.name);
                }
            }
        }
        out
    };
    if names.is_empty() {
        return None;
    }
    Some(format!(
        "\n\nApply these references (my coding standards) when writing code, and check your \
         changes against them — run `bn ref <name>` for the full text of each: {}.",
        names.join(", ")
    ))
}

/// A fresh lowercase UUID (via `uuidgen`, universal on macOS/Linux) to pin an agent's session id so
/// `bn agent send` can steer it via `-p --resume`. Falls back to a self-generated UUIDv4 when
/// `uuidgen` is absent (#646): copilot ≥1.0.71 rejects a `--session-id` that isn't a valid UUID, so
/// the old `bn-<pid>-<ts>` fallback broke steering entirely — the fallback must still be a real UUID.
pub(crate) fn gen_uuid() -> String {
    std::process::Command::new("uuidgen")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_lowercase())
        .filter(|s| is_uuid(s))
        .unwrap_or_else(fallback_uuid)
}

/// True if `s` is a canonical 8-4-4-4-12 hex UUID string (the shape copilot validates).
pub(crate) fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// A self-generated RFC 4122 v4 UUID for when `uuidgen` isn't on PATH. Draws entropy from
/// `/dev/urandom` when available, else derives 16 bytes from a splitmix64 seeded by wall-clock nanos
/// and pid — either way the version/variant bits are stamped so the result is a *valid* UUID.
fn fallback_uuid() -> String {
    let mut bytes = [0u8; 16];
    let filled = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| {
            use std::io::Read;
            f.read_exact(&mut bytes)
        })
        .is_ok();
    if !filled {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut state = nanos ^ ((std::process::id() as u64) << 32) ^ 0x9E37_79B9_7F4A_7C15;
        for chunk in bytes.chunks_mut(8) {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            let b = z.to_le_bytes();
            chunk.copy_from_slice(&b[..chunk.len()]);
        }
    }
    format_uuid_v4(bytes)
}

/// Stamp the version (4) and variant (RFC 4122) bits on 16 bytes and format as a canonical UUID.
fn format_uuid_v4(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let mut s = String::with_capacity(36);
    for (i, b) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Longest inline `--seed` we'll risk on the tmux launch command. A long multi-paragraph seed
/// (backticks, quotes, `#`, newlines) has been observed truncating in transit and leaving an
/// unbalanced quote that wedges the shell (#646), so past this — or on any newline — we refuse to
/// inline and force the caller to fail loud instead of garbling the command.
pub(crate) const MAX_INLINE_SEED: usize = 512;

/// Build the seed argument fragment for the launch command. Prefers a file-based seed (`--seed-file`,
/// always short); a short single-line inline `--seed` is fine; but a long/multiline seed with no
/// seed file is refused (#646) — a wedged shell is worse than a clear failure.
pub(crate) fn seed_launch_arg(
    seed_file: Option<&std::path::Path>,
    seed: &str,
) -> Result<String, String> {
    match seed_file {
        Some(f) => Ok(format!(" --seed-file {}", shq(&f.to_string_lossy()))),
        None if seed.len() <= MAX_INLINE_SEED && !seed.contains('\n') => {
            Ok(format!(" --seed {}", shq(seed)))
        }
        None => Err(format!(
            "seed-file write failed and the {}-byte seed is too long to inline safely — a long \
             inline seed truncates in transit and wedges the shell. Free space/permissions in the \
             temp dir (or set TMPDIR to a fast local dir) and re-dispatch.",
            seed.len()
        )),
    }
}

/// True if `s` is fully quote-balanced as a POSIX shell command: every `'`/`"` is closed and no
/// trailing backslash dangles. A truncated command (odd/unterminated quote) never ends its string
/// and drops the tmux window to a stuck shell (surfacing as `unknown option: --zsh`, #646); this
/// correctly accepts `shq`'s `'\''` escaping (close-escape-reopen) which a naive quote count would
/// mis-flag.
pub(crate) fn command_quoting_is_balanced(s: &str) -> bool {
    #[derive(PartialEq)]
    enum St {
        Bare,
        Single,
        Double,
    }
    let mut st = St::Bare;
    let mut escaped = false;
    for c in s.chars() {
        match st {
            St::Bare => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '\'' {
                    st = St::Single;
                } else if c == '"' {
                    st = St::Double;
                }
            }
            // In single quotes nothing is special except the closing quote (no backslash escaping).
            St::Single => {
                if c == '\'' {
                    st = St::Bare;
                }
            }
            St::Double => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    st = St::Bare;
                }
            }
        }
    }
    st == St::Bare && !escaped
}

/// The `bn agent launch` invocation prefix — the native replacement for the old `workflow/bin/agent`
/// zsh launcher (#683+). Points at the *installed* bn (`$HOME/.config/bn/bn`, the same stable path
/// tmux.conf uses for `bn jump` / `bn tmux-cockpit`), because the tmux server's PATH lacks the
/// bindir, so callers embed an absolute command. Callers append ` {kind} …flags` to this.
pub(crate) fn agent_launcher_bin() -> String {
    std::env::var("HOME")
        .map(|h| format!("{h}/.config/bn/bn agent launch"))
        .unwrap_or_else(|_| "bn agent launch".to_string())
}

fn worker_memory_wrapped_command(cmd: &str, memory_ceiling_mb: u64, systemd_run: bool) -> String {
    let inner = vec!["sh".to_string(), "-lc".to_string(), cmd.to_string()];
    bn_core::worker::spawn::wrap(
        bn_core::worker::spawn::MemoryCap::from_estimate_mb(memory_ceiling_mb),
        systemd_run,
        &inner,
    )
    .iter()
    .map(|arg| shq(arg))
    .collect::<Vec<_>>()
    .join(" ")
}

fn worker_completion_wrapped_command(
    cmd: &str,
    request_id: &str,
    executable: &std::path::Path,
) -> String {
    let script = r#"sh -lc "$1"
code=$?
if [ "$code" -eq 0 ]; then
  "$3" worker finish "$2" --exit-code "$code"
else
  "$3" worker finish "$2" --fail --exit-code "$code"
fi
finish_code=$?
if [ "$finish_code" -ne 0 ]; then
  exit "$finish_code"
fi
exit "$code""#;
    ["sh", "-c", script, "bn-worker-complete", cmd, request_id]
        .into_iter()
        .map(shq)
        .chain(std::iter::once(shq(&executable.to_string_lossy())))
        .collect::<Vec<_>>()
        .join(" ")
}

fn systemd_run_available() -> bool {
    std::process::Command::new("systemd-run")
        .args(["--user", "--scope", "--quiet", "--collect", "--", "true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// The shared gh config dir a pinned worktree copy is seeded from: `$GH_CONFIG_DIR` if set (so an
/// already-pinned parent shell propagates), else gh's default `~/.config/gh`.
fn gh_source_config_dir() -> std::path::PathBuf {
    if let Some(d) = std::env::var_os("GH_CONFIG_DIR").filter(|s| !s.is_empty()) {
        return std::path::PathBuf::from(d);
    }
    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".config")
        .join("gh")
}

/// Pin a worktree to its own `GH_CONFIG_DIR` so a dispatched agent's `gh auth switch` is scoped to
/// that worktree and can't flip a sibling lane's (or bn's own) gh account (#456). Returns the dir
/// to point `GH_CONFIG_DIR` at, or `None` to leave the agent on the shared config (no source config
/// to seed from — pinning to an empty, unauthed dir would break the agent's gh, which is worse).
///
/// Idempotent: an already-seeded dir is reused as-is so the agent's own switched account survives a
/// re-dispatch. `branch` is the sanitized (slash-flattened) form, matching the worktree dir.
fn pin_worktree_gh_account(repo: &str, branch: &str) -> Option<std::path::PathBuf> {
    // Machine-LOCAL base (~/.bn), never the synced notes tree — in gh's insecure mode the seeded
    // hosts.yml carries a token, which must not sync into git.
    let base = bn_core::resolver::bn_home().join("gh");
    let dir = bn_core::gh_pin::worktree_gh_config_dir(&base, repo, branch);
    // Already pinned by a prior dispatch — keep whatever account the agent switched to.
    if dir.join("hosts.yml").is_file() {
        return Some(dir);
    }
    let src = gh_source_config_dir();
    // No shared account to copy → don't pin (leave the agent on the global config).
    if !src.join("hosts.yml").is_file() {
        return None;
    }
    std::fs::create_dir_all(&dir).ok()?;
    for f in bn_core::gh_pin::GH_STATE_FILES {
        let from = src.join(f);
        if from.is_file() {
            // copy preserves the source's 0600 perms on unix, so a copied inline token stays owner-
            // only. A missing optional file (e.g. no config.yml) is fine.
            let _ = std::fs::copy(&from, dir.join(f));
        }
    }
    // Confirm the account file landed; otherwise don't pin (avoid an unauthed agent).
    dir.join("hosts.yml").is_file().then_some(dir)
}

/// Orchestrate `bn dispatch`: resolve the target repo (an explicit `--repo` name from anywhere, else
/// the cwd's), resolve the item, create a worktree off `base` for it, seed that branch's note (goal
/// = the item, plus a source link), run the post-create hook, mark the source todo done, and open a
/// window on it in the repo's tmux session — an agent window with `--agent`. The execution end of
/// `bn next`. `ctx` is the cwd's note context (for todo-id resolution / closing the source todo);
/// it's `None` only when dispatching into another repo from outside any repo.
fn enter_dispatched_investigate_stage(
    cfg: &Config,
    target: &Resolved,
) -> Result<bn_core::pipeline::gate::GateDecision, bn_core::store::StoreError> {
    bn_core::pipeline::stage::transition(
        cfg,
        &cfg.note_file(target),
        bn_core::pipeline::stage::Stage::Investigate,
    )
}

/// Run the fully-seeded harness command `cmd` in `dir` in the foreground, inheriting this process's
/// stdio/tty so the agent takes over the current pane, and block until it exits. Used by
/// `bn dispatch --attach` (the `claude`/`copilot` wrappers): control returns here on exit, which is
/// exactly where the leased pool slot gets handed back. Returns whether the harness exited cleanly.
fn run_agent_foreground(cmd: &str, dir: &std::path::Path) -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The repo whose session this dispatch is running in: an inherited `BN_WORKER_REPO` (set on every
/// dispatched agent/controller lane) takes precedence, else the cwd's repo. `None` means a neutral
/// origin — not inside any repo's session (a bare shell in a non-repo dir, or a daemon launched
/// from `/`).
fn current_session_repo(ctx: Option<&Resolved>) -> Option<String> {
    std::env::var("BN_WORKER_REPO")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| ctx.map(|c| c.repo.clone()))
}

/// Repo-session containment: a dispatch that *explicitly* targets `--repo X` must originate from
/// X's own session, never from a **different** repo's session — so an agent (or a human) working in
/// repo Y can't accidentally fan work into repo X and cross the streams. A neutral origin (no
/// session repo) and X's own session are both allowed, which preserves `--repo`-from-anywhere and
/// the orchestrator (its `sh_dispatch` declares `BN_WORKER_REPO=<target>`, so it always dispatches
/// as the target's own session). Returns the operator-facing rejection message on a cross-repo
/// dispatch, else `None`. Pure for tests.
fn cross_repo_session_block(
    explicit_repo: bool,
    session_repo: Option<&str>,
    target_repo: &str,
) -> Option<String> {
    if !explicit_repo {
        // No `--repo`: the dispatch targets the current session's own repo — never cross-repo.
        return None;
    }
    match session_repo {
        // Repo names are case-insensitive identifiers here — the cwd-derived `ctx.repo` is
        // canonically lowercased (see `resolver::context::normalize`) while a resolved `--repo`
        // target keeps its on-disk casing (e.g. `BigMono`). Compare case-insensitively so a repo with
        // uppercase letters isn't falsely blocked from dispatching to its own session.
        Some(s) if !s.eq_ignore_ascii_case(target_repo) => Some(format!(
            "bn dispatch: --repo {target_repo} can only run from {target_repo}'s session — \
             you're in {s}'s session. cd into a {target_repo} worktree (or dispatch from \
             {target_repo}'s session) and retry."
        )),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_dispatch(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo_override: Option<&str>,
    base: Option<&str>,
    raw: &str,
    name: Option<&str>,
    branch: Option<String>,
    no_tmux: bool,
    agent: Option<&str>,
    scout: bool,
    orchestrator: bool,
    skill: Option<&str>,
    verify: bool,
    yolo: bool,
    plan: PlanGate,
    fresh: bool,
    keep_project_mcps: bool,
    no_pool: bool,
    with_main: bool,
    on_pool_full: crate::cli::OnPoolFull,
    task_id: Option<&str>,
    attach: bool,
    refs: &[String],
    // The pipeline stage this dispatch *is*, when the caller knows; `None` infers it.
    stage: Option<bn_core::pipeline::stage::Stage>,
    // A fully composed seed to use verbatim instead of rendering one from a workflow skill.
    // `bn agent run --worktree` passes the catalog agent's own body; everything else passes
    // `None` and gets the skill-driven seed.
    seed_override: Option<&str>,
    json: bool,
) {
    let controller_id = std::env::var("BN_CONTROLLER_ID")
        .ok()
        .map(|id| id.trim().to_string())
        .filter(|id| {
            !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        });
    // The investigate skill IS the scout workflow (investigate first, findings to the note), so an
    // explicit `--skill investigate` gets scout semantics without needing --scout too.
    let scout = scout || skill == Some("investigate");
    // `--verify` still applies only to ship seeds. A scout now decides inside its workflow whether
    // the findings should graduate into a verified PR or finish report-only.
    let verify = verify && !scout;
    if raw.trim().is_empty() {
        fail_json(json, "dispatch", "empty-item", "bn dispatch: empty item");
    }
    // #503: opportunistic fleet GC before adding load. A dispatch wave is exactly when stale
    // stragglers from prior waves pile up (this is what drove load to 147), so reap the orphaned
    // process trees + prune dead windows first. Safe + quiet: never touches a working / WIP /
    // orchestrator agent, skips the per-branch PR network calls.
    crate::reap::sweep(cfg);
    // `--agent` present (bare or valued) launches an agent; omitted opens just a worktree/shell
    // window. A bare `--agent` arrives as the "auto" sentinel — launch, but let the launch stage's
    // configured harness decide. An explicit `--agent claude|copilot` PINS the harness (a user pin
    // wins over stage config). Validate the pin up front so a typo fails before we create a
    // worktree; the final harness (pin > stage default > copilot, then the capability gate) is
    // resolved below once repo + launch stage are known.
    let launch_agent = agent.is_some();
    let pinned_harness: Option<&'static str> = agent
        .map(str::trim)
        .filter(|a| !a.eq_ignore_ascii_case("auto"))
        .map(|a| {
            dp::normalize_agent(a).unwrap_or_else(|| {
                fail_json(
                    json,
                    "dispatch",
                    "bad-agent",
                    &format!("bn dispatch: unknown --agent '{a}' (use: copilot | claude)"),
                )
            })
        });

    let (repo, bare) = authorization::resolve_authorized_repo(cfg, ctx, repo_override, json);
    // Repo-session containment: an explicit `--repo X` fired from a *different* repo's session is
    // refused (a neutral origin or X's own session pass through). Keeps each repo's work in its own
    // session without breaking `--repo`-from-anywhere or the orchestrator.
    if let Some(msg) = cross_repo_session_block(
        repo_override.is_some(),
        current_session_repo(ctx).as_deref(),
        &repo,
    ) {
        fail_json(json, "dispatch", "cross-repo-session", &msg);
    }
    let dispatch_base = ensure_dispatch_base(cfg, &repo, &bare, base).unwrap_or_else(|e| {
        fail_json(
            json,
            "dispatch",
            "base-bootstrap-failed",
            &format!("bn dispatch: base bootstrap failed: {e}"),
        )
    });
    if let Some(message) = dispatch_base_bootstrap_message(&repo, &dispatch_base) {
        eprintln!("{message}");
    }
    let base = dispatch_base.branch;

    let (title, url, source_todo) = resolve_dispatch_item(cfg, ctx, raw);
    let task_name = bn_core::agent::derive_task_name(name, &title);

    // Resolve the launch stage's model + harness. The stage (Investigate/Plan/Execute from the
    // scout/skill flags) selects `[pipeline.stages.<stage>]`, whose `harness` is the stage-aware
    // default when the user didn't pin one with `--agent`. Precedence: pin > stage harness >
    // copilot, then the machine capability gate degrades an unavailable harness to copilot
    // (surfaced, never silent). A single dispatched run uses this one harness+model for its whole
    // pipeline, so the override only distinguishes separate dispatches by their launch stage.
    let stage = dp::dispatch_pipeline_stage(stage, scout, skill);
    let model = cfg.pipeline_model(&repo, stage);
    let agent_kind: Option<&'static str> = launch_agent.then(|| {
        let stage_harness: Option<&'static str> = model
            .harness
            .as_deref()
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(|h| {
                dp::normalize_agent(h).unwrap_or_else(|| {
                    fail_json(
                        json,
                        "dispatch",
                        "bad-stage-harness",
                        &format!(
                            "bn dispatch: unknown harness '{h}' in [pipeline.stages.{}] (use: copilot | claude)",
                            stage.as_str()
                        ),
                    )
                })
            });
        let resolved = dp::effective_launch_harness(cfg, pinned_harness, stage_harness);
        if let Some((from, reason)) = &resolved.fallback_from {
            if !json {
                eprintln!(
                    "bn dispatch: {from} unavailable ({reason}); falling back to {}",
                    resolved.harness
                );
            }
        }
        resolved.harness
    });

    // Security: --yolo launches the agent with permission gating OFF. A free-text goal is the
    // user's own words — the same trust as an interactive launch, which already yolos — so that's
    // fine. But when the item resolved to an EXTERNAL url/issue, the seed embeds attacker-
    // influenceable content (a fetched issue/PR title), and running THAT unconfirmed is a prompt-
    // injection → command-execution path. So refuse to yolo an externally-sourced seed: it keeps
    // permission gating even under --yolo.
    let yolo_blocked_external = yolo && url.is_some();
    let yolo = yolo && url.is_none();
    if yolo_blocked_external {
        eprintln!(
            "bn dispatch: --yolo ignored — this item carries an external link, so its seed runs \
             WITH permission gating (an external title could inject commands)."
        );
    }
    // Approval-free by default: the launched harness drops the per-tool "allow this?" prompt for any
    // TRUSTED (non-external) dispatch — your own repo/goal — so an agent never wedges on approval
    // gating. This is decoupled from `--yolo`: `--yolo` additionally switches the *seed* to
    // autonomous framing + the consensus plan gate (below), whereas `launch_yolo` only drops the
    // per-tool gating while leaving the plan-first seed intact. An EXTERNAL seed (untrusted,
    // injection-prone) keeps full per-tool gating — the same guard as the `yolo` narrowing above.
    let launch_yolo = url.is_none();

    // Plan-review gate (#349): a ship agent proposes a plan via `bn review submit` and pauses for
    // approval before implementing / opening a PR. On by `--plan` or the `[dispatch].plan_review`
    // config default. Trusted `--yolo` lanes still pass through agent consensus so sensitive plans
    // cannot bypass a mandatory human decision; low-risk quorum approval remains unattended.
    //
    // The default ship lane is `ship-solo`, which carries no plan artifact — it scopes in-session
    // and asks a human when unclear — so the plan-review gate (and its editor/nvim plan split)
    // never applies, even under `--yolo`. The legacy nine-stage pipeline auto-route is off unless
    // `$BN_DISPATCH_PIPELINE=1`; the `bn pipeline` machine stays reachable regardless.
    let pipeline_opt_in = std::env::var("BN_DISPATCH_PIPELINE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let pipeline_investigate =
        routes_to_pipeline_investigate(pipeline_opt_in, agent_kind.is_some(), scout, skill);
    let ship_solo_lane =
        !pipeline_investigate && !scout && matches!(skill, None | Some("ship-solo"));
    let plan_gate =
        plan_gate_enabled(plan, cfg.dispatch_plan_review, yolo, scout) && !ship_solo_lane;

    let branch = match branch {
        Some(branch) => branch,
        None => match dp::choose_implicit_dispatch_branch(
            &title,
            fresh,
            agent_kind.is_some(),
            |candidate| dispatch_branch_occupied(&bare, &cfg.worktree_dir, &repo, candidate, &base),
        ) {
            dp::DispatchBranchChoice::Use(branch) => branch,
            // A refusal, not a failure: the leftover ref of a merged branch satisfies this test
            // with no agent alive, so asking again later gets the same answer. `EXIT_REFUSED`
            // tells a caller not to retry.
            dp::DispatchBranchChoice::AlreadyActive(branch) => refuse_json(
                json,
                "dispatch",
                "dispatch-already-active",
                &format!(
                    "bn dispatch: '{branch}' is already active for this item; not creating a \
                     duplicate suffix branch. Use --fresh to recover, or --branch for separate \
                     same-title work."
                ),
            ),
        },
    };
    // Never dispatch an agent onto the repo's main/master branch — that would run (and later reap)
    // the primary worktree. Mirrors the guards in reap.rs and wt/cleanup.rs.
    if is_reserved_branch(&branch) || branch == base {
        fail_json(
            json,
            "dispatch",
            "refuse-main",
            &format!(
                "bn dispatch: refusing to dispatch onto '{branch}' — that's the repo's primary \
                 branch/worktree. Pick a feature branch (e.g. --branch dispatch/my-fix)."
            ),
        );
    }

    // Notes key the branch sanitized (resolver::normalize does slash→dash), and the worktree dir
    // uses the same form — so the note we seed here is the one the user sees on `cd`.
    let sanitized = branch.replace('/', "-");

    // Pool-backed dispatch is the default (#457 generalized): every repo with a bn-managed bare
    // clone leases a warm, reap-safe slot (deps/build cache intact) instead of paying a cold
    // worktree checkout + install. `--no-pool` forces the legacy cold worktree. The pool-vs-cold
    // decision is pure policy in bn-core; the slot lease is the existing `wt pool` primitive. A repo
    // without a bn-managed bare clone, or a full pool, transparently falls back to a cold worktree —
    // so pooling is always a safe best-effort default, and `--no-pool` reproduces the old cold path
    // exactly. Pool size is derived from the slots that exist (floored at DEFAULT_POOL_SIZE and
    // grown on demand), so no per-repo registration or sizing is required.
    let mode = bn_core::pool::dispatch_worktree_mode(no_pool);
    // The cold path — a fresh worktree at the conventional dir — used both without `--pool` and as
    // the fallback when a warm slot can't be leased.
    let cold_worktree = || -> (std::path::PathBuf, bool) {
        let wt_path = cfg.worktree_dir.join(&repo).join(&sanitized);
        let created = match create_worktree(&bare, &wt_path, &branch, &base) {
            Ok(true) => true,
            Ok(false) if may_reuse_occupied_worktree(fresh, agent_kind.is_some()) => false,
            Ok(false) => fail_json(
                json,
                "dispatch",
                "worktree-occupied",
                &format!(
                    "bn dispatch: refusing to reuse occupied worktree {} — choose another \
                     --branch or omit --branch to allocate a unique dispatch branch",
                    wt_path.display()
                ),
            ),
            Err(e) => fail_json(
                json,
                "dispatch",
                "worktree-failed",
                &format!("bn dispatch: worktree creation failed: {e}"),
            ),
        };
        (wt_path, created)
    };
    // Last field: should the lease below evict the slot's holder? `--fresh`, or occupants `done`.
    let (wt_path, comparison_path, created, used_pool, replace_prior) = match mode {
        bn_core::pool::WorktreeMode::Pool => match wt_pool::lease_for_dispatch_mode(
            cfg,
            &repo,
            &branch,
            Some(&base),
            with_main,
        ) {
            Some((slot, comparison, true)) => (slot, comparison, true, true, fresh),
            // A same-branch slot that is already leased: reusable for `--fresh`, or when its only
            // occupants have finished — see `slot_reuse`. Either way the lease below evicts them.
            Some((slot, comparison, false)) => {
                match slot_reuse::verdict(fresh, agent_kind.is_some(), &slot, &branch) {
                    Some(replace) => (slot, comparison, false, true, replace),
                    None => fail_json(
                        json,
                        "dispatch",
                        "worktree-occupied",
                        &slot_reuse::refusal(&slot, &branch),
                    ),
                }
            }
            // Every worker slot is leased. Cutting a cold worktree here is what the code used to
            // do unconditionally, and it is unbounded — nothing ever reclaims those directories,
            // so a busy day left dozens behind. Capacity is finite; the honest answer is to
            // schedule the work and let it start when a slot frees.
            None => match on_pool_full {
                crate::cli::OnPoolFull::Queue => {
                    // An orchestrate run stores only the item text, so handing a paired request to
                    // it would silently drop --with-main. Normal captured dispatches already live
                    // in the typed backlog and will retry there; a forced direct dispatch must fail
                    // intact rather than queue different work.
                    if with_main {
                        fail_json(
                            json,
                            "dispatch",
                            "paired_pool_unavailable",
                            &format!(
                                "bn dispatch: --with-main needs two warm worker slots for '{repo}'; \
                                 the paired request was not downgraded or queued without its flag"
                            ),
                        );
                    }
                    schedule_on_pool_full(&repo, raw, json);
                    // `schedule_on_pool_full` returns only when there was no run to queue onto.
                    eprintln!(
                        "bn dispatch: pool full for '{repo}' and no orchestrate run to queue onto \
                         — falling back to a cold worktree."
                    );
                    let (p, c) = cold_worktree();
                    (p, None, c, false, fresh)
                }
                crate::cli::OnPoolFull::Fail => fail_json(
                    json,
                    "dispatch",
                    "pool_full",
                    &format!(
                        "bn dispatch: every warm slot for '{repo}' is leased. Free one with \
                         `bn agent kill <repo:branch>` or `wt pool return`, or pass \
                         --on-pool-full cold to cut a throwaway worktree."
                    ),
                ),
                crate::cli::OnPoolFull::Cold => {
                    if with_main {
                        fail_json(
                            json,
                            "dispatch",
                            "paired_pool_unavailable",
                            "bn dispatch: --with-main requires two pool slots and cannot fall back \
                             to one cold worktree",
                        );
                    }
                    eprintln!(
                        "bn dispatch: couldn't lease a warm slot for '{repo}' (pool full? run \
                         `bn wt pool warm`, or pass --no-pool) — falling back to a cold worktree."
                    );
                    let (p, c) = cold_worktree();
                    (p, None, c, false, fresh)
                }
            },
        },
        bn_core::pool::WorktreeMode::Cold => {
            let (p, c) = cold_worktree();
            (p, None, c, false, fresh)
        }
    };

    if created {
        // A cold worktree pays the post-create install hook; a pooled slot is already warm (deps
        // installed by `bn wt pool warm`), so it skips the cold install — the whole point of the
        // pool (#457). MCP-extras wiring is cheap config (no install), so both paths get it.
        if !used_pool {
            run_post_create_hook(cfg, &wt_path, &repo, &branch, &base);
        }
        // Curated planning/productivity MCPs (#323): when `[mcp] extras` is configured, wire them
        // into the worktree so the dispatched agent boots with them available. Server
        // configs only — the instruction block would dirty a tracked CLAUDE.md into the agent's
        // diff. Best-effort: a wiring failure never blocks a dispatch.
        let extras = bn_core::resolver::mcp_extras();
        if !extras.is_empty() {
            let harness_name = agent_kind
                .map(str::to_string)
                .unwrap_or_else(|| wt::review_harness_name(cfg));
            if let Some(h) = bn_core::mcp::Harness::parse(&harness_name) {
                match bn_core::mcp::wire_servers(&wt_path, h, &extras) {
                    Ok(report) => {
                        for m in &report.missing_runners {
                            eprintln!(
                                "bn dispatch: MCP extra runner '{}' not on PATH — {}",
                                m.runner, m.fix
                            );
                        }
                    }
                    Err(e) => eprintln!("bn dispatch: MCP extras not wired: {e}"),
                }
            }
        }
    }

    // Resolve the workflow skill that will drive the agent (#321): explicit --skill, else by task
    // type (scout → investigate, ship → ship-solo). Loaded against the fresh worktree so repo-tier
    // skills apply, falling back to the dispatcher's cwd (where the bn checkout / BN_MCP_SKILLS_DIR
    // may be visible). Runtime workflows are required: missing defaults fail loudly rather than
    // falling back to a compiled prompt.
    let skill_loaded = if agent_kind.is_some() {
        let name = if pipeline_investigate && skill.is_none() {
            "investigate"
        } else {
            dispatch_skill_name(skill, scout)
        };
        let found = bn_core::skills::load_skill(cfg, &wt_path, name).unwrap_or_else(|error| {
            fail_json(
                json,
                "dispatch",
                "invalid-skill-catalog",
                &format!("bn dispatch: {error}"),
            )
        });
        let found = if found.is_some() {
            found
        } else {
            std::env::current_dir().ok().and_then(|dir| {
                bn_core::skills::load_skill(cfg, &dir, name).unwrap_or_else(|error| {
                    fail_json(
                        json,
                        "dispatch",
                        "invalid-skill-catalog",
                        &format!("bn dispatch: {error}"),
                    )
                })
            })
        };
        if found.is_none() {
            fail_json(
                json,
                "dispatch",
                "skill-not-found",
                &format!(
                    "bn dispatch: required skill '{name}' not found — `bn skills` lists the available ones"
                ),
            );
        }
        found
    } else {
        if skill.is_some() {
            eprintln!(
                "bn dispatch: --skill shapes a dispatched agent's seed — pass --agent to launch one"
            );
        }
        None
    };

    // Seed the new branch's note: goal = the item; record the source link. Only on a *fresh*
    // worktree — re-dispatching an existing branch (or a fanout hitting one already in flight) must
    // not clobber the note a live agent has been writing. `ensure_note` is safe either way.
    let target = Resolved {
        repo: repo.clone(),
        branch: sanitized.clone(),
    };
    let _ = commands::ensure_note(cfg, &target, &today());

    // Bind the task to this worktree, so the fleet can say what the agent is working on and link
    // through to it. Done here — after the branch and worktree exist, before the agent launches —
    // so the very first supervisor poll after the launch already sees the binding. Not fatal: an
    // unknown id is worth a warning, not a refusal to start work that is otherwise ready.
    if let Some(id) = task_id {
        match bn_core::task_store::task_store(cfg).claim(id, &repo, &sanitized) {
            Ok(true) => {}
            Ok(false) => eprintln!(
                "bn dispatch: no task '{id}' in the store — dispatching anyway, but the fleet \
                 won't show a task for this agent"
            ),
            Err(e) => eprintln!("bn dispatch: could not bind task '{id}': {e}"),
        }
    }

    if created {
        let _ = commands::set_goal(cfg, &target, &title, &today());
        // The goal says what the worktree is *for*; the todo makes it trackable work — it lands in
        // `bn todo` / `bn next` and can be closed with `bn done`, so a dispatched task is visible in
        // the same places as anything you captured by hand.
        // `if_absent` on both, matching the `decision:` writes just below. Without it a
        // re-dispatch (or two dispatches racing the same branch) seeds the same todo twice as `t1`
        // and `t2`, and the source link twice — the note then claims two pieces of work where
        // there is one.
        let _ = commands::add(cfg, &target, "todo", &title, Some(&title), &today());
        if let Some(u) = &url {
            let source = format!("source: {u}");
            let _ = commands::add(cfg, &target, "collab", &source, Some(&source), &today());
        }
        // Record the task type in the note itself (bn's own store — no new format): a scout
        // investigates and reports, a ship delivers a change. `bn promote` flips scout → ship.
        let task_line = if scout {
            "task: scout — investigate & report, no code change"
        } else {
            "task: ship — deliver a change"
        };
        let _ = commands::add(cfg, &target, "decision", task_line, Some("task:"), &today());
        if let Some(s) = &skill_loaded {
            let _ = commands::add(
                cfg,
                &target,
                "decision",
                &format!("skill: {} — the workflow this dispatch follows", s.name),
                Some("skill:"),
                &today(),
            );
        }
        if let Some(main) = &comparison_path {
            let comparison = format!(
                "comparison worktree: {} — detached at the latest remote main",
                main.display()
            );
            let _ = commands::add(
                cfg,
                &target,
                "decision",
                &comparison,
                Some("comparison worktree:"),
                &today(),
            );
        }
        if verify {
            let _ = commands::add(
                cfg,
                &target,
                "decision",
                "gate: run `bn verify` before done — only report done when the build/test/lint gate passes",
                Some("gate: run `bn verify`"),
                &today(),
            );
        }
        if plan_gate {
            let _ = commands::add(
                cfg,
                &target,
                "decision",
                "gate: plan-review — propose a plan via `bn review submit` and wait for approval before implementing / opening a PR (#349)",
                Some("gate: plan-review"),
                &today(),
            );
        }
        if pipeline_investigate {
            match enter_dispatched_investigate_stage(cfg, &target) {
                Err(e) => fail_json(
                    json,
                    "dispatch",
                    "pipeline-stage",
                    &format!("bn dispatch: failed to enter investigate stage: {e}"),
                ),
                // The SMART gate (#998) refused. This is a real stop, not a warning: dispatching
                // an unscoped task is exactly the wasted-worktree case the gate exists to prevent.
                // The task has been stamped and an attention item raised to get it fixed.
                Ok(decision) if !decision.admits() => {
                    let why = decision.refusal_line().unwrap_or_default();
                    fail_json(
                        json,
                        "dispatch",
                        "smart-gate",
                        &format!("bn dispatch: {why}"),
                    );
                }
                Ok(_) => {}
            }
        }
    }

    // Close the source todo in the originating (cwd) note — it's now in flight.
    if let (Some(ctx), Some(id)) = (ctx, &source_todo) {
        let _ = commands::done(cfg, ctx, id, &today(), None);
    }

    // Open the worktree's window in the *repo's* tmux session (the layout `tat` expects), creating
    // the session if needed — so this works whether or not you were already in tmux. With --agent,
    // the window runs the seeded agent instead of a shell. `--no-tmux` skips it entirely.
    let mut tmux_opened = false;
    let mut agent_launched = false;
    // Project MCP servers suppressed for a dispatched Copilot agent (#384) — surfaced in the JSON
    // envelope and the human summary so it's visible why a repo's MCP didn't load.
    let mut suppressed_mcps: Vec<String> = Vec::new();
    // The per-worktree gh config dir this agent is pinned to (#456), for the JSON envelope. `None`
    // when no agent launched or the shared gh config couldn't be found to seed one.
    let mut gh_config_dir: Option<String> = None;
    if !no_tmux {
        if let Some(kind) = agent_kind {
            // Pre-grant the harness's folder-trust for the new worktree — unconditionally, so NO
            // dispatched agent boots into a "trust this folder?" prompt (the yolo flags don't cover
            // it). Folder trust only silences that dialog; it never grants tool execution (per-tool
            // gating still applies for an untrusted/external seed), so it's safe for every dispatch.
            // In-process (no dependency on which `bn` the launcher finds).
            let _ = apply_folder_trust(&wt_path.to_string_lossy(), kind);
            // Pin a session id so `bn agent send` can steer this exact agent via `-p --resume`, and
            // record it after this launch wins the per-worktree lease.
            let sid = gen_uuid();
            let plan_path = if plan_gate && !pipeline_investigate {
                Some(
                    bn_core::plan::init_agent(cfg, &target, &sid).unwrap_or_else(|error| {
                        fail_json(
                            json,
                            "dispatch",
                            "plan-file",
                            &format!("bn dispatch: cannot create plan draft: {error}"),
                        )
                    }),
                )
            } else {
                None
            };
            // Attribute this run's work-class (#381) so `bn cost` categorizes it once the harness
            // session is ingested, and (#500) so the live fleet stamps its role. A scout is
            // research; an `--orchestrator` monitor is orchestration (which the dashboard pins as a
            // distinct fleet tier); every other dispatched ship agent is coding (orchestrated waves
            // shell out to `bn dispatch`, so they land here as coding too).
            let work_class = if scout {
                bn_core::cost::WorkClass::Research
            } else if orchestrator {
                bn_core::cost::WorkClass::Orchestration
            } else {
                bn_core::cost::WorkClass::Coding
            };
            // A caller that already composed the whole prompt (`bn agent run --worktree`, seeding a
            // *catalog agent* rather than a workflow skill) hands it over verbatim, and the skill
            // machinery is skipped rather than wrapped around a body it knows nothing about — an
            // agent manifest is already self-contained. Everything after this point still applies:
            // the references section, standing directives and the plan gate are properties of the
            // dispatch, not of the skill.
            let seed = match seed_override {
                Some(seed) => seed.to_string(),
                None => {
                    let workflow = skill_loaded
                        .as_ref()
                        .expect("agent dispatch requires a loaded workflow");
                    if pipeline_investigate {
                        pipeline_investigate_seed(cfg, &wt_path, &title, url.as_deref(), workflow)
                            .unwrap_or_else(|error| {
                                fail_json(
                                    json,
                                    "dispatch",
                                    "prompt-render",
                                    &format!("bn dispatch: {error}"),
                                )
                            })
                    } else {
                        skill_seed_prompt(
                            cfg,
                            &wt_path,
                            SkillSeedContext {
                                title: &title,
                                url: url.as_deref(),
                                scout,
                                yolo,
                                verify,
                                plan_gate: plan_gate && !pipeline_investigate,
                            },
                            workflow,
                        )
                        .unwrap_or_else(|error| {
                            fail_json(
                                json,
                                "dispatch",
                                "prompt-render",
                                &format!("bn dispatch: {error}"),
                            )
                        })
                    }
                }
            };
            // Fold the coding-standards references (#623) and the branch's standing directives
            // (`bn agent instruct`) into the seed. See `seed_context::section`.
            let seed = format!(
                "{seed}{}",
                super::seed_context::section(cfg, &repo, &branch, scout, refs)
            );
            // No separate "## Plan artifact" section: the plan-review gate's step 1 names the
            // bn-owned draft file and forbids note-item decomposition itself.
            let plan_file_s = plan_path.as_ref().map(|path| path.display().to_string());
            // Append the concrete plan-review-gate protocol (#349) last, so it's the authoritative
            // final instruction: register a plan, poll, and don't ship until approved.
            let seed = append_plan_gate(
                cfg,
                &wt_path,
                seed,
                plan_gate,
                pipeline_investigate,
                PlanGateContext {
                    repo: &repo,
                    branch: &branch,
                    sid: &sid,
                    verify,
                    reviewers: cfg.dispatch_plan_reviewers,
                    quorum: cfg.dispatch_plan_review_quorum,
                    plan_file: plan_file_s.as_deref(),
                },
            )
            .unwrap_or_else(|error| {
                fail_json(
                    json,
                    "dispatch",
                    "prompt-render",
                    &format!("bn dispatch: {error}"),
                )
            });
            // `launch_yolo` (approval-free for trusted dispatches), NOT the seed-framing `yolo`:
            // this flag only drops the launched harness's per-tool gating. The plan-first vs.
            // autonomous seed and the consensus plan gate are decided separately by `yolo` above.
            let yolo_flag = if launch_yolo { " --yolo" } else { "" };
            // On a recovery re-dispatch, force a brand-new harness session instead of resuming the
            // worktree's idle/dead one (#383) — a resumed Copilot session comes back at an empty
            // prompt having done nothing; a fresh, re-seeded one actually resumes the work.
            let fresh_flag = if fresh { " --fresh" } else { "" };
            // File-based seed: keep the window's launch command short (a long inline seed has
            // been observed truncating in transit, leaving an unbalanced quote and a stuck
            // shell, #646). The launcher reads the file via --seed-file; a short single-line seed
            // may inline, but a long/multiline seed with no seed file is refused (fail loud) rather
            // than garbled onto the tmux command.
            let seed_file = wt::prompt_to_file("seed", &repo, &sanitized, &seed);
            let mut cmd = match seed_launch_arg(seed_file.as_deref(), &seed) {
                Ok(seed_arg) => format!(
                    "{} {kind}{yolo_flag}{fresh_flag} --session {sid}{seed_arg}",
                    agent_launcher_bin(),
                ),
                Err(msg) => {
                    if json {
                        println!(
                            "{}",
                            Envelope::failure("dispatch", "seed_write_failed", &msg).to_json()
                        );
                    } else {
                        eprintln!("dispatch: could not launch agent — {msg}");
                    }
                    return;
                }
            };
            // #384: an unattended Copilot agent hangs at boot when a repo's project MCP server
            // fails or wants interactive auth — it wedges on "Loading: … MCP servers" and never
            // reaches its prompt (0 AIC). Copilot has no blanket `--strict-mcp-config`, so we ask
            // it to skip each project server by name via the launcher's pass-through (`-- <args>`
            // become the agent's own argv). bn's own server is always kept. Off with
            // `--keep-project-mcps` (repo MCPs known-good and needed); no-op for Claude, which has
            // native strict handling.
            let suppressed = if kind == "copilot" && !keep_project_mcps {
                bn_core::mcp::copilot_project_servers(&wt_path)
            } else {
                Vec::new()
            };
            // `stage`/`model` were resolved up front (with the harness); reuse them here. The
            // suffix builder ignores `model.harness` — it only emits `--model`/`--effort`.
            cmd.push_str(&crate::cmd::launch_flags::agent_model_suffix(
                kind,
                &model,
                &suppressed,
            ));
            suppressed_mcps = suppressed;
            // #456: pin this worktree to its own GH_CONFIG_DIR so the agent's `gh auth switch`
            // (e.g. work → personal to reach a private repo) is scoped here and can't flip a
            // sibling lane's — or bn's own — gh account. Seeded from the shared config; if that
            // can't be found we leave the agent on the global config (no worse than before).
            if let Some(dir) = pin_worktree_gh_account(&repo, &sanitized) {
                cmd = format!("GH_CONFIG_DIR={} {cmd}", shq(&dir.to_string_lossy()));
                gh_config_dir = Some(dir.to_string_lossy().to_string());
            }
            if let Some(controller_id) = &controller_id {
                cmd = format!(
                    "BN_CONTROLLER_ID={} BN_WORKER_SESSION_ID={} BN_WORKER_REPO={} \
                     BN_WORKER_BRANCH={} {cmd}",
                    shq(controller_id),
                    shq(&sid),
                    shq(&repo),
                    shq(&branch),
                );
            }
            if let Some(memory_ceiling_mb) = std::env::var("BN_AGENT_MEMORY_CEILING_MB")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
            {
                cmd =
                    worker_memory_wrapped_command(&cmd, memory_ceiling_mb, systemd_run_available());
            }
            if let Ok(request_id) = std::env::var("BN_WORKER_REQUEST_ID") {
                let executable =
                    std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("bn"));
                cmd = worker_completion_wrapped_command(&cmd, &request_id, &executable);
            }
            // #646: an odd number of single quotes in the launch command never terminates its
            // string, dropping the window to a stuck shell (surfacing as `unknown option: --zsh`).
            // Everything we append is `shq`-quoted, so this should always hold — but assert it before
            // send and refuse loud rather than wedge a shell.
            if !command_quoting_is_balanced(&cmd) {
                let msg = "assembled launch command has unbalanced quotes; refusing to launch \
                           (would wedge the shell)";
                if json {
                    println!(
                        "{}",
                        Envelope::failure("dispatch", "unbalanced_command", msg).to_json()
                    );
                } else {
                    eprintln!("dispatch: {msg}");
                }
                return;
            }
            // --attach (interactive foreground): run the investigation in THIS pane instead of a
            // background tmux lane. Record the session for cost/collab exactly like the lane path,
            // run the seeded harness in the foreground (it takes over the terminal), and — once it
            // exits — hand any leased pool slot straight back, so the `claude`/`copilot` wrappers
            // leave no residue. Deliberately skips the agent-lease, tmux window, and controller
            // events: those are background-lane bookkeeping with no meaning for a live pane.
            if attach {
                let worktree = wt_path.to_string_lossy().into_owned();
                crate::cmd::agent::record_launched_session(
                    cfg,
                    crate::cmd::agent::LaunchedSession {
                        ctx: &target,
                        harness: kind,
                        session_id: &sid,
                        worktree: &worktree,
                        work_class,
                        task_name: Some(&task_name),
                        controller_id: controller_id.as_deref(),
                    },
                );
                if !json {
                    eprintln!(
                        "bn dispatch: {kind} {} in this pane → {}{}",
                        if scout { "scout" } else { "ship" },
                        wt_path.display(),
                        if used_pool {
                            " (pooled slot; returns on exit)"
                        } else {
                            ""
                        }
                    );
                }
                let ran = run_agent_foreground(&cmd, &wt_path);
                // The launcher's placement guard leased this worktree under our `sid` (one agent per
                // checkout, even for a hand-typed `claude`). The pane is now free, so release it
                // here — the window it was keyed to is the user's own shell and outlives the agent,
                // so nothing else would ever notice it went idle.
                let _ = agent_lease::release(&sid);
                // A scout leaves the slot clean, so it returns to the warm pool; `Return` refuses a
                // dirty slot (a ship that wrote code) rather than discard work — surfaced, not forced.
                let slot_returned = used_pool
                    && wt_pool::run(
                        cfg,
                        wt::PoolCmd::Return {
                            target: Some(worktree.clone()),
                            force: false,
                        },
                    );
                if json {
                    let env = Envelope::success(
                        "dispatch",
                        serde_json::json!({
                            "attach": true,
                            "agent": kind,
                            "agent_launched": ran,
                            "repo": repo,
                            "branch": branch,
                            "task": if scout { "scout" } else { "ship" },
                            "worktree": worktree,
                            "main_worktree": comparison_path,
                            "pooled": used_pool,
                            "slot_returned": slot_returned,
                            "session_id": sid,
                        }),
                    );
                    println!("{}", env.to_json());
                } else {
                    let slot = if slot_returned {
                        format!("returned pool slot → {worktree}")
                    } else if used_pool {
                        format!("pool slot kept (dirty? `bn wt pool return --force`) → {worktree}")
                    } else {
                        format!("worktree → {worktree}")
                    };
                    eprintln!(
                        "bn dispatch: {kind} exited {}; {slot}",
                        if ran { "cleanly" } else { "non-zero" }
                    );
                }
                return;
            }
            // The lane's tmux window MUST be named by the sanitized branch: `observe`, reap, the
            // cockpit and the sweeps all reconstruct `<repo>:<sanitized branch>`. A readable
            // "<title> · <hash>" name made the daemon miss its own live lanes → `has_window=false`
            // → misclassified Dead → hard-restart churn.
            let window_name = bn_core::orchestrate::tmux_window_name(&branch);
            let lease_target = format!("{repo}:{window_name}");
            let reservation = match agent_lease::reserve(agent_lease::AgentLeaseRequest {
                repo: &repo,
                branch: &branch,
                worktree: &wt_path,
                session_id: &sid,
                harness: kind,
                kind: bn_core::agent::AgentLeaseKind::Dispatch,
                tmux_target: &lease_target,
                replace: replace_prior,
                in_place: false,
                trusted: launch_yolo,
            }) {
                Ok(lease) => lease,
                Err(error) => {
                    fail_json(
                        json,
                        "dispatch",
                        "agent-lease-held",
                        &format!("bn dispatch: refusing agent launch — {error}"),
                    );
                }
            };
            if let Some(controller_id) = &controller_id {
                let _ = commands::add(
                    cfg,
                    &target,
                    "collab",
                    &format!("controller-id: {controller_id}"),
                    Some("controller-id:"),
                    &today(),
                );
            }
            let worktree = wt_path.to_string_lossy().into_owned();
            crate::cmd::agent::record_launched_session(
                cfg,
                crate::cmd::agent::LaunchedSession {
                    ctx: &target,
                    harness: kind,
                    session_id: &sid,
                    worktree: &worktree,
                    work_class,
                    task_name: Some(&task_name),
                    controller_id: controller_id.as_deref(),
                },
            );
            agent_launched = wt::open_worktree_window_cmd_named(
                &repo,
                &sanitized,
                &window_name,
                &wt_path,
                false,
                Some(&cmd),
                true,
            );
            if !agent_launched {
                if let Err(error) = reservation.rollback() {
                    eprintln!("dispatch: agent launch failed; {error}");
                }
            } else {
                agent_lease::mark_window(reservation.lease());
                reservation.commit();
                if let Some(path) = &plan_path {
                    let _ = wt::open_plan_split(&repo, &window_name, path);
                }
            }
            tmux_opened = agent_launched;
            if agent_launched {
                if let Some(controller_id) = &controller_id {
                    if let Err(error) = append_controller_worker_event(
                        controller_id,
                        &sid,
                        &repo,
                        &branch,
                        bn_core::controller::ControllerWorkerEventType::Progress,
                        &format!("worker dispatched: {title}"),
                        url.as_deref(),
                    ) {
                        eprintln!(
                            "dispatch: worker launched but controller event append failed: {error}"
                        );
                    }
                }
            }
        } else {
            // Same lane-window naming contract as the agent-launch branch above: the sanitized
            // branch, so `bn orchestrate` can adopt/observe this lane by name.
            let window_name = bn_core::orchestrate::tmux_window_name(&branch);
            tmux_opened = wt::open_worktree_window_cmd_named(
                &repo,
                &sanitized,
                &window_name,
                &wt_path,
                false,
                None,
                false,
            );
        }
    }

    if json {
        let env = Envelope::success(
            "dispatch",
            serde_json::json!({
                "title": title,
                "name": task_name,
                "url": url,
                "repo": repo,
                "branch": branch,
                "base": base,
                "task": if scout { "scout" } else { "ship" },
                "skill": skill_loaded.as_ref().map(|s| s.name.clone()),
                "verify": verify,
                "yolo": yolo,
                "yolo_blocked_external": yolo_blocked_external,
                "launch_yolo": launch_yolo,
                "worktree": wt_path.to_string_lossy(),
                "main_worktree": comparison_path,
                "created": created,
                "pooled": used_pool,
                "tmux_opened": tmux_opened,
                "agent": agent_kind,
                "agent_launched": agent_launched,
                "suppressed_mcps": suppressed_mcps,
                "gh_config_dir": gh_config_dir,
                "source_todo": source_todo,
                "controller_id": controller_id,
            }),
        );
        println!("{}", env.to_json());
        return;
    }

    if created {
        println!("Dispatched → {}", wt_path.display());
    } else {
        println!("Worktree already exists → {}", wt_path.display());
    }
    let task = if scout { "scout" } else { "ship" };
    println!("  repo: {repo}  base: {base}  task: {task}");
    if let Some(controller_id) = &controller_id {
        println!("  controller: {controller_id}");
    }
    if used_pool {
        println!(
            "  pool: leased a warm slot (no cold install) — {}",
            wt_path.display()
        );
    }
    if let Some(main) = &comparison_path {
        println!("  main: paired comparison slot — {}", main.display());
    }
    if let Some(s) = &skill_loaded {
        println!("  skill: {}", s.name);
    }
    println!("  goal: {title}");
    println!("  name: {task_name}");
    if let Some(u) = &url {
        println!("  source: {u}");
    }
    if let Some(id) = &source_todo {
        println!("  closed source todo [{id}]");
    }
    if !suppressed_mcps.is_empty() {
        println!(
            "  suppressed {} project MCP server(s) so Copilot boots clean: {} (keep with --keep-project-mcps)",
            suppressed_mcps.len(),
            suppressed_mcps.join(", ")
        );
    }
    if agent_launched {
        let kind = agent_kind.unwrap_or("agent");
        println!(
            "  launched {kind} in task-named tmux window — \u{f013} working · \u{f0f3} waiting · \u{f00c} done"
        );
    } else if agent_kind.is_some() {
        println!(
            "  agent requested but tmux is unavailable; cd {} and run the agent",
            wt_path.display()
        );
    } else if tmux_opened {
        println!("  task-named tmux window opened");
    } else {
        println!("  cd {}", wt_path.display());
    }
}
