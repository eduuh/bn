#!/bin/sh
# tmux-tunnels -- the tunnel segment of the tmux status bar: `⇄ AB12C✓ AB12C✗`.
#
# Reads the status file the tunnel-auto watchdog writes every minute (~/.cache/bn/tunnels.status,
# "<name> ok|down" per line) and never checks anything itself, so the bar stays instant. Names are
# shortened to their last dash-separated part; `host` is this machine's own hosting. A file older
# than 10 minutes means the watchdog stopped, shown as `⇄ ?`. A machine with no tunnels prints
# nothing.
f="$HOME/.cache/bn/tunnels.status"
[ -f "$f" ] || exit 0
if [ -n "$(find "$f" -mmin +10 2>/dev/null)" ]; then
  printf '#[fg=#565f89]⇄ ? '
  exit 0
fi
out=""
while read -r name state; do
  [ -n "$name" ] || continue
  short=${name##*-}
  if [ "$state" = ok ]; then
    out="$out #[fg=#9ece6a]$short✓"
  else
    out="$out #[fg=#f7768e,bold]$short✗#[nobold]"
  fi
done < "$f"
[ -n "$out" ] && printf '#[fg=#7aa2f7]⇄%s ' "$out"
