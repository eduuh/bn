#!/bin/sh
# remote-app -- run a repo's dev server on a tunnel host, in a warm pool slot, and forward its ports
# to this machine.
#
# Run on the machine you connect FROM (needs the `Host <machine>` entry from tunnel-connect):
#   sh remote-app.sh <machine> start|stop|status [repo]      (repo defaults to $BN_APP_REPO)
# or via bn run: BN_TUNNEL_TO=<machine> BN_APP=start|stop|status bn run remote-app
#
# start:
#   1. leases a warm pool slot for <repo> on the host (branch `remote-app`; reused if it is already
#      leased there) so the deps and build are already in place;
#   2. runs the repo's `bn build start` (its start.sh) in window `app-<repo>` of the host's tmux
#      session `bn-jobs`, where it keeps running whatever the link does;
#   3. waits until every port in BN_APP_PORTS listens on the host;
#   4. forwards those ports to the SAME port numbers here, in window `app-<machine>` of the local
#      tmux session `bn-tunnels` (ssh -N -L loop that reconnects). Same numbers because apps often
#      reference their own ports (a dev server may load assets from https://localhost:<DEV_SERVER_PORT>/) and
#      sign-in redirects back to them. A port already taken here is reported and not forwarded;
#      stop the local server first, or use the host's own browser.
# stop: stops the host's app window (start.sh frees its ports on exit), returns the slot to the
#   pool, and closes the local forward.
# status: slot, whether each port listens on the host, and whether each is forwarded here.
#
# Env: BN_APP_PORTS (default 3000,3001), BN_APP_WAIT_SECONDS (default 900: a cold dev server
# compiles for minutes).
set -u

TO="${1:-${BN_TUNNEL_TO:-}}"; [ $# -gt 0 ] && shift
ACTION="${1:-${BN_APP:-}}"; [ $# -gt 0 ] && shift
REPO="${1:-${BN_APP_REPO:-}}"
PORTS=$(printf '%s' "${BN_APP_PORTS:-3000,3001}" | tr ',' ' ')
WAIT="${BN_APP_WAIT_SECONDS:-900}"

say() { echo "remote-app: $*"; }
die() { echo "remote-app: $*" >&2; exit 1; }
[ -n "$TO" ] && [ -n "$ACTION" ] && [ -n "$REPO" ] || die "usage: remote-app.sh <machine> start|stop|status <repo> (or set BN_APP_REPO)"
case "$REPO" in *[!A-Za-z0-9_.-]*) die "bad repo name '$REPO'" ;; esac
for p in $PORTS; do case "$p" in *[!0-9]*|'') die "'$p' is not a port" ;; esac; done
case "$WAIT" in ''|*[!0-9]*) die "BN_APP_WAIT_SECONDS must be a number" ;; esac

# Remote commands get bn and node on PATH: our tunnel sshd runs without PAM, so a non-login ssh
# command has neither ~/.local/bin nor nvm. node matters beyond the app: a repo post-checkout hook
# may run node, and without it `git switch` exits non-zero and `bn wt pool get` reports a failed switch
# that actually succeeded.
remote() { ssh -o BatchMode=yes -o ConnectTimeout=30 "$TO" "export PATH=\$HOME/.local/bin:\$PATH; [ -s \$HOME/.nvm/nvm.sh ] && . \$HOME/.nvm/nvm.sh >/dev/null 2>&1; $*"; }
win="app-$REPO"
fwd="app-$TO"

# The slot this machine's app uses on the host: the one leased on branch remote-app, if any.
slot_of() { remote "cd ~/projects/worktree/$REPO/_pool/1 2>/dev/null && bn wt pool status 2>/dev/null" | awk '/remote-app/ {gsub(/[][]/, "", $1); print $1; exit}'; }

ports_up() { # every port listens on the host
  for p in $PORTS; do remote "ss -tln | grep -q '127.0.0.1:$p \|\*:$p \|\[::\]:$p \|0.0.0.0:$p '" || return 1; done
}

forward() {
  . "$(dirname "$0")/lib/tunnel.sh"
  sess=$(bn_tunnel_session) || { say "no tmux here: forward by hand with ssh -N -L"; return 0; }
  args=""; skipped=""
  for p in $PORTS; do
    if ss -tln 2>/dev/null | grep -q "127.0.0.1:$p \|\[::1\]:$p \|0.0.0.0:$p \|\*:$p "; then
      skipped="$skipped $p"
    else
      args="$args -L $p:127.0.0.1:$p"
    fi
  done
  [ -n "$skipped" ] && say "port(s)$skipped already in use here; not forwarded (stop the local server, or use $TO's own browser)"
  [ -n "$args" ] || return 0
  tmux kill-window -t "$sess:$fwd" 2>/dev/null
  tmux new-window -d -t "$sess" -n "$fwd" "while :; do ssh -N -o ControlPath=none -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=4$args $TO; echo 'remote-app: forward to $TO dropped; reconnecting in 5s'; sleep 5; done"
  say "forwarding$(printf '%s' "$args" | sed 's/ -L \([0-9]*\):[^ ]*/ \1/g') from $TO in tmux window $sess:$fwd"
}

case "$ACTION" in
  start)
    remote "true" || die "cannot reach $TO (is tunnel-connect running?)"
    n=$(slot_of)
    if [ -z "$n" ]; then
      path=$(remote "cd ~/projects/worktree/$REPO/_pool/1 && bn wt pool get remote-app --holder remote-app-$(hostname -s) --path 2>/dev/null" | tail -n 1)
      [ -n "$path" ] && remote "test -d '$path'" || die "no warm $REPO slot could be leased on $TO"
      say "leased $path on $TO"
    else
      path=$(remote "echo ~/projects/worktree/$REPO/_pool/$n")
      say "reusing slot $n ($path) on $TO"
    fi
    if remote "tmux list-windows -t bn-jobs -F '#W' 2>/dev/null | grep -qx '$win'"; then
      say "app already running in $TO bn-jobs:$win"
    else
      remote "tmux has-session -t bn-jobs 2>/dev/null || tmux new-session -d -s bn-jobs; tmux new-window -d -t bn-jobs -n '$win' 'cd \"$path\" && PATH=\$HOME/.local/bin:\$PATH bn build start; echo \"--- app exited \$? ---\"; exec \$SHELL'" \
        || die "could not start the app window on $TO"
      say "starting the dev server in $TO bn-jobs:$win (logs: ssh $TO, tmux attach -t bn-jobs)"
    fi
    waited=0
    until ports_up; do
      [ "$waited" -ge "$WAIT" ] && die "ports$(printf ' %s' $PORTS) not listening on $TO after ${WAIT}s; see $TO bn-jobs:$win"
      remote "tmux list-windows -t bn-jobs -F '#W' | grep -qx '$win'" || die "the app window on $TO closed; see its output there"
      sleep 10; waited=$((waited + 10))
    done
    say "ports$(printf ' %s' $PORTS) listening on $TO (${waited}s)"
    forward
    say "open: https://localhost:$(printf '%s' "$PORTS" | awk '{print $1}')"
    ;;
  stop)
    remote "tmux kill-window -t 'bn-jobs:$win' 2>/dev/null" && say "stopped $TO bn-jobs:$win"
    n=$(slot_of)
    [ -n "$n" ] && remote "cd ~/projects/worktree/$REPO/_pool/1 && bn wt pool return $n --force" >/dev/null 2>&1 && say "returned slot $n on $TO"
    . "$(dirname "$0")/lib/tunnel.sh"
    sess=$(bn_tunnel_session 2>/dev/null) && tmux kill-window -t "$sess:$fwd" 2>/dev/null && say "closed the local forward"
    ;;
  status)
    n=$(slot_of); say "slot: ${n:-none}"
    for p in $PORTS; do
      r=down; remote "ss -tln | grep -q ':$p '" && r=listening
      l=no; ss -tln 2>/dev/null | grep -q "127.0.0.1:$p " && l=yes
      say "port $p: $TO $r, forwarded here: $l"
    done
    ;;
  *) die "unknown action '$ACTION' (start|stop|status)" ;;
esac
