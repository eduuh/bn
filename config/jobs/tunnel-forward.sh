#!/bin/sh
# tunnel-forward -- keep ports on a tunnel host forwarded to this machine, without a session open.
#
# Run on the machine you connect FROM:
#   BN_TUNNEL_TO=<machine> BN_TUNNEL_PORTS=3000,3001 bn run tunnel-forward
#   (or: sh tunnel-forward.sh <machine> 3000,3001)
# e.g. the BigMono dev server running on a Cloud PC shows up at https://localhost:3000 here.
#
# Inside tmux it runs in a window `fwd-<machine>` (so it outlives this command); outside tmux it
# runs in the foreground. Either way it is `ssh -N -L ...` in a loop: when the link drops it
# reconnects (5s, backing off to 30s). ExitOnForwardFailure makes a taken local port an error you
# can see instead of a silently missing forward. It keeps its own connection (ControlPath=none)
# rather than riding the shared one tunnel-connect sets up: it is long-lived anyway, and a forward
# added to a shared master would outlive this loop and never report a failure.
#
# Env: BN_TUNNEL_TO (target machine), BN_TUNNEL_PORTS (comma-separated remote ports; each is
# forwarded to the same local port), BN_TUNNEL_FOREGROUND=1 to stay in this terminal.
set -u

TO="${1:-${BN_TUNNEL_TO:-}}"
PORTS="${2:-${BN_TUNNEL_PORTS:-}}"
[ -n "$TO" ] || { echo "tunnel-forward: which machine? BN_TUNNEL_TO=<machine> BN_TUNNEL_PORTS=3000 bn run tunnel-forward" >&2; exit 2; }
[ -n "$PORTS" ] || { echo "tunnel-forward: which ports? BN_TUNNEL_PORTS=3000,3001" >&2; exit 2; }

args=""
for p in $(printf '%s' "$PORTS" | tr ',' ' '); do
  case "$p" in *[!0-9]*|'') echo "tunnel-forward: '$p' is not a port" >&2; exit 2 ;; esac
  args="$args -L $p:127.0.0.1:$p"
done

loop="wait_s=5; while :; do started=\$(date +%s); ssh -N -o ControlPath=none -o ExitOnForwardFailure=yes$args $TO; rc=\$?; [ \$((\$(date +%s)-started)) -gt 60 ] && wait_s=5; echo \"tunnel-forward: ssh to $TO exited (\$rc); reconnecting in \${wait_s}s (Ctrl-C twice to stop)\"; sleep \$wait_s; [ \$wait_s -lt 30 ] && wait_s=\$((wait_s*2)); [ \$wait_s -gt 30 ] && wait_s=30; done"

if [ -n "${TMUX:-}" ] && [ "${BN_TUNNEL_FOREGROUND:-0}" != 1 ]; then
  sess=$(tmux display-message -p '#S')
  if tmux list-windows -t "$sess" -F '#W' | grep -qx "fwd-$TO"; then
    echo "tunnel-forward: already forwarding in tmux window $sess:fwd-$TO"
  else
    tmux new-window -d -t "$sess" -n "fwd-$TO" "$loop"
    echo "tunnel-forward: forwarding $PORTS from $TO in tmux window $sess:fwd-$TO"
  fi
else
  echo "tunnel-forward: forwarding $PORTS from $TO (Ctrl-C twice to stop)"
  exec sh -c "$loop"
fi
