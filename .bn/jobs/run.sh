#!/bin/sh
# bn — the `run` recipe: boot THIS worktree's dashboard so an agent can drive the app it just built.
#
# `bn build` proves the code compiles; this proves it runs, and hands back a URL to drive. The
# explore pass calls it (`bn build run`) before touching anything.
#
# Deliberately NOT the machine's dashboard. `bn serve --daemon` owns 7770 and the user is looking
# at it; restarting that from an agent's worktree would yank the page out from under them and, on
# a broken build, leave them with no dashboard at all. So this builds the worktree and serves that
# binary on its own port, leaving 7770 alone.
#
# Env knobs:
#   BN_RUN_PORT     port to bind                      (default: 7791)
#   BN_RUN_TIMEOUT  seconds to wait for health        (default: 90)
set -eu

repo=$(git rev-parse --show-toplevel)
cd "$repo"

PORT="${BN_RUN_PORT:-7791}"
TIMEOUT="${BN_RUN_TIMEOUT:-90}"
url="http://127.0.0.1:$PORT"
logfile="$repo/target/bn-run-$PORT.log"
pidfile="$repo/target/bn-run-$PORT.pid"

# shellcheck source=scripts/install-toolchain.sh
. "$repo/scripts/install-toolchain.sh"
bn_install_activate_cargo "$repo" || {
  echo "bn run: no usable Cargo toolchain found" >&2
  exit 1
}

ui="crates/bn-web/ui"
if [ -f "$ui/package.json" ]; then
  command -v npm >/dev/null 2>&1 || { echo "bn run: npm not found" >&2; exit 1; }
  echo "== bn-web UI (npm ci + build) =="
  (cd "$ui" && npm ci --silent && npm run build)
fi

echo "== cargo build (bn + bn-web) =="
cargo build -p bn-cli -p bn-web

bin="$repo/target/debug/bn"
[ -x "$bin" ] || { echo "bn run: no binary at $bin after build" >&2; exit 1; }

# Reclaim the port from a previous pass. Only ever this port, never 7770.
if [ -f "$pidfile" ]; then
  old=$(cat "$pidfile" 2>/dev/null || true)
  if [ -n "$old" ] && kill -0 "$old" 2>/dev/null; then
    echo "== stopping the previous run server (pid $old) =="
    kill "$old" 2>/dev/null || true
    sleep 1
  fi
  rm -f "$pidfile"
fi

echo "== serving $url =="
mkdir -p "$repo/target"
nohup "$bin" serve --port "$PORT" >"$logfile" 2>&1 &
echo $! >"$pidfile"

# Health-gate the handoff. Returning a URL that isn't up yet makes the first thing an agent does
# load the page, a false failure, and it would file that as a bug.
i=0
while [ "$i" -lt "$TIMEOUT" ]; do
  if curl -sf -o /dev/null "$url/" 2>/dev/null; then
    cat <<EOF

bn is running.

  URL     $url
  commit  $(git rev-parse --short HEAD)
  logs    $logfile
  stop    kill \$(cat $pidfile)

Drive this instance, not 7770. 7770 is the user's own dashboard.
EOF
    exit 0
  fi
  i=$((i + 1))
  sleep 1
done

echo "bn run: $url did not come up within ${TIMEOUT}s — last log lines:" >&2
tail -20 "$logfile" >&2 || true
exit 1
