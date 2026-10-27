#!/bin/sh
# remote-job -- run a long command on a tunnel host, detached, and follow it from here.
#
# A long job (a monorepo build, a test suite) must not live in the foreground of an ssh session: the
# session is only as reliable as the tunnel. This starts the command in a window of the host's
# tmux session `bn-jobs` with its output in a log, so it runs to completion whatever the link does;
# following just reads that log and survives reconnects.
#
# Run on the machine you connect FROM (needs the `Host <machine>` entry from tunnel-connect):
#   sh remote-job.sh <machine> start  <name> <command...>   start it (refuses if <name> is running)
#   sh remote-job.sh <machine> follow <name>                 stream the log until it exits; returns
#                                                            the job's exit code
#   sh remote-job.sh <machine> status <name>                 running / exited N / unknown
# or via bn run: BN_TUNNEL_TO=<machine> BN_JOB=start|follow|status BN_JOB_NAME=<name>
#                BN_JOB_CMD='<command>' bn run remote-job
#
# The command runs in the host's login shell (bn, nvm, yarn on PATH), from the host's $HOME unless
# it cd's. Logs: ~/.bn-remote-jobs/<name>.log on the host; a local copy under
# ~/.cache/bn-remote-jobs/ lets `follow` resume after a drop without repeating lines.
set -u

TO="${1:-${BN_TUNNEL_TO:-}}"; [ $# -gt 0 ] && shift
ACTION="${1:-${BN_JOB:-}}"; [ $# -gt 0 ] && shift
NAME="${1:-${BN_JOB_NAME:-}}"; [ $# -gt 0 ] && shift
CMD="${*:-${BN_JOB_CMD:-}}"

usage() { echo "usage: remote-job.sh <machine> start|follow|status <name> [command...]" >&2; exit 2; }
[ -n "$TO" ] && [ -n "$ACTION" ] && [ -n "$NAME" ] || usage
case "$NAME" in *[!A-Za-z0-9_.-]*) echo "remote-job: bad job name '$NAME' (letters, digits, _ . -)" >&2; exit 2 ;; esac

RDIR='$HOME/.bn-remote-jobs'
RLOG="$RDIR/$NAME.log"
LOCAL="$HOME/.cache/bn-remote-jobs/$TO-$NAME.log"

remote() { ssh -o BatchMode=yes "$TO" "$@"; }

status() {
  remote "if [ -f $RLOG ] && grep -q '^--- exited [0-9]* ---\$' $RLOG; then grep '^--- exited ' $RLOG | tail -n 1; elif \${SHELL:-sh} -lc 'tmux list-windows -t bn-jobs -F \"#W\"' 2>/dev/null | grep -qx '$NAME'; then echo running; else echo unknown; fi"
}

case "$ACTION" in
  start)
    [ -n "$CMD" ] || usage
    st=$(status 2>/dev/null) || { echo "remote-job: cannot reach $TO (is tunnel-connect running?)" >&2; exit 1; }
    [ "$st" = running ] && { echo "remote-job: '$NAME' is already running on $TO" >&2; exit 1; }
    # The command travels on stdin: no quoting of the user's command through two shells.
    printf '%s\n' "$CMD" | remote "mkdir -p $RDIR && cat > $RDIR/$NAME.cmd && : > $RLOG" || exit 1
    # A runner file, also over stdin, runs the command with the PATH an interactive login shell on
    # the host would have (tmux windows get the tmux server's environment, and on zsh setups PATH
    # for nvm/bn lives in .zshrc, which only interactive shells read). Asking once for PATH keeps
    # that shell's startup noise out of the job log. Then it appends the exit marker.
    printf '%s\n' '#!/bin/sh' \
      "P=\$(\"\${SHELL:-sh}\" -lic 'printf \"\\n%s\" \"\$PATH\"' 2>/dev/null | tail -n 1)" \
      "[ -n \"\$P\" ] && PATH=\"\$P\" && export PATH" \
      "sh \"\$HOME/.bn-remote-jobs/$NAME.cmd\" >> \"\$HOME/.bn-remote-jobs/$NAME.log\" 2>&1" \
      "echo \"--- exited \$? ---\" >> \"\$HOME/.bn-remote-jobs/$NAME.log\"" \
      | remote "cat > $RDIR/$NAME.run" || exit 1
    remote "\${SHELL:-sh} -lc 'tmux has-session -t bn-jobs 2>/dev/null || tmux new-session -d -s bn-jobs; tmux new-window -d -t bn-jobs -n $NAME \"sh \$HOME/.bn-remote-jobs/$NAME.run\"'" || exit 1
    mkdir -p "$(dirname "$LOCAL")"; : > "$LOCAL"
    echo "remote-job: started '$NAME' on $TO (tmux bn-jobs:$NAME). Follow: sh $0 $TO follow $NAME"
    ;;
  follow)
    mkdir -p "$(dirname "$LOCAL")"; touch "$LOCAL"
    wait_s=5
    while :; do
      seen=$(wc -l < "$LOCAL" | tr -d ' ')
      # Stream from the first unseen line; stop once the exit marker is in the log.
      remote "tail -n +$((seen + 1)) -F $RLOG 2>/dev/null & t=\$!; until grep -q '^--- exited [0-9]* ---\$' $RLOG 2>/dev/null; do sleep 2; done; sleep 1; kill \$t 2>/dev/null" | tee -a "$LOCAL"
      if grep -q '^--- exited [0-9]* ---$' "$LOCAL"; then
        rc=$(grep '^--- exited ' "$LOCAL" | tail -n 1 | tr -dc '0-9')
        exit "${rc:-1}"
      fi
      echo "remote-job: lost $TO while following '$NAME'; resuming in ${wait_s}s" >&2
      sleep "$wait_s"; [ "$wait_s" -lt 30 ] && wait_s=$((wait_s * 2)); [ "$wait_s" -gt 30 ] && wait_s=30
    done
    ;;
  status) status ;;
  *) usage ;;
esac
