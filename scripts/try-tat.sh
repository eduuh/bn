#!/bin/bash
# Drive the real `bn tat` picker — fzf and all — inside a tmux window with send-keys, and assert on
# the screen plus real state (dirs, `bn wt pool status`, tmux windows, the event log). This is the
# only way to reach the keypress → `execute`/`reload` path and the parent's `--expect` handling
# (ctrl-o / ctrl-s menus), which no unit test and no `--pick-stage` call can.
#
# Every case creates and removes its own fixtures (`tat-zz-*` branches, leases, sessions, a
# repo-tier script), and no key is sent unless the picker is confirmed open — a stray ctrl-d in a
# shell is a logout. Selecting a place row switches the bn session's active window, which is what
# the picker is for; the harness switches back to the window it was started from after each case.
#
# Needs: tmux (run me inside it), fzf, the bn repo in the bare+worktree layout with a warm pool,
# and at least two idle slots. Leaves nothing behind on success; on a failure, read the residue
# sweep at the end and `bn wt pool status --repo bn`.
#
#   scripts/try-tat.sh                     # every case, against target/release/bn
#   scripts/try-tat.sh menu-cmd cmd-away   # just these
#   B=/path/to/bn scripts/try-tat.sh       # another binary
set -u
REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
B="${B:-$REPO_DIR/target/release/bn}"; B=$(realpath "$B") || exit 2
REPO=bn
BARE=$(git -C "$REPO_DIR" rev-parse --path-format=absolute --git-common-dir)
WT="$(dirname "$(dirname "$BARE")")/worktree/$REPO"
NOTE_DIR=$(bn --path 2>/dev/null); MACHINE_DIR=$(dirname "$(dirname "$NOTE_DIR")")
JOBS="$(dirname "$MACHINE_DIR")/jobs/$REPO"; EVENTS="$MACHINE_DIR/state/events.jsonl"
[ -x "$B" ] || { echo "build first:  cd $REPO_DIR && cargo build --release -p bn-cli" >&2; exit 1; }
[ -d "$WT/_pool" ] || { echo "no warm pool at $WT/_pool — this harness needs one" >&2; exit 1; }
USER_WIN=$(tmux display -p -t "${TMUX_PANE:?run me inside tmux}" '#{window_id}')
SCRATCH=$(mktemp -d)
pass=0; fail=0; W=""
ok(){ pass=$((pass+1)); echo "OK   $*"; }
bad(){ fail=$((fail+1)); echo "FAIL $*"; }
say(){ echo; echo "== $*"; }
alive(){ tmux list-panes -t "$W" >/dev/null 2>&1; }
screen(){ tmux capture-pane -p -J -t "$W" 2>/dev/null; }
show(){ screen | grep -v '^ *$' | tail -"${1:-6}" | cut -c1-140 | sed 's/^/   | /'; }
key(){ tmux send-keys -t "$W" "$@"; }
typ(){ tmux send-keys -t "$W" -l -- "$1"; }
wait_for(){ local re=$1 t=${2:-20} i=0; until screen | grep -qE -- "$re"; do alive || { echo "   drive window is gone"; return 1; }; i=$((i+1)); if [ $i -ge $((t*5)) ]; then echo "   timeout ${t}s waiting for /$re/"; show 8; return 1; fi; sleep 0.2; done; }
wait_gone(){ local re=$1 t=${2:-20} i=0; while screen | grep -qE -- "$re"; do i=$((i+1)); if [ $i -ge $((t*5)) ]; then echo "   timeout ${t}s waiting for /$re/ to go"; show 8; return 1; fi; sleep 0.2; done; }
wait_until(){ local t=$1 i=0; shift; until "$@"; do i=$((i+1)); [ $i -ge $((t*5)) ] && return 1; sleep 0.2; done; }
wins(){ tmux list-windows -t "=$REPO" -F '#{window_id} #{window_name}'; }
kill_new_wins(){ comm -13 <(echo "$1" | sort) <(wins | sort) | while read -r id name; do [ "$id" = "$W" ] && continue; tmux kill-window -t "$id" && echo "   closed window $name ($id)"; done; }
restore_user(){ tmux select-window -t "$USER_WIN" 2>/dev/null; }
slot_of(){ bn wt pool status --repo $REPO 2>/dev/null | grep -E "\] leased  $1( |\$)" | sed -E 's/^ *\[([0-9]+)\].*/\1/' | head -1; }
leased(){ [ -n "$(slot_of "$1")" ]; }
release_branch(){ local b=$1 n; tmux kill-window -t "=$REPO:$b" 2>/dev/null; n=$(slot_of "$b")
  if [ -n "$n" ]; then bn wt pool return "$n" >/dev/null 2>&1 || bn wt pool return "$n" --force >/dev/null 2>&1; fi
  git --git-dir="$BARE" branch -D "$b" >/dev/null 2>&1; ! leased "$b"; }
sublist(){ "$B" tat $REPO --pick-stage sublist </dev/null 2>/dev/null; }
first_kind(){ sublist | awk -F'|' -v k="$1" '$1==k{print $2; exit}'; }
# The driving window: one zsh we keep sending picker invocations to. EOF and XON/XOFF disarmed so a
# key that misses the picker cannot log the shell out or freeze it.
W=$(tmux new-window -d -t "=$REPO:" -n tat-drive -P -F '#{pane_id}' -c "$REPO_DIR"); [ -n "$W" ] || { echo "tmux refused the window"; exit 2; }
key "setopt IGNORE_EOF 2>/dev/null; set -o ignoreeof 2>/dev/null; stty -ixon; clear" Enter
picker(){ tmux clear-history -t "$W"; key "clear; $B tat $*; echo \"--- drive exited \$? ---\"" Enter; }
picker_done(){ wait_for '--- drive exited [0-9]+ ---' "${1:-20}"; }
SUB_HDR='ctrl-d del'; PRJ_HDR='Enter: open \| ctrl-r: refresh'
open_prj(){ picker; wait_for "$PRJ_HDR" 30 || { bad "project picker did not open"; return 1; }; }
open_sub(){ picker $REPO; wait_for "$SUB_HDR" 30 || { bad "sub-picker did not open"; return 1; }; }
close_picker(){ key C-u; key Escape; picker_done "${1:-30}" || bad "picker did not exit on Esc"; restore_user; }
finish(){ restore_user; tmux kill-window -t "$W" 2>/dev/null; rm -rf "$SCRATCH"; echo; echo "== pass=$pass fail=$fail"; [ $fail -eq 0 ]; }
trap finish EXIT
want(){ [ $# -eq 0 ] && return 0; for c in "$@"; do [ "$c" = "$CASE" ] && return 0; done; return 1; }
ALL=("$@")

CASE=smoke; if want "${ALL[@]}"; then say "$CASE: project picker opens and Esc closes it"
if open_prj; then ok "project picker rendered"; close_picker && ok "picker exited on Esc"; fi; fi

CASE=prj-refresh; if want "${ALL[@]}"; then say "$CASE: ctrl-r reloads the project list"
if open_prj; then key C-r; wait_gone "\[\*[0-9]+w\] $REPO" 3 >/dev/null; wait_for "\[\*[0-9]+w\] $REPO" 40 && ok "list back after refresh" || bad "list missing after refresh"; close_picker; fi; fi

CASE=prj-kill; if want "${ALL[@]}"; then say "$CASE: ctrl-x kills a throwaway session from its row, list reloads without it"
tmux new-session -d -s tat-zz-sess -c /tmp
if open_prj; then typ "tat-zz-sess"; wait_for ' 1/[0-9]+' && ok "row filtered" || bad "row not found"
  key C-x; wait_until 15 bash -c '! tmux has-session -t =tat-zz-sess 2>/dev/null' && ok "session killed by ctrl-x" || bad "session survived ctrl-x"
  wait_for ' 0/[0-9]+' 10 && ok "reloaded list no longer has the row" || bad "row still listed"; close_picker; fi
tmux kill-session -t '=tat-zz-sess' 2>/dev/null; fi

CASE=sub-preview-toggle; if want "${ALL[@]}"; then say "$CASE: ctrl-p hides and shows the side pane"
if open_sub; then wait_for '╭' 10 && ok "preview visible" || bad "no preview box"
  key C-p; wait_gone '╭' 5 && ok "preview hidden" || bad "preview still shown"
  key C-p; wait_for '╭' 10 && ok "preview back" || bad "preview not back"; close_picker; fi; fi

CASE=sub-del-refused; if want "${ALL[@]}"; then say "$CASE: ctrl-d on a window row refuses, hold() needs Enter, list returns"
if open_sub; then key C-d; wait_for "window row can't be removed" 10 && ok "refusal shown" || bad "no refusal"
  wait_for 'Press Enter or Esc' 5 && ok "hold prompt shown" || bad "no hold prompt"; key Enter
  wait_for "$SUB_HDR" 10 && ok "back in the list" || bad "list did not return"; close_picker; fi; fi

CASE=sub-del-ok; if want "${ALL[@]}"; then say "$CASE: ctrl-d removes a clean fixture worktree by keypress"
git --git-dir="$BARE" worktree add -b tat-zz-tmp "$WT/tat-zz-tmp" origin/main >/dev/null 2>&1 && ok "fixture worktree" || bad "fixture failed"
if open_sub; then typ "tat-zz-tmp"; wait_for ' 1/[0-9]+' && ok "row filtered" || bad "row not found"
  key C-d; wait_until 30 bash -c "[ ! -d '$WT/tat-zz-tmp' ]" && ok "worktree dir gone" || bad "worktree dir remains"
  wait_for ' 0/[0-9]+' 15 && ok "row gone after reload" || bad "row still listed"; close_picker; fi
git --git-dir="$BARE" worktree remove --force "$WT/tat-zz-tmp" 2>/dev/null; git --git-dir="$BARE" branch -D tat-zz-tmp >/dev/null 2>&1; fi

CASE=sub-warm; if want "${ALL[@]}"; then say "$CASE: ctrl-w spawns the pool-warm window and the list reloads"
before=$(wins); n0=$(echo "$before" | wc -l)
if open_sub; then key C-w; wait_until 20 bash -c "[ \$(tmux list-windows -t '=$REPO' | wc -l) -gt $n0 ]" && ok "a new window appeared" || bad "no new window"
  wait_for "$SUB_HDR" 15 && ok "list back" || bad "list did not return"; close_picker; fi
sleep 2; kill_new_wins "$before"; fi

CASE=sub-release; if want "${ALL[@]}"; then say "$CASE: ctrl-r releases a leased slot by keypress"
before=$(wins); bn wt pool get tat-zz-lease --holder tat-harness >/dev/null 2>&1; sleep 1; kill_new_wins "$before" >/dev/null
leased tat-zz-lease && ok "fixture lease on slot $(slot_of tat-zz-lease)" || bad "fixture lease failed"
if open_sub; then typ "tat-zz-lease"; wait_for ' 1/[0-9]+' && ok "row filtered" || bad "row not found"
  key C-r; wait_until 30 bash -c "! bn wt pool status --repo $REPO | grep -qE '\] leased  tat-zz-lease( |\$)'" && ok "slot back to idle" || bad "still leased"; close_picker; fi
release_branch tat-zz-lease; fi

CASE=menu-worktree; if want "${ALL[@]}"; then say "$CASE: ctrl-o on a worktree row opens the action menu; Esc returns to the picker"
wrow=$(first_kind worktree); wbr=$( [ -n "$wrow" ] && git -C "$WT/$wrow" branch --show-current 2>/dev/null)
if [ -z "$wbr" ]; then echo "   skip: no idle worktree row to act on"; elif open_sub; then typ "$wrow"; wait_for ' 1/[0-9]+' || bad "row not found"
  key C-o; wait_for "$wbr" 15 && ok "menu opened with the branch as header" || bad "no action menu"
  key Escape; wait_for "$SUB_HDR" 15 && ok "back in the picker after Esc" || bad "picker did not reopen"; close_picker; fi; fi

CASE=menu-cmd; if want "${ALL[@]}"; then say "$CASE: ctrl-o on a command row opens the queue-time menu; Esc returns"
if open_sub; then typ "Fetch + refresh"; wait_for ' 1/[0-9]+' || bad "row not found"
  key C-o; wait_for 'Run it now|Run it when' 15 && ok "queue-time menu opened" || bad "no queue menu"
  key Escape; wait_for "$SUB_HDR" 15 && ok "back in the picker" || bad "picker did not reopen"; close_picker; fi; fi

CASE=menu-settings; if want "${ALL[@]}"; then say "$CASE: Enter on Queue settings… opens the settings menu; Esc returns"
if open_sub; then typ "Queue settings"; wait_for ' 1/[0-9]+' || bad "row not found"
  key Enter; wait_for 'queue settings' 15 && ok "settings menu opened" || bad "no settings menu"
  key Escape; wait_for "$SUB_HDR" 15 && ok "back in the picker" || bad "picker did not reopen"; close_picker; fi; fi

CASE=cmd-away; if want "${ALL[@]}"; then say "$CASE: ctrl-s files fetch-prs for away (row shows ⏾); Enter on it un-queues (row back to ▸)"
if open_sub; then typ "Fetch + refresh"; wait_for ' 1/[0-9]+' || bad "row not found"
  key C-s; wait_for '⏾ +Fetch' 20 && ok "row shows the away glyph after reopen" || bad "no away glyph"
  typ "Fetch + refresh"; wait_for ' 1/[0-9]+'; key Enter; wait_for '▸ +Fetch' 20 && ok "row back to idle after un-queue" || bad "row not idle"; close_picker; fi; fi

CASE=typed-lease; if want "${ALL[@]}"; then say "$CASE: typing a new branch name and Enter leases a slot and opens its window"
before=$(wins)
if open_sub; then typ "tat-zz-typed"; wait_for ' 0/[0-9]+'; key Enter; picker_done 60 || bad "picker did not finish"
  wait_until 30 bash -c "bn wt pool status --repo $REPO | grep -qE '\] leased  tat-zz-typed( |\$)'" && ok "slot leased to tat-zz-typed" || bad "no lease"
  wins | grep -q ' tat-zz-typed$' && ok "window opened" || bad "no window"; fi
restore_user; release_branch tat-zz-typed && ok "released" || bad "release failed"; kill_new_wins "$before" >/dev/null; fi

CASE=idle-prompt; if want "${ALL[@]}"; then say "$CASE: Enter on an idle slot row with an empty query prompts for a branch"
before=$(wins); idx=$(sublist | awk -F'|' '$1=="poolidle"{print NR; exit}')
[ -n "$idx" ] && ok "first idle row is #$idx" || bad "no idle row"
if [ -n "$idx" ] && open_sub; then for _ in $(seq 2 "$idx"); do key Down; done; key Enter
  wait_for 'branch to create:' 15 && ok "prompt shown" || bad "no prompt"; typ "tat-zz-prompt"; key Enter; picker_done 60 || bad "picker did not finish"
  wait_until 30 bash -c "bn wt pool status --repo $REPO | grep -qE '\] leased  tat-zz-prompt( |\$)'" && ok "leased from the prompt" || bad "no lease"
  restore_user; release_branch tat-zz-prompt && ok "released" || bad "release failed"; kill_new_wins "$before" >/dev/null
  if open_sub; then for _ in $(seq 2 "$idx"); do key Down; done; key Enter
    wait_for 'branch to create:' 15; key Enter; picker_done 30 && ok "empty answer just exits" || bad "picker hung on empty answer"
    bn wt pool status --repo $REPO | grep -q "tat-zz" && bad "stray lease" || ok "no stray lease"; fi; fi
restore_user; fi

CASE=remote-pr; if want "${ALL[@]}"; then say "$CASE: selecting a remote PR row checks its branch out into a slot"
row=$(sublist | grep '^remotepr|' | head -1); pr=$(echo "$row" | cut -d'|' -f2); br=$(echo "$row" | sed -E 's/.*PR: ([^ ]+) .*/\1/')
[ -n "$pr" ] && ok "using PR #$pr ($br)" || echo "   skip: no remote-only PR row to test with"
before=$(wins)
if [ -n "$pr" ] && open_sub; then typ "#$pr"; wait_for ' 1/[0-9]+' || bad "row not found"; key Enter; picker_done 120 || bad "picker did not finish"
  wait_until 30 bash -c "bn wt pool status --repo $REPO | grep -qE '\] leased  $br( |\$)'" && ok "PR branch leased into slot $(slot_of "$br")" || bad "PR branch not leased"
  restore_user; release_branch "$br" && ok "released and local branch deleted" || bad "release failed"; fi
kill_new_wins "$before" >/dev/null; fi

CASE=script-row; if want "${ALL[@]}"; then say "$CASE: Enter on a repo-tier script row runs it"
mark=$SCRATCH/tat-zz-echo.ran; mkdir -p "$JOBS"; printf '#!/bin/sh\necho "tat-zz-echo ran"\ntouch %s\n' "$mark" > "$JOBS/tat-zz-echo.sh"; chmod +x "$JOBS/tat-zz-echo.sh"
sublist | grep -q 'cmd|script:tat-zz-echo|' && ok "script row appears" || bad "script row missing"
before=$(wins)
if open_sub; then typ "tat-zz-echo"; wait_for ' 1/[0-9]+' || bad "row not found"; key Enter
  # A direct start runs it within seconds. When a dispatcher pass holds the tick lock (express work
  # runs inline, bounded at five minutes) the verb is filed instead and the next pass runs it — so
  # wait for either, and say which happened.
  t0=$(date +%s); wait_until 300 test -f "$mark" && ok "script ran after $(( $(date +%s)-t0 ))s" || { bad "script did not run within 300s"; show 14; }
  how=$(grep -a '"id":"tat-zz-echo-bn"' "$EVENTS" 2>/dev/null | grep -a queue_dispatch | tail -1 | grep -q "started by hand" && echo direct || echo "queue fallback (a pass held the tick lock)")
  echo "   path: $how"
  wait_for "$SUB_HDR" 10; close_picker; fi
rm -f "$JOBS/tat-zz-echo.sh"; sleep 2; kill_new_wins "$before" >/dev/null; fi

say "residue sweep"
git --git-dir="$BARE" branch --list 'tat-zz*' | grep -q . && bad "tat-zz branches remain" || ok "no tat-zz branches"
bn wt pool status --repo $REPO | grep -q tat-zz && bad "tat-zz lease remains" || ok "no tat-zz lease"
wins | grep -v " $W " | grep -q tat-zz && bad "tat-zz window remains" || ok "no tat-zz window"
