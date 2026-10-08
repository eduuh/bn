#!/bin/sh
# mesh -- put a machine on the bn mesh with plain ssh, and keep both directions working.
#
#   bn machine join <name> <user@host[:port]>    once, from the machine you are on
#   bn machine leave <name>
#   bn machine mesh                              how every node looks from here (reads only)
#   bn run mesh                                  the keeper (tunnel-auto runs it every pass)
#
# A node is any machine this one reaches with plain ssh: a VPS, a box on the LAN, a Cloud PC with
# its own sshd. No Dev Tunnels account. Joining:
#   1. writes `# >>> bn mesh <name> >>>` into ~/.ssh/config and proves `ssh <name> true`;
#   2. trusts the node's own key in ~/.ssh/authorized_keys (a managed block);
#   3. holds `ssh -R` open from the node's loopback to this machine's sshd (tmux window
#      bn-tunnels:mesh-<name>), so the node can reach back even when this machine is behind NAT;
#   4. writes a block for this machine into the node's ~/.ssh/config pointing at that forward.
# After that `bn agent msg` reaches agents on either side, and replies come back the same way.
#
# --back <user@host[:port]>: this machine is reachable directly at that address, so no reverse
# forward; the node's block for this machine points there instead. --no-back: one way only.
#
# The keeper (no arguments, or `keep`) restarts a dead forward window, the local sshd it lands on,
# and a stale shared connection, then writes ~/.cache/bn/mesh.status. It is idempotent and quiet
# when everything works.
set -eu

. "$(dirname "$0")/lib/mesh.sh"
SELF_SCRIPT="$(cd "$(dirname "$0")" && pwd -P)/$(basename "$0")"
STATUS="$HOME/.cache/bn/mesh.status"
ME=$(bn_mesh_self)

say() { echo "mesh: $*"; }
die() { echo "mesh: $*" >&2; exit 1; }

# The remote side of every call starts with this: a non-login shell does not read the profile.
RPATH='export PATH="$HOME/.bn/bin:$HOME/.bin:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH";'

# remote_set_block <node> <name> <block> -- replace a managed block in the node's ~/.ssh/config. The
# block travels on stdin and the edit is plain awk, so it works whatever bn the node has.
remote_set_block() {
  printf '%s\n' "$3" | bn_tunnel_timeout 20 ssh -o BatchMode=yes "$1" "
    set -e; mkdir -p ~/.ssh; chmod 700 ~/.ssh; touch ~/.ssh/config; chmod 600 ~/.ssh/config
    t=\$(mktemp); awk -v b='# >>> bn mesh $2 >>>' -v e='# <<< bn mesh $2 <<<' '\$0==b{skip=1} !skip{print} \$0==e{skip=0}' ~/.ssh/config > \"\$t\"
    cat >> \"\$t\"; cat \"\$t\" > ~/.ssh/config; rm -f \"\$t\""
}

window_of() { tmux list-windows -a -F '#{session_name}:#{window_name}' 2>/dev/null | grep ":mesh-$1\$" | head -n 1; }

# start_window <node> -- the reverse forward's tmux window, unless one is up.
# A running window keeps the code it started with, so after a bn update it is restarted once: each
# window is tagged with the version of this script and its library it runs.
code_ver() { cat "$SELF_SCRIPT" "$(dirname "$SELF_SCRIPT")/lib/mesh.sh" 2>/dev/null | cksum | cut -d' ' -f1; }

start_window() {
  w=$(window_of "$1")
  if [ -n "$w" ]; then
    [ "$(tmux show-options -wqv -t "$w" @bn_mesh_ver 2>/dev/null)" != "$(code_ver)" ] || return 0
    tmux kill-window -t "$w" 2>/dev/null || true
    say "$1: bn's mesh code changed; restarting the reverse forward"
  fi
  sess=$(bn_tunnel_session) || { say "$1: no tmux; run the forward by hand: sh '$SELF_SCRIPT' serve $1"; return 1; }
  tmux new-window -d -t "$sess" -n "mesh-$1" "sh '$SELF_SCRIPT' serve '$1'"
  tmux set-option -w -t "$sess:mesh-$1" @bn_mesh_ver "$(code_ver)" 2>/dev/null || true
  say "$1: reverse forward started in tmux window $sess:mesh-$1"
}

# back_ok <node> -- the node can ssh back to this machine.
back_ok() { bn_mesh_ssh "$1" "ssh -o BatchMode=yes -o ConnectTimeout=6 '$ME' true" >/dev/null 2>&1; }

# fwd_ok <node> -- this machine can ssh to the node; a stale shared connection gets one reset.
# A frozen master (the laptop slept) still "works": each ssh waits 10s for its hello, then falls
# back to a fresh connection. So a master that does not answer `-O check` at once is reset first.
fwd_ok() {
  bn_tunnel_timeout 3 ssh -O check "$1" >/dev/null 2>&1; [ $? -ne 124 ] || bn_mesh_reset_master "$1"
  bn_mesh_ssh "$1" true >/dev/null 2>&1 && return 0
  bn_mesh_reset_master "$1"
  bn_mesh_ssh "$1" true >/dev/null 2>&1
}

cmd_join() {
  name=""; target=""; back=reverse
  while [ $# -gt 0 ]; do
    case "$1" in
      --back) [ $# -ge 2 ] || die "--back needs user@host[:port]"; back=$2; shift ;;
      --back=*) back=${1#--back=} ;;
      --no-back) back=none ;;
      -*) die "unknown option $1" ;;
      *) if [ -z "$name" ]; then name=$1; elif [ -z "$target" ]; then target=$1; else die "too many arguments"; fi ;;
    esac
    shift
  done
  [ -n "$name" ] && [ -n "$target" ] || die "usage: bn machine join <name> <user@host[:port]> [--back user@host[:port] | --no-back]"
  name=$(printf '%s' "$name" | tr 'A-Z' 'a-z')
  bn_mesh_valid_name "$name" || die "'$name' is not a usable name (letters, digits, dashes)"
  [ "$name" != "$ME" ] || die "'$name' is this machine's own name"
  bn_mesh_split "$target" || die "'$target' is not user@host[:port]"
  user=$_user; host=$_host; port=$_port

  # 1. the node's block here, and a working ssh to it.
  bn_mesh_set_block "$HOME/.ssh/config" "$name" "$(bn_mesh_block "$name" "$host" "$user" "$port")"
  bn_mesh_reset_master "$name"
  fwd_ok "$name" || die "cannot ssh to $name ($target) with a key. Authorize this machine's key there first: ssh-copy-id ${user:+$user@}$host${port:+ -p $port}"
  say "$name: ssh $name works"
  bn_mesh_put "$name" target "$target"
  bn_mesh_put "$name" back "$back"

  ver=$(bn_mesh_ssh "$name" "$RPATH bn --version 2>/dev/null" || true)
  if [ -n "$ver" ]; then say "$name: $ver"; else say "$name: bn is not installed there, so agents there cannot be messaged yet (install bn on $name)"; fi

  [ "$back" != none ] || { say "$name: joined one way (--no-back); $name cannot reach this machine"; return 0; }

  # 2. trust the node's key here, so it can come back.
  pub=$(bn_mesh_ssh "$name" '[ -f ~/.ssh/id_ed25519 ] || ssh-keygen -q -t ed25519 -N "" -f ~/.ssh/id_ed25519 </dev/null; cat ~/.ssh/id_ed25519.pub') \
    || die "$name: could not read its ssh key"
  bn_mesh_set_key "$name" "$pub"
  say "$name: its key may now log in here"

  # 3 and 4. the way back.
  if [ "$back" = reverse ]; then
    lport=$(bn_mesh_local_sshd_port) || die "no sshd on this machine for $name to come back to. On macOS: System Settings > General > Sharing > Remote Login; on Linux: sudo apt-get install -y openssh-server"
    rport=$(bn_mesh_rport "$name")
    bn_mesh_put "$name" lport "$lport"
    bn_mesh_put "$name" rport "$rport"
    remote_set_block "$name" "$ME" "$(bn_mesh_block "$ME" 127.0.0.1 "$(id -un)" "$rport" "bn-mesh-$ME")" \
      || die "$name: could not write its ssh config"
    # A new join may land on a different sshd here; forget the old host key there.
    bn_mesh_ssh "$name" "ssh-keygen -R bn-mesh-$ME >/dev/null 2>&1; ssh-keygen -R '[bn-mesh-$ME]:$rport' >/dev/null 2>&1; true" || true
    w=$(window_of "$name"); [ -z "$w" ] || tmux kill-window -t "$w" 2>/dev/null || true
    start_window "$name" || true
    i=0; until back_ok "$name"; do
      i=$((i + 1)); [ "$i" -ge 15 ] && die "$name: the reverse forward is up but '$name' still cannot ssh to $ME; see tmux window bn-tunnels:mesh-$name"
      sleep 2
    done
  else
    bn_mesh_split "$back" || die "--back '$back' is not user@host[:port]"
    remote_set_block "$name" "$ME" "$(bn_mesh_block "$ME" "$_host" "$_user" "$_port")" || die "$name: could not write its ssh config"
    back_ok "$name" || die "$name cannot ssh to this machine at $back"
  fi
  say "$name: ssh $ME from $name works"
  say "joined: agents on $ME and $name can now message each other (bn agent msg <repo:branch> \"...\")"
}

cmd_leave() {
  [ $# -eq 1 ] || die "usage: bn machine leave <name>"
  name=$1
  w=$(window_of "$name"); [ -z "$w" ] || { tmux kill-window -t "$w" 2>/dev/null || true; say "$name: reverse forward stopped"; }
  if bn_mesh_ssh "$name" true >/dev/null 2>&1; then
    remote_set_block "$name" "$ME" "" && say "$name: removed $ME from its ssh config" || say "$name: could not edit its ssh config"
  else
    say "$name: not reachable; its ssh config still names $ME"
  fi
  bn_mesh_reset_master "$name"
  bn_mesh_set_block "$HOME/.ssh/config" "$name" ""
  bn_mesh_set_key "$name" ""
  git config --file "$BN_MESH_CONF" --remove-section "mesh.$name" 2>/dev/null || true
  say "$name: left the mesh"
}

# serve <node> -- the reverse forward, run in its tmux window. Comes straight back after a drop,
# backs off to at most 15s while the node is unreachable: a failed try is one cheap ssh, and a
# longer cap kept a rebooted node unreachable for minutes after its sshd was back (2026-10-08).
cmd_serve() {
  name=$1
  lport=$(bn_mesh_get "$name" lport); rport=$(bn_mesh_get "$name" rport)
  [ -n "$lport" ] && [ -n "$rport" ] || die "$name: no reverse forward configured (bn machine join $name ...)"
  delay=2
  while :; do
    start=$(date +%s)
    echo "mesh: $(date '+%H:%M:%S') forwarding $name:127.0.0.1:$rport -> 127.0.0.1:$lport"
    ssh -N -o BatchMode=yes -o ExitOnForwardFailure=yes -o ConnectTimeout=10 \
      -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -o ControlMaster=no -o ControlPath=none \
      -R "127.0.0.1:$rport:127.0.0.1:$lport" "$name" 2>&1 | tail -n 5 || true
    if [ $(( $(date +%s) - start )) -ge 60 ]; then delay=2; fi
    # The usual reason a fresh forward fails: the node still holds the port for a session that died
    # with this machine's sleep (its sshd never times idle sessions out by default). When nothing
    # answers through that port, the holder is dead; free it.
    if ! back_ok "$name"; then
      bn_mesh_ssh "$name" "command -v ss >/dev/null || exit 0
        for p in \$(ss -Htlnp 'sport = :$rport' 2>/dev/null | grep -o 'pid=[0-9]*' | cut -d= -f2 | sort -u); do
          case \$(cat /proc/\$p/comm 2>/dev/null) in sshd*) kill -9 \$p 2>/dev/null && echo \"mesh: freed port $rport (stale session \$p)\" ;; esac
        done" 2>/dev/null || true
    fi
    echo "mesh: $(date '+%H:%M:%S') forward to $name dropped; retrying in ${delay}s"
    sleep "$delay"
    delay=$((delay * 2)); [ "$delay" -le 15 ] || delay=15
  done
}

# keep [--quiet] -- check every node, repair what is broken, write the status file.
cmd_keep() {
  nodes=$(bn_mesh_nodes)
  [ -n "$nodes" ] || return 0
  mkdir -p "$(dirname "$STATUS")"
  tmp=$(mktemp)
  for name in $nodes; do
    back=$(bn_mesh_get "$name" back)
    fwd=down; bk=-
    fwd_ok "$name" && fwd=ok
    if [ "$back" = reverse ]; then
      lport=$(bn_mesh_get "$name" lport)
      if [ -n "$lport" ] && ! bn_tunnel_listening "$lport"; then
        [ "$lport" = 22 ] && say "$name: the system sshd is not listening on 22; turn Remote Login back on" \
          || { bn_mesh_user_sshd && say "$name: restarted the local sshd on $lport"; }
      fi
      start_window "$name" || true
      bk=down
      if [ "$fwd" = ok ]; then
        if back_ok "$name"; then bk=ok
        else
          # A window older than two minutes whose forward does not answer is stuck; restart it.
          w=$(window_of "$name")
          age=$(( $(date +%s) - $(tmux display-message -p -t "$w" '#{window_activity}' 2>/dev/null || date +%s) ))
          if [ -n "$w" ] && [ "$age" -ge 120 ]; then
            tmux kill-window -t "$w" 2>/dev/null || true
            start_window "$name" || true
          fi
        fi
      fi
    elif [ "$back" != none ] && [ "$fwd" = ok ]; then
      back_ok "$name" && bk=ok || bk=down
    fi
    printf '%s fwd=%s back=%s %s\n' "$name" "$fwd" "$bk" "$(date +%s)" >> "$tmp"
  done
  mv "$tmp" "$STATUS"
}

cmd_status() {
  nodes=$(bn_mesh_nodes)
  [ -n "$nodes" ] || { say "no mesh nodes yet. Join one: bn machine join <name> <user@host[:port]>"; return 0; }
  echo "mesh: $ME"
  for name in $nodes; do
    target=$(bn_mesh_get "$name" target); back=$(bn_mesh_get "$name" back)
    fwd=down; bk=-; ver=""; agents=""
    if fwd_ok "$name"; then
      fwd=ok
      ver=$(bn_mesh_ssh "$name" "$RPATH bn --version 2>/dev/null | awk '{print \$2}'" 2>/dev/null || true)
      agents=$(bn_mesh_ssh "$name" "$RPATH bn agent live --json 2>/dev/null" 2>/dev/null \
        | grep -o '"has_window": *true' | wc -l | tr -d ' ' || true)
    fi
    case "$back" in
      none) bk=off ;;
      *) [ "$fwd" = ok ] && { back_ok "$name" && bk=ok || bk=down; } ;;
    esac
    w=""; [ "$back" != reverse ] || { w=$(window_of "$name"); w=${w:-"(no window)"}; }
    printf '  %-16s %-28s to:%-5s back:%-5s bn:%-8s agents:%-3s %s\n' "$name" "$target" "$fwd" "$bk" "${ver:--}" "${agents:--}" "$w"
  done
}

case "${1:-keep}" in
  join) shift; cmd_join "$@" ;;
  leave) shift; cmd_leave "$@" ;;
  serve) shift; cmd_serve "$@" ;;
  keep) cmd_keep ;;
  status) cmd_status ;;
  *) die "usage: mesh.sh join|leave|status|keep|serve" ;;
esac
