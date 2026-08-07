#!/bin/bash
# Try the queue in a throwaway sandbox — its own BN_HOME, state dir and notes root, so nothing
# here touches your real backlog, crons, launchd or fleet. Ctrl-C cleans up.
#
#   ./try-queue.sh            # dashboard on :7793 + a few demo entries
#   ./try-queue.sh --cli      # same sandbox, but drop into a shell for the CLI instead
set -u
REPO="${BN_REPO:-$(cd "$(dirname "$0")/.." && pwd)}"
BN="$REPO/target/debug/bn"
PORT="${PORT:-7793}"
SB="$(mktemp -d)"

[ -x "$BN" ] || { echo "build first:  cd $REPO && cargo build --workspace" >&2; exit 1; }

cleanup() { pkill -f "port $PORT" 2>/dev/null; rm -rf "$SB"; }
trap cleanup EXIT INT TERM

mkdir -p "$SB/home/config" "$SB/notes" "$SB/state" "$SB/repo"
cat > "$SB/home/config/config.toml" <<EOF
[personal]
notes_root = "$SB/notes"

[queue]
capture = ["cron"]
coding = 2
research = 1
EOF
( cd "$SB/repo" && git init -q . && git -c user.email=t@t -c user.name=t commit -q --allow-empty -m init )

# BN_NOTES_DIR, not just the config key, because the notes root is what holds `tasks.txt` and
# `bn queue add` mints a task for every entry it creates. This sandbox used to write `root = ...`,
# which is not a key bn reads at all (it wants `[personal].notes_root`) — so the backlog was
# isolated by BN_STATE_DIR while every demo entry's *task* landed in the real store. The env var is
# the belt-and-braces version: it wins over config, so a future typo in the file cannot silently
# re-point the store at your real one.
export BN_HOME="$SB/home" BN_STATE_DIR="$SB/state" BN_NOTES_DIR="$SB/notes" BN_NO_AUTO_TICK=1
cd "$SB/repo" || exit 1

# A few things to look at. Shell entries so nothing launches an agent.
"$BN" queue add "fix the login bug"       --repo demo --kind ship        --priority 10  --shell "sleep 2" >/dev/null
"$BN" queue add "investigate flaky test"  --repo demo --kind investigate --priority 5   --shell "sleep 2" >/dev/null
"$BN" queue add "warm the pool"           --repo demo --kind cron        --priority -50 --shell "sleep 2" >/dev/null
"$BN" queue add "this one fails"          --repo demo --kind cron        --priority 1   --shell "exit 3"  >/dev/null

if [ "${1:-}" = "--cli" ]; then
  cat <<EOF

Sandbox ready. Nothing here touches your real machine.

  bn queue status                     what the machine can run, and what bounds it
  bn queue list --repo demo           what is waiting
  bn queue priority <id> 99 --repo demo    move it to the front
  bn queue cancel <id> --repo demo         take it back out
  bn queue tick                       run one dispatcher pass
  bn queue log                        what the dispatcher just did
  bn queue log --kind finish          only completions (one of them failed)
  bn queue timeline                   the day on an axis
  bn queue pause --reason "testing"   stop admission; tick again to see it held

Type 'exit' to tear the sandbox down.

EOF
  PATH="$(dirname "$BN"):$PATH" BN_HOME="$SB/home" BN_STATE_DIR="$SB/state" \
    BN_NOTES_DIR="$SB/notes" BN_NO_AUTO_TICK=1 "${SHELL:-/bin/bash}"
  exit 0
fi

"$BN" serve --port "$PORT" > "$SB/web.log" 2>&1 &
sleep 6
cat <<EOF

Sandbox dashboard: http://127.0.0.1:$PORT/?experimental=queue

  That URL turns the queue pages on (sticky, so you only need it once).

  /queue          queue work, raise/lower it, cancel it, pause the machine
  /queue/events   what the dispatcher has been doing, filtered
  /               "Today's queue" tab — the day from the dispatcher's view
  /today          the same, on the Today page

  Four demo entries are waiting. Nothing runs until a tick — in another terminal:

    BN_HOME=$SB/home BN_STATE_DIR=$SB/state BN_NOTES_DIR=$SB/notes $BN queue tick

  Run it a couple of times, then reload /queue/events. One entry exits 3 on purpose,
  so you can see a failure render.

Ctrl-C to tear it all down.
EOF
while true; do sleep 3600; done
