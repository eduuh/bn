#!/bin/sh
# tunnel-auto -- keep this machine's tunnels up without being asked, and say how they are.
#
# tmux runs it at server start (config/workflow/tmux.conf), so a reboot, a WSL restart or a
# `tmux kill-server` puts things back as they were:
#   - a machine that has hosted before (tunnel-host left ~/.ssh/bn-sshd/sshd_config) hosts again;
#   - a machine that has connected before (a managed `# >>> bn tunnel` block in ~/.ssh/config)
#     reconnects to every bn host on the account.
# A machine that has done neither is left alone.
#
# bn's own cron (config/crons.toml, machine-global) also runs it every 5 minutes, so after a
# reboot or a WSL restart the tunnels come back with nobody signed in: with linger on, the user's
# systemd starts at boot, and bn_tunnel_session gives a tmux server started from there a scope of
# its own.
#
# It then keeps a watchdog window `tunnel-watch` in the tmux session `bn-tunnels` that runs
# `tunnel-auto.sh --once` every BN_TUNNEL_WATCH_SECONDS (default 60). Each pass is a cheap health
# check: this host's sshd is listening and its tunnel window exists, and a plain `ssh <host> true`
# answers for every host this machine connects to (over the shared connection, so well under a
# second when it is up). The full repair (tunnel-host / tunnel-connect: restart a deaf sshd,
# recreate missing windows, reconnect a dead forward, pick up new hosts) runs only when a check
# fails or every 15 minutes, so a drop heals within about a minute without hammering GitHub or the
# Dev Tunnels service.
#
# The result of the last check is written to ~/.cache/bn/tunnels.status ("<name> ok|down" per line)
# for the tmux status bar (config/workflow/bin/tmux-tunnels.sh).
#
# Unattended: nothing here may wait for a person. The Dev Tunnels sign-in only prompts at a
# terminal (lib/tunnel.sh), so a signed-out machine logs the command to run instead.
# Output: ~/.cache/bn/tunnel-auto.log (repairs and failures only; kept to its last 2000 lines).
set -u

# Physical path: the same script reached through ~/.config/bn/repo (a symlink) or bn/main must give
# the same watchdog command, or the one-watchdog check below sees two different watchdogs.
dir=$(cd "$(dirname "$0")" && pwd -P)
cache="$HOME/.cache/bn"
log="$cache/tunnel-auto.log"
status="$cache/tunnels.status"
stamp="$cache/tunnel-auto.repaired"
mkdir -p "$cache"
exec < /dev/null

# fresh <file> <seconds> -- the file was touched within the last <seconds>. A timestamp in the
# future counts as stale: WSL's clock drifts while a laptop sleeps and jumps when corrected, and a
# `find -mmin -5` on a future-dated marker stayed true for hours, so a hung forwarder was never
# restarted (2026-09-27).
fresh() {
  [ -e "$1" ] || return 1
  _m=$(stat -c %Y "$1" 2>/dev/null || stat -f %m "$1" 2>/dev/null) || return 1
  _age=$(( $(date +%s) - _m ))
  [ "$_age" -ge 0 ] && [ "$_age" -lt "$2" ]
}

is_host=0; [ -f "$HOME/.ssh/bn-sshd/sshd_config" ] && is_host=1
hosts=$(awk '/^# >>> bn tunnel /{print $5}' "$HOME/.ssh/config" 2>/dev/null)
if [ "$is_host" = 0 ] && [ -z "$hosts" ]; then
  rm -f "$status"
  exit 0
fi

# health -- check everything once, write the status file, and fail if anything is down.
health() {
  _out=""; _bad=0
  if [ "$is_host" = 1 ]; then
    _port=$(awk '$1 == "Port" {print $2; exit}' "$HOME/.ssh/bn-sshd/sshd_config")
    if ss -tln 2>/dev/null | grep -q "127.0.0.1:$_port " \
        && tmux list-windows -a -F '#{window_name}' 2>/dev/null | grep -qx tunnel; then
      _out="${_out}host ok
"
    else
      _out="${_out}host down
"; _bad=1
    fi
  fi
  for _h in $hosts; do
    if timeout 25 ssh -o BatchMode=yes -o ConnectTimeout=15 "$_h" true >/dev/null 2>&1; then
      _out="${_out}$_h ok
"
    else
      _out="${_out}$_h down
"; _bad=1
    fi
  done
  printf '%s' "$_out" > "$status.tmp" && mv "$status.tmp" "$status"
  return "$_bad"
}

# One check-and-repair pass at a time: the watchdog, a tmux-start run and a hand run can overlap
# (seen 2026-09-27: two repairs 14s apart). A pass that loses the race skips straight to the
# watchdog part. A lock older than 20 minutes is from a pass that died and is taken over.
lock="$cache/tunnel-auto.lock"
have_lock=0
take_lock() { mkdir "$lock" 2>/dev/null && echo $$ > "$lock/pid" && have_lock=1; }
if ! take_lock; then
  # The holder's pid is in the lock. A holder that is gone (killed before its cleanup ran) or a
  # lock older than 20 minutes is taken over, so one dead pass cannot stall every later one
  # (2026-09-27: a lock left at 07:54 stopped every repair for ten minutes).
  holder=$(cat "$lock/pid" 2>/dev/null)
  if { [ -n "$holder" ] && ! kill -0 "$holder" 2>/dev/null; } || ! fresh "$lock" 1200; then
    rm -rf "$lock" && take_lock
  fi
fi
[ "$have_lock" = 1 ] && trap 'rm -rf "$lock"' EXIT && trap 'exit 1' HUP INT TERM

# A repair runs when a check fails or the last one is 15 minutes old, whoever runs this: the
# watchdog, tmux at server start, or the tunnel-auto timer (every 5 minutes, so tunnels come back
# after a reboot or WSL restart even before anyone opens tmux).
repair_due=0
fresh "$stamp" 900 || repair_due=1
[ "$have_lock" = 1 ] && { health || repair_due=1; }

if [ "$have_lock" = 1 ] && [ "$repair_due" = 1 ]; then
  if [ -f "$log" ] && [ "$(wc -l < "$log")" -gt 4000 ]; then
    tail -n 2000 "$log" > "$log.tmp" && mv "$log.tmp" "$log"
  fi
  # A client whose ssh check failed gets its `devtunnel connect` restarted: the local listener can
  # stay up while the process behind it has hung (2026-09-27: 127.0.0.1:2222 listening with 12
  # connections queued and none accepted), and tunnel-connect only restarts a window whose port is
  # not listening. At most once per host every 5 minutes, so a host that is really down is not
  # churned every pass.
  for _h in $(awk '$2 == "down" && $1 != "host" {print $1}' "$status"); do
    _mark="$cache/tunnel-restarted.$_h"
    fresh "$_mark" 300 && continue
    for _w in $(tmux list-windows -a -F '#{session_name}:#{window_name}' 2>/dev/null | grep ":tunnel-$_h\$"); do
      tmux kill-window -t "$_w" 2>/dev/null && echo "--- $(date '+%F %T') restarted $_w (ssh check failed)" >> "$log"
    done
    touch "$_mark"
  done
  {
    echo "--- $(date '+%F %T') tunnel-auto repair ($(tr '\n' ' ' < "$status"))"
    [ "$is_host" = 1 ] && { sh "$dir/tunnel-host.sh" || echo "tunnel-auto: tunnel-host failed ($?)"; }
    [ -n "$hosts" ] && { sh "$dir/tunnel-connect.sh" || echo "tunnel-auto: tunnel-connect failed ($?)"; }
  } >> "$log" 2>&1
  touch "$stamp"
  # tunnel-connect may have added hosts; re-read them and record the result of the repair.
  hosts=$(awk '/^# >>> bn tunnel /{print $5}' "$HOME/.ssh/config" 2>/dev/null)
  health || echo "--- $(date '+%F %T') still down after repair: $(grep ' down$' "$status" | tr '\n' ' ')" >> "$log"
fi

[ "${1:-}" = --once ] && exit 0
every="${BN_TUNNEL_WATCH_SECONDS:-60}"
case "$every" in ''|*[!0-9]*) every=60 ;; esac
. "$dir/lib/tunnel.sh"
sess=$(bn_tunnel_session) || exit 0
want="while :; do sleep $every; sh '$dir/tunnel-auto.sh' --once; done"
# Exactly one watchdog: keep the first window running this command, close every other
# `tunnel-watch` (an older version, another interval, or a duplicate from a second start).
have=""
for w in $(tmux list-windows -t "$sess" -F '#{window_id}:#{window_name}' 2>/dev/null | awk -F: '$2 == "tunnel-watch" {print $1}'); do
  cmd=$(tmux display-message -p -t "$w" '#{pane_start_command}' 2>/dev/null)
  if [ -z "$have" ] && { [ "$cmd" = "$want" ] || [ "$cmd" = "\"$want\"" ]; }; then
    have=$w
  else
    tmux kill-window -t "$w" 2>/dev/null
  fi
done
if [ -z "$have" ]; then
  tmux new-window -d -t "$sess" -n tunnel-watch "$want"
  echo "--- $(date '+%F %T') watchdog started in tmux window $sess:tunnel-watch (every ${every}s)" >> "$log"
fi
