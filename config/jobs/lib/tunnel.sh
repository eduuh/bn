# Sourced helper for tunnel-host.sh / tunnel-connect.sh: find or install the devtunnel CLI, make
# sure it is signed in, and move client public keys through the synced notes tree.
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

# bn_tunnel_keys_dir -- echo the directory client public keys are exchanged through.
# It must live in a notes tree that actually syncs between machines, i.e. one inside a git repo:
# on this setup ~/projects/branch-notes is machine-local, while ~/projects/branch-notes-work is the
# git-synced tree (2026-09-26). Order: $BN_TUNNEL_KEYS_DIR, $BN_NOTES_ROOT, then the first of the
# usual roots that is inside a git work tree. Falls back to ~/projects/branch-notes (unsynced).
bn_tunnel_keys_dir() {
  if [ -n "${BN_TUNNEL_KEYS_DIR:-}" ]; then echo "$BN_TUNNEL_KEYS_DIR"; return 0; fi
  if [ -n "${BN_NOTES_ROOT:-}" ]; then echo "$BN_NOTES_ROOT/ssh-keys"; return 0; fi
  for _root in "$HOME/projects/branch-notes-work/bn" "$HOME/projects/branch-notes"; do
    [ -d "$_root" ] || continue
    git -C "$_root" rev-parse --is-inside-work-tree >/dev/null 2>&1 && { echo "$_root/ssh-keys"; return 0; }
  done
  echo "tunnel: no git-synced notes tree found; keys stay local in ~/projects/branch-notes/ssh-keys" >&2
  echo "$HOME/projects/branch-notes/ssh-keys"
}

# bn_tunnel_sync_pull <keys_dir> -- fetch keys other machines published (fast-forward only).
# Runs in the keys dir's parent: on a fresh host ssh-keys/ does not exist until this pull brings it.
bn_tunnel_sync_pull() {
  _repo=$(dirname "$1")
  [ -d "$_repo" ] || return 0
  git -C "$_repo" rev-parse --is-inside-work-tree >/dev/null 2>&1 || return 0
  git -C "$_repo" pull --ff-only --quiet >/dev/null 2>&1 \
    || echo "tunnel: could not pull the notes repo; using the keys already here" >&2
}

# bn_tunnel_sync_push <keys_dir> <file> -- commit and push one published PUBLIC key, so the host
# picks it up without waiting for the commit-notes cron (which may be off on that machine).
bn_tunnel_sync_push() {
  git -C "$1" rev-parse --is-inside-work-tree >/dev/null 2>&1 || return 0
  git -C "$1" add -- "$2" >/dev/null 2>&1 || return 0
  git -C "$1" diff --cached --quiet -- "$2" 2>/dev/null && return 0
  if git -C "$1" commit --quiet -m "ssh-keys: publish $(basename "$2")" -- "$2" >/dev/null 2>&1; then
    git -C "$1" push --quiet >/dev/null 2>&1 \
      && echo "tunnel: pushed $(basename "$2") to the notes repo" >&2 \
      || echo "tunnel: committed $(basename "$2") but could not push; the next notes sync will" >&2
  fi
}

# bn_tunnel_session -- echo the tmux session tunnel windows live in, creating it detached when
# missing. A dedicated session, not whichever one the caller is in: killing a work session (or a
# `tmux kill-server` and restart) must not strand a tunnel, and tunnel-auto can find the windows.
bn_tunnel_session() {
  command -v tmux >/dev/null 2>&1 || return 1
  tmux has-session -t bn-tunnels 2>/dev/null || tmux new-session -d -s bn-tunnels -n home || return 1
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
bn_tunnel_hosts() {
  "$1" list --labels bn-host -j 2>/dev/null | awk -F'"' '
    /"tunnelId"/ { id = $4 }
    /"description"/ { if (id != "" && $4 != "") print id, $4; id = "" }'
}

# bn_tunnel_ports <devtunnel> <tunnel-id> -- the ports the tunnel forwards, one per line. A bn
# host tunnel carries exactly one: the host's SSH port.
bn_tunnel_ports() {
  "$1" port list "$2" -j 2>/dev/null | awk -F: '/"portNumber"/ { gsub(/[ ,]/, "", $2); print $2 }'
}
