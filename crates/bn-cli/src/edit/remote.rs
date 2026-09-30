//! `bn edit remote <machine>`: this machine's nvim window, that machine's nvim.
//!
//! The editor runs on the remote machine, headless and listening on a socket, in a tmux window
//! there (session `bn-remote-edit`), so its buffers, undo, LSP and git are the remote checkout's
//! and survive any connection. This side forwards the socket over its own ssh connection and runs
//! `nvim --remote-ui` on it. When the UI ends, what happened decides the next step:
//! - the forward died with it: the connection dropped, so connect again (the editor is untouched);
//! - the forward is still up and the remote nvim is gone: you quit (`:qa`), so stop;
//! - the forward is still up and the remote nvim is alive: you detached (`:detach`), so stop.
//!   `bn edit remote` again picks up the same editor.
//!
//! A branch whose worktree already has a `bn edit` nvim on that machine (`@bn_nvim_sock`) reuses
//! it, so the pane there and the window here are one editor.

use std::process::{Command, Stdio};

/// Exit code of the remote script when nvim did not come up on its socket.
const NOSTART: i32 = 4;

/// The script run on the remote machine: find or make the checkout, find a live editor socket for
/// it or start one, and print `SOCK=<path>`.
pub(crate) fn remote_script(root: &str, repo: &str, branch: &str, window: &str) -> String {
    let q = crate::wt::shq;
    format!(
        r#"export PATH="$HOME/.local/bin:$HOME/.bn/bin:$PATH"
root={root}; repo={repo}; branch={branch}; win={win}
{checkout}
alive() {{ [ -S "$1" ] && nvim --server "$1" --remote-expr 1 >/dev/null 2>&1; }}
sock=$(tmux list-windows -a -F '#{{@bn_worktree}}	#{{@bn_nvim_sock}}' 2>/dev/null | awk -F'	' -v w="$wt" '$1 == w && $2 != "" {{print $2; exit}}')
alive "$sock" || sock="/tmp/bn-nvim-$(id -u)-$repo-$win.sock"
if ! alive "$sock"; then
  rm -f "$sock"
  tmux has-session -t '=bn-remote-edit' 2>/dev/null || tmux new-session -d -s bn-remote-edit -n home
  tmux new-window -d -t '=bn-remote-edit:' -n "$repo-$win" -c "$wt" "nvim --headless --listen '$sock'"
  i=0; until alive "$sock" || [ $i -ge 40 ]; do sleep 0.5; i=$((i + 1)); done
  alive "$sock" || {{ echo NOSTART; exit {NOSTART}; }}
fi
echo "SOCK=$sock""#,
        root = q(root),
        repo = q(repo),
        branch = q(branch),
        win = q(window),
        checkout = crate::cmd::agent::remote::checkout_snippet(),
    )
}

/// The local loop: forward the socket, run the UI, and connect again only after a drop.
pub(crate) fn ui_loop(host: &str, remote_sock: &str, local_sock: &str) -> String {
    let q = crate::wt::shq;
    let alive = q(&format!(
        "export PATH=\"$HOME/.local/bin:$PATH\"; nvim --server {} --remote-expr 1",
        q(remote_sock)
    ));
    format!(
        r#"h={h}; r={r}; l={l}
while :; do
  rm -f "$l"
  ssh -N -o ControlPath=none -o ExitOnForwardFailure=yes -o StreamLocalBindUnlink=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -L "$l:$r" "$h" &
  fw=$!
  i=0; until [ -S "$l" ] || [ $i -ge 60 ] || ! kill -0 $fw 2>/dev/null; do sleep 0.25; i=$((i + 1)); done
  [ -S "$l" ] && nvim --remote-ui --server "$l"
  if kill -0 $fw 2>/dev/null; then
    kill $fw 2>/dev/null; wait $fw 2>/dev/null; rm -f "$l"
    if ssh -o BatchMode=yes -o ConnectTimeout=10 "$h" {alive} >/dev/null 2>&1; then
      echo "bn: detached; the editor keeps running on $h (bn edit remote $h to come back)."
    else
      echo "bn: the editor on $h has quit."
    fi
    exit 0
  fi
  wait $fw 2>/dev/null
  echo "bn: lost the connection to $h; the editor is still running there. Reconnecting in 3s (Ctrl-C to stop)."
  sleep 3
done"#,
        h = q(host),
        r = q(remote_sock),
        l = q(local_sock),
    )
}

/// A short local socket path (a unix socket path must stay under 108 bytes).
fn local_socket(host: &str, repo: &str, window: &str) -> String {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|d| std::path::Path::new(d).is_dir())
        .unwrap_or_else(|| "/tmp".to_string());
    let mut name = format!("bn-nvim-{host}-{repo}-{window}");
    name.truncate(60);
    format!("{dir}/{name}.sock")
}

/// Pull the socket path out of what the remote script printed.
pub(crate) fn parse_sock(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("SOCK=").map(str::to_string))
        .filter(|s| s.starts_with('/'))
}

/// `bn edit remote <machine>` from inside a worktree: never returns.
pub(crate) fn run(cfg: &bn_core::resolver::Config, host: &str) -> ! {
    if !crate::cmd::agent::safe_ssh_host(host) {
        eprintln!("bn edit remote: '{host}' is not an ssh host alias");
        std::process::exit(2);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let ctx = match bn_core::resolver::resolve(cfg, &cwd) {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("bn edit remote: run it inside a worktree ({e:?})");
            std::process::exit(2);
        }
    };
    let root = cfg
        .hosts
        .get(host)
        .and_then(|e| e.repo_root.clone())
        .unwrap_or_else(|| cfg.worktree_dir.to_string_lossy().into_owned());
    let window = bn_core::orchestrate::tmux_window_name(&ctx.branch);
    let script = remote_script(&root, &ctx.repo, &ctx.branch, &window);
    let out = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15"])
        .args(crate::cmd::agent::SSH_SHARED_CONNECTION)
        .args([host, &format!("sh -c {}", crate::wt::shq(&script))])
        .stdin(Stdio::null())
        .output();
    let (code, stdout) = match out {
        Ok(o) => (
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
        ),
        Err(e) => {
            eprintln!("bn edit remote: ssh: {e}");
            std::process::exit(1);
        }
    };
    let Some(remote_sock) = parse_sock(&stdout) else {
        let why = match code {
            Some(3) => format!(
                "{} has no checkout on {host}, and none could be made (push it first)",
                ctx.branch
            ),
            Some(NOSTART) => {
                format!("nvim did not start on {host}; try `ssh {host} nvim --headless` to see why")
            }
            Some(255) => format!("could not reach {host} over ssh"),
            c => format!("the remote step failed (exit {c:?})"),
        };
        eprintln!("bn edit remote: {why}");
        std::process::exit(1);
    };
    let local_sock = local_socket(host, &ctx.repo, &window);
    let err = {
        use std::os::unix::process::CommandExt;
        Command::new("sh")
            .args(["-c", &ui_loop(host, &remote_sock, &local_sock)])
            .exec()
    };
    eprintln!("bn edit remote: {err}");
    std::process::exit(127);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_remote_script_reuses_a_live_socket_before_starting_one() {
        let s = remote_script("/w", "bn", "feat/x", "feat-x");
        let reuse = s.find("@bn_nvim_sock").unwrap();
        let start = s.find("nvim --headless --listen").unwrap();
        assert!(reuse < start, "an existing editor is looked for first");
        assert!(s.contains("bn wt add"), "a missing checkout is made");
        assert!(s.contains("echo \"SOCK=$sock\""));
    }

    #[test]
    fn the_ui_loop_stops_on_quit_or_detach_and_retries_only_after_a_drop() {
        let l = ui_loop("CPC-a", "/tmp/r.sock", "/tmp/l.sock");
        assert!(l.contains("-L \"$l:$r\""));
        assert!(l.contains("StreamLocalBindUnlink=yes"));
        assert!(l.contains("nvim --remote-ui --server \"$l\""));
        // Forward still up after the UI ends = the user ended it: exit either way.
        assert!(l.contains("if kill -0 $fw"));
        assert!(l.contains("exit 0"));
        assert!(l.contains("Reconnecting in 3s"));
    }

    #[test]
    fn the_socket_path_is_read_from_the_last_sock_line() {
        assert_eq!(
            parse_sock("motd\nSOCK=/tmp/a.sock\n"),
            Some("/tmp/a.sock".to_string())
        );
        assert_eq!(parse_sock("SOCK=relative"), None);
        assert_eq!(parse_sock("NOSTART"), None);
    }

    #[test]
    fn the_local_socket_stays_short() {
        let p = local_socket(&"h".repeat(80), "bn", "branch");
        assert!(p.len() < 108, "{p}");
    }
}
