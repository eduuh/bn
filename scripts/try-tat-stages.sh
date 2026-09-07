#!/bin/bash
# Exercise every non-interactive `bn tat` stage the way fzf invokes it — the hidden `--pick-stage`
# callbacks, one call per row kind the sub-picker paints, every guarded refusal, and `kill` on a
# throwaway session. Read-only apart from that session: nothing here leases, removes or queues.
#
# Runs against the bn repo itself, on a machine with the bare+worktree layout and a warm pool.
# stdin is not a TTY, so `hold()` returns at once and a refusal cannot hang the run.
#
#   scripts/try-tat-stages.sh                 # uses target/release/bn
#   B=/path/to/bn scripts/try-tat-stages.sh   # another binary (the installed one, an old build)
#   OUT=/tmp/x scripts/try-tat-stages.sh      # keep every stage's full output under OUT/
set -u
REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
B="${B:-$REPO_DIR/target/release/bn}"
OUT="${OUT:-$(mktemp -d)}"
REPO=bn
[ -x "$B" ] || { echo "build first:  cd $REPO_DIR && cargo build --release -p bn-cli" >&2; exit 1; }
pass=0; fail=0
run() { # run <name> <want-rc|any> <cmd...>
  local name=$1 want=$2; shift 2
  local s; s=$(date +%s%N)
  timeout 90 "$@" >"$OUT/$name.txt" 2>&1 </dev/null
  local rc=$? ms=$(( ($(date +%s%N)-s)/1000000 )) mark=OK
  if [ "$want" != any ] && [ "$rc" != "$want" ]; then mark=FAIL; fi
  if [ $mark = OK ]; then pass=$((pass+1)); else fail=$((fail+1)); fi
  printf '%-4s %-30s rc=%-3s %5dms %3d lines | %s\n' "$mark" "$name" "$rc" "$ms" "$(wc -l <"$OUT/$name.txt")" "$(head -c 100 "$OUT/$name.txt" | tr '\n' ' ')"
}
sublist() { "$B" tat $REPO --pick-stage sublist </dev/null 2>/dev/null; }
first_kind() { sublist | awk -F'|' -v k="$1" '$1==k{print $2; exit}'; }
orphan_session() { "$B" tat --pick-stage list </dev/null 2>/dev/null | awk -F'\t' '$1 ~ /\(session\)/{print $2; exit}'; }

echo "== read-only stages"
run help             0 "$B" tat --help
run list             0 "$B" tat --pick-stage list
run list-json        0 "$B" tat --json --pick-stage list
run list-win         0 "$B" tat --root win --pick-stage list
run refresh          0 "$B" tat --pick-stage refresh
run refresh-repo     0 "$B" tat $REPO --pick-stage refresh
run preview-bare     0 "$B" tat $REPO --pick-stage preview
sess=$(orphan_session); [ -n "$sess" ] && run preview-session 0 "$B" tat "$sess" --pick-stage preview || echo "skip preview-session: no orphan tmux session to preview"
run preview-missing  0 "$B" tat no-such-repo-zz --pick-stage preview
run preview-empty    0 "$B" tat --pick-stage preview
run preview-cs       0 "$B" tat cs:foo/bar/baz --pick-stage preview
run sublist          0 "$B" tat $REPO --pick-stage sublist
run subtick          0 "$B" tat $REPO --pick-stage subtick
run sublist-missing  0 "$B" tat no-such-repo-zz --pick-stage sublist
run sublist-win      0 "$B" tat $REPO --root win --pick-stage sublist
run sublist-empty    0 "$B" tat --pick-stage sublist
run warm-remote      0 "$B" tat $REPO --pick-stage warm-remote
run warm-remote-none 0 "$B" tat --pick-stage warm-remote

echo "== subpreview: one call per row the sub-picker paints (what fzf does per cursor move)"
sublist | while IFS='|' read -r kind payload _; do
  n=$(printf '%s' "$kind-$payload" | tr -c 'A-Za-z0-9_.-' '_' | cut -c1-40)
  run "sp-$n" 0 "$B" tat $REPO --pick-stage subpreview --pick-arg "${kind}~${payload}"
done
run sp-unknown-kind  0 "$B" tat $REPO --pick-stage subpreview --pick-arg "bogus~x"
run sp-no-tilde      0 "$B" tat $REPO --pick-stage subpreview --pick-arg "justtext"
run sp-empty         0 "$B" tat $REPO --pick-stage subpreview
run sp-cmd-missing   0 "$B" tat $REPO --pick-stage subpreview --pick-arg "cmd~no-such-verb"
run sp-remotepr-bad  0 "$B" tat $REPO --pick-stage subpreview --pick-arg "remotepr~999999"
run sp-window-bad    0 "$B" tat $REPO --pick-stage subpreview --pick-arg "window~99"
run sp-poolidle-bad  0 "$B" tat $REPO --pick-stage subpreview --pick-arg "poolidle~99"

echo "== guarded stages: every refusal path (nothing here may change anything)"
run rm-window        0 "$B" tat $REPO --pick-stage rmworktree --pick-arg "window~1"
run rm-cmd           0 "$B" tat $REPO --pick-stage rmworktree --pick-arg "cmd~build-main"
run rm-gone          0 "$B" tat $REPO --pick-stage rmworktree --pick-arg "worktree~no-such-wt-zz"
run rm-empty         0 "$B" tat $REPO --pick-stage rmworktree
pb=$(first_kind poolbranch); [ -n "$pb" ] && run rm-poolbranch 0 "$B" tat $REPO --pick-stage rmworktree --pick-arg "poolbranch~$pb"
# `worktree~main` is only safe against a binary that carries the #1333 guard; older builds remove
# the main checkout. Check the guard exists before asking.
if "$B" tat $REPO --pick-stage rmworktree --pick-arg "worktree~no-such-wt-zz" 2>&1 | grep -q "already gone" \
   && strings "$B" 2>/dev/null | grep -q "is the repo's main checkout"; then
  run rm-main-guarded 0 "$B" tat $REPO --pick-stage rmworktree --pick-arg "worktree~main"
  grep -q "main checkout" "$OUT/rm-main-guarded.txt" && echo "OK   rm-main-guarded: refused" || { echo "FAIL rm-main-guarded: not refused"; fail=$((fail+1)); }
else
  echo "skip rm-main-guarded: this binary predates the main-checkout guard (#1333)"
fi
run warm-win         0 "$B" tat $REPO --root win --pick-stage warmpool
run warm-tagged      0 "$B" tat $REPO@win --pick-stage warmpool
run rel-worktree     0 "$B" tat $REPO --pick-stage releasepool --pick-arg "worktree~main"
run rel-nolease      0 "$B" tat $REPO --pick-stage releasepool --pick-arg "poolbranch~no-such-branch-zz"
run rel-empty        0 "$B" tat $REPO --pick-stage releasepool
run runcmd-window    0 "$B" tat $REPO --pick-stage runcmd --pick-arg "window~1"
run runcmd-sep       0 "$B" tat $REPO --pick-stage runcmd --pick-arg "cmdsep~"
run runcmd-nosuch    0 "$B" tat $REPO --pick-stage runcmd --pick-arg "cmd~no-such-verb"
run runcmd-empty     0 "$B" tat $REPO --pick-stage runcmd
run runcmd-script-gone 0 "$B" tat $REPO --pick-stage runcmd --pick-arg "cmd~script:no-such-script-zz"
run runcmd-pool-win  0 "$B" tat $REPO --root win --pick-stage runcmd --pick-arg "cmd~pool-warm"
run away-place       0 "$B" tat $REPO --pick-stage runcmd-away --pick-arg "worktree~main"
run away-nosuch      0 "$B" tat $REPO --pick-stage runcmd-away --pick-arg "cmd~no-such-verb"

echo "== direct open of an unknown name (TMUX unset so the attach fails harmlessly)"
run open-unknown     0 env -u TMUX -u TMUX_PANE "$B" tat no-such-repo-zz
grep -q "No such project" "$OUT/open-unknown.txt" && echo "OK   open-unknown: says No such project" || { echo "FAIL open-unknown: no message"; fail=$((fail+1)); }
tmux has-session -t '=no-such-repo-zz' 2>/dev/null && { echo "FAIL open-unknown: created a session"; fail=$((fail+1)); tmux kill-session -t '=no-such-repo-zz'; } || echo "OK   open-unknown: no stray session"

echo "== kill: a throwaway session only"
tmux new-session -d -s tat-zz-kill -c /tmp 2>/dev/null
run kill-throwaway   0 "$B" tat tat-zz-kill --pick-stage kill
tmux has-session -t '=tat-zz-kill' 2>/dev/null && { echo "FAIL kill-throwaway: session still alive"; fail=$((fail+1)); tmux kill-session -t '=tat-zz-kill'; } || echo "OK   kill-throwaway: session gone"
run kill-missing     0 "$B" tat no-such-session-zz --pick-stage kill
run kill-empty       0 "$B" tat --pick-stage kill

echo "== pass=$pass fail=$fail  (outputs under $OUT)"
[ $fail -eq 0 ]
