#!/bin/sh
# tunnel-auto -- keep this machine's tunnels up without being asked.
#
# tmux runs it at server start (config/workflow/tmux.conf), so a reboot, a WSL restart or a
# `tmux kill-server` puts things back as they were:
#   - a machine that has hosted before (tunnel-host left ~/.ssh/bn-sshd/sshd_config) hosts again;
#   - a machine that has connected before (a managed `# >>> bn tunnel` block in ~/.ssh/config)
#     reconnects to every bn host on the account.
# A machine that has done neither is left alone.
#
# It then keeps a watchdog window `tunnel-watch` in the tmux session `bn-tunnels` that repeats
# those checks every BN_TUNNEL_WATCH_SECONDS (default 300). Both jobs are idempotent and repair
# what they find: an sshd that stopped listening is restarted, a missing host or connect window is
# recreated, and a connection whose port is not listening is restarted. So a drop heals within
# minutes and nobody has to notice. `sh tunnel-auto.sh --once` runs the checks without the watchdog.
#
# Unattended: nothing here may wait for a person. The Dev Tunnels sign-in only prompts at a
# terminal (lib/tunnel.sh), so a signed-out machine logs the command to run instead.
# Output: ~/.cache/bn/tunnel-auto.log (kept to its last 2000 lines).
set -u

dir=$(cd "$(dirname "$0")" && pwd)
log="$HOME/.cache/bn/tunnel-auto.log"
mkdir -p "$(dirname "$log")"
if [ -f "$log" ] && [ "$(wc -l < "$log")" -gt 4000 ]; then
  tail -n 2000 "$log" > "$log.tmp" && mv "$log.tmp" "$log"
fi
exec >> "$log" 2>&1 < /dev/null
echo "--- $(date '+%F %T') tunnel-auto"

role=0
if [ -f "$HOME/.ssh/bn-sshd/sshd_config" ]; then
  role=1
  sh "$dir/tunnel-host.sh" || echo "tunnel-auto: tunnel-host failed ($?)"
fi
if grep -q '^# >>> bn tunnel ' "$HOME/.ssh/config" 2>/dev/null; then
  role=1
  sh "$dir/tunnel-connect.sh" || echo "tunnel-auto: tunnel-connect failed ($?)"
fi

[ "${1:-}" = --once ] && exit 0
[ "$role" = 1 ] || exit 0
every="${BN_TUNNEL_WATCH_SECONDS:-300}"
case "$every" in ''|*[!0-9]*) every=300 ;; esac
. "$dir/lib/tunnel.sh"
sess=$(bn_tunnel_session) || exit 0
if ! tmux list-windows -t "$sess" -F '#W' | grep -qx tunnel-watch; then
  tmux new-window -d -t "$sess" -n tunnel-watch "while :; do sleep $every; sh '$dir/tunnel-auto.sh' --once; done"
  echo "tunnel-auto: watchdog started in tmux window $sess:tunnel-watch (every ${every}s)"
fi
