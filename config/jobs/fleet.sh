#!/bin/sh
# fleet -- one view of every machine this one works with: tunnel, bn version, warm pool slots,
# the last pool build, and the remote apps and agents running there.
#
#   bn run fleet        (or: sh fleet.sh [repo])   repo defaults to BigMono
#
# Asks every machine at once: this one locally, and each tunnel host (the managed `Host` blocks
# tunnel-connect writes) over its shared ssh connection, so the whole view takes a few seconds.
# A host that does not answer within 20s shows as unreachable instead of holding up the rest.
# Reads only; changes nothing.
set -u

REPO="${1:-${BN_FLEET_REPO:-BigMono}}"
case "$REPO" in *[!A-Za-z0-9_.-]*) echo "fleet: bad repo name '$REPO'" >&2; exit 2 ;; esac

# The probe, run on each machine. One line out: version|pool|build|windows.
PROBE='export PATH=$HOME/.local/bin:$PATH XDG_RUNTIME_DIR=/run/user/$(id -u)
REPO=__REPO__
v=$(bn --version 2>/dev/null | sed -n "s/.*git \([0-9a-f]\{7\}\).*/\1/p")
pool="no pool"
if [ -d "$HOME/projects/worktree/$REPO/_pool" ]; then
  st=$(bn wt pool status --repo "$REPO" 2>/dev/null)
  total=$(printf "%s\n" "$st" | grep -cE "^ +\[[0-9]+\]")
  warm=$(printf "%s\n" "$st" | grep -c "idle (warm)")
  dirty=$(printf "%s\n" "$st" | grep -c "dirty")
  leased=$(printf "%s\n" "$st" | grep -c "leased")
  pool="$warm/$total warm, $leased leased, $dirty dirty"
fi
build=""
for j in pool-build-remote pool-build; do
  s=$(bn cron show "$j" 2>/dev/null | head -n 1)
  case "$s" in "$j: "*) build="${s#"$j: "}"; break ;; esac
done
run=$(systemctl --user is-active bn-pool-build-remote.service 2>/dev/null)
[ "$run" = activating ] && build="BUILDING now; last: $build"
win=$(tmux list-windows -a -F "#{session_name}:#{window_name}" 2>/dev/null | grep -E ":(app|debug)-" | sed "s/^[^:]*://" | tr "\n" " ")
printf "%s|%s|%s|%s\n" "${v:-?}" "$pool" "${build:-no pool build yet}" "${win:--}"'
PROBE=$(printf '%s' "$PROBE" | sed "s/__REPO__/$REPO/")

tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
me=$(hostname -s 2>/dev/null || hostname)
sh -c "$PROBE" > "$tmp/0-$me" 2>/dev/null &
hosts=$(awk '/^# >>> bn tunnel /{print $5}' "$HOME/.ssh/config" 2>/dev/null)
for h in $hosts; do
  timeout 20 ssh -o BatchMode=yes -o ConnectTimeout=15 "$h" 'sh -s' > "$tmp/1-$h" 2>/dev/null <<EOF &
$PROBE
EOF
done
wait

tstate() { awk -v h="$1" '$1 == h {print $2}' "$HOME/.cache/bn/tunnels.status" 2>/dev/null; }
printf '%-16s %-7s %-8s %-34s %s\n' MACHINE TUNNEL BN "$REPO POOL" "LAST POOL BUILD"
for f in "$tmp"/0-* "$tmp"/1-*; do
  [ -e "$f" ] || continue
  name=${f##*/}; name=${name#?-}; short=${name##*-}
  if [ "${f##*/}" = "0-$name" ]; then t=local; else t=$(tstate "$name"); t=${t:-?}; fi
  line=$(tail -n 1 "$f")
  if [ -z "$line" ]; then
    printf '%-16s %-7s %s\n' "$short" "$t" "unreachable (no answer in 20s)"
    continue
  fi
  v=$(printf '%s' "$line" | cut -d'|' -f1); pool=$(printf '%s' "$line" | cut -d'|' -f2)
  build=$(printf '%s' "$line" | cut -d'|' -f3); win=$(printf '%s' "$line" | cut -d'|' -f4)
  printf '%-16s %-7s %-8s %-34s %s\n' "$short" "$t" "$v" "$pool" "$build"
  [ "$win" != "-" ] && [ -n "$win" ] && printf '%-16s %-7s running: %s\n' "" "" "$win"
done
exit 0
