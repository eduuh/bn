#!/bin/sh
# tmux-tunnels -- connected tunnels in the tmux status bar: `⇄ AB12C✓`.
#
# Reads the status file the tunnel-auto watchdog writes every minute (~/.cache/bn/tunnels.status,
# "<name> ok|down" per line) and never checks anything itself, so the bar stays instant. Names are
# shown by their last dash-separated part (cloud-pc-AB12C → AB12C; the prefix says nothing).
# Only `ok` entries are shown. A file older than 10 minutes means the watchdog stopped, shown as
# `⇄ ?`. A machine with no connected tunnels prints nothing.
f="$HOME/.cache/bn/tunnels.status"
[ -f "$f" ] || exit 0
# Stale when older than 10 minutes, or dated in the future (a WSL clock that jumped).
m=$(stat -c %Y "$f" 2>/dev/null || stat -f %m "$f" 2>/dev/null); age=$(( $(date +%s) - ${m:-0} ))
if [ "$age" -lt 0 ] || [ "$age" -ge 600 ]; then
  printf '#[fg=#565f89]⇄ ? '
  exit 0
fi
# A host's nickname (the second name on its `Host` line in tunnel-connect's block) wins over the
# hostname's last part.
nicks=$(awk '/^# >>> bn tunnel /{inb=1} inb && $1 == "Host" && NF >= 3 {print $2, $3} /^# <<< bn tunnel /{inb=0}' "$HOME/.ssh/config" 2>/dev/null)
out=""
while read -r name state; do
  [ -n "$name" ] || continue
  [ "$state" = ok ] || continue
  short=$(printf '%s\n' "$nicks" | awk -v n="$name" '$1 == n {print $2; exit}')
  [ -n "$short" ] || short=${name##*-}
  out="$out #[fg=#9ece6a]${short}✓"
done < "$f"
if [ -n "$out" ]; then
  printf '#[fg=#7aa2f7]⇄%s ' "$out"
fi
