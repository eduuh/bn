# Sourced helper for mesh.sh: plain-ssh mesh nodes. A node is any machine this one reaches with
# plain ssh (a VPS, a box on the LAN, a Cloud PC with its own sshd), no Dev Tunnels account needed.
# Source it from a job:  . "$(dirname "$0")/lib/mesh.sh"   (it sources lib/tunnel.sh too)
#
# A node is a managed block in ~/.ssh/config, the same shape as tunnel-connect's:
#   # >>> bn mesh <name> >>>  ...  # <<< bn mesh <name> <<<
# so `bn agent msg`'s relay, `bn machine tunnels` and tunnel-auto treat both kinds alike.
#
# The machine that joins is often behind NAT (a laptop), and the node could not ssh back to answer.
# So join also holds a reverse forward open (ssh -R) from the node's loopback to this machine's
# sshd, and writes a block for this machine into the node's ssh config that points at it.
#
# POSIX sh; never run directly.

. "$(dirname "$0")/lib/tunnel.sh"

BN_MESH_CONF="${BN_MESH_CONF:-$HOME/.config/bn/mesh.conf}"

# bn_mesh_self -- this machine's name on the mesh: its nickname (bn.tunnelNick), else its short
# hostname; lowercase, so the alias other machines type is the same everywhere.
bn_mesh_self() {
  _n=$(bn_tunnel_nick)
  [ -n "$_n" ] || _n=$(hostname -s 2>/dev/null || hostname)
  printf '%s' "$_n" | tr 'A-Z' 'a-z' | tr -c 'a-z0-9-' '-'
}

# bn_mesh_valid_name <name> -- letters, digits and dashes, 1-32 of them, starting with one of the
# first two. The name goes into ssh config, tmux window names and remote commands unquoted.
bn_mesh_valid_name() {
  printf '%s' "$1" | grep -qE '^[a-z0-9][a-z0-9-]{0,31}$'
}

# bn_mesh_get <name> <key> / bn_mesh_put <name> <key> <value> -- the saved node record.
bn_mesh_get() { git config --file "$BN_MESH_CONF" --get "mesh.$1.$2" 2>/dev/null || true; }
bn_mesh_put() {
  mkdir -p "$(dirname "$BN_MESH_CONF")"
  git config --file "$BN_MESH_CONF" "mesh.$1.$2" "$3"
}

# bn_mesh_nodes -- every node joined from here, one name per line.
bn_mesh_nodes() {
  git config --file "$BN_MESH_CONF" --get-regexp '^mesh\..*\.target$' 2>/dev/null \
    | awk '{ sub(/^mesh\./, "", $1); sub(/\.target$/, "", $1); print $1 }'
}

# bn_mesh_split <user@host[:port]> -- sets _user _host _port (port 22 when not given).
bn_mesh_split() {
  _t=$1; _user=""; _port=22
  case "$_t" in *@*) _user=${_t%%@*}; _t=${_t#*@} ;; esac
  case "$_t" in *:*) _port=${_t##*:}; _t=${_t%:*} ;; esac
  _host=$_t
  [ -n "$_host" ] && printf '%s' "$_port" | grep -qE '^[0-9]+$'
}

# bn_mesh_block <name> <hostname> <user> <port> [host-key-alias] -- the ssh config block for a
# node. One shared connection per node (ControlMaster), and keep-alives so a dead link is noticed
# within a minute instead of hanging the next ssh.
bn_mesh_block() {
  printf '# >>> bn mesh %s >>>\nHost %s\n  HostName %s\n' "$1" "$1" "$2"
  [ -z "$3" ] || printf '  User %s\n' "$3"
  printf '  Port %s\n' "$4"
  [ -z "${5:-}" ] || printf '  HostKeyAlias %s\n' "$5"
  printf '  StrictHostKeyChecking accept-new\n  ConnectTimeout 8\n'
  printf '  ServerAliveInterval 15\n  ServerAliveCountMax 3\n'
  printf '  ControlMaster auto\n  ControlPath ~/.ssh/cm-%%C\n  ControlPersist 10m\n'
  printf '# <<< bn mesh %s <<<\n' "$1"
}

# bn_mesh_strip <begin> <end> -- stdin without the block between those two lines (inclusive).
bn_mesh_strip() { awk -v b="$1" -v e="$2" '$0==b{skip=1} !skip{print} $0==e{skip=0}'; }

# bn_mesh_set_block <file> <name> <block> -- replace the node's managed block in an ssh config (or
# add it). Written in place, so a symlinked ~/.ssh/config stays a symlink.
bn_mesh_set_block() {
  mkdir -p "$(dirname "$1")"; touch "$1"; chmod 600 "$1"
  _tmp=$(mktemp)
  bn_mesh_strip "# >>> bn mesh $2 >>>" "# <<< bn mesh $2 <<<" < "$1" > "$_tmp"
  [ -z "$3" ] || printf '%s\n' "$3" >> "$_tmp"
  cat "$_tmp" > "$1"; rm -f "$_tmp"
}

# bn_mesh_set_key <name> <pubkey> -- trust a node's key in ~/.ssh/authorized_keys, in a managed
# block of its own (an empty key removes it).
bn_mesh_set_key() {
  _ak="$HOME/.ssh/authorized_keys"
  mkdir -p "$HOME/.ssh"; chmod 700 "$HOME/.ssh"; touch "$_ak"; chmod 600 "$_ak"
  _tmp=$(mktemp)
  bn_mesh_strip "# >>> bn mesh $1 >>>" "# <<< bn mesh $1 <<<" < "$_ak" > "$_tmp"
  [ -z "$2" ] || printf '# >>> bn mesh %s >>>\n%s\n# <<< bn mesh %s <<<\n' "$1" "$2" "$1" >> "$_tmp"
  cat "$_tmp" > "$_ak"; rm -f "$_tmp"
}

# bn_mesh_local_sshd_port -- the port of an sshd on this machine's loopback that the reverse
# forward can land on: the system's on 22 when it listens, else bn's user-level one (tunnel-host's,
# started on demand). Empty when there is neither.
bn_mesh_local_sshd_port() {
  if bn_tunnel_listening 22 127.0.0.1 0.0.0.0 && command -v nc >/dev/null 2>&1 && nc -z -w 2 127.0.0.1 22 >/dev/null 2>&1; then
    echo 22; return 0
  fi
  _cfg="$HOME/.ssh/bn-sshd/sshd_config"
  _p=$(awk '$1 == "Port" {print $2; exit}' "$_cfg" 2>/dev/null)
  if [ -n "$_p" ] && bn_tunnel_listening "$_p"; then echo "$_p"; return 0; fi
  bn_mesh_user_sshd || return 1
  awk '$1 == "Port" {print $2; exit}' "$_cfg"
}

# bn_mesh_user_sshd -- start bn's key-only user-level sshd on loopback (no root, no Dev Tunnel).
# Shares its state directory with tunnel-host, so a machine that hosts a Dev Tunnel too runs one
# sshd, not two.
bn_mesh_user_sshd() {
  _st="$HOME/.ssh/bn-sshd"
  _sshd=$(bn_tunnel_sshd) || { bn_tunnel_sshd_unpack && _sshd=$(bn_tunnel_sshd); } || return 1
  mkdir -p "$_st"; chmod 700 "$HOME/.ssh" "$_st"
  [ -f "$_st/host_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -f "$_st/host_ed25519"
  if [ ! -f "$_st/sshd_config" ]; then
    cat > "$_st/sshd_config" <<EOF
# Written by bn mesh (the return path for mesh nodes). Loopback only.
Port $(bn_tunnel_port_for "$(bn_mesh_self)")
ListenAddress 127.0.0.1
HostKey $_st/host_ed25519
PidFile $_st/sshd.pid
AuthorizedKeysFile .ssh/authorized_keys
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
PermitRootLogin prohibit-password
AllowUsers $(id -un)
Subsystem sftp internal-sftp
ClientAliveInterval 30
ClientAliveCountMax 3
AllowTcpForwarding local
EOF
  fi
  _p=$(awk '$1 == "Port" {print $2; exit}' "$_st/sshd_config")
  bn_tunnel_listening "$_p" && return 0
  _pid=$(cat "$_st/sshd.pid" 2>/dev/null || true)
  [ -z "$_pid" ] || kill "$_pid" 2>/dev/null || true
  LD_LIBRARY_PATH="$(bn_tunnel_sshd_libs "$_sshd")" "$_sshd" -f "$_st/sshd_config" || return 1
  _i=0; until bn_tunnel_listening "$_p"; do _i=$((_i + 1)); [ "$_i" -ge 10 ] && return 1; sleep 1; done
}

# bn_mesh_rport <name> -- the port this machine's reverse forward takes on that node's loopback:
# derived from this machine's name, so two machines that join the same node do not clash.
bn_mesh_rport() {
  _p=$(bn_mesh_get "$1" rport)
  [ -n "$_p" ] || _p=$(bn_tunnel_port_for "mesh-$(bn_mesh_self)")
  echo "$_p"
}

# bn_mesh_ssh <name> <command> -- one batch ssh to a node over its shared connection.
bn_mesh_ssh() { bn_tunnel_timeout 20 ssh -o BatchMode=yes -o ConnectTimeout=8 "$1" "$2"; }

# bn_mesh_reset_master <name> -- close a node's shared connection. After a laptop sleeps the master
# socket outlives the link, and every ssh through it hangs until ServerAlive gives up.
# A frozen master does not answer `-O exit` either, so that is bounded, and then the socket goes and
# the master process (titled "ssh: <socket> [mux]") is killed.
bn_mesh_reset_master() {
  bn_tunnel_timeout 5 ssh -O exit "$1" >/dev/null 2>&1 && return 0
  _cp=$(ssh -G "$1" 2>/dev/null | awk '$1 == "controlpath" {print $2; exit}')
  [ -n "$_cp" ] && [ "$_cp" != none ] || return 0
  pkill -9 -f "ssh: $_cp \[mux\]" 2>/dev/null || true
  rm -f "$_cp"
}
