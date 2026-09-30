#!/bin/sh
# tunnels -- how the tunnel mesh looks from this machine, and why a machine is missing or down.
#
#   bn tunnels        (or: bn run tunnels, sh tunnels.sh)
#
# The status bar only says ok or down for hosts this machine already connects to. This asks the
# Dev Tunnels account, so it also lists a host this machine has not picked up yet, and for each one
# says which route ssh takes (LAN or tunnel), whether the host end of its tunnel is up, and what is
# wrong when ssh does not answer. It checks every host at once, live, so it takes a few seconds.
# Each host's own report (published by its tunnel-auto to the notes repo, lib/tunnel.sh) says why
# it is down in its own words, or since when it has been silent. Reads only; changes nothing.
set -u

dir=$(cd "$(dirname "$0")" && pwd -P)
. "$dir/lib/tunnel.sh"
ME=$(hostname -s 2>/dev/null || hostname)
cache="$HOME/.cache/bn"
lower() { printf '%s' "$1" | tr 'A-Z' 'a-z'; }
flag() { git config --global "$1" 2>/dev/null | tr 'A-Z' 'a-z'; }

tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT

# --- this machine -------------------------------------------------------------------
DT=$(command -v devtunnel 2>/dev/null || true)
[ -n "$DT" ] || [ ! -x "$HOME/bin/devtunnel" ] || DT="$HOME/bin/devtunnel"
# The account listing is the slow part (about ten seconds: the service asks every region), so it
# starts now and everything local is worked out while it runs. It carries all that is needed per
# host: "<tunnel-id> <machine> <host connections> <lan address or ->".
[ -z "$DT" ] || "$DT" list --labels bn-host -j 2>/dev/null | awk -F'"' '
  /"tunnelId"/ { id = $4; hc = "?"; lan = "-" }
  /"hostConnections"/ { hc = $3; gsub(/[^0-9]/, "", hc) }
  $2 ~ /^bn-lan-[0-9-]+$/ { lan = substr($2, 8); gsub(/-/, ".", lan) }
  /"description"/ { if (id != "" && $4 != "") print id, $4, hc, lan; id = "" }' > "$tmp/list" &
# The other machines' reports come from the notes repo, fetched alongside.
bn_tunnel_reports_fetch > "$tmp/notes" 2>/dev/null &
if [ -z "$DT" ]; then
  account="no devtunnel CLI (bn run tunnel-setup)"
elif who=$("$DT" user show 2>/dev/null | grep -i 'logged in as'); then
  account=$(printf '%s\n' "$who" | head -n 1 | sed 's/^Logged in as //; s/\.$//')
else
  account="signed out (bn run tunnel-setup, or: $DT user login -g -d)"
fi

port=$(awk '$1 == "Port" {print $2; exit}' "$HOME/.ssh/bn-sshd/sshd_config" 2>/dev/null)
case "$(flag bn.tunnelHost)" in off|false|no|0) host_off=1 ;; *) host_off=0 ;; esac
if [ "$host_off" = 1 ]; then
  hosting="not hosting (bn.tunnelHost off)"
elif ! command -v sshd >/dev/null 2>&1 && [ ! -x /usr/sbin/sshd ]; then
  hosting="not hosting: no sshd (bn run tunnel-setup)"
elif [ -z "$port" ]; then
  hosting="not hosting yet"
elif ! bn_tunnel_listening "$port"; then
  hosting="HOST DOWN: sshd is not listening on port $port"
elif ! tmux list-windows -a -F '#{window_name}' 2>/dev/null | grep -qx tunnel; then
  hosting="HOST DOWN: no tmux window is running devtunnel host"
else
  hosting="hosting on port $port"
fi
case "$(flag bn.tunnelLan)" in on|true|yes|1) hosting="$hosting, local network mode on" ;; esac
case "$(flag bn.tunnelAuto)" in off|false|no|0) hosting="$hosting, auto-enrol off" ;; esac

if [ -f "$cache/tunnels.status" ]; then
  m=$(stat -c %Y "$cache/tunnels.status" 2>/dev/null || stat -f %m "$cache/tunnels.status" 2>/dev/null)
  age=$(( $(date +%s) - ${m:-0} ))
  if [ "$age" -ge 0 ] && [ "$age" -lt 600 ]; then watchdog="checked ${age}s ago"; else watchdog="STOPPED (last check $((age / 60))m ago; run: sh $dir/tunnel-auto.sh)"; fi
else
  watchdog="not running on this machine"
fi

if [ -f "$cache/tunnel-report.pushed" ]; then
  m=$(stat -c %Y "$cache/tunnel-report.pushed" 2>/dev/null || stat -f %m "$cache/tunnel-report.pushed" 2>/dev/null)
  report="published $(( ($(date +%s) - ${m:-0}) / 60 ))m ago"
else
  report="not published yet (tunnel-auto does it; needs the work-notes repo)"
fi

echo "tunnels: $ME"
echo "  account    $account"
echo "  this host  $hosting"
echo "  watchdog   $watchdog"
echo "  report     $report"

# --- every other host ---------------------------------------------------------------
# Hosts on the account, then any this machine still has a Host block for but the account no longer
# lists (tunnel id `-`).
wait
listed=$(cat "$tmp/list" 2>/dev/null)
notes_git=$(cat "$tmp/notes" 2>/dev/null)
now=$(date +%s)

# report <machine> -- what that machine last said about itself, as a phrase; nothing without one.
# Reports come every 15 minutes, so one older than 25 is a machine gone quiet.
report() {
  [ -n "$notes_git" ] || return 0
  r=$(git -C "$notes_git" cat-file -p "refs/bn-status/$1:tunnels.status" 2>/dev/null) || return 0
  at=$(printf '%s\n' "$r" | sed -n 's/^at=//p'); why=$(printf '%s\n' "$r" | sed -n 's/^reason=//p')
  role=$(printf '%s\n' "$r" | sed -n 's/^role=//p')
  case "$at" in ''|*[!0-9]*) return 0 ;; esac
  ago=$((now - at)); when=$(date -d "@$at" '+%a %H:%M' 2>/dev/null || date -r "$at" '+%a %H:%M' 2>/dev/null)
  if [ "$ago" -gt 1500 ]; then
    echo "silent since $when: off, asleep, or its watchdog stopped${why:+ (it last said: $why)}"
  elif [ -n "$why" ]; then
    echo "it says ($((ago / 60))m ago): $why"
  elif [ "$role" = off ]; then
    echo "hosting is turned off there (bn.tunnelHost off)"
  else
    echo "it reported itself fine $((ago / 60))m ago"
  fi
}
known=$(awk '/^# >>> bn tunnel /{print $5}' "$HOME/.ssh/config" 2>/dev/null)
for k in $known; do
  printf '%s\n' "$listed" | awk -v n="$(lower "$k")" 'tolower($2) == n {f = 1} END {exit !f}' || listed="$listed
- $k ? -"
done

# check <tunnel-id> <machine> <host connections> <lan> -- one line: state|route|host end|note
check() {
  id=$1; name=$2; lan=$4
  case "$3" in '?') hostup="?" ;; 0) hostup=no ;; *) hostup=yes ;; esac
  if ! printf '%s\n' "$known" | grep -qix "$name"; then
    r=$(report "$name")
    echo "new|-|$hostup|not connected from here yet; the next repair pass picks it up (or: bn run tunnel-connect)${r:+; $r}"
    return
  fi
  target=$(ssh -G "$name" 2>/dev/null | awk '$1 == "hostname" {print $2; exit}')
  p=$(ssh -G "$name" 2>/dev/null | awk '$1 == "port" {print $2; exit}')
  case "$target" in 127.0.0.1) route="tunnel :$p" ;; *) route="LAN $target:$p" ;; esac
  # Through the tunnel with nothing hosting it, ssh can only wait for its timeout; skip the wait.
  err=""
  if { [ "$target" != 127.0.0.1 ] || [ "$hostup" != no ]; } && err=$(bn_tunnel_timeout 15 ssh -o BatchMode=yes -o ConnectTimeout=10 "$name" true 2>&1 >/dev/null); then
    note=""; [ "$lan" != - ] && [ "$target" = 127.0.0.1 ] && note="offers LAN $lan, not reachable from this network"
    echo "ok|$route|$hostup|$note"
    return
  fi
  seen=$(grep -a 'tunnel-auto repair' "$cache/tunnel-auto.log" 2>/dev/null | grep -aE "[ (]$name ok" | tail -n 1 | awk '{print $2, substr($3, 1, 5)}')
  if [ "$id" = - ]; then
    note="its tunnel is gone from the account; run bn run tunnel-host there"
  elif [ "$hostup" = no ]; then
    note="nothing is hosting its tunnel: the machine is off or asleep, its WSL stopped, or devtunnel is signed out there"
  elif [ "$target" = 127.0.0.1 ] && ! bn_tunnel_listening "$p" 127.0.0.1; then
    note="the local forward is not listening (tmux window bn-tunnels:tunnel-$name)"
  else
    note="ssh: $(printf '%s\n' "$err" | tail -n 1)"
    case "$err" in *"Permission denied"*) note="$note (this machine's key is not on the GitHub account $name trusts, or $name has not re-read the keys: bn run tunnel-host there)" ;; esac
  fi
  # Its own report, when there is one, beats a guess from this side.
  r=$(report "$name")
  case "$r" in
    '') [ -n "$seen" ] && note="$note; last ok $seen" ;;
    "it reported itself fine"*) note="$note; $r" ;;
    *) note="$r"; [ "$hostup" = no ] && note="$note (nothing is hosting its tunnel)" ;;
  esac
  echo "DOWN|$route|$hostup|$note"
}

n=0
while read -r id name hc lan; do
  [ -n "$id" ] || continue
  [ "$(lower "$name")" = "$(lower "$ME")" ] && continue
  n=$((n + 1))
  check "$id" "$name" "$hc" "$lan" > "$tmp/$n" 2>/dev/null &
  echo "$name" > "$tmp/$n.name"
done <<EOF
$listed
EOF
wait

echo
if [ "$n" = 0 ]; then
  echo "  no other hosts on this account. On a machine to reach: bn run tunnel-setup"
  exit 0
fi
printf '  %-20s %-5s %-24s %-8s %s\n' MACHINE STATE ROUTE HOST-END NOTE
i=1
while [ "$i" -le "$n" ]; do
  IFS='|' read -r state route hostup note < "$tmp/$i"
  printf '  %-20s %-5s %-24s %-8s %s\n' "$(cat "$tmp/$i.name")" "$state" "$route" "$hostup" "$note"
  i=$((i + 1))
done
