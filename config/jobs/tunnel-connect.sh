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
#      for at a terminal). tunnel-host authorizes that account's keys; nothing else is needed;
#   2. for each host, runs `devtunnel connect` in window `tunnel-<machine>` of the tmux session
#      `bn-tunnels`, forwarding the host's SSH port (derived from its name, read from its tunnel)
#      to the same port on 127.0.0.1 here;
#   3. writes a managed `Host <machine>` block in ~/.ssh/config, so `ssh <machine>` and
#      `bn connect <machine>` work.
# A host in local network mode (tunnel-host, bn.tunnelLan) that answers on its LAN address gets a
# `Match ... exec` probe in front of its Host block: ssh goes straight to it on the same network
# and through the tunnel everywhere else.
# tmux runs tunnel-auto at server start, which re-runs this, so connections come back by themselves.
#
# Env: BN_TUNNEL_TO (only this machine), BN_TUNNEL_USER (remote user, default: yours),
#      BN_TUNNEL_KEY (use this key instead of choosing one), BN_TUNNEL_GITHUB_USER,
#      BN_TUNNEL_FORWARD_AGENT=1 to add `ForwardAgent yes` (opt-in; see below),
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
  # GitHub could not be asked (offline, a slow keys fetch, gh signed out). Keep the key this
  # machine already uses for tunnels rather than switching: falling back to id_ed25519 here
  # re-published a different key on every flaky pass, a notes commit each time (2026-09-27).
  if [ -f "$HOME/.ssh/bn_tunnel_ed25519" ]; then
    KEY="$HOME/.ssh/bn_tunnel_ed25519"
  else
    KEY="$HOME/.ssh/id_ed25519"
    [ -f "$KEY" ] || ssh-keygen -q -t ed25519 -N '' -f "$KEY"
  fi
fi
say "using key $KEY"

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
  if bn_tunnel_listening "$port" 127.0.0.1; then
    say "$to: 127.0.0.1:$port already forwarded"
  else
    for w in $running; do tmux kill-window -t "$w" 2>/dev/null || true; done
    # Restart loop: `devtunnel connect` reconnects on its own through short relay drops, but
    # exits when the host is gone for a while (Cloud PC asleep); come back once it is up again.
    tmux new-window -d -t "$SESS" -n "$win" "while :; do $DT connect $id; echo 'tunnel-connect: devtunnel connect exited; retrying in 10s (Ctrl-C twice to stop)'; sleep 10; done"
    say "$to: connecting in tmux window $SESS:$win (127.0.0.1:$port; retries itself if it drops)"
  fi

  # Local network mode: a host that published its LAN address and answers on it right now gets a
  # Match in front of its Host block. ssh takes the first HostName it finds, so while the probe
  # passes it connects straight to the host and skips the relay; when this machine leaves that
  # network the probe fails within a second and ssh uses the tunnel, and the next pass here drops
  # the Match. The host key is checked under the same alias either way, so another machine
  # holding that address is refused.
  lan=""
  lan_ip=$(bn_tunnel_lan_labels "$DT" "$id" | head -n 1 | sed 's/^bn-lan-//' | tr - .)
  if [ -n "$lan_ip" ] && command -v nc >/dev/null 2>&1 && nc -z -w 1 "$lan_ip" "$port" >/dev/null 2>&1; then
    lan="Match originalhost $to exec \"nc -z -w 1 $lan_ip $port >/dev/null 2>&1\"
  HostName $lan_ip
"
    say "$to: on this network at $lan_ip:$port; ssh goes direct, the tunnel is the fallback"
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
${lan}Host $to
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

listed=1
hosts=$(bn_tunnel_hosts "$DT") || { listed=0; hosts=""; say "could not list the hosts on the account; keeping every connection as it is"; }
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
elif [ "$found" = 0 ] && [ "$listed" = 1 ]; then
  say "no bn hosts on this Dev Tunnels account yet. Run 'bn run tunnel-host' on the machine to reach."
fi

# A host gone from the account (it turned hosting off, or its tunnel was deleted) loses its Host
# block and its connect window, so it stops showing as down. Only after a listing that worked, so
# an offline pass never empties the config.
if [ "$listed" = 1 ] && [ -z "$TO" ]; then
  cfg="$HOME/.ssh/config"
  for k in $(awk '/^# >>> bn tunnel /{print $5}' "$cfg" 2>/dev/null); do
    printf '%s\n' "$hosts" | awk -v n="$(lower "$k")" 'tolower($2) == n {f = 1} END {exit !f}' && continue
    tmp=$(mktemp)
    awk -v b="# >>> bn tunnel $k >>>" -v e="# <<< bn tunnel $k <<<" '$0==b{skip=1} !skip{print} $0==e{skip=0}' "$cfg" > "$tmp"
    cat "$tmp" > "$cfg"; rm -f "$tmp"
    for w in $(tmux list-windows -a -F '#{session_name}:#{window_name}' 2>/dev/null | grep ":tunnel-$k\$" || true); do
      tmux kill-window -t "$w" 2>/dev/null || true
    done
    say "$k: no longer on the account; removed its ssh config and connection"
  done
fi
