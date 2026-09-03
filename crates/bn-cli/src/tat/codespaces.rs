//! Codespaces for `bn tat` (phase 3c) — GitHub Codespaces as remote worktrees.
//!
//! Split out of `tat.rs` unchanged. A codespace is a (repo, branch) pair, so it slots into tat's
//! model — session = repo, window = "<branch> ☁", remote tmux session = branch. This is the ONLY
//! place tat shells out to `gh`, which is why the cache policy for it lives here too. Everything
//! degrades silently: no gh / not-logged-in / a missing `codespace` scope all yield an empty list
//! (with an actionable auth row), never an error.

use super::{
    cache_home, config_dir, exact_session_target, exact_window_target, goto_session, has_session,
    session_window_target, spawn_remote_warm, window_named_exists,
};
use crate::proc::{tmux_command, tmux_raw, tmux_run};
use crate::wt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

// --- codespaces (3c) -------------------------------------------------------
// GitHub Codespaces as remote worktrees: a codespace is a (repo, branch) pair, so it slots into
// tat's model — session = repo, window = "<branch> ☁", remote tmux session = branch. This is the
// ONLY place tat shells out to `gh`. Everything degrades silently: no gh / not-logged-in / a missing
// `codespace` scope all yield an empty list (with an actionable auth row), never an error.

/// A normalized codespace row: `repo` is the LOCAL clone name (resolved through the repo map).
pub(super) struct Codespace {
    pub(super) repo: String,
    pub(super) branch: String,
    pub(super) state: String,
    pub(super) name: String,
}

/// `$HOME` (the codespace session/window root), or `.` as a last resort.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The cached `gh cs list` JSON path (`$WF_CS_CACHE`, else `<cache>/wt/codespaces.json`).
pub(super) fn cs_cache_path() -> PathBuf {
    std::env::var_os("WF_CS_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| cache_home().join("wt").join("codespaces.json"))
}

/// The auth-marker path (`$WF_CS_AUTH_MARKER`, else `<cache>.auth`): non-empty when `gh cs list`
/// last failed for an auth reason, holding the remedy command.
fn cs_auth_marker_path() -> PathBuf {
    if let Some(p) = std::env::var_os("WF_CS_AUTH_MARKER") {
        return PathBuf::from(p);
    }
    let mut p = cs_cache_path().into_os_string();
    p.push(".auth");
    PathBuf::from(p)
}

/// The codespace gh-account config (`$WF_CS_ACCOUNT_CFG`, else `~/.config/wt/cs-account`).
pub(super) fn cs_account_cfg_path() -> PathBuf {
    std::env::var_os("WF_CS_ACCOUNT_CFG")
        .map(PathBuf::from)
        .unwrap_or_else(|| config_dir().join("wt").join("cs-account"))
}

/// The codespace-repo→local-repo map (`$WF_CS_REPO_MAP`, else `~/.config/wt/cs-repo-map`).
fn cs_repo_map_path() -> PathBuf {
    std::env::var_os("WF_CS_REPO_MAP")
        .map(PathBuf::from)
        .unwrap_or_else(|| config_dir().join("wt").join("cs-repo-map"))
}

/// Resolve a codespace repo basename to its local repo name via the repo map; identity when
/// unmapped or no config. Mirrors `codespaces.sh:wf_cs_repo_alias`.
fn cs_repo_alias(cs: &str) -> String {
    let Ok(content) = std::fs::read_to_string(cs_repo_map_path()) else {
        return cs.to_string();
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k: String = k.chars().filter(|c| !c.is_whitespace()).collect();
        let v: String = v.chars().filter(|c| !c.is_whitespace()).collect();
        if k == cs {
            return if v.is_empty() { cs.to_string() } else { v };
        }
    }
    cs.to_string()
}

/// Is `cmd` runnable (a cheap `--version` probe; the silent-degrade gate for `gh`)?
fn has_cmd(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Raw `gh cs list` JSON for the picker: **whatever is cached**, never a network call.
///
/// `gh cs list` is a ~4s round trip on this network, and the old TTL made the picker pay it on
/// every open more than 60s after the last one — which is every real open. The list is decoration
/// (rows for codespaces with no local clone), so it is not worth a single second of a keystroke's
/// latency, let alone four. Serve the last answer however old it is and hand the refresh to a
/// detached background run; the fresh rows land on the next open, and `ctrl-r` forces one now.
///
/// A cold cache returns `[]` — no codespace rows on the very first open, populated by the time of
/// the second.
pub(super) fn cs_raw() -> String {
    let cached = std::fs::read_to_string(cs_cache_path()).ok();
    if !cs_cache_fresh() {
        spawn_remote_warm(None, false);
    }
    cached.unwrap_or_else(|| "[]".to_string())
}

/// Whether the codespaces cache is inside its TTL (`$WF_CS_TTL`, default 60s).
pub(super) fn cs_cache_fresh() -> bool {
    let ttl: u64 = std::env::var("WF_CS_TTL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    std::fs::metadata(cs_cache_path())
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|m| m.elapsed().ok())
        .map(|age| age.as_secs() < ttl)
        .unwrap_or(false)
}

/// The blocking half of [`cs_raw`]: shell `gh cs list`, write the cache, return the JSON. Only the
/// background warm stage and an explicit `ctrl-r` refresh call this — never an interactive paint.
/// An auth failure is distinguished from "no codespaces": the remedy is stashed in the auth marker
/// and `[]` negative-cached. Mirrors `codespaces.sh`.
pub(super) fn cs_fetch_blocking() -> String {
    let cache = cs_cache_path();
    if let Some(parent) = cache.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let limit = std::env::var("WF_CS_LIMIT")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "100".to_string());
    let out = crate::gh::gh_command(&[
        "cs",
        "list",
        "--limit",
        &limit,
        "--json",
        "name,repository,gitStatus,state,lastUsedAt",
    ])
    .output();
    let marker = cs_auth_marker_path();
    match out {
        Ok(o)
            if o.status.success() && {
                let s = String::from_utf8_lossy(&o.stdout);
                s.trim_start().starts_with('[')
            } =>
        {
            let json = String::from_utf8_lossy(&o.stdout).into_owned();
            let _ = std::fs::write(&cache, &json);
            let _ = std::fs::remove_file(&marker); // auth healthy again
            json
        }
        out => {
            // Failure: classify gh's stderr so the picker can offer the exact fix; negative-cache
            // `[]` so we don't re-shell every open until the TTL.
            let stderr = out
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stderr).to_lowercase())
                .unwrap_or_default();
            let scope_issue = stderr.contains("required scopes")
                || stderr.contains("auth refresh")
                || (stderr.contains("missing") && stderr.contains("scope"));
            let login_issue = stderr.contains("not logged in")
                || stderr.contains("gh auth login")
                || stderr.contains("authentication");
            if scope_issue {
                let _ = std::fs::write(&marker, "gh auth refresh -h github.com -s codespace\n");
            } else if login_issue {
                let _ = std::fs::write(&marker, "gh auth login\n");
            }
            let _ = std::fs::write(&cache, "[]");
            "[]".to_string()
        }
    }
}

/// Normalized codespaces (repo resolved to the local clone name; rows with no repo/branch dropped).
/// Parses `gh cs list --json` natively — no `jq` dependency (the bash oracle required it).
pub(super) fn codespaces() -> Vec<Codespace> {
    parse_codespaces(&cs_raw())
}

/// Parse `gh cs list --json name,repository,gitStatus,state,…` into [`Codespace`] rows. Split out
/// (pure) so it is unit-testable. Mirrors the oracle's `jq … | @tsv | awk '$1!="" && $2!=""'`.
fn parse_codespaces(json: &str) -> Vec<Codespace> {
    let value: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for cs in arr {
        let repo_full = cs.get("repository").and_then(|x| x.as_str()).unwrap_or("");
        let repo_base = repo_full.rsplit('/').next().unwrap_or(repo_full);
        let branch = cs
            .get("gitStatus")
            .and_then(|g| g.get("ref"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let state = cs.get("state").and_then(|x| x.as_str()).unwrap_or("");
        let name = cs.get("name").and_then(|x| x.as_str()).unwrap_or("");
        if repo_base.is_empty() || branch.is_empty() {
            continue;
        }
        out.push(Codespace {
            repo: cs_repo_alias(repo_base),
            branch: branch.to_string(),
            state: state.to_string(),
            name: name.to_string(),
        });
    }
    out
}

/// Codespace rows for a single local repo (by name).
pub(super) fn cs_for_repo(repo: &str) -> Vec<Codespace> {
    codespaces()
        .into_iter()
        .filter(|c| c.repo == repo)
        .collect()
}

/// The tmux window name for a codespace branch: sanitized branch + the ☁ marker (the single source
/// of truth shared by the row display and the open/dedup paths).
pub(super) fn cs_window_name(branch: &str) -> String {
    format!("{} ☁", wt::sanitize_branch(branch))
}

/// The shell command a "<branch> ☁" window runs: ssh into the codespace and attach-or-create its
/// per-branch tmux session (installing tmux first if the codespace lacks it). The branch is quoted
/// for BOTH the remote shell (inner) and the local shell (outer) so a crafted ref can't inject.
fn cs_attach_cmd(name: &str, branch: &str) -> String {
    let b = wt::sanitize_branch(branch);
    let payload = format!(
        "command -v tmux >/dev/null 2>&1 || {{ sudo apt-get update -qq && sudo apt-get install -y -qq tmux; }}; tmux new-session -A -s {}",
        crate::shq(&b)
    );
    format!(
        "gh cs ssh -c {} -- -t {}",
        crate::shq(name),
        crate::shq(&payload)
    )
}

/// Does the cached `gh cs list` indicate an auth problem we can prompt to fix?
pub(super) fn cs_auth_needed() -> bool {
    std::fs::metadata(cs_auth_marker_path())
        .map(|m| m.len() > 0)
        .unwrap_or(false)
}

/// The remedy command gh suggested (e.g. `gh auth refresh -h github.com -s codespace`).
pub(super) fn cs_auth_cmd() -> String {
    std::fs::read_to_string(cs_auth_marker_path())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// The configured codespace gh account (machine-local), or empty if unset.
pub(super) fn cs_account() -> String {
    std::fs::read_to_string(cs_account_cfg_path())
        .ok()
        .and_then(|s| s.lines().next().map(str::to_string))
        .map(|l| l.chars().filter(|c| !c.is_whitespace()).collect())
        .unwrap_or_default()
}

/// Codespace ref helpers. A codespace ref is `cs:<repo>/<branch>/<csname>` — a hidden picker field,
/// NOT a project ref, so it must be intercepted before any `ref_*`/path call. `csname` has no `/`
/// (so it right-splits cleanly) and a branch may itself contain `/`.
pub(super) fn is_cs_ref(s: &str) -> bool {
    s.starts_with("cs:")
}
pub(super) fn cs_ref_csname(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}
pub(super) fn cs_ref_repo(s: &str) -> &str {
    let r = s.strip_prefix("cs:").unwrap_or(s);
    r.split_once('/').map(|(a, _)| a).unwrap_or(r)
}
pub(super) fn cs_ref_branch(s: &str) -> &str {
    let r = s.strip_prefix("cs:").unwrap_or(s);
    let r = r.rsplit_once('/').map(|(a, _)| a).unwrap_or(r); // drop csname
    r.split_once('/').map(|(_, b)| b).unwrap_or("")
}

/// Focus the window named `name` in `session` by its index. The codespace window name carries a
/// space (the ☁ marker), so `select-window -t "sess:name"` would mis-parse — look the index up.
fn cs_focus_window(session: &str, name: &str) {
    let Some(raw) = tmux_raw(&[
        "list-windows",
        "-t",
        &exact_session_target(session),
        "-F",
        "#{window_index}\t#{window_name}",
    ]) else {
        return;
    };
    for line in raw.lines() {
        if let Some((idx, wname)) = line.split_once('\t') {
            if wname == name {
                let _ = tmux_run(&["select-window", "-t", &exact_window_target(session, idx)]);
                return;
            }
        }
    }
}

/// Open a codespace (ref `cs:<repo>/<branch>/<csname>`): ensure the local session `<repo>`, then
/// focus-or-open its "<branch> ☁" window (a `gh cs ssh` attach), and switch to it.
pub(super) fn open_codespace(r: &str) {
    let repo = cs_ref_repo(r);
    let branch = cs_ref_branch(r);
    let csname = cs_ref_csname(r);
    let win = cs_window_name(branch);
    let home = home_dir();
    if !has_session(repo) {
        let _ = tmux_command(&["new-session", "-d", "-s", repo, "-n", "main", "-c"])
            .arg(&home)
            .status();
    }
    if window_named_exists(repo, &win) {
        cs_focus_window(repo, &win);
    } else {
        let _ = tmux_command(&[
            "new-window",
            "-t",
            &session_window_target(repo),
            "-n",
            &win,
            "-c",
        ])
        .arg(&home)
        .arg(cs_attach_cmd(csname, branch))
        .status();
    }
    goto_session(repo);
}

/// Fix codespace auth: first try switching to the configured account (the common cause); only if
/// that doesn't restore `gh cs list` fall back to the interactive scope refresh (browser/device).
pub(super) fn cs_auth_refresh() {
    if !has_cmd("gh") {
        println!("gh not installed");
        return;
    }
    let acct = cs_account();
    if !acct.is_empty() {
        println!("Switching to codespace account: {acct}");
        let _ = crate::gh::gh_ok(&["auth", "switch", "-u", &acct]);
        let _ = std::fs::remove_file(cs_cache_path());
        let _ = std::fs::remove_file(cs_auth_marker_path());
        let healthy = crate::gh::gh_ok(&["cs", "list"]);
        if healthy {
            println!("Done — {acct} has codespace access.");
            return;
        }
        println!("{acct} still lacks the codespace scope — refreshing it.\n");
    }
    let cmd = {
        let c = cs_auth_cmd();
        if c.is_empty() {
            "gh auth refresh -h github.com -s codespace".to_string()
        } else {
            c
        }
    };
    println!("→ {cmd}");
    println!("  Copy the one-time code shown, then approve in the browser that opens");
    println!("  (else open https://github.com/login/device manually).\n");
    let ok = Command::new("sh")
        .args(["-c", &cmd])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        let _ = std::fs::remove_file(cs_auth_marker_path());
        let _ = std::fs::remove_file(cs_cache_path());
        println!("\nDone — codespaces will reappear on the next picker open.");
    } else {
        println!("\nAuth command failed.");
    }
}
