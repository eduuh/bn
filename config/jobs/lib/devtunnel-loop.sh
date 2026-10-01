#!/bin/sh
# devtunnel-loop host|connect <devtunnel> <tunnel-id> <name> -- keep `devtunnel host` or
# `devtunnel connect` running, with backoff.
#
# tunnel-host and tunnel-connect run this in a tmux window. It used to retry every 5s (host) or
# 10s (connect) forever. With a host down, every machine's connect loop hit the Dev Tunnels
# service several times a minute, and the account got "Rate limit exceeded (429)": every
# connection on every machine failed together, and the fast retries kept the limit in force
# (2026-10-01). Now:
#   - a retry waits 10s, then doubles up to 10 minutes; a run that stayed up for 2 minutes resets
#     the wait to 10s, so a short relay blip still heals fast;
#   - a 429 waits 5 minutes, then the next try starts the backoff again from 20s;
#   - every wait gets up to a quarter more at random, so machines that failed together do not
#     retry in step (from #1460).
# The last run's output is kept in ~/.cache/bn/devtunnel-<mode>.<name>.log, where tunnel-auto
# looks for a 429 before restarting this window (it does not restart a window that is waiting
# out a rate limit).
set -u
mode=$1; dt=$2; id=$3; name=$4
case "$mode" in host|connect) ;; *) echo "usage: devtunnel-loop.sh host|connect <devtunnel> <id> <name>" >&2; exit 2 ;; esac
log="$HOME/.cache/bn/devtunnel-$mode.$name.log"
mkdir -p "$HOME/.cache/bn"
# jitter <seconds> -- a random 0..seconds/4.
jitter() { echo $(( $(od -An -N2 -tu2 /dev/urandom 2>/dev/null | tr -d ' ' || echo 0) % ($1 / 4 + 1) )); }
wait_s=10
while :; do
  started=$(date +%s)
  "$dt" "$mode" "$id" 2>&1 | tee "$log"
  if [ $(( $(date +%s) - started )) -ge 120 ]; then wait_s=10; fi
  if grep -q '(429)' "$log" 2>/dev/null; then
    # A fixed 5 minutes, then back to a short wait: the limit lifts on its own, and the next try
    # after it should not sit behind a backoff that grew while the service was refusing.
    w=$(( 300 + $(jitter 300) ))
    echo "tunnel-$mode: Dev Tunnels rate limit (429); waiting ${w}s before trying again (Ctrl-C twice to stop)"
    sleep "$w"
    wait_s=20
    continue
  fi
  w=$(( wait_s + $(jitter "$wait_s") ))
  echo "tunnel-$mode: devtunnel $mode exited; retrying in ${w}s (Ctrl-C twice to stop)"
  sleep "$w"
  wait_s=$(( wait_s * 2 )); [ "$wait_s" -le 600 ] || wait_s=600
done
