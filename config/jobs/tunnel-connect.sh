#!/bin/sh
# tunnel-connect -- reach machines running tunnel-host over SSH, through their Dev Tunnels.
#
# Run on the machine you connect FROM:  bn run tunnel-connect
# (or directly: sh tunnel-connect.sh [machine]). Pair: tunnel-host.sh on each target.
#
# With no machine named it connects to every bn host on the Dev Tunnels account (tunnels labelled
# bn-host), except this machine. What it does, idempotently:
#   1. picks this machine's SSH key: one already on the trusted GitHub account (see
#      bn_tunnel_github_user), else ~/.ssh/bn_tunnel_ed25519, created and added to that account
#      with `gh ssh-key add` (first time only; needs gh's admin:public_key scope, which it asks
#      for at a terminal). tunnel-host authorizes that account's keys. The key is also published
#      to the synced notes tree, for hosts still on the older key exchange;
#   2. for each host, runs `devtunnel connect` in window `tunnel-<machine>` of the tmux session
#      `bn-tunnels`, forwarding the host's SSH port (derived from its name, read from its tunnel)
#      to the same port on 127.0.0.1 here;
#   3. writes a managed `Host <machine>` block in ~/.ssh/config, so `ssh <machine>` and
#      `bn connect <machine>` work.
# tmux runs tunnel-auto at server start, which re-runs this, so connections come back by themselves.
#
# Env: BN_TUNNEL_TO (only this machine), BN_TUNNEL_USER (remote user, default: yours),
#      BN_TUNNEL_KEY (use this key instead of choosing one), BN_TUNNEL_GITHUB_USER,
#      BN_TUNNEL_KEYS_DIR (default: ssh-keys/ in the git-synced notes tree, see
#      bn_tunnel_keys_dir), BN_TUNNEL_FORWARD_AGENT=1 to add `ForwardAgent yes` (opt-in; see below),
#      BN_TUNNEL_PORTS=3000,3001 to forward those remote ports to the same local ports on every
#      ssh to a host (LocalForward). For forwards without a session, see tunnel-forward.sh.
#      BN_TUNNEL_PERSIST (default 10m): how long the shared connection stays open after its last
#      ssh ends; see step 3.
set -eu

TO="${1:-${BN_TUNNEL_TO:-}}"
PERSIST="${BN_TUNNEL_PERSIST:-10m}"
case "$PERSIST" in ''|*[!0-9smhdwyeno]*) echo "tunnel-connect: bad BN_TUNNEL_PERSIST '$PERSIST' (e.g. 10m, 1h, no)" >&2; exit 2 ;; esac
RUSER="${BN_TUNNEL_USER:-$(id -un)}"
ME="$(hostname -s 2>/dev/null || hostname)"

say() { echo "tunnel-connect: $*"; }
die() { echo "tunnel-connect: $*" >&2; exit 1; }
lower() { printf '%s' "$1" | tr 'A-Z' 'a-z'; }

# devtunnel CLI (installed into ~/bin when missing) and its sign-in: see lib/tunnel.sh.
. "$(dirname "$0")/lib/tunnel.sh"
DT=$(bn_tunnel_devtunnel) || exit 1
bn_tunnel_login "$DT" || exit 1

# --- 1. this machine's key ------------------------------------------------------
GH_USER=$(bn_tunnel_github_user)
KEY="${BN_TUNNEL_KEY:-}"
if [ -z "$KEY" ] && [ -n "$GH_USER" ] && gh_keys=$(bn_tunnel_github_keys "$GH_USER"); then
  for pub in "$HOME"/.ssh/*.pub; do
    [ -f "$pub" ] && [ -f "${pub%.pub}" ] || continue
    printf '%s\n' "$gh_keys" | grep -qF "$(awk '{print $2}' "$pub")" && { KEY="${pub%.pub}"; break; }
  done
  if [ -z "$KEY" ]; then
    KEY="$HOME/.ssh/bn_tunnel_ed25519"
    [ -f "$KEY" ] || ssh-keygen -q -t ed25519 -N '' -C "bn-tunnel $ME" -f "$KEY"
    # gh adds the key to the account it is signed in as, which is GH_USER unless that was set by
    # hand. Adding a key needs the admin:public_key scope, which gh does not ask for by default.
    if gh ssh-key add "$KEY.pub" --title "bn-tunnel $ME" >/dev/null 2>&1; then
      say "added $KEY.pub to github.com/$GH_USER"
    elif bn_tunnel_interactive && gh auth refresh -h github.com -s admin:public_key \
        && gh ssh-key add "$KEY.pub" --title "bn-tunnel $ME" >/dev/null; then
      say "added $KEY.pub to github.com/$GH_USER"
    else
      say "could not add $KEY.pub to GitHub. Run once: gh auth refresh -h github.com -s admin:public_key && gh ssh-key add $KEY.pub --title 'bn-tunnel $ME'"
    fi
  fi
fi
if [ -z "$KEY" ]; then
  # No GitHub account to go by (offline, or gh signed out): the older default.
  KEY="$HOME/.ssh/id_ed25519"
  [ -f "$KEY" ] || ssh-keygen -q -t ed25519 -N '' -f "$KEY"
fi
say "using key $KEY"

# Publish it to the synced notes tree as well, committed and pushed right away: hosts that have
# not re-run tunnel-host since the GitHub-keys change authorize keys from there.
KEYS_DIR=$(bn_tunnel_keys_dir)
mkdir -p "$KEYS_DIR"
if ! cmp -s "$KEY.pub" "$KEYS_DIR/$ME.pub" 2>/dev/null; then
  cp "$KEY.pub" "$KEYS_DIR/$ME.pub"
  say "published $KEY.pub as $KEYS_DIR/$ME.pub"
fi
bn_tunnel_sync_push "$KEYS_DIR" "$KEYS_DIR/$ME.pub"

# --- 2 + 3. connect to each host --------------------------------------------------
SESS=$(bn_tunnel_session) || die "tmux is needed to keep the tunnel connections running"

# ForwardAgent is opt-in. It lets the target use this machine's ssh-agent keys while connected,
# e.g. to reach a GitHub repo only this machine's key can access (the host's notes sync). The key
# never leaves this machine, but anyone with root on the target can use it during the session.
EXTRA=""
[ "${BN_TUNNEL_FORWARD_AGENT:-0}" = 1 ] && EXTRA="
  ForwardAgent yes"
# Port forwards ride every ssh to the host; a port already in use locally is skipped, not fatal.
for p in $(printf '%s' "${BN_TUNNEL_PORTS:-}" | tr ',' ' '); do
  case "$p" in *[!0-9]*|'') say "skipping port '$p' (not a number)"; continue ;; esac
  EXTRA="$EXTRA
  LocalForward $p 127.0.0.1:$p"
done
[ -n "${BN_TUNNEL_PORTS:-}" ] && EXTRA="$EXTRA
  ExitOnForwardFailure no"
# The older default key rides along, so a host that authorized it before the switch still lets
# this machine in until it re-runs tunnel-host.
IDS="  IdentityFile $KEY"
[ "$KEY" = "$HOME/.ssh/id_ed25519" ] || [ ! -f "$HOME/.ssh/id_ed25519" ] || IDS="$IDS
  IdentityFile $HOME/.ssh/id_ed25519"

connect_one() { # <tunnel-id> <machine>
  id=$1; to=$2
  port=$(bn_tunnel_ports "$DT" "$id" | head -n 1)
  [ -n "$port" ] || { say "$to: its tunnel has no port yet (re-run tunnel-host there); skipped"; return 0; }

  # `devtunnel connect` forwards the host's port to the same local port. A window that exists
  # while that port is not listening is stale (the host moved ports) or still starting: restart
  # it either way. Older runs used whichever session they ran in, so look in all of them.
  win="tunnel-$to"
  running=$(tmux list-windows -a -F '#{session_name}:#{window_name}' 2>/dev/null | grep ":$win\$" || true)
  if ss -tln 2>/dev/null | grep -q "127.0.0.1:$port "; then
    say "$to: 127.0.0.1:$port already forwarded"
  else
    for w in $running; do tmux kill-window -t "$w" 2>/dev/null || true; done
    # Restart loop: `devtunnel connect` reconnects on its own through short relay drops, but
    # exits when the host is gone for a while (Cloud PC asleep); come back once it is up again.
    tmux new-window -d -t "$SESS" -n "$win" "while :; do $DT connect $id; echo 'tunnel-connect: devtunnel connect exited; retrying in 10s (Ctrl-C twice to stop)'; sleep 10; done"
    say "$to: connecting in tmux window $SESS:$win (127.0.0.1:$port; retries itself if it drops)"
  fi

  # One shared connection: the first ssh to the host opens a master and every later one
  # (remote-job, tunnel-attach, git, scp) rides it, skipping the Dev Tunnel channel setup and the
  # SSH handshake. %C is a hash of host, port and user, so the socket path stays short and a port
  # change gets a new master. After a drop the master dies with the link (ServerAlive finds a
  # dead link within a minute) and the next ssh opens a new one. Close it: ssh -O exit <machine>.
  cfg="$HOME/.ssh/config"
  touch "$cfg"; chmod 600 "$cfg"
  begin="# >>> bn tunnel $to >>>"; end="# <<< bn tunnel $to <<<"
  tmp=$(mktemp)
  awk -v b="$begin" -v e="$end" '$0==b{skip=1} !skip{print} $0==e{skip=0}' "$cfg" > "$tmp"
  cat >> "$tmp" <<EOF
$begin
Host $to
  HostName 127.0.0.1
  Port $port
  User $RUSER
$IDS
  HostKeyAlias bn-tunnel-$to
  ServerAliveInterval 15
  ServerAliveCountMax 4
  TCPKeepAlive yes
  ControlMaster auto
  ControlPath ~/.ssh/cm-%C
  ControlPersist $PERSIST$EXTRA
$end
EOF
  cat "$tmp" > "$cfg"; rm -f "$tmp"
  say "$to: ssh $to  (or: bn connect $to)"
}

hosts=$(bn_tunnel_hosts "$DT")
found=0
while read -r id name; do
  [ -n "$id" ] || continue
  [ "$(lower "$name")" = "$(lower "$ME")" ] && continue
  [ -z "$TO" ] || [ "$(lower "$name")" = "$(lower "$TO")" ] || continue
  connect_one "$id" "$name"
  found=1
done <<EOF
$hosts
EOF

if [ "$found" = 0 ] && [ -n "$TO" ]; then
  # A host that has not re-run tunnel-host since discovery labels were added: find it by its own
  # label and name it after the request.
  label="bn-host-$(lower "$TO" | tr -c 'a-z0-9-\n' '-')"
  id=$("$DT" list --labels "$label" 2>/dev/null | awk 'NR>1 && $1 ~ /\./ {print $1; exit}')
  [ -n "$id" ] || die "no tunnel for $TO. Run 'bn run tunnel-host' on $TO first (same Dev Tunnels account)."
  connect_one "$id" "$TO"
elif [ "$found" = 0 ]; then
  say "no bn hosts on this Dev Tunnels account yet. Run 'bn run tunnel-host' on the machine to reach."
fi
