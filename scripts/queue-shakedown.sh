#!/bin/bash
# Queue shakedown. Exercises the dispatcher's decision policy, the build pool, and the
# daemon's survival rules.
#
# Isolation: own BN_HOME, own state dir, BN_NO_AUTO_TICK=1 everywhere except the two
# repair tests — and those deliberately use a successor binary that CANNOT dispatch, so
# the install guard refuses before anything touches the real launchd. The suite asserts
# the real plist is byte-identical at the end.
#
# Usage: ./scripts/queue-shakedown.sh            (uses target/debug/bn)
#        BN_SRC=/path/to/bn ./scripts/queue-shakedown.sh
set -u
REPO="$(cd "$(dirname "$0")/.." && pwd)"
SC="$(mktemp -d)"
trap 'rm -rf "$SC"' EXIT
SB="$SC/qt"
SRC="${BN_SRC:-$REPO/target/debug/bn}"
if [ ! -x "$SRC" ]; then
  echo "no bn at $SRC — run 'cargo build --workspace' first, or set BN_SRC" >&2
  exit 1
fi
PLIST="$HOME/Library/LaunchAgents/com.bn.queue.plist"
PLIST_BEFORE=$(md5 -q "$PLIST" 2>/dev/null || echo none)

PASS=0; FAIL=0

# The queue units an older bn could have installed. bn no longer writes any; this is here to prove
# a run does not start.
queue_units() {
  ls "$HOME/.config/systemd/user" 2>/dev/null | grep -E '^bn-queue(-tick)?\.(service|timer)$' | sort | tr '\n' ' '
}
UNITS_BEFORE=$(queue_units)
ok()   { PASS=$((PASS+1)); printf '  \033[32m✓\033[0m %s\n' "$1"; }
no()   { FAIL=$((FAIL+1)); printf '  \033[31m✗ %s\033[0m\n' "$1"; }
is()   { [ "$2" = "$3" ] && ok "$1" || no "$1 (want '$3', got '$2')"; }
has()  { echo "$2" | grep -q -- "$3" && ok "$1" || no "$1 (missing '$3' in: $(echo "$2" | head -2))"; }
hasnt(){ echo "$2" | grep -q -- "$3" && no "$1 (unexpected '$3')" || ok "$1"; }
sect() { printf '\n\033[1m%s\033[0m\n' "$1"; }

fresh() {   # a clean sandbox + repo
  rm -rf "$SB"; mkdir -p "$SB/home/config" "$SB/notes" "$SB/state" "$SB/repo" "$SB/marks" "$SB/run"
  # A pinned hostname and its machine profile, and both halves matter.
  #
  # Without a profile bn concludes the queue is disabled *on this host* — and then reconciles,
  # which means removing the real machine's launchd service. BN_HOME, BN_STATE_DIR and
  # BN_NOTES_DIR isolate bn's files; launchd is per-uid and isolates nothing. So a suite that
  # believed it was sandboxed deleted the developer's dispatcher on every run.
  #
  # Pinned rather than auto-detected so the fixture is the same on every machine: the real
  # hostname would have to be in the profile too, and the suite would pass or fail depending on
  # what the laptop was called.
  cat > "$SB/home/config/config.toml" <<EOF
hostname = "bn-shakedown"
[personal]
notes_root = "$SB/notes"
[machines.bn-shakedown]
repos = ["*"]
queue = true
cron = true
[queue]
capture = ["cron"]
coding = 2
research = 1

# No pool warming in the fixture. A fresh state dir has no record of a warm ever having run, so
# every first tick finds one due, enqueues it, and — correctly — lets it own machine-wide admission
# while it runs. That is the product behaving as designed, and it makes the ordering cases below
# non-deterministic: they would be asserting against whatever the warm left room for.
[jobs.only_on]
pool-warm = ["not-this-host"]
keep-warm = ["not-this-host"]
EOF
  ( cd "$SB/repo" && git init -q . && git -c user.email=t@t -c user.name=t commit -q --allow-empty -m init )
  cp "$SRC" "$SB/run/bn"
  # BN_NOTES_DIR belongs in the isolation set: `bn queue add` mints a task per entry, and tasks
  # live under the *notes* root, not the state dir. This sandbox wrote `root = ...`, which bn does
  # not read (the key is `[personal].notes_root`), so the backlog was isolated and every shakedown
  # fixture — `bulk item N`, `express-N`, `racy-N` — was appended to the real `tasks.txt`.
  export BN_HOME="$SB/home" BN_STATE_DIR="$SB/state" BN_NOTES_DIR="$SB/notes" BN_NO_AUTO_TICK=1
  BN="$SB/run/bn"
  cd "$SB/repo" || exit 1
}

# ---------------------------------------------------------------- ordering + execution
fresh
sect "dispatch: priority order, real exit codes, marker proof"
$BN queue add "c-low"  --repo repo --kind cron --priority 1  --shell "touch $SB/marks/low"  >/dev/null
$BN queue add "a-high" --repo repo --kind cron --priority 99 --shell "touch $SB/marks/high" >/dev/null
$BN queue add "b-fail" --repo repo --kind cron --priority 50 --shell "exit 3"               >/dev/null
$BN queue tick >/dev/null 2>&1
ORDER=$($BN queue log --kind dispatch 2>/dev/null | grep -oE 'repo/[a-z-]+' | sed 's|repo/||' | tr '\n' ' ')
is "starts in priority order" "$(echo $ORDER)" "a-high b-fail c-low"
[ -f "$SB/marks/high" ] && [ -f "$SB/marks/low" ] && ok "shell bodies actually ran" || no "shell bodies did not run"
has "a failure reports its real exit code" "$($BN queue log --kind finish 2>&1)" "FAILED (exit 3)"
# Column 5, not 4: the listing grew an `exec` column ("shell"/"job"/"agent") between the status and
# the kind, and this assertion kept reading the old position — so it compared "shell" to "failed"
# and reported a status bug that was never there.
is "failed entry resolves to failed" "$($BN queue list --repo repo --all 2>/dev/null | grep b-fail | awk '{print $5}')" "failed"

sect "event log: the envelope is not shadowed"
KINDS=$(python3 -c "
import json,collections
c=collections.Counter(json.loads(l)['kind'] for l in open('$SB/state/events.jsonl'))
print(' '.join(sorted(c)))")
has "queue_dispatch survives a JSON parse" "$KINDS" "queue_dispatch"
has "queue_finish survives a JSON parse" "$KINDS" "queue_finish"
hasnt "no payload key masquerades as the envelope" "$KINDS" "cron"

# ---------------------------------------------------------------- pause semantics
sect "pause: holds ordinary work, exempts recovery, resume releases"
$BN queue add "held" --repo repo --kind cron --priority 10 --shell "touch $SB/marks/held" >/dev/null
$BN queue pause --reason "testing" >/dev/null 2>&1
$BN queue tick >/dev/null 2>&1
[ -f "$SB/marks/held" ] && no "pause held ordinary work" || ok "pause held ordinary work"
$BN queue add "recov" --repo repo --kind cron --job agent-supervise >/dev/null 2>&1
OUT=$($BN queue tick 2>&1)
has "recovery runs despite the pause" "$OUT" "repo/recov"
is "recovery is queued at the recovery priority" "$($BN queue list --repo repo --all 2>/dev/null | grep recov | awk '{print $1}')" "50"
$BN queue resume >/dev/null 2>&1
$BN queue tick >/dev/null 2>&1
[ -f "$SB/marks/held" ] && ok "resume released the held work" || no "resume did not release"
has "the log records the stall and its clearing" "$($BN queue log --kind tick 2>&1)" "paused"

# ---------------------------------------------------------------- build pool
sect "build pool: concurrency, liveness, TTL, fail-open"
cat >> "$SB/home/config/config.toml" <<EOF

[repo.repo]
verify = "sleep 6"
EOF
CAP=$($BN queue status 2>/dev/null | grep -oE 'builds: [0-9]+/[0-9]+' | cut -d/ -f2)
T0=$(date +%s)
for i in 1 2 3; do ( $BN verify >"$SB/v$i.txt" 2>&1 ) & done
wait
EL=$(( $(date +%s) - T0 ))
[ "$EL" -ge 11 ] && ok "3 builds against a cap of $CAP serialized (${EL}s, one waited)" \
                 || no "builds did not serialize (${EL}s — expected >=12)"
has "the waiter names who it waits for" "$(cat "$SB"/v*.txt)" "waiting for a build slot"
sleep 300 & OWNED=$!
printf '{"held":[{"id":"a","pid":%s,"label":"x","repo":"r","since":%s},{"id":"b","pid":%s,"label":"x","repo":"r","since":%s}]}' \
  "$OWNED" "$(date +%s)" "$OWNED" "$(date +%s)" > "$SB/state/build-slots.json"
has "a live holder holds the pool" "$($BN queue status 2>/dev/null)" "builds: 2/"
printf '{"held":[{"id":"a","pid":%s,"label":"x","repo":"r","since":%s},{"id":"b","pid":%s,"label":"x","repo":"r","since":%s}]}' \
  "$OWNED" "$(( $(date +%s) - 7200 ))" "$OWNED" "$(( $(date +%s) - 7200 ))" > "$SB/state/build-slots.json"
has "a live holder past its TTL is reclaimed" "$($BN queue status 2>/dev/null)" "builds: 0/"
kill $OWNED 2>/dev/null
echo 'garbage {{{' > "$SB/state/build-slots.json"
T0=$(date +%s); $BN verify >/dev/null 2>&1; EL=$(( $(date +%s) - T0 ))
[ "$EL" -lt 12 ] && ok "an unreadable pool fails open (${EL}s)" || no "unreadable pool blocked the build"

# ---------------------------------------------------------------- daemon survival
sect "daemon: never retires into a hole"
printf '#!/bin/sh\nexit 2\n' > "$SB/home/config/bn"; chmod +x "$SB/home/config/bn"
"$SB/run/bn" queue run --interval 2 > "$SB/d1.log" 2>&1 & D=$!
sleep 3
cp "$SRC" "$SB/run/bn.t" && mv "$SB/run/bn.t" "$SB/run/bn"
sleep 5
kill -0 $D 2>/dev/null && ok "survives a successor that cannot dispatch" || no "retired into a hole"
has "and explains why" "$(cat "$SB/d1.log")" "cannot run"
kill $D 2>/dev/null; wait $D 2>/dev/null

cp "$SRC" "$SB/home/config/bn"; cp "$SRC" "$SB/run/bn"
"$SB/run/bn" queue run --interval 2 > "$SB/d2.log" 2>&1 & D=$!
sleep 3
cp "$SRC" "$SB/run/bn.t" && mv "$SB/run/bn.t" "$SB/run/bn"
sleep 5
kill -0 $D 2>/dev/null && { no "a GOOD successor was refused — self-update would break"; kill $D 2>/dev/null; } \
                       || ok "still hands over to a good successor"

sect "daemon: the lock is the liveness signal"
# Liveness is a kernel-held flock now, not a timestamp. That removes the property the old heartbeat
# could never have: there is no window in which a dead dispatcher still reads as alive, because the
# lock is released by the kernel on ANY exit — including a SIGKILL that runs no cleanup.
rm -f "$SB/state/queue-daemon.lock"
"$SB/run/bn" queue run --interval 2 > "$SB/d3.log" 2>&1 & D=$!
sleep 3
OUT=$("$SB/run/bn" queue status 2>&1)
has "a running dispatcher reports live" "$OUT" "dispatcher live"
is "and the lock names it" "$(cat "$SB/state/queue-daemon.lock" 2>/dev/null)" "$D"

# A second dispatcher must be refused, not admitted alongside the first.
OUT2=$("$SB/run/bn" queue run --interval 2 2>&1)
has "a second dispatcher is refused" "$OUT2" "already running"

# SIGKILL: no Drop, no cleanup, and the lock file is left on disk still naming the dead pid. Under
# a heartbeat this read as alive for up to ten minutes, and producers filed work into it the whole
# time. It must read as dead immediately.
kill -9 $D 2>/dev/null; wait $D 2>/dev/null
sleep 1
[ -s "$SB/state/queue-daemon.lock" ] \
  && ok "the killed dispatcher left its lock file behind" \
  || no "expected a leftover lock file naming the dead pid"
OUT3=$("$SB/run/bn" queue status 2>&1)
has "a SIGKILLed dispatcher reads as gone at once" "$OUT3" "dispatcher NOT running"

sect "self-repair: a dead dispatcher is replaced, a bad binary is refused"
# No plist, no unit, no launchctl: bn supervises its own dispatcher, so this is answerable entirely
# inside the sandbox — which is the point. The old version of this section could only run when the
# HOST had no dispatcher loaded, because launchd domains are per-uid and HOME does not sandbox them.
rm -f "$SB/state/queue-dispatcher-failed" "$SB/state/queue-daemon.lock"
unset BN_NO_AUTO_TICK

# An ordinary command finds no dispatcher and starts one.
OUT=$("$SB/run/bn" status 2>&1)
has "an ordinary command starts a missing dispatcher" "$OUT" "started the queue dispatcher"
sleep 2
pgrep -f "$SB/home/config/bn queue run" >/dev/null \
  && ok "and it is actually running" \
  || no "no dispatcher process after the start"

# Kill it; the next command puts a new one back.
pkill -9 -f "$SB/home/config/bn queue run" 2>/dev/null; sleep 1
OUT2=$("$SB/run/bn" status 2>&1)
has "a killed dispatcher is replaced" "$OUT2" "started the queue dispatcher"
sleep 2
pkill -f "$SB/home/config/bn queue run" 2>/dev/null

# A binary that cannot dispatch must be refused BEFORE the spawn. Without that check the child
# exits instantly, the next command finds the lock free and spawns again — a spawn loop paced by
# the user's typing, which is what the init system's throttle used to absorb.
rm -f "$SB/state/queue-dispatcher-failed" "$SB/state/queue-daemon.lock"
printf '#!/bin/sh\nexit 2\n' > "$SB/home/config/bn.bad"; chmod +x "$SB/home/config/bn.bad"
mv "$SB/home/config/bn.bad" "$SB/home/config/bn"
OUT3=$("$SB/run/bn" status 2>&1)
has "refuses a binary that cannot dispatch" "$OUT3" "cannot run"
OUT4=$("$SB/run/bn" status 2>&1)
hasnt "and backs off instead of retrying every command" "$OUT4" "cannot run"
[ -s "$SB/state/queue-daemon.lock" ] && no "a refused start still took the lock" \
                                     || ok "a refused start leaves the machine dispatcher-less, not wedged"
export BN_NO_AUTO_TICK=1

sect "isolation: a sandboxed run touches no init system at all"
# This section used to be two, and both existed because the queue's dispatcher lived in launchd or
# systemd — which are per-uid and per-user, and respect neither BN_HOME nor BN_STATE_DIR. One
# guarded against `bn cron register` reaping the dispatcher's plist as an orphaned cron (the nightly
# self-update did exactly that); the other checksummed the developer's real plist, because running
# this suite used to DELETE it.
#
# bn supervises its own dispatcher now, so the whole class is gone: there is nothing to reap and
# nothing outside the sandbox to damage. What remains is the assertion that this is true.
is "the real launchd plist is untouched" "$(md5 -q "$PLIST" 2>/dev/null || echo none)" "$PLIST_BEFORE"

# Before-and-after, like the plist checksum above, so a unit an OLDER bn left on this machine is
# not mistaken for one this suite just wrote — the only thing under test is what the run changed.
is "bn wrote no systemd unit for the queue" "$(queue_units)" "$UNITS_BEFORE"

RSB=$(mktemp -d); mkdir -p "$RSB/Library/LaunchAgents" "$RSB/home/config" "$RSB/notes"
printf '[personal]\nnotes_root = "%s/notes"\n' "$RSB" > "$RSB/home/config/config.toml"
HOME="$RSB" BN_HOME="$RSB/home" BN_NOTES_DIR="$RSB/notes" BN_NO_AUTO_TICK=1 "$SB/run/bn" status >/dev/null 2>&1
is "and no launchd plist in a fresh HOME" "$(ls "$RSB/Library/LaunchAgents" | wc -l | tr -d ' ')" "0"
rm -rf "$RSB"

printf '\n\033[1m%d passed, %d failed\033[0m\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
