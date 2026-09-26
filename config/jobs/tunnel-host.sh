#!/bin/sh
# tunnel-host -- make this machine reachable over SSH through an authenticated Dev Tunnel.
#
# Run on the machine others connect TO (e.g. a Cloud PC's WSL):  bn run tunnel-host
# Pair: tunnel-connect.sh on the machine you connect FROM.
#
# What it does, idempotently:
#   1. starts a user-level sshd on 127.0.0.1:$BN_TUNNEL_SSH_PORT (key-only, no root daemon);
#   2. authorizes the public keys client machines published to the synced notes tree;
#   3. finds or creates a Dev Tunnel labelled bn-host-<machine> (authenticated, never anonymous)
#      with that port, and hosts it in a tmux window named `tunnel` (foreground outside tmux).
#
# Discovery needs no shared config: both machines sign in to Dev Tunnels with the same account,
# so tunnel-connect finds this tunnel by its label.
#
# Prereqs (once per machine): `sudo apt-get install -y openssh-server` (for the sshd binary; the
# system service is not needed). The devtunnel CLI and its GitHub sign-in are set up on first run.
#
# Env: BN_TUNNEL_NAME (default: short hostname), BN_TUNNEL_SSH_PORT (default 2222),
#      BN_TUNNEL_KEYS_DIR (default: ssh-keys/ in the git-synced notes tree, see
#      bn_tunnel_keys_dir), BN_TUNNEL_FOREGROUND=1 to host in this terminal.
set -eu

NAME="${BN_TUNNEL_NAME:-$(hostname -s 2>/dev/null || hostname)}"
PORT="${BN_TUNNEL_SSH_PORT:-2222}"
LABEL="bn-host-$(printf '%s' "$NAME" | tr 'A-Z' 'a-z' | tr -c 'a-z0-9-\n' '-')"
STATE="$HOME/.ssh/bn-sshd"

say() { echo "tunnel-host: $*"; }
die() { echo "tunnel-host: $*" >&2; exit 1; }

# --- prerequisites -----------------------------------------------------------
SSHD=$(command -v sshd 2>/dev/null || true)
[ -n "$SSHD" ] || [ ! -x /usr/sbin/sshd ] || SSHD=/usr/sbin/sshd
[ -n "$SSHD" ] || die "sshd not found. Install it once: sudo apt-get install -y openssh-server"
# devtunnel CLI (installed into ~/bin when missing) and its sign-in: see lib/tunnel.sh.
. "$(dirname "$0")/lib/tunnel.sh"
DT=$(bn_tunnel_devtunnel) || exit 1
bn_tunnel_login "$DT" || exit 1
KEYS_DIR=$(bn_tunnel_keys_dir)

# --- 1. user-level sshd -------------------------------------------------------
mkdir -p "$STATE" "$HOME/.ssh"
chmod 700 "$HOME/.ssh" "$STATE"
[ -f "$STATE/host_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -f "$STATE/host_ed25519"
cat > "$STATE/sshd_config" <<EOF
# Written by bn tunnel-host. Loopback only: the Dev Tunnel is the one way in.
Port $PORT
ListenAddress 127.0.0.1
HostKey $STATE/host_ed25519
PidFile $STATE/sshd.pid
AuthorizedKeysFile .ssh/authorized_keys
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
PermitRootLogin no
AllowUsers $(id -un)
EOF

if [ -f "$STATE/sshd.pid" ] && kill -0 "$(cat "$STATE/sshd.pid")" 2>/dev/null; then
  say "sshd already running on 127.0.0.1:$PORT (pid $(cat "$STATE/sshd.pid"))"
else
  "$SSHD" -t -f "$STATE/sshd_config" || die "sshd rejected $STATE/sshd_config"
  "$SSHD" -f "$STATE/sshd_config" || die "sshd failed to start on port $PORT"
  say "sshd started on 127.0.0.1:$PORT"
fi

# --- 2. authorize published client keys --------------------------------------
# Public keys only. A client commits and pushes its key as $KEYS_DIR/<machine>.pub; pull first so
# a key published minutes ago is here without waiting for a notes-sync cron.
bn_tunnel_sync_pull "$KEYS_DIR"
touch "$HOME/.ssh/authorized_keys"
chmod 600 "$HOME/.ssh/authorized_keys"
if [ -d "$KEYS_DIR" ]; then
  for k in "$KEYS_DIR"/*.pub; do
    [ -f "$k" ] || continue
    line=$(head -n 1 "$k")
    case "$line" in ssh-ed25519\ *|ssh-rsa\ *|ecdsa-sha2-*) ;; *) say "skipping $k (not a public key)"; continue ;; esac
    grep -qxF "$line" "$HOME/.ssh/authorized_keys" || { printf '%s\n' "$line" >> "$HOME/.ssh/authorized_keys"; say "authorized $(basename "$k")"; }
  done
else
  say "no published client keys yet in $KEYS_DIR (run tunnel-connect on the client, then re-run this)"
fi

# --- 3. find or create the tunnel --------------------------------------------
tunnel_id() { "$DT" list --labels "$LABEL" 2>/dev/null | awk 'NR>1 && $1 ~ /\./ {print $1; exit}'; }
ID=$(tunnel_id)
if [ -z "$ID" ]; then
  "$DT" create --labels "$LABEL" >/dev/null || die "devtunnel create failed"
  ID=$(tunnel_id)
  [ -n "$ID" ] || die "created a tunnel but could not find it by label $LABEL"
  say "created tunnel $ID ($LABEL)"
fi
"$DT" port show "$ID" -p "$PORT" >/dev/null 2>&1 || "$DT" port create "$ID" -p "$PORT" >/dev/null || die "could not add port $PORT to $ID"

# --- host it -------------------------------------------------------------------
if [ -n "${TMUX:-}" ] && [ "${BN_TUNNEL_FOREGROUND:-0}" != 1 ]; then
  sess=$(tmux display-message -p '#S')
  if tmux list-windows -t "$sess" -F '#W' | grep -qx tunnel; then
    say "already hosting in tmux window $sess:tunnel"
  else
    tmux new-window -d -t "$sess" -n tunnel "$DT host $ID; exec \${SHELL:-sh}"
    say "hosting $ID in tmux window $sess:tunnel"
  fi
  say "connect from another machine:  BN_TUNNEL_TO=$NAME bn run tunnel-connect"
else
  say "hosting $ID in the foreground (Ctrl-C stops it)"
  exec "$DT" host "$ID"
fi
