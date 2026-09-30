//! `bn plan view [--host <machine>] [--port <n>]`: see a plan's HTML here, wherever it was made.
//!
//! An agent drafts plans as HTML in its branch note folder. On another machine that folder is out
//! of reach of this machine's browser, and forwarding a port is overkill for a static file. This
//! copies the newest `*.html` from the branch's note folder (on `--host`, over the shared ssh
//! connection) and opens it in the local browser. `--port <n>` is for a live page instead (a Lavish
//! session, a dev server): it forwards that port from the machine and opens
//! `http://localhost:<n>`, forwarding again after a drop until Ctrl-C.

use bn_core::resolver::{Config, Resolved};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Exit code of the remote script when the branch has no plan HTML there.
const NOPLAN: i32 = 5;

/// Newest `*.html` at most three levels under `dir`.
fn newest_html(dir: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path, depth: u8, best: &mut Option<(std::time::SystemTime, PathBuf)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() && depth < 3 {
                walk(&p, depth + 1, best);
            } else if p.extension().is_some_and(|x| x == "html") {
                if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                    if best.as_ref().is_none_or(|(t, _)| m > *t) {
                        *best = Some((m, p));
                    }
                }
            }
        }
    }
    let mut best = None;
    walk(dir, 0, &mut best);
    best.map(|(_, p)| p)
}

/// The remote script: print `FILE=<path>` and then the newest plan HTML of the branch there.
pub(crate) fn remote_script(root: &str, repo: &str, branch: &str) -> String {
    let q = crate::wt::shq;
    format!(
        r#"export PATH="$HOME/.local/bin:$HOME/.bn/bin:$PATH"
root={root}; repo={repo}; branch={branch}
{checkout}
d=$(cd "$wt" && bn --path 2>/dev/null)
f=$(find "$d" -maxdepth 3 -type f -name '*.html' -exec ls -t {{}} + 2>/dev/null | head -n 1)
[ -n "$f" ] || {{ echo NOPLAN; exit {NOPLAN}; }}
printf 'FILE=%s\n' "$f"
cat "$f""#,
        root = q(root),
        repo = q(repo),
        branch = q(branch),
        checkout = crate::cmd::agent::remote::checkout_snippet(),
    )
}

/// Split the remote output into the file name and its contents.
pub(crate) fn split_remote(out: &[u8]) -> Option<(String, Vec<u8>)> {
    let nl = out.iter().position(|&b| b == b'\n')?;
    let first = std::str::from_utf8(&out[..nl]).ok()?;
    let name = first.strip_prefix("FILE=")?;
    let base = Path::new(name).file_name()?.to_string_lossy().into_owned();
    Some((base, out[nl + 1..].to_vec()))
}

/// Open a file or URL in this machine's browser: the Windows one from WSL, else `open` / `xdg-open`.
fn open_here(target: &str) -> bool {
    let wsl = std::fs::read_to_string("/proc/version").is_ok_and(|v| v.to_lowercase().contains("microsoft"));
    let run = |bin: &str, arg: &str| {
        Command::new(bin)
            .arg(arg)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    };
    if wsl {
        if run("wslview", target) {
            return true;
        }
        // explorer.exe needs a Windows path for a file; a URL goes as it is.
        let arg = if target.contains("://") {
            target.to_string()
        } else {
            Command::new("wslpath")
                .args(["-w", target])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_else(|| target.to_string())
        };
        // explorer.exe exits 1 even when it opened the page, so only a failure to start counts.
        return run("explorer.exe", &arg);
    }
    if cfg!(target_os = "macos") {
        return run("open", target);
    }
    run("xdg-open", target)
}

fn ssh_capture(host: &str, script: &str) -> std::io::Result<std::process::Output> {
    Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15"])
        .args(crate::cmd::agent::SSH_SHARED_CONNECTION)
        .args([host, &format!("sh -c {}", crate::wt::shq(script))])
        .stdin(Stdio::null())
        .output()
}

fn fail(msg: &str) -> ! {
    eprintln!("bn plan view: {msg}");
    std::process::exit(1);
}

/// Forward `port` from `host` until Ctrl-C, opening the page once it answers.
fn forward_port(host: &str, port: u16) -> ! {
    let url = format!("http://localhost:{port}");
    let mut opened = false;
    loop {
        let child = Command::new("ssh")
            .args(["-N", "-o", "ControlPath=none", "-o", "ExitOnForwardFailure=yes"])
            .args(["-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=4"])
            .args(["-L", &format!("{port}:127.0.0.1:{port}"), host])
            .stdin(Stdio::null())
            .spawn();
        let Ok(mut child) = child else { fail("could not start ssh") };
        if !opened {
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline
                && std::net::TcpStream::connect(("127.0.0.1", port)).is_err()
            {
                std::thread::sleep(Duration::from_millis(250));
            }
            opened = open_here(&url);
            eprintln!("bn plan view: {url} forwarded from {host}; Ctrl-C stops it");
        }
        let status = child.wait();
        eprintln!(
            "bn plan view: the forward to {host}:{port} ended ({status:?}); forwarding again in 3s (Ctrl-C to stop)"
        );
        std::thread::sleep(Duration::from_secs(3));
    }
}

pub(crate) fn run(cfg: &Config, ctx: &Resolved, host: Option<&str>, port: Option<u16>) -> ! {
    if let Some(h) = host {
        if !crate::cmd::agent::safe_ssh_host(h) {
            fail(&format!("'{h}' is not an ssh host alias"));
        }
    }
    match (host, port) {
        (Some(h), Some(p)) => forward_port(h, p),
        (None, Some(p)) => {
            open_here(&format!("http://localhost:{p}"));
            std::process::exit(0);
        }
        (None, None) => {
            let Some(file) = newest_html(&cfg.note_dir(ctx)) else {
                fail("no plan HTML in this branch's note folder yet");
            };
            println!("{}", file.display());
            std::process::exit(if open_here(&file.to_string_lossy()) { 0 } else { 1 });
        }
        (Some(h), None) => {
            let root = cfg
                .hosts
                .get(h)
                .and_then(|e| e.repo_root.clone())
                .unwrap_or_else(|| cfg.worktree_dir.to_string_lossy().into_owned());
            let out = ssh_capture(h, &remote_script(&root, &ctx.repo, &ctx.branch))
                .unwrap_or_else(|e| fail(&format!("ssh: {e}")));
            let Some((name, body)) = split_remote(&out.stdout) else {
                match out.status.code() {
                    Some(NOPLAN) => fail(&format!("{} has no plan HTML on {h} yet", ctx.branch)),
                    Some(3) => fail(&format!("{} has no checkout on {h}", ctx.branch)),
                    Some(255) => fail(&format!("could not reach {h} over ssh")),
                    c => fail(&format!("the remote step failed (exit {c:?})")),
                }
            };
            let dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .filter(|d| Path::new(d).is_dir())
                .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
            let local = Path::new(&dir).join(format!("bn-plan-{h}-{name}"));
            if let Err(e) = std::fs::write(&local, body) {
                fail(&format!("writing {}: {e}", local.display()));
            }
            println!("{}", local.display());
            std::process::exit(if open_here(&local.to_string_lossy()) { 0 } else { 1 });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_remote_output_splits_into_name_and_body() {
        let (name, body) = split_remote(b"FILE=/n/plans/a.html\n<html>x</html>\n").unwrap();
        assert_eq!(name, "a.html");
        assert_eq!(body, b"<html>x</html>\n");
        assert!(split_remote(b"NOPLAN\n").is_none());
    }

    #[test]
    fn the_newest_html_wins_and_other_files_are_ignored() {
        let dir = std::env::temp_dir().join(format!("bn-plan-view-{}", std::process::id()));
        let sub = dir.join("plans");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.join("old.html"), "a").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(sub.join("new.html"), "b").unwrap();
        std::fs::write(sub.join("notes.md"), "c").unwrap();
        assert_eq!(newest_html(&dir), Some(sub.join("new.html")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_remote_script_looks_in_the_note_folder() {
        let s = remote_script("/w", "bn", "feat");
        assert!(s.contains("bn --path"));
        assert!(s.contains("-name '*.html'"));
        assert!(s.contains("printf 'FILE=%s\\n'"));
    }
}
