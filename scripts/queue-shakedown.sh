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

sect "daemon: heartbeat is the liveness signal"
rm -f "$SB/state/queue-daemon.heartbeat"
"$SB/run/bn" queue run --interval 2 > "$SB/d3.log" 2>&1 & D=$!
# The daemon beats on start and then every BEAT_SECS (30) — deliberately well inside the 600s TTL,
# so one missed write is not "dead". This used to sample twice six seconds apart and require the
# value to have CHANGED, which no correct dispatcher does: it asserted a thirty-second timer on a
# six-second window. What is actually claimed is that a running daemon writes a fresh heartbeat,
# and that it stops when it dies — the latter is the case below, which was always the real test.
sleep 3; A=$(cat "$SB/state/queue-daemon.heartbeat" 2>/dev/null)
AGE=$(( $(date +%s) - ${A:-0} ))
[ -n "$A" ] && [ "$AGE" -ge 0 ] && [ "$AGE" -lt 120 ] \
  && ok "the daemon beats while running" \
  || no "no heartbeat (value '${A:-}', age ${AGE}s)"
kill $D 2>/dev/null; wait $D 2>/dev/null
sleep 3; C=$(cat "$SB/state/queue-daemon.heartbeat" 2>/dev/null)
# Compared against the reading taken while it was alive: a dead daemon writes no further beats, so
# the value must be exactly what it was. (This used to compare against a second live sample; with
# the beat at 30s both samples were identical anyway, so the assertion held for the wrong reason.)
is "the heartbeat freezes when it dies" "$A" "$C"

sect "self-repair: detects a dropped service, refuses a bad binary"
# Hermetic: its own HOME, so the unit file it reasons about is one this test created. Depending on
# the real machine's plist made this fail the moment the reconcile bug deleted it — and a test that
# needs the host to be healthy cannot be the thing that tells you it is not.
HSB=$(mktemp -d); mkdir -p "$HSB/Library/LaunchAgents"
touch "$HSB/Library/LaunchAgents/com.bn.queue.plist"
printf '#!/bin/sh\nexit 2\n' > "$SB/home/config/bn"; chmod +x "$SB/home/config/bn"
rm -f "$SB/state/queue-tick-install-failed" "$SB/state/queue-daemon.heartbeat"
# A FRESH decision file: the state that hid this failure before the heartbeat existed.
echo "{\"at\":$(date +%s),\"outcome\":\"empty\",\"dry_run\":false,\"pending\":0,\"started\":0,\"lanes\":[],\"kinds\":[],\"deferred\":[],\"dispatched\":[]}" \
  > "$SB/state/queue-decision.json"
unset BN_NO_AUTO_TICK
# `service_loaded` asks launchctl, and launchd domains are per-uid — HOME does not sandbox them.
# So these two can only run when the host's own dispatcher is NOT loaded; otherwise bn correctly
# concludes the service is fine and never reaches the branch under test. Skipped out loud rather
# than asserted conditionally: a test that quietly changes meaning with the host is worse than one
# that says it did not run.
if launchctl print "gui/$(id -u)/com.bn.queue" >/dev/null 2>&1; then
  printf '  \033[33m—\033[0m skipped (host dispatcher is loaded; launchd is not sandboxable by HOME): %s\n' \
    "detects installed-but-not-running, refuses a bad binary"
else
  OUT=$(HOME="$HSB" "$SB/run/bn" status 2>&1)
  has "detects installed-but-not-running" "$OUT" "installed but not running"
  has "refuses to install a binary that cannot dispatch" "$OUT" "cannot run"
  OUT2=$(HOME="$HSB" "$SB/run/bn" status 2>&1)
  hasnt "backs off instead of re-diagnosing every command" "$OUT2" "installed but not running"
fi
rm -f "$SB/state/queue-tick-install-failed"; date +%s > "$SB/state/queue-daemon.heartbeat"
OUT3=$(HOME="$HSB" "$SB/run/bn" status 2>&1)
hasnt "silent while the daemon is beating" "$OUT3" "installed but not running"
is "it never wrote a unit for a binary that cannot dispatch" "$(ls "$HSB/Library/LaunchAgents" | wc -l | tr -d ' ')" "1"
export BN_NO_AUTO_TICK=1
rm -rf "$HSB"

sect "cron reconcile: the dispatcher is not a cron"
# `bn cron register` reaps launchd agents whose job is in no manifest. The dispatcher's label maps
# to the job name "queue", which is in none — so without an explicit exclusion the nightly
# install.sh (via self-update) deleted the dispatcher outright. --dry-run, so nothing is touched.
RSB=$(mktemp -d); mkdir -p "$RSB/Library/LaunchAgents" "$RSB/home/config" "$RSB/notes"
printf '[personal]\nnotes_root = "%s/notes"\n' "$RSB" > "$RSB/home/config/config.toml"
touch "$RSB/Library/LaunchAgents/com.bn.queue.plist"
ORPH=$(HOME="$RSB" BN_HOME="$RSB/home" BN_NOTES_DIR="$RSB/notes" BN_NO_AUTO_TICK=1 "$SB/run/bn" cron register --dry-run --json 2>/dev/null |
       python3 -c "import json,sys; print(','.join(json.load(sys.stdin).get('data',{}).get('orphans',[])))" 2>/dev/null)
hasnt "the reconcile does not reap the queue dispatcher" "$ORPH" "queue"
rm -rf "$RSB"

sect "isolation"
is "the real launchd plist is untouched" "$(md5 -q "$PLIST" 2>/dev/null || echo none)" "$PLIST_BEFORE"

printf '\n\033[1m%d passed, %d failed\033[0m\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
