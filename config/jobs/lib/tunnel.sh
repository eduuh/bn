# Sourced helper for tunnel-host.sh / tunnel-connect.sh: find or install the devtunnel CLI, make
# sure it is signed in, keep the tunnel tmux session, and look up hosts, ports and GitHub keys.
# Source it from a job:  . "$(dirname "$0")/lib/tunnel.sh"
#
# POSIX sh; never run directly. User-level only: the CLI goes into ~/bin and nothing needs sudo.
# The host's openssh-server stays a one-time manual install (tunnel-host says so when missing).
#
# Interactive vs unattended: the device-code sign-in needs a human, so it runs only when stdin is
# a terminal. Under cron or a pipe it fails fast with the command to run instead of hanging.

bn_tunnel_interactive() { [ -t 0 ]; }

# bn_tunnel_devtunnel -- echo the path of a working devtunnel CLI, downloading it into ~/bin when
# missing. The binary is fetched directly and made executable here: the official installer script
# ends with a sudo step, which failed unattended and left ~/bin/devtunnel non-executable
# (2026-09-26).
bn_tunnel_devtunnel() {
  _dt=$(command -v devtunnel 2>/dev/null || true)
  [ -n "$_dt" ] || [ ! -x "$HOME/bin/devtunnel" ] || _dt="$HOME/bin/devtunnel"
  [ -n "$_dt" ] && { echo "$_dt"; return 0; }

  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) _asset=linux-x64-devtunnel ;;
    Linux-aarch64|Linux-arm64) _asset=linux-arm64-devtunnel ;;
    Darwin-x86_64) _asset=osx-x64-devtunnel ;;
    Darwin-arm64) _asset=osx-arm64-devtunnel ;;
    *) echo "tunnel: no devtunnel build known for $(uname -s)-$(uname -m); install it by hand" >&2; return 1 ;;
  esac
  command -v curl >/dev/null 2>&1 || { echo "tunnel: curl is required to download devtunnel" >&2; return 1; }
  echo "tunnel: downloading the devtunnel CLI ($_asset) into ~/bin ..." >&2
  mkdir -p "$HOME/bin"
  if curl -fsSL -o "$HOME/bin/devtunnel.part" "https://tunnelsassetsprod.blob.core.windows.net/cli/$_asset"; then
    chmod +x "$HOME/bin/devtunnel.part" && mv "$HOME/bin/devtunnel.part" "$HOME/bin/devtunnel"
  else
    rm -f "$HOME/bin/devtunnel.part"
    echo "tunnel: devtunnel download failed" >&2
    return 1
  fi
  "$HOME/bin/devtunnel" --version >/dev/null 2>&1 || { echo "tunnel: downloaded devtunnel does not run" >&2; return 1; }
  echo "$HOME/bin/devtunnel"
}

# bn_tunnel_signed_in <devtunnel> -- true only when `user show` reports a signed-in account.
# Its exit code is not enough: signed out, it still exits 0 ("Not logged in"), and the next
# `devtunnel create` then fails as Anonymous (seen 2026-09-26 on cloud-pc-AB12C).
bn_tunnel_signed_in() {
  "$1" user show 2>/dev/null | grep -qi 'logged in as'
}

# bn_tunnel_login <devtunnel> -- make sure the CLI is signed in; at a terminal, start a
# device-code sign-in. GitHub by default (-g): a corporate Microsoft account may be blocked from
# Dev Tunnels. BN_TUNNEL_LOGIN=microsoft switches to a Microsoft account. Every machine must sign
# in with the SAME account: tunnel-connect finds the host's tunnel by label within that account.
bn_tunnel_login() {
  bn_tunnel_signed_in "$1" && return 0
  case "${BN_TUNNEL_LOGIN:-github}" in
    microsoft) _how="-d" ;;
    *) _how="-g -d" ;;
  esac
  if bn_tunnel_interactive; then
    echo "tunnel: signing in to Dev Tunnels with ${BN_TUNNEL_LOGIN:-github} (use the same account on every machine) ..." >&2
    # shellcheck disable=SC2086 -- $_how is one or two flags, split on purpose
    "$1" user login $_how >&2 && bn_tunnel_signed_in "$1" && return 0
  fi
  echo "tunnel: not signed in to Dev Tunnels. Run: $1 user login $_how" >&2
  return 1
}

# bn_tunnel_session -- echo the tmux session tunnel windows live in, creating it detached when
# missing. A dedicated session, not whichever one the caller is in: killing a work session (or a
# `tmux kill-server` and restart) must not strand a tunnel, and tunnel-auto can find the windows.
bn_tunnel_session() {
  command -v tmux >/dev/null 2>&1 || return 1
  tmux has-session -t bn-tunnels 2>/dev/null && { echo bn-tunnels; return 0; }
  # Started from a systemd unit (the tunnel-auto timer after a reboot, before anyone opened tmux),
  # a tmux server started here would live in the unit's cgroup and be killed when the job exits.
  # A scope of its own keeps it: `--scope` runs tmux in this environment under a new transient unit.
  if [ -n "${INVOCATION_ID:-}" ] && ! tmux list-sessions >/dev/null 2>&1 && command -v systemd-run >/dev/null 2>&1; then
    systemd-run --user --scope --quiet --collect --unit="bn-tmux-$$" tmux new-session -d -s bn-tunnels -n home >/dev/null 2>&1 || return 1
  else
    tmux new-session -d -s bn-tunnels -n home || return 1
  fi
  echo bn-tunnels
}

# bn_tunnel_port_for <machine> -- the SSH port a host uses, derived from its name so hosts never
# clash on a client (devtunnel connect forwards each port to the same local port and cannot remap).
# 2300-2999, stable for a name; BN_TUNNEL_SSH_PORT overrides it on the host.
bn_tunnel_port_for() {
  _sum=$(printf '%s' "$1" | tr 'A-Z' 'a-z' | cksum | cut -d' ' -f1)
  echo $((2300 + _sum % 700))
}

# bn_tunnel_github_user -- the GitHub account whose public keys may SSH in (tunnel-host) and that a
# client's key must belong to (tunnel-connect). BN_TUNNEL_GITHUB_USER, else `git config
# bn.tunnelGithubUser`, else the account gh is signed in as. Empty when none can be found.
bn_tunnel_github_user() {
  [ -n "${BN_TUNNEL_GITHUB_USER:-}" ] && { echo "$BN_TUNNEL_GITHUB_USER"; return 0; }
  _u=$(git config --global bn.tunnelGithubUser 2>/dev/null || true)
  [ -n "$_u" ] || _u=$(gh api user --jq .login 2>/dev/null || true)
  echo "$_u"
}

# bn_tunnel_github_keys <user> -- that account's public SSH keys, one per line (public endpoint,
# no sign-in). Fails when GitHub cannot be reached, so a caller never mistakes "offline" for "none".
bn_tunnel_github_keys() {
  curl -fsS --max-time 20 "https://github.com/$1.keys" 2>/dev/null | grep -E '^(ssh-ed25519|ssh-rsa|ecdsa-sha2-)'
}

# bn_tunnel_hosts <devtunnel> -- every bn host tunnel on this account, one "<tunnel-id> <machine>"
# per line. The machine name rides in the tunnel description (labels are lowercased).
# Fails when the account could not be listed, so a caller never mistakes "offline" for "no hosts".
bn_tunnel_hosts() {
  _list=$("$1" list --labels bn-host -j 2>/dev/null) || return 1
  printf '%s\n' "$_list" | grep -q '"tunnels"' || return 1
  printf '%s\n' "$_list" | awk -F'"' '
    /"tunnelId"/ { id = $4 }
    /"description"/ { if (id != "" && $4 != "") print id, $4; id = "" }'
}

# bn_tunnel_lan_ip -- this machine's address on the local network: the source address of its
# default route (`ip` on Linux; on macOS, the address of the default route's interface). Empty
# when there is none.
bn_tunnel_lan_ip() {
  if command -v ip >/dev/null 2>&1; then
    ip -4 route get 1.1.1.1 2>/dev/null | awk '{for (i = 1; i < NF; i++) if ($i == "src") {print $(i + 1); exit}}'
  elif command -v ipconfig >/dev/null 2>&1; then
    _if=$(route -n get default 2>/dev/null | awk '$1 == "interface:" {print $2; exit}')
    [ -z "$_if" ] || ipconfig getifaddr "$_if" 2>/dev/null
  fi
}

# Portability. macOS has no `ss` and no `timeout`, and without these two helpers every health
# check there failed, so a working Mac host would always have shown as down.

# bn_tunnel_listening <port> [address...] -- something listens on TCP <port> at one of the
# addresses (default 127.0.0.1 and 0.0.0.0; a wildcard listener counts as 0.0.0.0). `ss` on Linux,
# `netstat` elsewhere. With neither there is no way to tell, and it answers yes: a check that
# cannot look must not report a working host as down.
bn_tunnel_listening() {
  _p=$1; shift
  [ $# -gt 0 ] || set -- 127.0.0.1 0.0.0.0
  if command -v ss >/dev/null 2>&1; then
    _l=$(ss -tln 2>/dev/null | awk 'NR > 1 {print $4}')
  elif command -v netstat >/dev/null 2>&1; then
    # Linux: 127.0.0.1:2400 / 0.0.0.0:2400; macOS: 127.0.0.1.2400 / *.2400.
    _l=$(netstat -an 2>/dev/null | awk '/LISTEN/ && $1 ~ /^tcp/ {print $4}')
  else
    return 0
  fi
  printf '%s\n' "$_l" | awk -v p="$_p" -v want=" $* " '
    { n = match($0, /[:.][0-9]+$/); if (!n) next
      port = substr($0, n + 1); host = substr($0, 1, n - 1)
      if (host == "*") host = "0.0.0.0"
      if (port == p && index(want, " " host " ")) f = 1 }
    END { exit !f }'
}

# bn_tunnel_timeout <seconds> <command...> -- run it, killed after <seconds>. `timeout` (Linux),
# `gtimeout` (Homebrew coreutils), else perl's alarm, which macOS always has.
bn_tunnel_timeout() {
  if command -v timeout >/dev/null 2>&1; then timeout "$@"
  elif command -v gtimeout >/dev/null 2>&1; then gtimeout "$@"
  elif command -v perl >/dev/null 2>&1; then perl -e 'alarm shift; exec @ARGV or exit 127' "$@"
  else shift; "$@"; fi
}

# bn_tunnel_lan_labels <devtunnel> <tunnel-id> -- the tunnel's bn-lan-* labels, one per line. A host
# in local network mode publishes its address as one (a label cannot hold dots):
# bn-lan-192-168-1-20.
bn_tunnel_lan_labels() {
  "$1" show "$2" -j 2>/dev/null | awk -F'"' '$2 ~ /^bn-lan-[0-9-]+$/ {print $2}'
}

# Machine reports. Each machine publishes a few lines about its own tunnel state (tunnel-auto) as
# a one-commit ref, refs/heads/bn-status/<machine>, in the synced work-notes repo, and every other
# machine can read them (bn tunnels). Git rather than the Dev Tunnels account because the failure
# worth reporting most (devtunnel signed out) is the one that stops a machine writing to the
# account; and a machine that is off still has its last report, with its time.

# bn_tunnel_notes_git -- the git work tree of the work-notes root (bn's notes_roots()), or fail.
bn_tunnel_notes_git() {
  if [ -n "${BN_NOTES_DIR:-}" ]; then _r="${BN_NOTES_DIR_WORK:-$BN_NOTES_DIR}"
  elif [ -d "$HOME/projects/notes/bn" ]; then _r="$HOME/projects/notes/bn"
  else _r="$HOME/projects/branch-notes-work/bn"; fi
  [ -d "$_r" ] && git -C "$_r" rev-parse --show-toplevel 2>/dev/null
}

# bn_tunnel_git <dir> <git args...> -- git over the network that never waits for a person.
bn_tunnel_git() {
  _d=$1; shift
  GIT_TERMINAL_PROMPT=0 GCM_INTERACTIVE=never GIT_SSH_COMMAND="ssh -o BatchMode=yes -o ConnectTimeout=15" \
    bn_tunnel_timeout 60 git -C "$_d" "$@"
}

# bn_tunnel_report_push <machine> <file> -- publish <file> as that machine's report. Plumbing only,
# so the notes checkout and index are never touched. The ref belongs to that one machine and holds
# one commit, replaced each time (forced), so it never grows a history.
bn_tunnel_report_push() {
  _g=$(bn_tunnel_notes_git) || return 1
  _b=$(git -C "$_g" hash-object -w "$2") || return 1
  _t=$(printf '100644 blob %s\ttunnels.status\n' "$_b" | git -C "$_g" mktree) || return 1
  _c=$(GIT_AUTHOR_NAME=bn GIT_AUTHOR_EMAIL=bn@localhost GIT_COMMITTER_NAME=bn GIT_COMMITTER_EMAIL=bn@localhost \
    git -C "$_g" commit-tree "$_t" -m "tunnel report: $1") || return 1
  bn_tunnel_git "$_g" push -q --force origin "${_c}:refs/heads/bn-status/$1"
}

# bn_tunnel_reports_fetch -- bring every machine's report into refs/bn-status/* (outside the
# branches, so nothing else sees them); echoes the notes work tree.
bn_tunnel_reports_fetch() {
  _g=$(bn_tunnel_notes_git) || return 1
  bn_tunnel_git "$_g" fetch -q --prune origin '+refs/heads/bn-status/*:refs/bn-status/*' || return 1
  echo "$_g"
}

# bn_tunnel_ports <devtunnel> <tunnel-id> -- the ports the tunnel forwards, one per line. A bn
# host tunnel carries exactly one: the host's SSH port.
bn_tunnel_ports() {
  "$1" port list "$2" -j 2>/dev/null | awk -F: '/"portNumber"/ { gsub(/[ ,]/, "", $2); print $2 }'
}
