#!/bin/sh
# tunnel-host -- make this machine reachable over SSH through an authenticated Dev Tunnel.
#
# Run on the machine others connect TO (e.g. a Cloud PC's WSL):  bn run tunnel-host
# Pair: tunnel-connect.sh on the machine you connect FROM.
#
# What it does, idempotently:
#   1. starts a user-level sshd on 127.0.0.1:<port> (key-only, no root daemon). The port comes
#      from this machine's name (bn_tunnel_port_for), so no two hosts clash on a client;
#   2. authorizes the public SSH keys of a GitHub account (github.com/<user>.keys, see
#      bn_tunnel_github_user). Nothing else: no notes repo, no key files to sync;
#   3. finds or creates a Dev Tunnel labelled bn-host-<machine> and bn-host (authenticated, never
#      anonymous) carrying only that port, and hosts it in window `tunnel` of the tmux session
#      `bn-tunnels` (foreground without tmux). tmux runs tunnel-auto at server start, which
#      re-runs this, so a reboot or a tmux restart brings the host back by itself.
#
# Discovery needs no shared config: both machines sign in to Dev Tunnels with the same account,
# so `bn run tunnel-connect` finds every host by the bn-host label.
#
# Prereqs (once per machine): `sudo apt-get install -y openssh-server` (for the sshd binary; the
# system service is not needed). The devtunnel CLI and its GitHub sign-in are set up on first run.
#
# Env: BN_TUNNEL_NAME (default: short hostname), BN_TUNNEL_SSH_PORT (default: derived from the
#      name, or the port it already uses), BN_TUNNEL_GITHUB_USER (whose GitHub keys may log in; default: gh's signed-in
#      account), BN_TUNNEL_FOREGROUND=1 to host in this terminal.
set -eu

NAME="${BN_TUNNEL_NAME:-$(hostname -s 2>/dev/null || hostname)}"
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
# The port sticks: the one the running config uses wins over the derived default, so an
# unattended re-run (tunnel-auto) never moves a host that was set up with BN_TUNNEL_SSH_PORT.
PORT="${BN_TUNNEL_SSH_PORT:-$(awk '$1 == "Port" {print $2; exit}' "$STATE/sshd_config" 2>/dev/null)}"
[ -n "$PORT" ] || PORT=$(bn_tunnel_port_for "$NAME")

# --- 1. user-level sshd -------------------------------------------------------
mkdir -p "$STATE" "$HOME/.ssh"
chmod 700 "$HOME/.ssh" "$STATE"
[ -f "$STATE/host_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -f "$STATE/host_ed25519"
# internal-sftp: modern scp and VS Code Remote-SSH speak SFTP; without it `scp` just closes.
# ClientAlive*: drop a dead tunnel session in ~90s instead of holding it open forever.
old_cfg=$(cat "$STATE/sshd_config" 2>/dev/null || true)
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
Subsystem sftp internal-sftp
ClientAliveInterval 30
ClientAliveCountMax 3
AllowTcpForwarding local
EOF

# Healthy means listening on the port, not just alive: on 2026-09-27 cloud-pc-AB12C's sshd stayed
# up after a port-change reload but never answered on the new port, and nothing noticed. Without
# `ss` there is no way to tell, so alive has to do.
listening() {
  command -v ss >/dev/null 2>&1 || return 0
  ss -tln 2>/dev/null | grep -q "127.0.0.1:$PORT "
}
wait_listening() { _i=0; until listening; do _i=$((_i + 1)); [ "$_i" -ge 10 ] && return 1; sleep 1; done; }
start_sshd() {
  "$SSHD" -f "$STATE/sshd_config" || die "sshd failed to start on port $PORT"
  wait_listening || die "sshd started but is not listening on 127.0.0.1:$PORT"
  say "sshd started on 127.0.0.1:$PORT"
}

"$SSHD" -t -f "$STATE/sshd_config" || die "sshd rejected $STATE/sshd_config"
pid=$(cat "$STATE/sshd.pid" 2>/dev/null || true)
if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
  if [ "$old_cfg" != "$(cat "$STATE/sshd_config")" ]; then
    # A running sshd re-reads its config on SIGHUP; existing sessions are kept.
    kill -HUP "$pid" && say "sshd config changed; reloaded (pid $pid)"
    wait_listening || { say "sshd is not listening on 127.0.0.1:$PORT after the reload; restarting it"; kill "$pid" 2>/dev/null; sleep 1; start_sshd; }
  elif ! listening; then
    # Only the listener is restarted; open sessions are separate processes and stay up.
    say "sshd (pid $pid) is not listening on 127.0.0.1:$PORT; restarting it"
    kill "$pid" 2>/dev/null; sleep 1
    start_sshd
  else
    say "sshd already running on 127.0.0.1:$PORT (pid $pid)"
  fi
else
  start_sshd
fi

# --- 2. authorize client keys ------------------------------------------------
touch "$HOME/.ssh/authorized_keys"
chmod 600 "$HOME/.ssh/authorized_keys"
# The GitHub account's keys live in a managed block that is rewritten every run, so a key removed
# on GitHub stops working here too. Offline, the block is left as it was rather than emptied.
GH_USER=$(bn_tunnel_github_user)
if [ -z "$GH_USER" ]; then
  say "no GitHub account to trust keys from (set BN_TUNNEL_GITHUB_USER, or sign in with gh)"
elif gh_keys=$(bn_tunnel_github_keys "$GH_USER"); then
  begin="# >>> bn tunnel github:$GH_USER >>>"; end="# <<< bn tunnel github:$GH_USER <<<"
  tmp=$(mktemp)
  awk -v b="$begin" -v e="$end" '$0==b{skip=1} !skip{print} $0==e{skip=0}' "$HOME/.ssh/authorized_keys" > "$tmp"
  printf '%s\n%s\n%s\n' "$begin" "$gh_keys" "$end" >> "$tmp"
  cat "$tmp" > "$HOME/.ssh/authorized_keys"; rm -f "$tmp"
  say "authorized $(printf '%s\n' "$gh_keys" | wc -l | tr -d ' ') keys of github.com/$GH_USER"
else
  say "could not fetch github.com/$GH_USER.keys; keeping the keys already authorized"
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
"$DT" update "$ID" --add-labels bn-host -d "$NAME" >/dev/null 2>&1 || say "could not label $ID for discovery"
# Only this port on the tunnel: a client forwards every tunnel port, and a stale one (from an
# earlier port) would clash with another host's. A port change needs the host restarted.
restart=0
for p in $(bn_tunnel_ports "$DT" "$ID"); do
  [ "$p" = "$PORT" ] || { "$DT" port delete "$ID" -p "$p" >/dev/null 2>&1 && say "removed old port $p from $ID"; restart=1; }
done
if ! "$DT" port show "$ID" -p "$PORT" >/dev/null 2>&1; then
  "$DT" port create "$ID" -p "$PORT" >/dev/null || die "could not add port $PORT to $ID"
  restart=1
fi

# --- host it -------------------------------------------------------------------
if [ "${BN_TUNNEL_FOREGROUND:-0}" != 1 ] && sess=$(bn_tunnel_session); then
  # A `tunnel` window in any session counts: older runs hosted in whatever session they ran in.
  running=$(tmux list-windows -a -F '#{session_name}:#{window_name}' 2>/dev/null | grep ':tunnel$' || true)
  if [ -n "$running" ] && [ "$restart" = 0 ]; then
    say "already hosting in tmux window $(echo "$running" | head -n 1)"
  else
    for w in $running; do tmux kill-window -t "$w" 2>/dev/null || true; done
    # Restart loop: `devtunnel host` exits when the relay drops for good; come straight back.
    tmux new-window -d -t "$sess" -n tunnel "while :; do $DT host $ID; echo 'tunnel-host: devtunnel host exited; restarting in 5s (Ctrl-C twice to stop)'; sleep 5; done"
    say "hosting $ID in tmux window $sess:tunnel (restarts itself if it drops)"
  fi
  say "connect from another machine (same Dev Tunnels account):  bn run tunnel-connect"
else
  say "hosting $ID in the foreground (restarts if it drops; Ctrl-C twice stops it)"
  while :; do "$DT" host "$ID" || true; say "devtunnel host exited; restarting in 5s"; sleep 5; done
fi
