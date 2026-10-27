//! Refreshing origin in a bare clone, in a shape a huge monorepo on a flaky link survives.
//!
//! A large monorepo taught every rule here:
//!
//!   * **No refspec** — a bare clone made without `remote.origin.fetch` only writes `FETCH_HEAD`
//!     on a plain `fetch origin`, so `origin/main` never exists. Fetch `main` by name into it.
//!   * **Silence** — `--progress` keeps the server talking while it counts objects. `--quiet`
//!     asked it not to, and ADO dropped the silent connection after ~14 minutes.
//!   * **Stalls** — a pack that stops mid-transfer can't be resumed, so a real stall is retried
//!     from the start ([`bn_core::gitnet::stall_only`] decides what counts as one).
//!   * **Errors** — git's own last words go back to the caller, so a job log says why a fetch
//!     failed instead of "offline?".
//!
//! A clone with a refspec fetches exactly the refs it always did.

use crate::wt;
use std::time::Duration;

/// Tries per fetch, counting the first.
const ATTEMPTS: u32 = 3;
/// Wait before retry `n` (1-based) is `BACKOFF_SECS * n`.
const BACKOFF_SECS: u64 = 30;
fn has_fetch_refspec(bare_s: &str) -> bool {
    wt::git_out(&[
        "--git-dir",
        bare_s,
        "config",
        "--get-all",
        "remote.origin.fetch",
    ])
    .is_some_and(|s| !s.trim().is_empty())
}

const MAIN_SPEC: &str = "+refs/heads/main:refs/remotes/origin/main";

fn is_shallow(bare_s: &str) -> bool {
    wt::git_out(&["--git-dir", bare_s, "rev-parse", "--is-shallow-repository"])
        .is_some_and(|s| s.trim() == "true")
}

fn is_partial(bare_s: &str) -> bool {
    wt::git_out(&[
        "--git-dir",
        bare_s,
        "config",
        "--get",
        "remote.origin.promisor",
    ])
    .is_some_and(|s| s.trim() == "true")
}

/// Bring `origin/main` (on a normal clone: every remote-tracking ref) up to date. `Err` carries
/// git's own last words, for a job log that says why rather than "offline?".
///
/// A clone that is both shallow and blobless (a large monorepo) fetches `main` at `--depth=1
/// --filter=blob:none` first. A plain fetch there negotiates fine, yet ADO still sends every new
/// commit's full tree set: 1.3M objects for nine hours of a monorepo's `main`, which never finished on a
/// link that drops long transfers. The tip alone was 142K objects, 24 MiB, 18s. `main` itself is
/// all the warm and `main-sync` need; the plain fetch stays as the fallback.
pub(crate) fn refresh_origin(bare_s: &str) -> Result<(), String> {
    if is_shallow(bare_s) && is_partial(bare_s) {
        let tip = fetch(
            bare_s,
            &["--depth=1", "--filter=blob:none", "origin", MAIN_SPEC],
        );
        let Err(tip_err) = tip else {
            return Ok(());
        };
        eprintln!("shallow fetch of main failed ({tip_err}) — trying a plain fetch");
        return plain_refresh(bare_s)
            .map_err(|plain_err| format!("{tip_err}; plain fetch: {plain_err}"));
    }
    plain_refresh(bare_s)
}

fn plain_refresh(bare_s: &str) -> Result<(), String> {
    if has_fetch_refspec(bare_s) {
        fetch(bare_s, &["--prune", "origin"])
    } else {
        fetch(bare_s, &["origin", MAIN_SPEC])
    }
}

/// One `git fetch --progress <args>`, retried with backoff while the failure looks like the
/// network rather than the request. Progress is captured and only its tail survives, as the error.
fn fetch(bare_s: &str, args: &[&str]) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        if attempt > 1 {
            std::thread::sleep(Duration::from_secs(BACKOFF_SECS * u64::from(attempt - 1)));
        }
        match fetch_once(bare_s, args) {
            Ok(()) => return Ok(()),
            Err(err) => {
                let retry = attempt < ATTEMPTS && is_transient(&err);
                last = err;
                if !retry {
                    break;
                }
                eprintln!("fetch attempt {attempt}/{ATTEMPTS} failed ({last}) — retrying");
            }
        }
    }
    Err(last)
}

fn fetch_once(bare_s: &str, args: &[&str]) -> Result<(), String> {
    let mut full = vec!["--git-dir", bare_s, "fetch", "--progress"];
    full.extend_from_slice(args);
    let mut cmd = crate::git::git_command(&full);
    bn_core::gitnet::stall_only(&mut cmd);
    match cmd.output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(error_tail(&String::from_utf8_lossy(&o.stderr))),
        Err(e) => Err(e.to_string()),
    }
}

/// git's own complaint from a fetch's stderr. Progress redraws with `\r`, so a captured log is
/// thousands of counter frames; the `fatal:`/`error:` lines are what say why (the first one usually
/// names the actual problem, the rest are git's generic follow-ups). Without any, the last lines.
fn error_tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| !l.starts_with("remote: Found") && !l.contains("objects:"))
        .collect();
    let complaints: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| l.starts_with("fatal:") || l.starts_with("error:"))
        .take(4)
        .collect();
    let picked = if complaints.is_empty() {
        lines[lines.len().saturating_sub(3)..].to_vec()
    } else {
        complaints
    };
    if picked.is_empty() {
        "no error output".to_string()
    } else {
        picked.join(" / ")
    }
}

/// A failure worth trying again: the transfer broke, not the request.
fn is_transient(err: &str) -> bool {
    [
        "curl 18",
        "curl 28",
        "curl 56",
        "RPC failed",
        "early EOF",
        "bad pack header",
        "Connection reset",
        "Connection timed out",
        "unexpected disconnect",
        "Operation too slow",
    ]
    .iter()
    .any(|needle| err.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn git(args: &[&str]) -> bool {
        crate::git::git_ok(args)
    }

    /// A source repo with `n` commits on `main`, and a bare clone of it.
    fn clone(tmp: &Path, n: usize) -> (String, String) {
        let src = tmp.join("src");
        let src_s = src.to_string_lossy().to_string();
        assert!(git(&["init", "-q", "-b", "main", &src_s]));
        for i in 0..n {
            commit(&src_s, &format!("f{i}"));
        }
        let bare_s = tmp.join("bare.git").to_string_lossy().to_string();
        let url = format!("file://{src_s}");
        assert!(git(&["clone", "-q", "--bare", &url, &bare_s]));
        (src_s, bare_s)
    }

    fn commit(src_s: &str, name: &str) -> String {
        std::fs::write(Path::new(src_s).join(name), name).unwrap();
        assert!(git(&["-C", src_s, "add", "-A"]));
        assert!(git(&[
            "-C",
            src_s,
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            name,
        ]));
        wt::git_out(&["-C", src_s, "rev-parse", "HEAD"]).unwrap()
    }

    fn rev(bare_s: &str, r: &str) -> Option<String> {
        wt::git_out(&["--git-dir", bare_s, "rev-parse", "--verify", r])
    }

    #[test]
    fn a_clone_without_a_refspec_gets_origin_main() {
        let _env = bn_core::test_support::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        // `clone --bare` sets no fetch refspec — the shape a monorepo's bare clone has.
        let (src_s, bare_s) = clone(tmp.path(), 2);
        assert!(!has_fetch_refspec(&bare_s));
        let tip = commit(&src_s, "new");

        refresh_origin(&bare_s).expect("fetched");

        assert_eq!(rev(&bare_s, "refs/remotes/origin/main"), Some(tip));
    }

    /// A shallow blobless clone gets main's new tip at depth 1, and stays shallow and blobless.
    #[test]
    fn a_shallow_blobless_clone_fetches_main_at_depth_one() {
        let _env = bn_core::test_support::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let src_s = src.to_string_lossy().to_string();
        assert!(git(&["init", "-q", "-b", "main", &src_s]));
        assert!(git(&[
            "-C",
            &src_s,
            "config",
            "uploadpack.allowFilter",
            "true"
        ]));
        for i in 0..3 {
            commit(&src_s, &format!("f{i}"));
        }
        let bare_s = tmp.path().join("bare.git").to_string_lossy().to_string();
        let url = format!("file://{src_s}");
        assert!(git(&[
            "clone",
            "-q",
            "--bare",
            "--depth=1",
            "--filter=blob:none",
            &url,
            &bare_s,
        ]));
        assert!(is_shallow(&bare_s) && is_partial(&bare_s));
        commit(&src_s, "m1");
        let tip = commit(&src_s, "m2");

        refresh_origin(&bare_s).expect("fetched");

        assert_eq!(rev(&bare_s, "refs/remotes/origin/main"), Some(tip.clone()));
        assert!(is_shallow(&bare_s), "still shallow");
        let parents = wt::git_out(&["--git-dir", &bare_s, "log", "-1", "--format=%P", &tip]);
        assert_eq!(
            parents.as_deref(),
            Some(""),
            "only the tip came down: {parents:?}"
        );
    }

    #[test]
    fn a_normal_clone_still_fetches_every_branch() {
        let _env = bn_core::test_support::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let (src_s, bare_s) = clone(tmp.path(), 2);
        assert!(git(&[
            "--git-dir",
            &bare_s,
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ]));
        assert!(git(&["-C", &src_s, "branch", "other"]));

        refresh_origin(&bare_s).expect("fetched");

        assert!(rev(&bare_s, "refs/remotes/origin/other").is_some());
    }

    #[test]
    fn the_error_is_the_tail_not_the_progress() {
        let stderr = "remote: Found 12 objects to send.\rReceiving objects:  7% (1/12)\r\
                      error: RPC failed; curl 56 Recv failure: Connection reset by peer\n\
                      fatal: early EOF\nfatal: bad pack header\n";
        let tail = error_tail(stderr);
        assert_eq!(
            tail,
            "error: RPC failed; curl 56 Recv failure: Connection reset by peer / fatal: early EOF / fatal: bad pack header"
        );
        assert!(is_transient(&tail));
        assert!(!is_transient(
            "fatal: '/nonexistent/gone.git' does not appear to be a git repository"
        ));
    }
}
