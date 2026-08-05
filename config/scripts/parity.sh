#!/usr/bin/env bash
# Stage C — golden parity harness. Compares the Rust bn against the zsh oracle
# semantically (not byte-for-byte): old writes split note.yaml, new writes single-file
# note.md, so we project both to the same yaml shape via yq and diff the projections.
#
# Both binaries run under an isolated $HOME so they never touch real notes. Requires yq.
set -u

REPO="$HOME/projects/bn"
OLD="$REPO/bn"
NEW="$REPO/target/debug/bn"
PASS=0
FAIL=0

note_yaml() { echo "$1/projects/branch-notes/$2/note.yaml"; }
note_md()   { echo "$1/projects/branch-notes/$2/note.md"; }

# Project a yaml note (split note.yaml) to a stable semantic shape.
proj_split() {
  yq -o=json '{
    todos:     (.todos     // [] | map({id, text, done: (.done // false)})),
    blockers:  (.blockers  // [] | map({id, text})),
    decisions: (.decisions // [] | map({id, text})),
    research:  (.research  // [] | map({id, text}))
  }' "$1" 2>/dev/null
}

# Project a single-file note.md (frontmatter) to the same shape.
proj_single() {
  sed -n '/^---$/,/^---$/p' "$1" | sed '1d;$d' | yq -o=json '{
    todos:     (.todos     // [] | map({id, text, done: (.done // false)})),
    blockers:  (.blockers  // [] | map({id, text})),
    decisions: (.decisions // [] | map({id, text})),
    research:  (.research  // [] | map({id, text}))
  }' 2>/dev/null
}

mk_home() {
  local H; H=$(mktemp -d)
  local d="$H/projects/worktree/$1/$2"
  mkdir -p "$d"
  git -C "$d" init -q
  git -C "$d" checkout -q -b "$2" 2>/dev/null
  echo "$H"
}

run() { local H=$1 bn=$2; shift 2; ( cd "$H/projects/worktree/"* && cd "$(find "$H/projects/worktree" -mindepth 2 -maxdepth 2 -type d | head -1)" && HOME="$H" "$bn" "$@" ); }

# run in a specific repo/branch worktree
runin() { local H=$1 bn=$2 repo=$3 br=$4; shift 4; ( cd "$H/projects/worktree/$repo/$br" && HOME="$H" "$bn" "$@" ); }

check() {
  local name=$1 a=$2 b=$3
  if [[ "$a" == "$b" ]]; then PASS=$((PASS+1)); echo "PASS  $name"; else
    FAIL=$((FAIL+1)); echo "FAIL  $name"; echo "--- old ---"; echo "$a"; echo "--- new ---"; echo "$b"; fi
}

echo "================ add parity ================"
Ho=$(mk_home demo main); Hn=$(mk_home demo main)
for H in "$Ho:$OLD" "$Hn:$NEW"; do
  HH=${H%%:*}; BN=${H##*:}
  runin "$HH" "$BN" demo main add todo "alpha task" >/dev/null
  runin "$HH" "$BN" demo main add decision "use rust" >/dev/null
  runin "$HH" "$BN" demo main add todo "beta task" >/dev/null
  runin "$HH" "$BN" demo main add blocker "the blocker" >/dev/null
  runin "$HH" "$BN" demo main add research "look into yq" >/dev/null
done
check "add: item ids/text/done" "$(proj_split "$(note_yaml "$Ho" demo/main)")" "$(proj_single "$(note_md "$Hn" demo/main)")"

echo "================ done parity ================"
runin "$Ho" "$OLD" demo main done "alpha task" >/dev/null
runin "$Hn" "$NEW" demo main done "alpha task" >/dev/null
check "done: by substring" "$(proj_split "$(note_yaml "$Ho" demo/main)")" "$(proj_single "$(note_md "$Hn" demo/main)")"
runin "$Ho" "$OLD" demo main done t2 >/dev/null
runin "$Hn" "$NEW" demo main done t2 >/dev/null
check "done: by explicit id" "$(proj_split "$(note_yaml "$Ho" demo/main)")" "$(proj_single "$(note_md "$Hn" demo/main)")"

echo "================ todo rollup parity (open-todo text set) ================"
# old `bn todo` human text vs new `bn todo --json`: compare the SET of open todo texts.
old_todo_texts=$(runin "$Ho" "$OLD" demo main todo 2>/dev/null | grep -oE '\] .*$' | sed 's/^] //' | sort)
new_todo_texts=$(runin "$Hn" "$NEW" demo main todo --json 2>/dev/null | yq -p=json '.data[].text' 2>/dev/null | sort)
check "todo: open-todo text set" "$old_todo_texts" "$new_todo_texts"

echo "================ --path parity ================"
# Documented divergence: the new binary nests notes under a per-machine segment
# (branch-notes/<machine>/...), the oracle doesn't. Strip that one segment so the
# logical path is compared, not the host the runner happens to use.
check "path: demo/main" \
  "$(runin "$Ho" "$OLD" demo main --path | sed "s#$Ho##")" \
  "$(runin "$Hn" "$NEW" demo main --path | sed "s#$Hn##" | sed -E 's#(/projects/branch-notes)/[^/]+/#\1/#')"

echo "================ --cat item-set parity ================"
oc=$(runin "$Ho" "$OLD" demo main --cat 2>/dev/null | grep -E '^- ' | grep -v '^- \[x\]' | sort)
nc=$(runin "$Hn" "$NEW" demo main --cat 2>/dev/null | grep -E '^- ' | sort)
check "cat: non-done item lines" "$oc" "$nc"

echo "================================================"
echo "PASS=$PASS FAIL=$FAIL"
rm -rf "$Ho" "$Hn"
[[ $FAIL -eq 0 ]]
