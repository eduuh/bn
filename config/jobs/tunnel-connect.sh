#!/bin/sh
# tunnel-connect -- reach a machine running tunnel-host over SSH, through its Dev Tunnel.
#
# Run on the machine you connect FROM:  BN_TUNNEL_TO=<machine> bn run tunnel-connect
# (or directly: sh tunnel-connect.sh <machine>). Pair: tunnel-host.sh on the target.
#
# What it does, idempotently:
#   1. publishes this machine's PUBLIC key to the synced notes tree, so tunnel-host authorizes it;
#   2. finds the target's tunnel by its label bn-host-<machine> (same Dev Tunnels account);
#   3. runs `devtunnel connect` in a tmux window `tunnel-<machine>`, forwarding the target's SSH
#      port to 127.0.0.1:<port> here;
#   4. writes a managed `Host <machine>` block in ~/.ssh/config, so `ssh <machine>` and
#      `bn connect <machine>` work.
#
# Env: BN_TUNNEL_TO (target machine name), BN_TUNNEL_SSH_PORT (default 2222; must match the host),
#      BN_TUNNEL_USER (remote user, default: yours), BN_TUNNEL_KEY (default ~/.ssh/id_ed25519),
#      BN_TUNNEL_KEYS_DIR (default: ssh-keys/ in the git-synced notes tree, see
#      bn_tunnel_keys_dir), BN_TUNNEL_FORWARD_AGENT=1 to add `ForwardAgent yes` (opt-in; see below),
#      BN_TUNNEL_PORTS=3000,3001 to forward those remote ports to the same local ports on every
#      ssh to <machine> (LocalForward). For forwards without a session, see tunnel-forward.sh.
#      BN_TUNNEL_PERSIST (default 10m): how long the shared connection stays open after its last
#      ssh ends; see step 4.
set -eu

TO="${1:-${BN_TUNNEL_TO:-}}"
[ -n "$TO" ] || { echo "tunnel-connect: which machine? BN_TUNNEL_TO=<machine> bn run tunnel-connect" >&2; exit 2; }
PORT="${BN_TUNNEL_SSH_PORT:-2222}"
PERSIST="${BN_TUNNEL_PERSIST:-10m}"
case "$PERSIST" in ''|*[!0-9smhdwyeno]*) echo "tunnel-connect: bad BN_TUNNEL_PERSIST '$PERSIST' (e.g. 10m, 1h, no)" >&2; exit 2 ;; esac
RUSER="${BN_TUNNEL_USER:-$(id -un)}"
KEY="${BN_TUNNEL_KEY:-$HOME/.ssh/id_ed25519}"
ME="$(hostname -s 2>/dev/null || hostname)"
LABEL="bn-host-$(printf '%s' "$TO" | tr 'A-Z' 'a-z' | tr -c 'a-z0-9-\n' '-')"

say() { echo "tunnel-connect: $*"; }
die() { echo "tunnel-connect: $*" >&2; exit 1; }

# devtunnel CLI (installed into ~/bin when missing) and its sign-in: see lib/tunnel.sh.
. "$(dirname "$0")/lib/tunnel.sh"
DT=$(bn_tunnel_devtunnel) || exit 1
bn_tunnel_login "$DT" || exit 1
KEYS_DIR=$(bn_tunnel_keys_dir)

# --- 1. publish this machine's public key ------------------------------------
# Committed and pushed right away, so the host's next tunnel-host run (which pulls) authorizes it.
[ -f "$KEY" ] || ssh-keygen -q -t ed25519 -N '' -f "$KEY"
mkdir -p "$KEYS_DIR"
if ! cmp -s "$KEY.pub" "$KEYS_DIR/$ME.pub" 2>/dev/null; then
  cp "$KEY.pub" "$KEYS_DIR/$ME.pub"
  say "published $KEY.pub as $KEYS_DIR/$ME.pub (then re-run tunnel-host on $TO)"
fi
bn_tunnel_sync_push "$KEYS_DIR" "$KEYS_DIR/$ME.pub"

# --- 2. find the tunnel -------------------------------------------------------
ID=$("$DT" list --labels "$LABEL" 2>/dev/null | awk 'NR>1 && $1 ~ /\./ {print $1; exit}')
[ -n "$ID" ] || die "no tunnel labelled $LABEL. Run 'bn run tunnel-host' on $TO first (same Dev Tunnels account)."

# --- 3. connect (forwards the host's port to 127.0.0.1:$PORT) ---------------
if ss -tln 2>/dev/null | grep -q "127.0.0.1:$PORT "; then
  say "127.0.0.1:$PORT already forwarded"
elif [ -n "${TMUX:-}" ]; then
  sess=$(tmux display-message -p '#S')
  # Restart loop: `devtunnel connect` reconnects on its own through short relay drops, but exits
  # when the host is gone for a while (Cloud PC asleep); come back once it is up again.
  tmux list-windows -t "$sess" -F '#W' | grep -qx "tunnel-$TO" || tmux new-window -d -t "$sess" -n "tunnel-$TO" "while :; do $DT connect $ID; echo 'tunnel-connect: devtunnel connect exited; retrying in 10s (Ctrl-C twice to stop)'; sleep 10; done"
  say "connecting to $ID in tmux window $sess:tunnel-$TO (retries itself if it drops)"
else
  die "run inside tmux, or start the forward yourself in another terminal: $DT connect $ID"
fi

# --- 4. managed ~/.ssh/config block -------------------------------------------
# ForwardAgent is opt-in. It lets the target use this machine's ssh-agent keys while connected,
# e.g. to reach a GitHub repo only this machine's key can access (the host's notes sync). The key
# never leaves this machine, but anyone with root on the target can use it during the session.
FWD=""
[ "${BN_TUNNEL_FORWARD_AGENT:-0}" = 1 ] && FWD="
  ForwardAgent yes"
# Port forwards ride every ssh to the host; a port already in use locally is skipped, not fatal.
for p in $(printf '%s' "${BN_TUNNEL_PORTS:-}" | tr ',' ' '); do
  case "$p" in *[!0-9]*|'') say "skipping port '$p' (not a number)"; continue ;; esac
  FWD="$FWD
  LocalForward $p 127.0.0.1:$p"
done
[ -n "${BN_TUNNEL_PORTS:-}" ] && FWD="$FWD
  ExitOnForwardFailure no"
# One shared connection: the first ssh to <machine> opens a master and every later one (remote-job,
# tunnel-attach, git, scp) rides it, skipping the Dev Tunnel channel setup and the SSH handshake.
# %C is a hash, so the socket path stays short. After a drop the master dies with the link (the
# ServerAlive settings find a dead link within a minute) and the next ssh opens a new one.
# Close it by hand: ssh -O exit <machine>.
CFG="$HOME/.ssh/config"
touch "$CFG"; chmod 600 "$CFG"
begin="# >>> bn tunnel $TO >>>"; end="# <<< bn tunnel $TO <<<"
tmp=$(mktemp)
awk -v b="$begin" -v e="$end" '$0==b{skip=1} !skip{print} $0==e{skip=0}' "$CFG" > "$tmp"
cat >> "$tmp" <<EOF
$begin
Host $TO
  HostName 127.0.0.1
  Port $PORT
  User $RUSER
  IdentityFile $KEY
  HostKeyAlias bn-tunnel-$TO
  ServerAliveInterval 15
  ServerAliveCountMax 4
  TCPKeepAlive yes
  ControlMaster auto
  ControlPath ~/.ssh/cm-%C
  ControlPersist $PERSIST$FWD
$end
EOF
cat "$tmp" > "$CFG"; rm -f "$tmp"
say "ssh config: Host $TO -> 127.0.0.1:$PORT. Try: ssh $TO 'bn --version'   or   bn connect $TO"
