#!/bin/sh
# tunnel-attach -- a reliable terminal on a tunnel host: attach to (or create) a tmux session
# there, and re-attach automatically when the connection drops.
#
# Run on the machine you connect FROM, in a terminal:
#   BN_TUNNEL_TO=<machine> bn run tunnel-attach      (or: sh tunnel-attach.sh <machine> [session])
# Needs the `Host <machine>` entry tunnel-connect writes. Pair: tunnel-host / tunnel-connect.
#
# The remote tmux session keeps running whatever the link does, so a drop loses nothing: this
# loop just reconnects (5s, then backing off to 30s) and puts you back where you were.
# Detaching normally (prefix d) or exiting the session ends the loop. The remote command runs in a
# login shell, so bn and nvm are on PATH. Port forwards from BN_TUNNEL_PORTS (tunnel-connect) come
# along with every attach.
#
# Env: BN_TUNNEL_TO (target machine), BN_TUNNEL_SESSION (remote tmux session, default bn).
set -u

TO="${1:-${BN_TUNNEL_TO:-}}"
SESSION="${2:-${BN_TUNNEL_SESSION:-bn}}"
[ -n "$TO" ] || { echo "tunnel-attach: which machine? BN_TUNNEL_TO=<machine> bn run tunnel-attach" >&2; exit 2; }
[ -t 0 ] || { echo "tunnel-attach: needs a terminal (run it in a tmux pane, not a pipe)" >&2; exit 2; }
case "$SESSION" in *[!A-Za-z0-9_.-]*) echo "tunnel-attach: bad session name '$SESSION'" >&2; exit 2 ;; esac

# Remote side, run in a login shell (so PATH matches an interactive login on the host). It tries
# each installed tmux in turn: a tmux server keeps running the binary it was started from, so after
# an upgrade the tmux on PATH can be a different version than the server, and the attach fails at
# once with "open terminal failed: not a terminal" (seen 2026-09-27 on cloud-pc-AB12C: server
# started by a since-replaced /usr/local/bin/tmux, 3.7c on PATH, /usr/bin/tmux 3.4 compatible).
# A binary that fails within 3s is skipped; one that ran longer ended a real session, so its exit
# code stands. -u forces UTF-8 (an ssh session often has no UTF-8 locale; borders show as qqqq).
REMOTE='seen=; for b in $(command -v tmux) /usr/bin/tmux /usr/local/bin/tmux; do [ -x "$b" ] || continue; case " $seen " in *" $b "*) continue ;; esac; seen="$seen $b"; s=$(date +%s); "$b" -u new-session -A -s SESSION; rc=$?; [ "$rc" -eq 0 ] && exit 0; [ $(( $(date +%s) - s )) -ge 3 ] && exit "$rc"; echo "tunnel-attach: $b could not attach (exit $rc); trying another tmux" >&2; done; exit 1'
REMOTE=$(printf '%s' "$REMOTE" | sed "s/SESSION/$SESSION/")

wait_s=5
while :; do
  started=$(date +%s)
  ssh -t "$TO" "exec \${SHELL:-sh} -lc '$REMOTE'"
  rc=$?
  # 0 = detached or exited on purpose. 255 = ssh could not connect or the link dropped.
  [ "$rc" -eq 255 ] || exit "$rc"
  # A session that stayed up a while was a real drop, not a failing reconnect: start over at 5s.
  [ $(( $(date +%s) - started )) -gt 60 ] && wait_s=5
  echo "tunnel-attach: connection to $TO lost; reconnecting in ${wait_s}s (Ctrl-C to stop)" >&2
  sleep "$wait_s"
  [ "$wait_s" -lt 30 ] && wait_s=$((wait_s * 2))
  [ "$wait_s" -gt 30 ] && wait_s=30
done
